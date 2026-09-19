//! Consumer side of the remote service boundary, ported from
//! `packages/chord/src/services/consumer.ts`.
//!
//! [`RemoteServiceBinding`] is what a facet (or any host) uses to consume services a provider offers
//! elsewhere: it allowlists the service ids it may use, lazily subscribes on first use, installs the
//! initial snapshot, and keeps the consumer-visible handles stable across singleton replacement.
//!
//! # Why this does not look like upstream
//!
//! Upstream builds JavaScript `Proxy` objects, so a consumer writes `models.state.value` and
//! `models.select(model, context)`. Rust has no `Proxy`, and a reflective `get` trap cannot be
//! ported. This port keeps the *contract* — member kinds are fenced, state is a cold replica, a
//! released keyed observation fails with `service_stale_instance`, the singleton facade survives
//! `withdraw`/`replace` — but addresses members by name:
//!
//! ```ignore
//! let models = binding.use_service(&models_service)?;
//! binding.ready()?;
//! let state = models.state("state")?;          // a stable RemoteStateHandle
//! let value = state.value()?;                  // Option<JsonValue>
//! let select = models.method("select")?;       // a stable RemoteMethodHandle
//! select.call(&[json!({ "provider": "test" })], &context)?;
//! let stop = binding.observe_service(&question_service, |service, ctx| {
//!     let question = service.state("request")?.value()?;
//!     // ...
//! })?;
//! ```
//!
//! The other deviation is that the transport is synchronous. pi-chord has no async runtime by
//! design (it builds for `wasm32-unknown-unknown`), so there is nothing to await: `subscribe` is
//! eager, `activate` happens inside the same call that installs the snapshot, and `ready()` reports
//! the stored start failures instead of awaiting promises.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;

use crate::context::{background_context, Context};
use crate::delta::Op;
use crate::json::JsonValue;
use crate::state::{StateReplica, StateSubscription};
use crate::types::{Service, ServiceInstanceAddress, ServiceMode};

use super::errors::{RemoteServiceErrorCode, ServiceError};
use super::handle::ServiceSlot;
use super::instances::{InstanceDirectory, InstanceDirectoryEntry, ObserveHandle};
use super::provider::{ServiceProviderListener, ServiceSubscription};
use super::wire::{
    InstanceSnapshot, MemberSnapshot, ProviderUpdate, ServiceCall, ServiceMemberKind,
    ServiceMemberSnapshot, ServiceProviderUpdate,
};

/// Reports an error that could not be returned to the caller (a lifecycle listener).
pub type ErrorReporter = Arc<dyn Fn(ServiceError) + Send + Sync>;

/// Fails when the current handle is not allowed to be used.
pub type AccessAssert = Arc<dyn Fn() -> Result<(), ServiceError> + Send + Sync>;

/// A listener attached to a consumer state handle.
pub type RemoteStateListener = Arc<dyn Fn(&JsonValue, &Context) + Send + Sync>;

/// The sync transport a [`RemoteServiceBinding`] talks over.
///
/// Upstream returns promises; this port is synchronous because pi-chord is deliberately
/// runtime-free. Implementations are expected to be non-blocking in the same sense as a
/// `FacetEnvironment` call — a host that needs to cross a real network boundary buffers on both
/// sides and completes the round trip inside the call.
pub trait RemoteServiceTransport: Send + Sync {
    /// Applies a remote call.
    fn invoke(&self, call: &ServiceCall, context: &Context) -> Result<JsonValue, ServiceError>;

    /// Opens a subscription with its initial snapshot already available.
    fn subscribe(
        &self,
        service_id: &str,
        mode: ServiceMode,
        listener: ServiceProviderListener,
    ) -> Result<Arc<dyn ServiceSubscription>, ServiceError>;
}

fn allow_all() -> AccessAssert {
    Arc::new(|| Ok(()))
}

/// Options for [`RemoteServiceBinding::new`].
pub struct RemoteServiceBindingOptions {
    /// The remote service ids the binding may use.
    pub services: Vec<String>,
    /// The transport.
    pub transport: Arc<dyn RemoteServiceTransport>,
    /// Whether the binding starts in the bound state.
    pub bound: bool,
    /// An optional sink for errors that cannot be returned.
    pub on_error: Option<ErrorReporter>,
    /// An optional assertion applied to every consumer handle.
    pub assert_access: Option<AccessAssert>,
}

impl RemoteServiceBindingOptions {
    /// Options for `transport`, bound by default.
    pub fn new(transport: Arc<dyn RemoteServiceTransport>) -> Self {
        Self {
            services: Vec::new(),
            transport,
            bound: true,
            on_error: None,
            assert_access: None,
        }
    }

    /// Allowlists one service.
    pub fn service<T>(mut self, service: &Service<T>) -> Self {
        self.services.push(service.id().to_owned());
        self
    }

    /// Allowlists several services.
    pub fn services<T>(mut self, services: impl IntoIterator<Item = Service<T>>) -> Self {
        self.services
            .extend(services.into_iter().map(|service| service.id().to_owned()));
        self
    }

    /// Sets the initial bound state.
    pub fn bound(mut self, bound: bool) -> Self {
        self.bound = bound;
        self
    }

    /// Sets the error sink.
    pub fn on_error(mut self, reporter: ErrorReporter) -> Self {
        self.on_error = Some(reporter);
        self
    }

