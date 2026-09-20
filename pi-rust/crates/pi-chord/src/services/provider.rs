//! Provider side of the remote service boundary, ported from
//! `packages/chord/src/services/provider.ts`.
//!
//! [`RemoteServiceProvider`] owns the allowlisted catalogue, the live singleton and keyed instances,
//! and the subscribers that receive lifecycle updates. [`RemoteServiceEndpoint`] adapts it to a
//! caller: it recognises the three reserved control calls (catalogue, subscribe, unsubscribe) and
//! forwards everything else to [`RemoteServiceProvider::invoke`].
//!
//! Two structural differences from upstream, both forced by the language:
//!
//! * Upstream classifies a JavaScript object into "members" by reflection. Rust has no reflection,
//!   so [`ServiceImplementation`] is built explicitly — but it still rejects an empty implementation
//!   and still distinguishes methods from replicated states, and `provide`/`replace` still refuse a
//!   replacement that changes the member shape.
//! * Upstream methods receive a trailing `Context` argument. This port passes the context as a
//!   separate [`RemoteMethod::call`] parameter, which is a reshaping with identical observable
//!   semantics (arguments stay strict JSON, the context stays the last thing the method sees).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::context::{background_context, Context};
use crate::json::JsonValue;
use crate::types::{Service, ServiceCatalogueEntry, ServiceInstanceAddress, ServiceMode};

use super::errors::{RemoteServiceErrorCode, ServiceError};
use super::state::ReplicatedStateMember;
use super::wire::{
    create_service_catalogue_call, decode_service_control_call, service_catalogue_to_json,
    InstanceSnapshot, MemberSnapshot, ProviderUpdate, ServiceCall, ServiceControlCall,
    ServiceMemberKind, ServiceProviderUpdate, ServiceSubscriptionSnapshot, SubscriptionSnapshot,
};

/// A callable member of a remote service implementation.
pub trait RemoteMethod: Send + Sync {
    /// Applies `args` with `context`. The result must be strict JSON.
    fn call(&self, args: &[JsonValue], context: &Context) -> Result<JsonValue, ServiceError>;
}

struct FnMethod<F>(F);

impl<F> RemoteMethod for FnMethod<F>
where
    F: Fn(&[JsonValue], &Context) -> Result<JsonValue, ServiceError> + Send + Sync + 'static,
{
    fn call(&self, args: &[JsonValue], context: &Context) -> Result<JsonValue, ServiceError> {
        (self.0)(args, context)
    }
}

/// One named member of a [`ServiceImplementation`].
#[derive(Clone)]
pub enum ServiceMember {
    /// A callable method.
    Method(Arc<dyn RemoteMethod>),
    /// A replicated state.
    State(Arc<dyn ReplicatedStateMember>),
}

impl std::fmt::Debug for ServiceMember {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("ServiceMember")
            .field(&self.kind())
            .finish()
    }
}

impl ServiceMember {
    /// The member kind.
    pub fn kind(&self) -> ServiceMemberKind {
        match self {
            ServiceMember::Method(_) => ServiceMemberKind::Method,
            ServiceMember::State(_) => ServiceMemberKind::State,
        }
    }
}

/// A remote service implementation: named methods and replicated states.
///
/// This is the Rust counterpart of upstream's reflected implementation object. An implementation
/// with no members is rejected by [`RemoteServiceProvider::provide`] / `spawn`, exactly like
/// upstream's `classifyRemoteServiceImplementation`.
#[derive(Clone, Default)]
pub struct ServiceImplementation {
    members: BTreeMap<String, ServiceMember>,
}

impl std::fmt::Debug for ServiceImplementation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceImplementation")
            .field("members", &self.members.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ServiceImplementation {
    /// An implementation with no members.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a method member. Replaces an existing member with the same name.
    pub fn method<F>(mut self, name: impl Into<String>, method: F) -> Self
    where
        F: Fn(&[JsonValue], &Context) -> Result<JsonValue, ServiceError> + Send + Sync + 'static,
    {
        self.members.insert(
            name.into(),
            ServiceMember::Method(Arc::new(FnMethod(method))),
        );
        self
    }

    /// Adds a replicated-state member. Replaces an existing member with the same name.
    pub fn state<S>(mut self, name: impl Into<String>, state: Arc<S>) -> Self
    where
        S: ReplicatedStateMember + 'static,
    {
        self.members
            .insert(name.into(), ServiceMember::State(state));
        self
    }

    /// Adds an already-erased replicated-state member.
    pub fn shared_state(
        mut self,
        name: impl Into<String>,
        state: Arc<dyn ReplicatedStateMember>,
    ) -> Self {
        self.members
            .insert(name.into(), ServiceMember::State(state));
        self
    }

    /// Whether the implementation has no members.
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// The member names, in sorted order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.members.keys().map(String::as_str)
    }

    /// Looks a member up by name.
    pub fn member(&self, name: &str) -> Option<&ServiceMember> {
        self.members.get(name)
    }

    /// The member-name/kind shape, used to validate singleton replacement.
    pub fn shape(&self) -> MemberShape {
        self.members
            .iter()
            .map(|(name, member)| (name.clone(), member.kind()))
            .collect()
    }

    fn assert_valid(&self, service_id: &str) -> Result<(), ServiceError> {
        if self.members.keys().any(String::is_empty) {
            return Err(ServiceError::message(format!(
                "Remote service {service_id} has a member with an empty name"
            )));
        }
        if self.members.is_empty() {
            return Err(ServiceError::message(format!(
                "Remote service {service_id} has no members"
            )));
        }
        Ok(())
    }
}

