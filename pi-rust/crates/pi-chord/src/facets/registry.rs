//! The in-process service directory backing facet `use` / `provide` / `provideMany` / `observe`.
//!
//! Upstream `facets/host.ts` delegates the service kernel to `services/*` (provider, consumer,
//! handle, instances, loopback, state). The remote half of that is Stage 18; this module implements
//! the *local* half — singleton slots and a keyed instance registry with observers — so the facet
//! kernel is complete on its own. See the crate README for the mapping and the documented gaps.
//!
//! Two properties matter for parity:
//!
//! * a [`ServiceHandle`] is a stable view created during setup; it starts resolving only once the
//!   generation is active and stops resolving the moment disposal starts (`Facet <id> service
//!   handles cannot be used while <phase>`);
//! * every consumer of a singleton shares one slot, so a reload that re-provides the service is
//!   immediately visible to handles that were captured by previous generations.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::context::{background_context, Context};
use crate::facets::FacetLifecycle;
use crate::types::{FacetError, Service};

/// The erased service implementation stored in a slot: `Arc<dyn Any + Send + Sync>`.
pub type Erased = Arc<dyn Any + Send + Sync>;

/// A singleton slot: at most one erased implementation, shared by every consumer.
pub struct SingletonSlot {
    service_id: String,
    value: Mutex<Option<Erased>>,
}

impl SingletonSlot {
    fn new(service_id: String) -> Self {
        Self {
            service_id,
            value: Mutex::new(None),
        }
    }

    /// The service id this slot belongs to.
    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    /// Whether an implementation is bound.
    pub fn is_bound(&self) -> bool {
        self.value.lock().is_some()
    }

    /// Binds an implementation, replacing any previous one.
    pub fn bind<T: Send + Sync + 'static>(&self, implementation: T) {
        let erased: Erased = Arc::new(implementation);
        *self.value.lock() = Some(erased);
    }

    fn bind_erased(&self, implementation: Erased) {
        *self.value.lock() = Some(implementation);
    }

    /// Reads the bound implementation, if it has the expected type.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.value
            .lock()
            .as_ref()
            .and_then(|value| Arc::clone(value).downcast::<T>().ok())
    }

    /// Unbinds the implementation.
    pub fn clear(&self) {
        *self.value.lock() = None;
    }
}

/// Receives notifications about keyed service instances.
pub trait KeyedObserver: Send + Sync {
    /// Called for an existing instance at registration time and for every instance spawned later.
    fn observe(&self, key: &str, instance: &Erased, context: &Context);
}

struct ObserverEntry {
    id: u64,
    observer: Arc<dyn KeyedObserver>,
}

/// A keyed service slot: instances addressed by key, plus their observers.
pub struct KeyedSlot {
    service_id: String,
    instances: Mutex<HashMap<String, Erased>>,
    observers: Mutex<Vec<ObserverEntry>>,
    next_observer_id: Mutex<u64>,
}

impl KeyedSlot {
    fn new(service_id: String) -> Self {
        Self {
            service_id,
            instances: Mutex::new(HashMap::new()),
            observers: Mutex::new(Vec::new()),
            next_observer_id: Mutex::new(0),
        }
    }

    /// The service id this slot belongs to.
    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    /// Inserts an instance, failing when the key is already live.
    pub fn insert(&self, key: &str, implementation: Erased) -> Result<(), FacetError> {
        let mut instances = self.instances.lock();
        if instances.contains_key(key) {
            return Err(FacetError::new(format!(
                "Facet service {} already has a live instance with key {key}",
                self.service_id
            )));
        }
        instances.insert(key.to_string(), implementation);
        Ok(())
    }

    /// Removes an instance.
    pub fn remove(&self, key: &str) -> bool {
        self.instances.lock().remove(key).is_some()
    }

    /// Reads an instance with the expected type.
    pub fn get<T: Send + Sync + 'static>(&self, key: &str) -> Option<Arc<T>> {
        self.instances
            .lock()
            .get(key)
            .and_then(|value| Arc::clone(value).downcast::<T>().ok())
    }

    /// The live instance keys.
    pub fn keys(&self) -> Vec<String> {
        self.instances.lock().keys().cloned().collect()
    }

    /// Registers an observer, delivering the current instances immediately.
    pub fn add_observer(&self, observer: Arc<dyn KeyedObserver>) -> u64 {
        let id = {
            let mut next = self.next_observer_id.lock();
            *next += 1;
            *next
        };
        self.observers.lock().push(ObserverEntry {
            id,
            observer: Arc::clone(&observer),
        });
        let context = background_context();
        for (key, instance) in self.instances.lock().iter() {
            observer.observe(key, instance, context);
        }
        id
    }

    /// Removes an observer by id.
    pub fn remove_observer(&self, id: u64) {
        self.observers.lock().retain(|entry| entry.id != id);
    }

    /// Notifies every observer about an instance.
    pub fn notify(&self, key: &str, instance: &Erased, context: &Context) {
        let observers: Vec<Arc<dyn KeyedObserver>> = self
            .observers
            .lock()
            .iter()
            .map(|entry| Arc::clone(&entry.observer))
            .collect();
        for observer in observers {
            observer.observe(key, instance, context);
        }
    }
}