    /// Sets the access assertion.
    pub fn assert_access(mut self, assert: AccessAssert) -> Self {
        self.assert_access = Some(assert);
        self
    }
}

struct BindingInner {
    transport: Arc<dyn RemoteServiceTransport>,
    allowlist: HashSet<String>,
    report_error: ErrorReporter,
    assert_access: AccessAssert,
    modes: Mutex<HashMap<String, ServiceMode>>,
    bound: AtomicBool,
    disposed: AtomicBool,
    singletons: Mutex<HashMap<String, Arc<SingletonBinding>>>,
    keyed: Mutex<HashMap<String, Arc<KeyedBinding>>>,
}

impl BindingInner {
    fn assert_remotable<T>(&self, service: &Service<T>) -> Result<(), ServiceError> {
        if service.is_local() {
            return Err(ServiceError::remote(
                RemoteServiceErrorCode::ServiceNotAllowed,
                format!("Service {} is process-local", service.id()),
            ));
        }
        Ok(())
    }

    fn assert_available(&self, service_id: &str, mode: ServiceMode) -> Result<(), ServiceError> {
        if self.disposed.load(Ordering::SeqCst) {
            return Err(ServiceError::message("Remote service binding is disposed"));
        }
        if !self.allowlist.contains(service_id) {
            return Err(ServiceError::remote(
                RemoteServiceErrorCode::ServiceNotAllowed,
                format!("Remote service {service_id} is not allowlisted"),
            ));
        }
        let mut modes = self.modes.lock();
        if let Some(existing) = modes.get(service_id) {
            if *existing != mode {
                return Err(ServiceError::remote(
                    RemoteServiceErrorCode::ServiceModeMismatch,
                    format!("Remote service {service_id} is already used as {existing}"),
                ));
            }
        } else {
            modes.insert(service_id.to_owned(), mode);
        }
        Ok(())
    }

    fn assert_handle_access(&self) -> Result<(), ServiceError> {
        if self.disposed.load(Ordering::SeqCst) {
            return Err(ServiceError::message("Remote service binding is disposed"));
        }
        (self.assert_access)()
    }
}

/// A consumer binding: the allowlist, the transport, and the live local state.
#[derive(Clone)]
pub struct RemoteServiceBinding {
    inner: Arc<BindingInner>,
}

/// Creates a [`RemoteServiceBinding`].
pub fn create_remote_service_binding(
    options: RemoteServiceBindingOptions,
) -> Result<RemoteServiceBinding, ServiceError> {
    RemoteServiceBinding::new(options)
}