/// The member-name/kind shape of an implementation.
pub type MemberShape = BTreeMap<String, ServiceMemberKind>;

/// Publishes one subscription update back to a remote consumer.
///
/// Upstream allows a promise and swallows its rejection; this port is synchronous and infallible,
/// so a publisher that needs to report a failure does so through its own error sink.
pub type ServiceUpdatePublisher = Arc<dyn Fn(&str, &ServiceProviderUpdate, &Context) + Send + Sync>;

/// A provider-side subscription listener.
///
/// Upstream allows the listener to return a promise whose rejection is collected by the publisher.
/// This port returns the failure synchronously so [`RemoteServiceProvider`] can aggregate it exactly
/// like upstream aggregates rejected promises.
pub type ServiceProviderListener =
    Arc<dyn Fn(&ServiceProviderUpdate, &Context) -> Result<(), ServiceError> + Send + Sync>;

/// A live subscription created by [`RemoteServiceProvider::subscribe`].
pub trait ServiceSubscription: Send + Sync {
    /// The snapshot taken when the subscription was created.
    fn snapshot(&self) -> ServiceSubscriptionSnapshot;

    /// Starts delivering buffered updates.
    fn activate(&self) -> Result<(), ServiceError>;

    /// Closes the subscription. Idempotent.
    fn close(&self);
}

/// A catalogue entry a provider can be constructed with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceProviderEntry {
    /// The service id.
    pub service_id: String,
    /// The mode the service is provided with.
    pub mode: ServiceMode,
    /// Whether the service is process-local and therefore unpublishable.
    pub local: bool,
}

impl ServiceProviderEntry {
    /// A remote entry for a service id.
    pub fn new(service_id: impl Into<String>, mode: ServiceMode) -> Self {
        Self {
            service_id: service_id.into(),
            mode,
            local: false,
        }
    }

    /// A singleton entry for `service`.
    pub fn singleton<T>(service: &Service<T>) -> Self {
        Self {
            service_id: service.id().to_owned(),
            mode: ServiceMode::Singleton,
            local: service.is_local(),
        }
    }

    /// A keyed entry for `service`.
    pub fn keyed<T>(service: &Service<T>) -> Self {
        Self {
            service_id: service.id().to_owned(),
            mode: ServiceMode::Keyed,
            local: service.is_local(),
        }
    }
}

struct ProviderInstance {
    address: Option<ServiceInstanceAddress>,
    implementation: ServiceImplementation,
    remove_member_listeners: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    active: AtomicBool,
}

impl ProviderInstance {
    fn deactivate(&self) {
        self.active.store(false, Ordering::SeqCst);
        for remove in self.remove_member_listeners.lock().drain(..) {
            remove();
        }
    }

    fn snapshot(&self) -> InstanceSnapshot<crate::delta::Op> {
        let members = self
            .implementation
            .members
            .iter()
            .map(|(name, member)| match member {
                ServiceMember::Method(_) => MemberSnapshot::Method { name: name.clone() },
                ServiceMember::State(state) => MemberSnapshot::State {
                    name: name.clone(),
                    sequence: state.sequence(),
                    ops: vec![crate::delta::Op::Replace(state.snapshot_value())],
                },
            })
            .collect();
        InstanceSnapshot {
            instance: self.address.clone(),
            members,
        }
    }
}

struct ProviderSubscriber {
    listener: ServiceProviderListener,
    buffer: Mutex<Vec<(ServiceProviderUpdate, Context)>>,
    active: AtomicBool,
    terminated: AtomicBool,
    closed: AtomicBool,
}