/// The in-process service directory owned by a [`FacetHost`](crate::FacetHost).
#[derive(Default)]
pub struct FacetServiceDirectory {
    singletons: Mutex<HashMap<String, Arc<SingletonSlot>>>,
    keyed: Mutex<HashMap<String, Arc<KeyedSlot>>>,
}

impl FacetServiceDirectory {
    /// Creates an empty directory.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the singleton slot for `service_id`, creating it on first use.
    pub fn singleton_slot(&self, service_id: &str) -> Arc<SingletonSlot> {
        let mut singletons = self.singletons.lock();
        Arc::clone(
            singletons
                .entry(service_id.to_string())
                .or_insert_with(|| Arc::new(SingletonSlot::new(service_id.to_string()))),
        )
    }

    /// Returns the keyed slot for `service_id`, creating it on first use.
    pub fn keyed_slot(&self, service_id: &str) -> Arc<KeyedSlot> {
        let mut keyed = self.keyed.lock();
        Arc::clone(
            keyed
                .entry(service_id.to_string())
                .or_insert_with(|| Arc::new(KeyedSlot::new(service_id.to_string()))),
        )
    }

    /// Whether a singleton implementation is currently bound.
    pub fn has_singleton(&self, service_id: &str) -> bool {
        self.singletons
            .lock()
            .get(service_id)
            .is_some_and(|slot| slot.is_bound())
    }

    /// Binds a singleton implementation.
    pub fn bind_singleton<T: Send + Sync + 'static>(&self, service_id: &str, value: T) {
        let erased: Erased = Arc::new(value);
        self.singleton_slot(service_id).bind_erased(erased);
    }

    /// Creates a handle view for `service`, scoped to `lifecycle`.
    pub fn use_service<T>(&self, service: &Service<T>, lifecycle: Arc<FacetLifecycle>) -> ServiceHandle<T> {
        ServiceHandle {
            service_id: service.id().to_string(),
            slot: self.singleton_slot(service.id()),
            lifecycle,
            marker: PhantomData,
        }
    }

    /// Creates a keyed spawner for `service`, scoped to `lifecycle`.
    pub fn keyed_spawner<T>(
        &self,
        service: &Service<T>,
        lifecycle: Arc<FacetLifecycle>,
    ) -> KeyedServiceSpawner<T> {
        KeyedServiceSpawner {
            service_id: service.id().to_string(),
            slot: self.keyed_slot(service.id()),
            lifecycle,
            staged: Mutex::new(Vec::new()),
            owned_keys: Arc::new(Mutex::new(HashSet::new())),
            attached: AtomicBool::new(false),
            marker: PhantomData,
        }
    }

    /// Unbinds every singleton implementation.
    pub fn clear(&self) {
        for slot in self.singletons.lock().values() {
            slot.clear();
        }
    }
}

/// A stable, lifecycle-gated view of a singleton service.
///
/// Upstream returns a typed proxy whose property access asserts service access. Rust cannot
/// intercept dereference, so the view exposes [`ServiceHandle::get`], which performs the same
/// assertion and returns the shared implementation.
pub struct ServiceHandle<T> {
    service_id: String,
    slot: Arc<SingletonSlot>,
    lifecycle: Arc<FacetLifecycle>,
    marker: PhantomData<fn() -> T>,
}

impl<T> ServiceHandle<T> {
    /// The service id.
    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    /// Whether the service is bound, ignoring the lifecycle gate.
    pub fn is_available(&self) -> bool {
        self.slot.is_bound()
    }
}

impl<T> Clone for ServiceHandle<T> {
    fn clone(&self) -> Self {
        Self {
            service_id: self.service_id.clone(),
            slot: Arc::clone(&self.slot),
            lifecycle: Arc::clone(&self.lifecycle),
            marker: PhantomData,
        }
    }
}