impl RemoteServiceBinding {
    /// Creates a binding from `options`.
    pub fn new(options: RemoteServiceBindingOptions) -> Result<Self, ServiceError> {
        Ok(Self {
            inner: Arc::new(BindingInner {
                transport: options.transport,
                allowlist: options.services.into_iter().collect(),
                report_error: options.on_error.unwrap_or_else(|| Arc::new(|_| {})),
                assert_access: options.assert_access.unwrap_or_else(allow_all),
                modes: Mutex::new(HashMap::new()),
                bound: AtomicBool::new(options.bound),
                disposed: AtomicBool::new(false),
                singletons: Mutex::new(HashMap::new()),
                keyed: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Whether the binding currently holds live subscriptions.
    pub fn is_bound(&self) -> bool {
        self.inner.bound.load(Ordering::SeqCst)
    }

    /// Whether [`RemoteServiceBinding::dispose`] has run.
    pub fn is_disposed(&self) -> bool {
        self.inner.disposed.load(Ordering::SeqCst)
    }

    /// Starts (or returns) the singleton facade for `service`.
    pub fn use_service<T>(&self, service: &Service<T>) -> Result<RemoteServiceProxy, ServiceError> {
        self.inner.assert_remotable(service)?;
        self.inner
            .assert_available(service.id(), ServiceMode::Singleton)?;
        let existing = self.inner.singletons.lock().get(service.id()).cloned();
        let binding = match existing {
            Some(binding) => binding,
            None => {
                let candidate = Arc::new(SingletonBinding::new(
                    service.id(),
                    Arc::clone(&self.inner),
                ));
                let mut singletons = self.inner.singletons.lock();
                match singletons.get(service.id()).cloned() {
                    Some(existing) => existing,
                    None => {
                        singletons.insert(service.id().to_owned(), Arc::clone(&candidate));
                        candidate
                    }
                }
            }
        };
        if self.inner.bound.load(Ordering::SeqCst) {
            let revision = binding.revision.load(Ordering::SeqCst);
            if let Err(error) = start_singleton(&binding, revision) {
                *binding.starting_error.lock() = Some(error.clone());
                (self.inner.report_error)(error);
            }
        }
        Ok(RemoteServiceProxy {
            facade: Arc::clone(&binding.facade),
        })
    }

    /// Starts a keyed observation of `service`.
    ///
    /// `handler` runs once per live instance with a [`KeyedServiceProxy`]; when the last observation
    /// for a service is stopped the binding closes its keyed subscription.
    pub fn observe_service<T, F>(
        &self,
        service: &Service<T>,
        handler: F,
    ) -> Result<RemoteObservation, ServiceError>
    where
        F: Fn(KeyedServiceProxy, &Context) + Send + Sync + 'static,
    {
        self.inner.assert_remotable(service)?;
        self.inner
            .assert_available(service.id(), ServiceMode::Keyed)?;
        let existing = self.inner.keyed.lock().get(service.id()).cloned();
        let binding = match existing {
            Some(binding) => binding,
            None => {
                let candidate = Arc::new(KeyedBinding::new(service.id(), Arc::clone(&self.inner)));
                let mut keyed = self.inner.keyed.lock();
                match keyed.get(service.id()).cloned() {
                    Some(existing) => existing,
                    None => {
                        keyed.insert(service.id().to_owned(), Arc::clone(&candidate));
                        candidate
                    }
                }
            }
        };
        let handle = binding.register_observer(handler)?;
        binding.start_if_needed();
        Ok(RemoteObservation::new(handle, binding))
    }

    /// Reports the failure of the most recent start attempt, if any.
    pub fn ready(&self) -> Result<(), ServiceError> {
        if self.inner.disposed.load(Ordering::SeqCst) {
            return Err(ServiceError::message("Remote service binding is disposed"));
        }
        let singletons: Vec<Arc<SingletonBinding>> =
            self.inner.singletons.lock().values().cloned().collect();
        for binding in singletons {
            if let Some(error) = binding.starting_error.lock().clone() {
                return Err(error);
            }
        }
        let keyed: Vec<Arc<KeyedBinding>> = self.inner.keyed.lock().values().cloned().collect();
        for binding in keyed {
            if let Some(error) = binding.starting_error.lock().clone() {
                return Err(error);
            }
        }
        Ok(())
    }

    /// Enables or disables all subscriptions, hydrating facades on the way back in.
    pub fn rebind(&self, bound: bool) -> Result<(), ServiceError> {
        if self.inner.disposed.load(Ordering::SeqCst) {
            return Err(ServiceError::message("Remote service binding is disposed"));
        }
        self.inner.bound.store(bound, Ordering::SeqCst);
        let mut errors = Vec::new();
        let singletons: Vec<Arc<SingletonBinding>> =
            self.inner.singletons.lock().values().cloned().collect();
        for binding in singletons {
            binding.facade.clear();
            if let Some(subscription) = binding.subscription.lock().take() {
                subscription.close();
            }
            *binding.starting_error.lock() = None;
            binding.revision.fetch_add(1, Ordering::SeqCst);
            if bound {
                let revision = binding.revision.load(Ordering::SeqCst);
                if let Err(error) = start_singleton(&binding, revision) {
                    *binding.starting_error.lock() = Some(error.clone());
                    errors.push(error);
                }
            }
        }
        let keyed: Vec<Arc<KeyedBinding>> = self.inner.keyed.lock().values().cloned().collect();
        for binding in keyed {
            if let Err(error) = binding.rebind(bound) {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ServiceError::aggregate(
                "Failed to rebind remote services",
                errors,
            ))
        }
    }

    /// Disposes every facade and subscription. Idempotent.
    pub fn dispose(&self) -> Result<(), ServiceError> {
        if self.inner.disposed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.inner.bound.store(false, Ordering::SeqCst);
        let mut errors = Vec::new();
        let singletons = std::mem::take(&mut *self.inner.singletons.lock());
        for binding in singletons.into_values() {
            binding.facade.clear();
            if let Some(subscription) = binding.subscription.lock().take() {
                subscription.close();
            }
        }
        let keyed = std::mem::take(&mut *self.inner.keyed.lock());
        for binding in keyed.into_values() {
            if let Err(error) = binding.close() {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ServiceError::aggregate(
                "Failed to dispose remote services",
                errors,
            ))
        }
    }
}

struct SingletonBinding {
    service_id: String,
    inner: Arc<BindingInner>,
    facade: Arc<ServiceFacade>,
    active: Arc<AtomicBool>,
    revision: AtomicU64,
    subscription: Mutex<Option<Arc<dyn ServiceSubscription>>>,
    starting_error: Mutex<Option<ServiceError>>,
}

impl SingletonBinding {
    fn new(service_id: &str, inner: Arc<BindingInner>) -> Self {
        let active = Arc::new(AtomicBool::new(true));
        let facade = create_facade(
            service_id,
            None,
            Arc::clone(&inner),
            Arc::clone(&active),
            None,
        );
        Self {
            service_id: service_id.to_owned(),
            inner,
            facade,
            active,
            revision: AtomicU64::new(0),
            subscription: Mutex::new(None),
            starting_error: Mutex::new(None),
        }
    }
}

/// Starts the singleton subscription for `revision`.
fn start_singleton(binding: &Arc<SingletonBinding>, revision: u64) -> Result<(), ServiceError> {
    if binding.subscription.lock().is_some() {
        return Ok(());
    }
    let service_id = binding.service_id.clone();
    let weak = Arc::downgrade(binding);
    let weak_inner = Arc::downgrade(&binding.inner);
    let listener: ServiceProviderListener = Arc::new(move |update, context| {
        let Some(binding) = weak.upgrade() else {
            return Ok(());
        };
        if !binding.active.load(Ordering::SeqCst)
            || binding.revision.load(Ordering::SeqCst) != revision
        {
            return Ok(());
        }
        if let Err(error) = binding.facade.apply_singleton(update, context) {
            if let Some(inner) = weak_inner.upgrade() {
                (inner.report_error)(error);
            }
        }
        Ok(())
    });
    let subscription = binding.inner.transport.subscribe(
        &service_id,
        ServiceMode::Singleton,
        listener,
    )?;    if binding.subscription.lock().is_some()
        || !binding.active.load(Ordering::SeqCst)
        || binding.inner.disposed.load(Ordering::SeqCst)
        || !binding.inner.bound.load(Ordering::SeqCst)
        || binding.revision.load(Ordering::SeqCst) != revision
    {
        subscription.close();
        return Ok(());
    }
    let snapshot = subscription.snapshot();
    if snapshot.service_id != service_id
        || snapshot.mode != ServiceMode::Singleton
        || snapshot.instances.len() != 1
    {
        subscription.close();
        return Err(ServiceError::message(format!(
            "Remote service {service_id} returned an invalid singleton snapshot"
        )));
    }
    *binding.subscription.lock() = Some(Arc::clone(&subscription));
    if let Err(error) = binding
        .facade
        .install(&snapshot.instances[0], background_context())
    {
        *binding.subscription.lock() = None;
        subscription.close();
        return Err(error);
    }
    subscription.activate()?;
    Ok(())
}

struct KeyedBinding {
    service_id: String,
    inner: Arc<BindingInner>,
    directory: Arc<InstanceDirectory<KeyedInstance>>,
    closed: AtomicBool,
    bound: AtomicBool,
    revision: AtomicU64,
    subscription: Mutex<Option<Arc<dyn ServiceSubscription>>>,
    starting_error: Mutex<Option<ServiceError>>,
}

impl KeyedBinding {
    fn new(service_id: &str, inner: Arc<BindingInner>) -> Self {
        let bound = inner.bound.load(Ordering::SeqCst);
        Self {
            service_id: service_id.to_owned(),
            directory: Arc::new(InstanceDirectory::new(
                false,
                Arc::clone(&inner.report_error),
            )),
            inner,
            closed: AtomicBool::new(false),
            bound: AtomicBool::new(bound),
            revision: AtomicU64::new(0),
            subscription: Mutex::new(None),
            starting_error: Mutex::new(None),
        }
    }

    fn register_observer<F>(
        self: &Arc<Self>,
        handler: F,
    ) -> Result<ObserveHandle<KeyedInstance>, ServiceError>
    where
        F: Fn(KeyedServiceProxy, &Context) + Send + Sync + 'static,
    {
        let service_id = self.service_id.clone();
        let weak = Arc::downgrade(self);
        self.directory.observe(move |facade, context| {
            let Some(binding) = weak.upgrade() else {
                return;
            };
            let slot = Arc::new(ServiceSlot::new(service_id.clone()));
            slot.bind(Arc::clone(&facade));
            let guard = observation_guard(
                Arc::downgrade(&binding.inner),
                service_id.clone(),
                context.clone(),
            );
            handler(
                KeyedServiceProxy {
                    slot,
                    guard,
                    service_id: service_id.clone(),
                },
                context,
            );
        })
    }

    fn start_if_needed(self: &Arc<Self>) {
        if !self.bound.load(Ordering::SeqCst) || self.closed.load(Ordering::SeqCst) {
            return;
        }
        if self.subscription.lock().is_some() {
            return;
        }
        let revision = self.revision.load(Ordering::SeqCst);
        if let Err(error) = start_keyed(self, revision) {
            *self.starting_error.lock() = Some(error.clone());
            (self.inner.report_error)(error);
        }
    }

    fn rebind(self: &Arc<Self>, bound: bool) -> Result<(), ServiceError> {
        if self.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.bound.store(bound, Ordering::SeqCst);
        self.revision.fetch_add(1, Ordering::SeqCst);
        self.directory.reset();
        *self.starting_error.lock() = None;
        if let Some(subscription) = self.subscription.lock().take() {
            subscription.close();
        }
        if bound && self.directory.observer_count() > 0 {
            let revision = self.revision.load(Ordering::SeqCst);
            if let Err(error) = start_keyed(self, revision) {
                *self.starting_error.lock() = Some(error.clone());
                return Err(error);
            }
        }
        Ok(())
    }

    fn close(&self) -> Result<(), ServiceError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.revision.fetch_add(1, Ordering::SeqCst);
        self.directory.reset();
        if let Some(subscription) = self.subscription.lock().take() {
            subscription.close();
        }
        self.directory.dispose();
        Ok(())
    }

    fn on_empty(self: &Arc<Self>) {
        {
            let mut keyed = self.inner.keyed.lock();
            let current = keyed.get(&self.service_id).cloned();
            if current.is_some_and(|candidate| Arc::ptr_eq(&candidate, self)) {
                keyed.remove(&self.service_id);
            }
        }
        let _ = self.close();
    }

    fn handle_update(self: &Arc<Self>, update: &ServiceProviderUpdate, context: &Context) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let result = match update {
            ProviderUpdate::Unavailable | ProviderUpdate::Replaced { .. } => Err(
                ServiceError::message("Keyed service received a singleton lifecycle update"),
            ),
            ProviderUpdate::Spawned { instance } => self.spawn(instance, context),
            ProviderUpdate::Closed { instance } => {
                if let Some(entry) = self.directory.get(&instance.key) {
                    if entry.generation == instance.generation {
                        self.directory.remove(&entry);
                    }
                }
                Ok(())
            }
            ProviderUpdate::State {
                instance: None, ..
            } => Err(ServiceError::message(
                "Keyed service state update has no instance address",
            )),
            ProviderUpdate::State {
                instance: Some(address),
                member,
                sequence,
                ops,
            } => match self.directory.get(&address.key) {
                Some(entry) if entry.generation == address.generation => {
                    entry.facade.update(member, *sequence, ops, context)
                }
                _ => Ok(()),
            },
        };
        if let Err(error) = result {
            (self.inner.report_error)(error);
        }
    }

    fn spawn(
        self: &Arc<Self>,
        snapshot: &InstanceSnapshot<Op>,
        context: &Context,
    ) -> Result<(), ServiceError> {
        let address = snapshot.instance.clone().ok_or_else(|| {
            ServiceError::message("Keyed service instance snapshot has no address")
        })?;
        let active = Arc::new(AtomicBool::new(true));
        let facade = create_facade(
            &self.service_id,
            Some(address.clone()),
            Arc::clone(&self.inner),
            Arc::clone(&active),
            Some(Arc::clone(self)),
        );
        facade.install(snapshot, context)?;
        self.directory.replace(Arc::new(KeyedInstance {
            facade,
            key: address.key,
            generation: address.generation,
            active,
        }))
    }
}

fn start_keyed(binding: &Arc<KeyedBinding>, revision: u64) -> Result<(), ServiceError> {
    let weak = Arc::downgrade(binding);
    let listener: ServiceProviderListener = Arc::new(move |update, context| {
        if let Some(binding) = weak.upgrade() {
            if binding.revision.load(Ordering::SeqCst) == revision {
                binding.handle_update(update, context);
            }
        }
        Ok(())
    });
    let subscription =
        binding
            .inner
            .transport
            .subscribe(&binding.service_id, ServiceMode::Keyed, listener)?;
    if binding.closed.load(Ordering::SeqCst)
        || !binding.bound.load(Ordering::SeqCst)
        || binding.revision.load(Ordering::SeqCst) != revision
    {
        subscription.close();
        return Ok(());
    }
    let snapshot = subscription.snapshot();
    if snapshot.service_id != binding.service_id || snapshot.mode != ServiceMode::Keyed {
        subscription.close();
        return Err(ServiceError::message(format!(
            "Remote service {} returned an invalid keyed snapshot",
            binding.service_id
        )));
    }
    for instance in &snapshot.instances {
        binding.spawn(instance, background_context())?;
    }
    *binding.subscription.lock() = Some(Arc::clone(&subscription));
    subscription.activate()?;
    binding.directory.ready()?;
    Ok(())
}

/// A live keyed instance tracked by [`InstanceDirectory`].
struct KeyedInstance {
    facade: Arc<ServiceFacade>,
    key: String,
    generation: u64,
    active: Arc<AtomicBool>,
}

impl InstanceDirectoryEntry for KeyedInstance {
    type Service = Arc<ServiceFacade>;

    fn key(&self) -> &str {
        &self.key
    }

    fn generation(&self) -> u64 {
        self.generation
    }

    fn service(&self) -> Arc<ServiceFacade> {
        Arc::clone(&self.facade)
    }

    fn deactivate(&self) {
        self.active.store(false, Ordering::SeqCst);
        self.facade.clear();
    }
}

/// A running keyed observation; dropping or stopping it releases the instance handlers.
pub struct RemoteObservation {
    state: Arc<ObservationState>,
}

struct ObservationState {
    handle: Mutex<Option<ObserveHandle<KeyedInstance>>>,
    binding: Arc<KeyedBinding>,
    stopped: AtomicBool,
}

impl RemoteObservation {
    fn new(handle: ObserveHandle<KeyedInstance>, binding: Arc<KeyedBinding>) -> Self {
        Self {
            state: Arc::new(ObservationState {
                handle: Mutex::new(Some(handle)),
                binding,
                stopped: AtomicBool::new(false),
            }),
        }
    }

    /// Stops the observation now; dropping the observation afterwards is a no-op.
    pub fn stop(&self) {
        self.state.stop();
    }

    /// Whether the observation has been stopped.
    pub fn is_stopped(&self) -> bool {
        self.state.stopped.load(Ordering::SeqCst)
    }
}

impl Drop for RemoteObservation {
    fn drop(&mut self) {
        self.state.stop();
    }
}

impl ObservationState {
    fn stop(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(handle) = self.handle.lock().take() {
            handle.stop();
        }
        if self.binding.directory.observer_count() == 0 {
            self.binding.on_empty();
        }
    }
}

/// A consumer view of a singleton service.
#[derive(Clone)]
pub struct RemoteServiceProxy {
    facade: Arc<ServiceFacade>,
}

impl RemoteServiceProxy {
    /// The service id.
    pub fn service_id(&self) -> &str {
        self.facade.service_id()
    }

    /// Whether two proxies share the same stable facade.
    pub fn is_same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.facade, &other.facade)
    }

    /// The instance address, for keyed facades.
    pub fn address(&self) -> Option<&ServiceInstanceAddress> {
        self.facade.address()
    }

    /// Calls a method member.
    pub fn call(
        &self,
        member: &str,
        args: &[JsonValue],
        context: &Context,
    ) -> Result<JsonValue, ServiceError> {
        self.facade.call(member, args, context)
    }

    /// Returns a stable handle to a method member.
    pub fn method(&self, member: &str) -> Result<RemoteMethodHandle, ServiceError> {
        Ok(RemoteMethodHandle {
            slot: self.facade.slot(member)?,
            guard: allow_all(),
        })
    }

    /// Returns a stable handle to a state member.
    pub fn state(&self, member: &str) -> Result<RemoteStateHandle, ServiceError> {
        Ok(RemoteStateHandle {
            slot: self.facade.slot(member)?,
            guard: allow_all(),
        })
    }
}

/// A consumer view of one keyed service instance.
#[derive(Clone)]
pub struct KeyedServiceProxy {
    slot: Arc<ServiceSlot<Arc<ServiceFacade>>>,
    guard: AccessAssert,
    service_id: String,
}

impl KeyedServiceProxy {
    /// The service id.
    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    fn facade(&self) -> Result<Arc<ServiceFacade>, ServiceError> {
        let guard = Arc::clone(&self.guard);
        self.slot.resolve(move || guard(), Arc::clone)
    }