struct Registration {
    service_id: String,
    mode: ServiceMode,
    singleton: Mutex<Option<Arc<ProviderInstance>>>,
    singleton_shape: Mutex<Option<MemberShape>>,
    instances: Mutex<BTreeMap<String, Arc<ProviderInstance>>>,
    generations: Mutex<HashMap<String, u64>>,
    subscribers: Mutex<Vec<Arc<ProviderSubscriber>>>,
}

impl Registration {
    fn new(service_id: String, mode: ServiceMode) -> Self {
        Self {
            service_id,
            mode,
            singleton: Mutex::new(None),
            singleton_shape: Mutex::new(None),
            instances: Mutex::new(BTreeMap::new()),
            generations: Mutex::new(HashMap::new()),
            subscribers: Mutex::new(Vec::new()),
        }
    }

    fn snapshot(&self) -> ServiceSubscriptionSnapshot {
        let instances = if self.mode == ServiceMode::Singleton {
            self.singleton
                .lock()
                .as_ref()
                .map(|instance| vec![instance.snapshot()])
                .unwrap_or_default()
        } else {
            self.instances
                .lock()
                .values()
                .map(|instance| instance.snapshot())
                .collect()
        };
        SubscriptionSnapshot {
            service_id: self.service_id.clone(),
            mode: self.mode,
            instances,
        }
    }

    fn publish_pending(&self) {
        let context = background_context();
        let instances: Vec<Arc<ProviderInstance>> = if self.mode == ServiceMode::Singleton {
            self.singleton.lock().iter().cloned().collect()
        } else {
            self.instances.lock().values().cloned().collect()
        };
        for instance in instances {
            for member in instance.implementation.members.values() {
                if let ServiceMember::State(state) = member {
                    let _ = state.publish(context);
                }
            }
        }
    }
}

/// The provider half of a remote service boundary.
pub struct RemoteServiceProvider {
    catalogue: Vec<ServiceCatalogueEntry>,
    registrations: Mutex<HashMap<String, Arc<Registration>>>,
    disposed: AtomicBool,
}

impl std::fmt::Debug for RemoteServiceProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteServiceProvider")
            .field("catalogue", &self.catalogue)
            .field("disposed", &self.disposed.load(Ordering::SeqCst))
            .finish()
    }
}

impl RemoteServiceProvider {
    /// Creates a provider for `entries`.
    pub fn new(
        entries: impl IntoIterator<Item = ServiceProviderEntry>,
    ) -> Result<Self, ServiceError> {
        let entries: Vec<ServiceProviderEntry> = entries.into_iter().collect();
        for entry in &entries {
            if entry.local {
                return Err(ServiceError::message(format!(
                    "Local service {} cannot be published remotely",
                    entry.service_id
                )));
            }
        }
        let mut seen = std::collections::HashSet::new();
        for entry in &entries {
            if !seen.insert(entry.service_id.clone()) {
                return Err(ServiceError::message(
                    "Remote service catalogue contains duplicate IDs",
                ));
            }
        }
        let catalogue = entries
            .iter()
            .map(|entry| ServiceCatalogueEntry::new(entry.service_id.clone(), entry.mode))
            .collect::<Vec<_>>();
        let mut registrations = HashMap::new();
        for entry in entries {
            registrations.insert(
                entry.service_id.clone(),
                Arc::new(Registration::new(entry.service_id, entry.mode)),
            );
        }
        Ok(Self {
            catalogue,
            registrations: Mutex::new(registrations),
            disposed: AtomicBool::new(false),
        })
    }

    /// The provided catalogue, in declaration order.
    pub fn catalogue(&self) -> &[ServiceCatalogueEntry] {
        &self.catalogue
    }

    /// Whether [`RemoteServiceProvider::dispose`] has run.
    pub fn is_disposed(&self) -> bool {
        self.disposed.load(Ordering::SeqCst)
    }

    /// Installs the singleton provider for `service`.
    pub fn provide<T>(
        &self,
        service: &Service<T>,
        implementation: ServiceImplementation,
    ) -> Result<(), ServiceError> {
        self.assert_active()?;
        self.assert_remotable(service)?;
        self.assert_allowed(service.id())?;
        let registration = self.registration(service.id(), ServiceMode::Singleton)?;
        if registration.singleton.lock().is_some() {
            return Err(ServiceError::remote(
                RemoteServiceErrorCode::ServiceModeMismatch,
                format!("Remote service {} already has a provider", service.id()),
            ));
        }
        implementation.assert_valid(service.id())?;
        let shape = implementation.shape();
        self.assert_singleton_shape(&registration, &shape)?;
        let instance = create_instance(&registration, implementation, None);
        *registration.singleton.lock() = Some(instance);
        *registration.singleton_shape.lock() = Some(shape);
        Ok(())
    }