impl<T: Send + Sync + 'static> ServiceHandle<T> {
    /// Resolves the current implementation.
    ///
    /// Fails when the facet is not active (or is disposing/dead), and when the service is not bound
    /// in this generation.
    pub fn get(&self) -> Result<Arc<T>, FacetError> {
        self.lifecycle.assert_service_access()?;
        self.slot.get::<T>().ok_or_else(|| {
            FacetError::new(format!("Service {} is not provided", self.service_id))
        })
    }
}

impl<T> std::fmt::Debug for ServiceHandle<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceHandle")
            .field("service_id", &self.service_id)
            .field("bound", &self.slot.is_bound())
            .finish()
    }
}

/// Spawns keyed service instances, as returned by `FacetEnvironment::provide_many`.
///
/// Instances created during setup are staged and move into the directory when the generation
/// binds; instances created while active are visible immediately. All instances this spawner
/// created are removed when its facet disposes.
pub struct KeyedServiceSpawner<T> {
    service_id: String,
    slot: Arc<KeyedSlot>,
    lifecycle: Arc<FacetLifecycle>,
    staged: Mutex<Vec<(String, Erased)>>,
    owned_keys: Arc<Mutex<HashSet<String>>>,
    attached: AtomicBool,
    marker: PhantomData<fn() -> T>,
}

impl<T> KeyedServiceSpawner<T> {
    /// The service id.
    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    /// Whether the spawner has been attached to its generation.
    pub fn is_attached(&self) -> bool {
        self.attached.load(Ordering::Acquire)
    }

    /// Spawns one instance under `key`.
    ///
    /// `key` must not be empty and must not collide with a live instance of the same service.
    pub fn spawn(&self, key: &str, implementation: T) -> Result<(), FacetError>
    where
        T: Send + Sync + 'static,
    {
        self.lifecycle.assert_running("provide service instances")?;
        if key.is_empty() {
            return Err(FacetError::new(
                "Facet service instance key must not be empty",
            ));
        }
        if self.owned_keys.lock().contains(key) {
            return Err(FacetError::new(format!(
                "Facet service {} already has a live instance with key {key}",
                self.service_id
            )));
        }
        let erased: Erased = Arc::new(implementation);
        if self.is_attached() {
            self.slot.insert(key, Arc::clone(&erased))?;
            self.owned_keys.lock().insert(key.to_string());
            self.slot
                .notify(key, &erased, background_context());
        } else {
            self.staged.lock().push((key.to_string(), erased));
        }
        Ok(())
    }

    /// Moves staged instances into the directory and binds cleanup to the facet lifecycle.
    ///
    /// Called by the host during the generation's bind step; idempotent.
    pub(crate) fn attach(&self) {
        if self.attached.swap(true, Ordering::AcqRel) {
            return;
        }
        let staged: Vec<(String, Erased)> = std::mem::take(&mut *self.staged.lock());
        for (key, instance) in staged {
            if self.slot.insert(&key, instance).is_ok() {
                self.owned_keys.lock().insert(key);
            }
        }
        let slot = Arc::clone(&self.slot);
        let owned = Arc::clone(&self.owned_keys);
        self.lifecycle.own_unchecked(Box::new(move || {
            Box::pin(async move {
                let keys: Vec<String> = owned.lock().iter().cloned().collect();
                for key in keys {
                    slot.remove(&key);
                }
                Ok(())
            })
        }));
    }

    /// Removes every instance this spawner created.
    pub fn dispose_instances(&self) {
        let keys: Vec<String> = self.owned_keys.lock().iter().cloned().collect();
        for key in keys {
            self.slot.remove(&key);
        }
        self.owned_keys.lock().clear();
    }
}

impl<T> std::fmt::Debug for KeyedServiceSpawner<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeyedServiceSpawner")
            .field("service_id", &self.service_id)
            .field("attached", &self.is_attached())
            .finish()
    }
}

/// A typed keyed-service observer, used by `FacetEnvironment::observe`.
pub struct TypedObserver<T, H> {
    handler: H,
    marker: PhantomData<fn() -> T>,
}

impl<T, H> TypedObserver<T, H> {
    /// Wraps `handler`.
    pub fn new(handler: H) -> Self {
        Self {
            handler,
            marker: PhantomData,
        }
    }
}

impl<T, H> KeyedObserver for TypedObserver<T, H>
where
    T: Send + Sync + 'static,
    H: Fn(Arc<T>, &Context) + Send + Sync + 'static,
{
    fn observe(&self, _key: &str, instance: &Erased, context: &Context) {
        if let Ok(value) = Arc::clone(instance).downcast::<T>() {
            (self.handler)(value, context);
        }
    }
}