    /// Calls a method member.
    pub fn call(
        &self,
        member: &str,
        args: &[JsonValue],
        context: &Context,
    ) -> Result<JsonValue, ServiceError> {
        self.facade()?.call(member, args, context)
    }

    /// Returns a handle to a method member that re-checks the observation guard on every use.
    pub fn method(&self, member: &str) -> Result<RemoteMethodHandle, ServiceError> {
        Ok(RemoteMethodHandle {
            slot: self.facade()?.slot(member)?,
            guard: Arc::clone(&self.guard),
        })
    }

    /// Returns a handle to a state member that re-checks the observation guard on every use.
    pub fn state(&self, member: &str) -> Result<RemoteStateHandle, ServiceError> {
        Ok(RemoteStateHandle {
            slot: self.facade()?.slot(member)?,
            guard: Arc::clone(&self.guard),
        })
    }
}

/// A retained handle to a remote state member.
#[derive(Clone)]
pub struct RemoteStateHandle {
    slot: Arc<MemberSlot>,
    guard: AccessAssert,
}

impl RemoteStateHandle {
    /// The current replicated value, if the member has been hydrated.
    pub fn value(&self) -> Result<Option<JsonValue>, ServiceError> {
        (self.guard)()?;
        self.slot.value()
    }

    /// Subscribes to future deliveries. An already hydrated member delivers immediately.
    pub fn subscribe<F>(&self, listener: F) -> Result<StateSubscription<JsonValue>, ServiceError>
    where
        F: Fn(&JsonValue, &Context) + Send + Sync + 'static,
    {
        (self.guard)()?;
        self.slot
            .subscribe(Arc::new(move |value, context| listener(value, context)))
    }