    /// Disconnects the singleton provider while keeping subscriptions and facades alive.
    pub fn withdraw<T>(&self, service: &Service<T>) -> Result<(), ServiceError> {
        self.assert_active()?;
        self.assert_remotable(service)?;
        self.assert_allowed(service.id())?;
        let registration = self.registration(service.id(), ServiceMode::Singleton)?;
        let previous = registration.singleton.lock().take();
        let Some(previous) = previous else {
            return Ok(());
        };
        previous.deactivate();
        emit(&registration, &ProviderUpdate::Unavailable, None)
    }

    /// Validates a singleton replacement without installing it.
    pub fn validate_replacement<T>(
        &self,
        service: &Service<T>,
        implementation: &ServiceImplementation,
    ) -> Result<(), ServiceError> {
        self.assert_active()?;
        self.assert_remotable(service)?;
        self.assert_allowed(service.id())?;
        let registration = self.registration(service.id(), ServiceMode::Singleton)?;
        implementation.assert_valid(service.id())?;
        self.assert_singleton_shape(&registration, &implementation.shape())
    }

    /// Replaces the singleton provider without disconnecting stable facades.
    pub fn replace<T>(
        &self,
        service: &Service<T>,
        implementation: ServiceImplementation,
    ) -> Result<(), ServiceError> {
        self.assert_active()?;
        self.assert_remotable(service)?;
        self.assert_allowed(service.id())?;
        let registration = self.registration(service.id(), ServiceMode::Singleton)?;
        implementation.assert_valid(service.id())?;
        let shape = implementation.shape();
        self.assert_singleton_shape(&registration, &shape)?;
        let replacement = create_instance(&registration, implementation, None);
        let previous = registration
            .singleton
            .lock()
            .replace(Arc::clone(&replacement));
        if let Some(previous) = previous {
            previous.deactivate();
        }
        *registration.singleton_shape.lock() = Some(shape);
        let snapshot = replacement.snapshot();
        emit(&registration, &ProviderUpdate::Replaced { snapshot }, None)
    }

    /// Returns the local implementation of a provided singleton (upstream `use`).
    pub fn use_implementation<T>(
        &self,
        service: &Service<T>,
    ) -> Result<ServiceImplementation, ServiceError> {
        self.assert_active()?;
        self.assert_remotable(service)?;
        self.assert_allowed(service.id())?;
        let registration = self.registration(service.id(), ServiceMode::Singleton)?;
        let singleton = registration.singleton.lock().clone();
        match singleton {
            Some(instance) => Ok(instance.implementation.clone()),
            None => Err(ServiceError::remote(
                RemoteServiceErrorCode::ServiceNotFound,
                format!("Remote service {} has no local provider", service.id()),
            )),
        }
    }

    /// Spawns a keyed instance, returning the handle that closes it.
    pub fn spawn<T>(
        &self,
        service: &Service<T>,
        key: &str,
        implementation: ServiceImplementation,
    ) -> Result<RemoteSpawnHandle, ServiceError> {
        self.assert_active()?;
        self.assert_remotable(service)?;
        self.assert_allowed(service.id())?;
        if key.is_empty() {
            return Err(ServiceError::message(
                "Remote service instance key must not be empty",
            ));
        }
        let registration = self.registration(service.id(), ServiceMode::Keyed)?;
        if registration.instances.lock().contains_key(key) {
            return Err(ServiceError::remote(
                RemoteServiceErrorCode::ServiceModeMismatch,
                format!(
                    "Remote service {} already has a live instance with key {key}",
                    service.id()
                ),
            ));
        }
        implementation.assert_valid(service.id())?;
        let generation = {
            let mut generations = registration.generations.lock();
            let next = generations.get(key).copied().unwrap_or(0) + 1;
            generations.insert(key.to_owned(), next);
            next
        };
        let address = ServiceInstanceAddress::new(key, generation);
        let instance = create_instance(&registration, implementation, Some(address.clone()));
        registration
            .instances
            .lock()
            .insert(key.to_owned(), Arc::clone(&instance));
        emit(
            &registration,
            &ProviderUpdate::Spawned {
                instance: instance.snapshot(),
            },
            None,
        )?;
        Ok(RemoteSpawnHandle {
            registration,
            instance,
            address,
            closed: AtomicBool::new(false),
        })
    }