    /// Whether two handles address the same member.
    pub fn is_same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.slot, &other.slot)
    }
}

/// A retained handle to a remote method member.
#[derive(Clone)]
pub struct RemoteMethodHandle {
    slot: Arc<MemberSlot>,
    guard: AccessAssert,
}

impl std::fmt::Debug for RemoteServiceProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteServiceProxy")
            .field("service_id", &self.service_id())
            .field("address", &self.address())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for KeyedServiceProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyedServiceProxy")
            .field("service_id", &self.service_id)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for RemoteStateHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteStateHandle")
            .field("service_id", &self.slot.service_id)
            .field("member", &self.slot.member)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for RemoteMethodHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteMethodHandle")
            .field("service_id", &self.slot.service_id)
            .field("member", &self.slot.member)
            .finish_non_exhaustive()
    }
}

impl RemoteMethodHandle {
    /// Calls the method.
    pub fn call(&self, args: &[JsonValue], context: &Context) -> Result<JsonValue, ServiceError> {
        (self.guard)()?;
        self.slot.call(args, context)
    }

    /// Whether two handles address the same member.
    pub fn is_same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.slot, &other.slot)
    }
}

/// One member of a consumer facade: its kind, its invocation, and its replicated state.
struct MemberSlot {
    service_id: String,
    member: String,
    kind: Mutex<Option<ServiceMemberKind>>,
    expected_kind: Mutex<Option<ServiceMemberKind>>,
    state: StateReplica<JsonValue>,
    invoke: InvokeFn,
    is_active: Arc<dyn Fn() -> bool + Send + Sync>,
    assert_access: AccessAssert,
}

/// A facade member invocation closure.
type InvokeFn =
    Arc<dyn Fn(&[JsonValue], &Context) -> Result<JsonValue, ServiceError> + Send + Sync>;

impl MemberSlot {
    fn new(
        service_id: &str,
        member: &str,
        invoke: InvokeFn,
        is_active: Arc<dyn Fn() -> bool + Send + Sync>,
        assert_access: AccessAssert,
    ) -> Self {
        Self {
            service_id: service_id.to_owned(),
            member: member.to_owned(),
            kind: Mutex::new(None),
            expected_kind: Mutex::new(None),
            state: StateReplica::new(),
            invoke,
            is_active,
            assert_access,
        }
    }

    fn set_description(&self, kind: ServiceMemberKind) -> Result<(), ServiceError> {
        {
            let mut current = self.kind.lock();
            if let Some(existing) = *current {
                if existing != kind {
                    return Err(ServiceError::message(format!(
                        "Remote service member {}.{} changed kind",
                        self.service_id, self.member
                    )));
                }
            }
            *current = Some(kind);
        }
        self.expect(kind)
    }

    fn expect(&self, kind: ServiceMemberKind) -> Result<(), ServiceError> {
        {
            let mut expected = self.expected_kind.lock();
            if let Some(existing) = *expected {
                if existing != kind {
                    return Err(ServiceError::remote(
                        RemoteServiceErrorCode::ServiceMemberMismatch,
                        format!(
                            "Remote service member {}.{} was used as two different kinds",
                            self.service_id, self.member
                        ),
                    ));
                }
            }
            *expected = Some(kind);
        }
        if let Some(actual) = *self.kind.lock() {
            if actual != kind {
                return Err(ServiceError::remote(
                    RemoteServiceErrorCode::ServiceMemberMismatch,
                    format!(
                        "Remote service member {}.{} is {actual}, not {kind}",
                        self.service_id, self.member
                    ),
                ));
            }
        }
        Ok(())
    }