    /// Applies a remote call.
    pub fn invoke(&self, call: &ServiceCall, context: &Context) -> Result<JsonValue, ServiceError> {
        self.assert_active()?;
        self.assert_allowed(&call.service_id)?;
        let registration = self.registration_any(&call.service_id)?;
        let instance = resolve_instance(&registration, call.instance.as_ref())?;
        let Some(member) = instance.implementation.member(&call.member) else {
            return Err(ServiceError::remote(
                RemoteServiceErrorCode::ServiceMemberNotFound,
                format!(
                    "Unknown remote service member {}.{}",
                    call.service_id, call.member
                ),
            ));
        };
        match member {
            ServiceMember::Method(method) => method.call(&call.args, context),
            ServiceMember::State(_) => Err(ServiceError::remote(
                RemoteServiceErrorCode::ServiceMemberMismatch,
                format!(
                    "Remote service member {}.{} is not a method",
                    call.service_id, call.member
                ),
            )),
        }
    }

    /// Opens a provider-side subscription.
    pub fn subscribe(
        &self,
        service_id: &str,
        mode: ServiceMode,
        listener: ServiceProviderListener,
    ) -> Result<Arc<dyn ServiceSubscription>, ServiceError> {
        self.assert_active()?;
        self.assert_allowed(service_id)?;
        let registration = self.registration(service_id, mode)?;
        if registration.mode == ServiceMode::Singleton && registration.singleton.lock().is_none() {
            return Err(ServiceError::remote(
                RemoteServiceErrorCode::ServiceNotFound,
                format!("Remote service {service_id} has no provider"),
            ));
        }
        let subscriber = Arc::new(ProviderSubscriber {
            listener,
            buffer: Mutex::new(Vec::new()),
            active: AtomicBool::new(false),
            terminated: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        });
        registration.publish_pending();
        registration
            .subscribers
            .lock()
            .push(Arc::clone(&subscriber));
        let snapshot = registration.snapshot();
        Ok(Arc::new(ProviderSubscription {
            registration,
            subscriber,
            snapshot,
        }))
    }

    /// Disposes every instance and subscriber. Idempotent.
    pub fn dispose(&self) -> Result<(), ServiceError> {
        if self.disposed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let registrations = std::mem::take(&mut *self.registrations.lock());
        let mut errors = Vec::new();
        for registration in registrations.values() {
            let singleton = registration.singleton.lock().take();
            if let Some(singleton) = singleton {
                singleton.deactivate();
                if let Err(error) = emit(registration, &ProviderUpdate::Unavailable, None) {
                    errors.push(error);
                }
            }
            let instances = std::mem::take(&mut *registration.instances.lock());
            for instance in instances.into_values() {
                instance.deactivate();
                if let Err(error) = emit(
                    registration,
                    &ProviderUpdate::Closed {
                        instance: instance
                            .address
                            .clone()
                            .expect("keyed instances always have an address"),
                    },
                    None,
                ) {
                    errors.push(error);
                }
            }
            for subscriber in registration.subscribers.lock().drain(..) {
                if subscriber.active.load(Ordering::SeqCst) {
                    subscriber.closed.store(true, Ordering::SeqCst);
                    subscriber.buffer.lock().clear();
                } else {
                    subscriber.terminated.store(true, Ordering::SeqCst);
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ServiceError::aggregate(
                "Failed to dispose remote service provider",
                errors,
            ))
        }
    }

    /// Opens a subscriber directly, as `subscribe` does, but without a mode check on the catalogue.
    fn registration_any(&self, service_id: &str) -> Result<Arc<Registration>, ServiceError> {
        self.registrations
            .lock()
            .get(service_id)
            .cloned()
            .ok_or_else(|| {
                ServiceError::remote(
                    RemoteServiceErrorCode::ServiceNotFound,
                    format!("Unknown remote service {service_id}"),
                )
            })
    }

    fn registration(
        &self,
        service_id: &str,
        mode: ServiceMode,
    ) -> Result<Arc<Registration>, ServiceError> {
        let registration = self.registration_any(service_id)?;
        if registration.mode != mode {
            return Err(ServiceError::remote(
                RemoteServiceErrorCode::ServiceModeMismatch,
                format!(
                    "Remote service {service_id} is {}, not {mode}",
                    registration.mode
                ),
            ));
        }
        Ok(registration)
    }

    fn assert_singleton_shape(
        &self,
        registration: &Registration,
        replacement: &MemberShape,
    ) -> Result<(), ServiceError> {
        let current = registration.singleton_shape.lock().clone();
        match current {
            None => Ok(()),
            Some(current) if &current == replacement => Ok(()),
            Some(_) => Err(ServiceError::remote(
                RemoteServiceErrorCode::ServiceMemberMismatch,
                format!(
                    "Remote service {} replacement must preserve its member shape",
                    registration.service_id
                ),
            )),
        }
    }

    fn assert_remotable<T>(&self, service: &Service<T>) -> Result<(), ServiceError> {
        if service.is_local() {
            return Err(ServiceError::remote(
                RemoteServiceErrorCode::ServiceNotAllowed,
                format!("Service {} is process-local", service.id()),
            ));
        }
        Ok(())
    }

    fn assert_allowed(&self, service_id: &str) -> Result<(), ServiceError> {
        if self.registrations.lock().contains_key(service_id) {
            return Ok(());
        }
        Err(ServiceError::remote(
            RemoteServiceErrorCode::ServiceNotAllowed,
            format!("Remote service {service_id} is not allowlisted"),
        ))
    }

    fn assert_active(&self) -> Result<(), ServiceError> {
        if self.disposed.load(Ordering::SeqCst) {
            return Err(ServiceError::message("Remote service provider is disposed"));
        }
        Ok(())
    }
}

/// The handle returned by [`RemoteServiceProvider::spawn`]; closing it withdraws the instance.
pub struct RemoteSpawnHandle {
    registration: Arc<Registration>,
    instance: Arc<ProviderInstance>,
    address: ServiceInstanceAddress,
    closed: AtomicBool,
}

impl RemoteSpawnHandle {
    /// The address of the spawned instance.
    pub fn address(&self) -> &ServiceInstanceAddress {
        &self.address
    }
    /// Closes the instance. Idempotent.
    pub fn close(&self) -> Result<(), ServiceError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let current = self
            .registration
            .instances
            .lock()
            .get(&self.address.key)
            .cloned();
        match current {
            Some(current) if Arc::ptr_eq(&current, &self.instance) => {}
            _ => return Ok(()),
        }
        self.instance.deactivate();
        self.registration.instances.lock().remove(&self.address.key);
        emit(
            &self.registration,
            &ProviderUpdate::Closed {
                instance: self.address.clone(),
            },
            None,
        )
    }
}