    fn hydrate(&self, sequence: u64, ops: &[Op], context: &Context) -> Result<(), ServiceError> {
        self.set_description(ServiceMemberKind::State)?;
        self.state
            .hydrate(sequence, ops, context)
            .map_err(ServiceError::from)
    }

    fn update(&self, sequence: u64, ops: &[Op], context: &Context) -> Result<(), ServiceError> {
        self.expect(ServiceMemberKind::State)?;
        self.state
            .update(sequence, ops, context)
            .map_err(ServiceError::from)
    }

    fn clear(&self) {
        self.state.clear();
    }

    fn value(&self) -> Result<Option<JsonValue>, ServiceError> {
        (self.assert_access)()?;
        self.expect(ServiceMemberKind::State)?;
        Ok(self.state.value())
    }

    fn subscribe(
        &self,
        listener: RemoteStateListener,
    ) -> Result<StateSubscription<JsonValue>, ServiceError> {
        (self.assert_access)()?;
        self.expect(ServiceMemberKind::State)?;
        Ok(self
            .state
            .subscribe(move |value, context, _delivery| listener(value, context)))
    }

    fn call(&self, args: &[JsonValue], context: &Context) -> Result<JsonValue, ServiceError> {
        (self.assert_access)()?;
        self.expect(ServiceMemberKind::Method)?;
        if !(self.is_active)() {
            return Err(ServiceError::remote(
                RemoteServiceErrorCode::ServiceStaleInstance,
                format!("Remote service {} binding is closed", self.service_id),
            ));
        }
        (self.invoke)(args, context)
    }
}

/// A consumer-side instance: one slot per member name plus the last known member kinds.
struct ServiceFacade {
    service_id: String,
    address: Option<ServiceInstanceAddress>,
    transport: Arc<dyn RemoteServiceTransport>,
    slots: Mutex<HashMap<String, Arc<MemberSlot>>>,
    descriptions: Mutex<HashMap<String, ServiceMemberKind>>,
    is_active: Arc<dyn Fn() -> bool + Send + Sync>,
    assert_access: AccessAssert,
}

impl ServiceFacade {
    fn apply_singleton(
        &self,
        update: &ServiceProviderUpdate,
        context: &Context,
    ) -> Result<(), ServiceError> {
        match update {
            ProviderUpdate::Unavailable => {
                self.clear();
                Ok(())
            }
            ProviderUpdate::Replaced { snapshot } => {
                if snapshot.instance.is_some() {
                    return Err(ServiceError::message(
                        "Singleton replacement has an instance address",
                    ));
                }
                self.install(snapshot, context)
            }
            ProviderUpdate::State {
                instance: None,
                member,
                sequence,
                ops,
            } => self.update(member, *sequence, ops, context),
            _ => Ok(()),
        }
    }

    fn install(
        &self,
        snapshot: &InstanceSnapshot<Op>,
        context: &Context,
    ) -> Result<(), ServiceError> {
        if !addresses_match(snapshot.instance.as_ref(), self.address.as_ref()) {
            return Err(ServiceError::message(
                "Remote service snapshot has the wrong address",
            ));
        }
        let members = validate_members(&snapshot.members)?;
        {
            let slots = self.slots.lock();
            for name in slots.keys() {
                if !members.contains_key(name) {
                    return Err(ServiceError::remote(
                        RemoteServiceErrorCode::ServiceMemberNotFound,
                        format!("Unknown remote service member {}.{}", self.service_id, name),
                    ));
                }
            }
        }
        {
            let mut descriptions = self.descriptions.lock();
            descriptions.clear();
            for (name, kind) in &members {
                descriptions.insert(name.clone(), *kind);
            }
        }
        for member in &snapshot.members {
            match member {
                MemberSnapshot::Method { name } => {
                    let slot = self.slots.lock().get(name).cloned();
                    if let Some(slot) = slot {
                        slot.set_description(ServiceMemberKind::Method)?;
                    }
                }
                MemberSnapshot::State {
                    name,
                    sequence,
                    ops,
                } => {
                    let existing = self.slots.lock().get(name).cloned();
                    let slot = match existing {
                        Some(slot) => slot,
                        None => self.slot(name)?,
                    };
                    slot.hydrate(*sequence, ops, context)?;
                }
            }
        }
        Ok(())
    }

    fn update(
        &self,
        member: &str,
        sequence: u64,
        ops: &[Op],
        context: &Context,
    ) -> Result<(), ServiceError> {
        if self.descriptions.lock().get(member) != Some(&ServiceMemberKind::State) {
            return Err(ServiceError::message(format!(
                "Remote service update targets non-state member {}.{}",
                self.service_id, member
            )));
        }
        self.slot(member)?.update(sequence, ops, context)
    }

    fn clear(&self) {
        let slots: Vec<Arc<MemberSlot>> = self.slots.lock().values().cloned().collect();
        for slot in slots {
            slot.clear();
        }
    }

    fn slot(&self, member: &str) -> Result<Arc<MemberSlot>, ServiceError> {
        if let Some(slot) = self.slots.lock().get(member).cloned() {
            return Ok(slot);
        }
        let service_id = self.service_id.clone();
        let member_owned = member.to_owned();
        let address = self.address.clone();
        let transport = Arc::clone(&self.transport);
        let is_active = Arc::clone(&self.is_active);
        let assert_access = Arc::clone(&self.assert_access);
        let invoke_service_id = service_id.clone();
        let invoke_member = member_owned.clone();
        let invoke: InvokeFn = Arc::new(move |args, context| {
            let call = ServiceCall {
                service_id: invoke_service_id.clone(),
                instance: address.clone(),
                member: invoke_member.clone(),
                args: args.to_vec(),
            };
            transport.invoke(&call, context)
        });
        let slot = Arc::new(MemberSlot::new(
            &service_id,
            &member_owned,
            invoke,
            is_active,
            assert_access,
        ));
        if let Some(kind) = self.descriptions.lock().get(member).copied() {
            slot.set_description(kind)?;
        }
        let mut slots = self.slots.lock();
        if let Some(existing) = slots.get(member).cloned() {
            return Ok(existing);
        }
        slots.insert(member_owned, Arc::clone(&slot));
        Ok(slot)
    }

    fn call(
        &self,
        member: &str,
        args: &[JsonValue],
        context: &Context,
    ) -> Result<JsonValue, ServiceError> {
        self.slot(member)?.call(args, context)
    }

    fn service_id(&self) -> &str {
        &self.service_id
    }

    fn address(&self) -> Option<&ServiceInstanceAddress> {
        self.address.as_ref()
    }
}

fn addresses_match(
    left: Option<&ServiceInstanceAddress>,
    right: Option<&ServiceInstanceAddress>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.key == right.key && left.generation == right.generation
        }
        _ => false,
    }
}

fn validate_members(
    members: &[ServiceMemberSnapshot],
) -> Result<BTreeMap<String, ServiceMemberKind>, ServiceError> {
    let mut result = BTreeMap::new();
    for member in members {
        let name = member.name();
        if name.is_empty() {
            return Err(ServiceError::message(
                "Remote service has an invalid member description",
            ));
        }
        if result.insert(name.to_owned(), member.kind()).is_some() {
            return Err(ServiceError::message(
                "Remote service has an invalid member description",
            ));
        }
    }
    Ok(result)
}

fn observation_guard(
    inner: Weak<BindingInner>,
    service_id: String,
    context: Context,
) -> AccessAssert {
    Arc::new(move || {
        let Some(inner) = inner.upgrade() else {
            return Err(stale_observation(&service_id));
        };
        inner.assert_handle_access()?;
        if context
            .abort_signal()
            .is_some_and(crate::context::AbortSignal::is_aborted)
        {
            return Err(stale_observation(&service_id));
        }
        Ok(())
    })
}

fn stale_observation(service_id: &str) -> ServiceError {
    ServiceError::remote(
        RemoteServiceErrorCode::ServiceStaleInstance,
        format!("Remote service {service_id} observation is closed"),
    )
}

fn create_facade(
    service_id: &str,
    address: Option<ServiceInstanceAddress>,
    inner: Arc<BindingInner>,
    active: Arc<AtomicBool>,
    binding: Option<Arc<KeyedBinding>>,
) -> Arc<ServiceFacade> {
    let weak_inner = Arc::downgrade(&inner);
    let weak_binding = binding.as_ref().map(Arc::downgrade);
    let active_for_is = Arc::clone(&active);
    let is_active: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
        if !active_for_is.load(Ordering::SeqCst) {
            return false;
        }
        let Some(inner) = weak_inner.upgrade() else {
            return false;
        };
        if inner.disposed.load(Ordering::SeqCst) {
            return false;
        }
        match weak_binding.as_ref().map(|binding| binding.upgrade()) {
            None => true,
            Some(None) => false,
            Some(Some(binding)) => !binding.closed.load(Ordering::SeqCst),
        }
    });
    let weak_assert = Arc::downgrade(&inner);
    let assert_access: AccessAssert = Arc::new(move || match weak_assert.upgrade() {
        Some(inner) => inner.assert_handle_access(),
        None => Err(ServiceError::message("Remote service binding is disposed")),
    });
    Arc::new(ServiceFacade {
        service_id: service_id.to_owned(),
        address,
        transport: inner.transport.clone(),
        slots: Mutex::new(HashMap::new()),
        descriptions: Mutex::new(HashMap::new()),
        is_active,
        assert_access,
    })
}