impl std::fmt::Debug for RemoteSpawnHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteSpawnHandle")
            .field("address", &self.address)
            .field("closed", &self.closed.load(Ordering::SeqCst))
            .finish()
    }
}

struct ProviderSubscription {
    registration: Arc<Registration>,
    subscriber: Arc<ProviderSubscriber>,
    snapshot: ServiceSubscriptionSnapshot,
}

impl ServiceSubscription for ProviderSubscription {
    fn snapshot(&self) -> ServiceSubscriptionSnapshot {
        self.snapshot.clone()
    }

    fn activate(&self) -> Result<(), ServiceError> {
        if self.subscriber.closed.load(Ordering::SeqCst)
            || self.subscriber.active.load(Ordering::SeqCst)
        {
            return Ok(());
        }
        self.subscriber.active.store(true, Ordering::SeqCst);
        let buffered = std::mem::take(&mut *self.subscriber.buffer.lock());
        let mut errors = Vec::new();
        for (update, context) in buffered {
            if let Err(error) = (self.subscriber.listener)(&update, &context) {
                errors.push(error);
            }
        }
        if self.subscriber.terminated.load(Ordering::SeqCst) {
            self.subscriber.closed.store(true, Ordering::SeqCst);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ServiceError::aggregate(
                "Failed to activate remote service subscription",
                errors,
            ))
        }
    }

    fn close(&self) {
        if self.subscriber.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.subscriber.buffer.lock().clear();
        self.registration
            .subscribers
            .lock()
            .retain(|candidate| !Arc::ptr_eq(candidate, &self.subscriber));
    }
}

/// Creates an instance and subscribes its state members to the registration.
fn create_instance(
    registration: &Arc<Registration>,
    implementation: ServiceImplementation,
    address: Option<ServiceInstanceAddress>,
) -> Arc<ProviderInstance> {
    let instance = Arc::new(ProviderInstance {
        address: address.clone(),
        implementation,
        remove_member_listeners: Mutex::new(Vec::new()),
        active: AtomicBool::new(true),
    });
    let mut removers: Vec<Box<dyn FnOnce() + Send>> = Vec::new();
    for (name, member) in instance.implementation.members.iter() {
        let ServiceMember::State(state) = member else {
            continue;
        };
        let registration = Arc::clone(registration);
        let weak = Arc::downgrade(&instance);
        let name = name.clone();
        let address = address.clone();
        let remove = state.subscribe_source(Arc::new(move |ops, sequence, context| {
            let Some(instance) = weak.upgrade() else {
                return;
            };
            if !instance.active.load(Ordering::SeqCst) {
                return;
            }
            let update = ProviderUpdate::State {
                instance: address.clone(),
                member: name.clone(),
                sequence,
                ops: ops.to_vec(),
            };
            // `emit` reports listener failures to the publisher; a state source listener cannot
            // return them to `publish`, so they are dropped here exactly like an unobserved error.
            let _ = emit(&registration, &update, Some(context));
        }));
        removers.push(remove);
    }
    *instance.remove_member_listeners.lock() = removers;
    instance
}

/// Delivers `update` to every live subscriber, buffering for inactive ones.
fn emit(
    registration: &Arc<Registration>,
    update: &ProviderUpdate<crate::delta::Op>,
    context: Option<&Context>,
) -> Result<(), ServiceError> {
    let subscribers: Vec<Arc<ProviderSubscriber>> = registration.subscribers.lock().clone();
    if subscribers.is_empty() {
        return Ok(());
    }
    let owned;
    let delivery_context = match context {
        Some(context) => context,
        None => {
            owned = background_context().clone();
            &owned
        }
    };
    let mut errors = Vec::new();
    for subscriber in subscribers {
        if subscriber.closed.load(Ordering::SeqCst) {
            continue;
        }
        if !subscriber.active.load(Ordering::SeqCst) {
            subscriber
                .buffer
                .lock()
                .push((update.clone(), delivery_context.clone()));
            continue;
        }
        if let Err(error) = (subscriber.listener)(update, delivery_context) {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ServiceError::aggregate(
            format!(
                "Failed to publish remote service {} update",
                registration.service_id
            ),
            errors,
        ))
    }
}

fn resolve_instance(
    registration: &Registration,
    address: Option<&ServiceInstanceAddress>,
) -> Result<Arc<ProviderInstance>, ServiceError> {
    if registration.mode == ServiceMode::Singleton {
        if address.is_some() {
            return Err(ServiceError::remote(
                RemoteServiceErrorCode::ServiceModeMismatch,
                format!("Remote service {} is singleton", registration.service_id),
            ));
        }
        return registration.singleton.lock().clone().ok_or_else(|| {
            ServiceError::remote(
                RemoteServiceErrorCode::ServiceNotFound,
                format!("Remote service {} has no provider", registration.service_id),
            )
        });
    }
    let Some(address) = address else {
        return Err(ServiceError::remote(
            RemoteServiceErrorCode::ServiceModeMismatch,
            format!("Remote service {} is keyed", registration.service_id),
        ));
    };
    let instance = registration.instances.lock().get(&address.key).cloned();
    let Some(instance) = instance else {
        return Err(ServiceError::remote(
            RemoteServiceErrorCode::ServiceInstanceNotFound,
            format!(
                "Remote service {} has no instance {}",
                registration.service_id, address.key
            ),
        ));
    };
    if instance.address.as_ref().map(|current| current.generation) != Some(address.generation) {
        return Err(ServiceError::remote(
            RemoteServiceErrorCode::ServiceStaleInstance,
            format!(
                "Remote service {} instance {} is stale",
                registration.service_id, address.key
            ),
        ));
    }
    Ok(instance)
}

/// Provider-side adapter for a remote caller.
pub struct RemoteServiceEndpoint {
    provider: Arc<RemoteServiceProvider>,
    subscriptions: Mutex<HashMap<String, Arc<dyn ServiceSubscription>>>,
    disposed: AtomicBool,
}

impl RemoteServiceEndpoint {
    /// Creates an endpoint over `provider`.
    pub fn new(provider: Arc<RemoteServiceProvider>) -> Self {
        Self {
            provider,
            subscriptions: Mutex::new(HashMap::new()),
            disposed: AtomicBool::new(false),
        }
    }

    /// Handles one call. Control calls are answered locally, everything else reaches the provider.
    pub fn invoke(
        &self,
        call: &ServiceCall,
        publish: &ServiceUpdatePublisher,
        context: &Context,
    ) -> Result<JsonValue, ServiceError> {
        if self.disposed.load(Ordering::SeqCst) {
            return Err(ServiceError::message("Remote service endpoint is disposed"));
        }
        match decode_service_control_call(call) {
            Some(ServiceControlCall::Catalogue) => {
                Ok(service_catalogue_to_json(self.provider.catalogue()))
            }
            Some(ServiceControlCall::Subscribe {
                subscription_id,
                service_id,
                mode,
            }) => {
                if self.subscriptions.lock().contains_key(&subscription_id) {
                    return Err(ServiceError::message(
                        "Service subscription ID is already active",
                    ));
                }
                let publish = Arc::clone(publish);
                let forwarded = subscription_id.clone();
                let subscription = self.provider.subscribe(
                    &service_id,
                    mode,
                    Arc::new(move |update, update_context| {
                        (publish)(&forwarded, update, update_context);
                        Ok(())
                    }),
                )?;
                let snapshot = subscription.snapshot();
                self.subscriptions
                    .lock()
                    .insert(subscription_id, Arc::clone(&subscription));
                subscription.activate()?;
                Ok(snapshot.to_json())
            }
            Some(ServiceControlCall::Unsubscribe { subscription_id }) => {
                let subscription = self.subscriptions.lock().remove(&subscription_id);
                match subscription {
                    Some(subscription) => {
                        subscription.close();
                        Ok(JsonValue::Null)
                    }
                    None => Err(ServiceError::message("Service subscription was not found")),
                }
            }
            None => self.provider.invoke(call, context),
        }
    }

    /// Closes every subscription this endpoint owns.
    pub fn dispose(&self) {
        if self.disposed.swap(true, Ordering::SeqCst) {
            return;
        }
        for (_, subscription) in std::mem::take(&mut *self.subscriptions.lock()) {
            subscription.close();
        }
    }

    /// Whether the endpoint has been disposed.
    pub fn is_disposed(&self) -> bool {
        self.disposed.load(Ordering::SeqCst)
    }
}

/// Creates a [`RemoteServiceEndpoint`] over `provider`.
pub fn create_remote_service_endpoint(
    provider: Arc<RemoteServiceProvider>,
) -> RemoteServiceEndpoint {
    RemoteServiceEndpoint::new(provider)
}

/// Validates an implementation without installing it (upstream `validateRemoteServiceImplementation`).
pub fn validate_remote_service_implementation(
    service_id: &str,
    implementation: &ServiceImplementation,
) -> Result<(), ServiceError> {
    implementation.assert_valid(service_id)
}

/// The catalogue control call, re-exported for hosts that build it by hand.
pub fn service_catalogue_call() -> ServiceCall {
    create_service_catalogue_call()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::state::ReplicatedStateMember;
    use crate::state::replicated_state;
    use crate::types::{define_local_service, define_service};

    fn models() -> Service<()> {
        define_service::<()>("test.models").expect("valid id")
    }

    fn implementation(revision: i64) -> ServiceImplementation {
        let state: Arc<dyn ReplicatedStateMember> = Arc::new(
            replicated_state(serde_json::json!({ "selected": null, "revision": revision }))
                .unwrap(),
        );
        ServiceImplementation::new().shared_state("state", state)
    }

    #[test]
    fn catalogue_rejects_locals_and_duplicates() {
        let local = define_local_service::<()>("test.local").expect("valid id");
        let error = RemoteServiceProvider::new([ServiceProviderEntry::singleton(&local)])
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "Local service test.local cannot be published remotely"
        );

        let error = RemoteServiceProvider::new([
            ServiceProviderEntry::singleton(&models()),
            ServiceProviderEntry::singleton(&models()),
        ])
        .unwrap_err()
        .to_string();
        assert_eq!(error, "Remote service catalogue contains duplicate IDs");
    }

    #[test]
    fn mode_mixing_is_reported() {
        let provider =
            RemoteServiceProvider::new([ServiceProviderEntry::singleton(&models())]).unwrap();
        provider.provide(&models(), implementation(0)).unwrap();
        let error = provider
            .spawn(&models(), "wrong", implementation(0))
            .unwrap_err();
        assert_eq!(
            error.code(),
            Some(RemoteServiceErrorCode::ServiceModeMismatch)
        );
        assert!(error.to_string().contains("singleton"));
        provider.dispose().unwrap();
    }

    #[test]
    fn replacement_requires_the_same_shape() {
        let provider =
            RemoteServiceProvider::new([ServiceProviderEntry::singleton(&models())]).unwrap();
        provider.provide(&models(), implementation(1)).unwrap();
        let shape_change = ServiceImplementation::new().method("state", |_, _| Ok(JsonValue::Null));
        let error = provider.replace(&models(), shape_change).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Remote service test.models replacement must preserve its member shape"
        );
        provider.dispose().unwrap();
    }

    #[test]
    fn an_empty_implementation_is_refused() {
        let provider =
            RemoteServiceProvider::new([ServiceProviderEntry::singleton(&models())]).unwrap();
        let error = provider
            .provide(&models(), ServiceImplementation::new())
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Remote service test.models has no members"
        );
        provider.dispose().unwrap();
    }
}
