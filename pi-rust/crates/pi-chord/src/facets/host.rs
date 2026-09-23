//! The facet kernel: setup, validation, assembly, activation, reload and disposal.
//!
//! Port of `FacetKernel` and `FacetHostImpl` in `packages/chord/src/facets/host.ts`, restricted to
//! locally provided services (remote service sources are Stage 18). The phase machine, the
//! validation errors and the reload cutover order are kept verbatim:
//!
//! ```text
//! setup -> assembling -> activating -> active
//!                                    -> reloading -> (candidate setup) -> (candidate activation)
//!                                                 -> (cutover: rebind singletons, retire previous)
//!                                                 -> (connect keyed instances) -> active
//! ```
//!
//! Reload deliberately activates the *replacement* generation before rebinding singletons, so a
//! replacement can still read the outgoing implementation while it starts, and only then becomes
//! visible to previously captured [`ServiceHandle`]s. Keyed instances connect last, after the
//! outgoing generation is retired.

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::context::Context;
use crate::facets::lifecycle::{Effect, FacetLifecycle};
use crate::facets::registry::{
    FacetServiceDirectory, KeyedServiceSpawner, ServiceHandle, TypedObserver,
};
use crate::state::MutableReplicatedState;
use crate::types::{Facet, FacetError, FacetFuture, FacetOptions, Service, ServiceMode};

/// A service requirement or provision recorded during setup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ServiceReference {
    pub(crate) service_id: String,
    pub(crate) mode: ServiceMode,
}

/// A service provision declared during setup, installed when a generation binds.
pub(crate) enum Provision {
    /// A singleton implementation, installed by writing it into the directory slot.
    Singleton {
        #[allow(dead_code)]
        service_id: String,
        install: Box<dyn FnOnce(&FacetServiceDirectory) + Send>,
    },
    /// A keyed spawner, attached (staged instances become visible) when a generation binds.
    Keyed {
        #[allow(dead_code)]
        service_id: String,
        attach: Box<dyn FnOnce() + Send>,
    },
}

impl std::fmt::Debug for Provision {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Provision::Singleton { service_id, .. } => formatter
                .debug_tuple("Provision::Singleton")
                .field(service_id)
                .finish(),
            Provision::Keyed { service_id, .. } => formatter
                .debug_tuple("Provision::Keyed")
                .field(service_id)
                .finish(),
        }
    }
}

/// The setup-time state of one facet, shared with its [`FacetEnvironment`].
pub(crate) struct FacetRuntimeState {
    requires: Mutex<Vec<ServiceReference>>,
    provides: Mutex<Vec<ServiceReference>>,
    provisions: Mutex<Vec<Provision>>,
}

impl Default for FacetRuntimeState {
    fn default() -> Self {
        Self {
            requires: Mutex::new(Vec::new()),
            provides: Mutex::new(Vec::new()),
            provisions: Mutex::new(Vec::new()),
        }
    }
}

impl FacetRuntimeState {
    fn record_requirement(&self, service_id: &str, mode: ServiceMode) {
        record_reference(&self.requires, service_id, mode);
    }

    fn record_provision(&self, service_id: &str, mode: ServiceMode) {
        record_reference(&self.provides, service_id, mode);
    }

    fn push_provision(&self, provision: Provision) {
        self.provisions.lock().push(provision);
    }

    fn take_provisions(&self) -> Vec<Provision> {
        std::mem::take(&mut *self.provisions.lock())
    }
}

fn record_reference(target: &Mutex<Vec<ServiceReference>>, service_id: &str, mode: ServiceMode) {
    let mut references = target.lock();
    if references
        .iter()
        .any(|reference| reference.service_id == service_id && reference.mode == mode)
    {
        return;
    }
    references.push(ServiceReference {
        service_id: service_id.to_string(),
        mode,
    });
}

/// One facet generation: its id, setup state and lifecycle.
pub(crate) struct FacetRuntime {
    pub(crate) id: String,
    pub(crate) state: Arc<FacetRuntimeState>,
    pub(crate) lifecycle: Arc<FacetLifecycle>,
}

impl Clone for FacetRuntime {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            state: Arc::clone(&self.state),
            lifecycle: Arc::clone(&self.lifecycle),
        }
    }
}

/// The registration surface a facet sees during `setup`.
///
/// Upstream this is an object literal closed over the runtime. In Rust the environment is a value
/// built by the kernel and handed to [`Facet::setup`] by mutable reference; registration methods are
/// only legal while the facet is setting up, matching `assertSettingUp`.
pub struct FacetEnvironment {
    facet_id: String,
    lifecycle: Arc<FacetLifecycle>,
    directory: Arc<FacetServiceDirectory>,
    state: Arc<FacetRuntimeState>,
    on_error: Option<Arc<dyn Fn(FacetError) + Send + Sync>>,
}

impl FacetEnvironment {
    pub(crate) fn new(
        facet_id: String,
        lifecycle: Arc<FacetLifecycle>,
        directory: Arc<FacetServiceDirectory>,
        state: Arc<FacetRuntimeState>,
        on_error: Option<Arc<dyn Fn(FacetError) + Send + Sync>>,
    ) -> Self {
        Self {
            facet_id,
            lifecycle,
            directory,
            state,
            on_error,
        }
    }

    /// The facet id this environment belongs to.
    pub fn facet_id(&self) -> &str {
        &self.facet_id
    }

    /// The facet's lifecycle, for advanced integrations.
    pub fn lifecycle(&self) -> &Arc<FacetLifecycle> {
        &self.lifecycle
    }

    /// Acquires a stable, lifecycle-gated view of a singleton service.
    ///
    /// The view exists from setup on but only resolves while the facet is active.
    pub fn use_service<T>(&mut self, service: &Service<T>) -> Result<ServiceHandle<T>, FacetError> {
        self.lifecycle.assert_setting_up("acquire services")?;
        self.state
            .record_requirement(service.id(), ServiceMode::Singleton);
        Ok(self
            .directory
            .use_service(service, Arc::clone(&self.lifecycle)))
    }

    /// Provides a singleton implementation for the generation.
    pub fn provide<T>(&mut self, service: &Service<T>, implementation: T) -> Result<(), FacetError>
    where
        T: Send + Sync + 'static,
    {
        self.lifecycle.assert_setting_up("provide services")?;
        self.state
            .record_provision(service.id(), ServiceMode::Singleton);
        let service_id = service.id().to_string();
        let install_id = service_id.clone();
        self.state.push_provision(Provision::Singleton {
            service_id,
            install: Box::new(move |directory: &FacetServiceDirectory| {
                directory.bind_singleton(&install_id, implementation);
            }),
        });
        Ok(())
    }

    /// Provides a keyed collection of instances, spawned by the caller.
    ///
    /// Instances may only be spawned while the facet is active (`spawn service instances`).
    /// A spawner created during a reload stages its instances until the cutover completes.
    pub fn provide_many<T: Send + Sync + 'static>(
        &mut self,
        service: &Service<T>,
    ) -> Result<Arc<KeyedServiceSpawner<T>>, FacetError> {
        self.lifecycle
            .assert_setting_up("provide service instances")?;
        self.state
            .record_provision(service.id(), ServiceMode::Keyed);
        let spawner = Arc::new(
            self.directory
                .keyed_spawner(service, Arc::clone(&self.lifecycle)),
        );
        let attach: Arc<KeyedServiceSpawner<T>> = Arc::clone(&spawner);
        self.state.push_provision(Provision::Keyed {
            service_id: service.id().to_string(),
            attach: Box::new(move || attach.attach()),
        });
        Ok(spawner)
    }

    /// Observes a keyed service: `handler` runs for existing instances at activation and for every
    /// instance spawned later.
    pub fn observe<T, H>(&mut self, service: &Service<T>, handler: H) -> Result<(), FacetError>
    where
        T: Send + Sync + 'static,
        H: Fn(Arc<T>, &Context) + Send + Sync + 'static,
    {
        self.lifecycle.assert_setting_up("observe services")?;
        self.state
            .record_requirement(service.id(), ServiceMode::Keyed);
        let directory = Arc::clone(&self.directory);
        let service_id = service.id().to_string();
        let facet_id = self.facet_id.clone();
        let on_error = self.on_error.clone();
        let handler = Arc::new(handler);
        self.lifecycle.observe(Box::new(move || {
            let slot = directory.keyed_slot(&service_id);
            let observer = Arc::new(TypedObserver::new(
                move |value: Arc<T>, context: &Context| {
                    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
                        handler(value, context);
                    }));
                    if let Err(panic) = outcome {
                        if let Some(report) = &on_error {
                            report(FacetError::new(format!(
                                "Facet {facet_id} observer panicked: {}",
                                panic_message(&panic)
                            )));
                        }
                    }
                },
            ));
            let observer_id = slot.add_observer(observer);
            Box::new(move || {
                Box::pin(async move {
                    slot.remove_observer(observer_id);
                    Ok(())
                }) as FacetFuture
            }) as Effect
        }))?;
        Ok(())
    }

    /// Creates a replicated state owned by the facet.
    pub fn replicated_state<T>(
        &mut self,
        initial: T,
    ) -> Result<MutableReplicatedState<T>, FacetError>
    where
        T: Clone + serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
    {
        self.lifecycle.assert_running("create replicated state")?;
        MutableReplicatedState::new(initial)
    }

    /// Owns a synchronous cleanup run when the facet disposes.
    pub fn own<F>(&mut self, disposal: F) -> Result<(), FacetError>
    where
        F: FnOnce() + Send + 'static,
    {
        self.lifecycle.own(Box::new(move || {
            Box::pin(async move {
                disposal();
                Ok(())
            })
        }))
    }

    /// Owns an asynchronous cleanup run when the facet disposes.
    pub fn own_async<F, Fut>(&mut self, disposal: F) -> Result<(), FacetError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(), FacetError>> + Send + 'static,
    {
        self.lifecycle
            .own(Box::new(move || Box::pin(disposal()) as FacetFuture))
    }

    /// Registers a callback run after the facet becomes active.
    pub fn on_activate<F, Fut>(&mut self, callback: F) -> Result<(), FacetError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(), FacetError>> + Send + 'static,
    {
        self.lifecycle
            .on_activate(Box::new(move || Box::pin(callback()) as FacetFuture))
    }

    /// Registers a callback run when the facet deactivates (upstream `onDeactivate`).
    pub fn on_deactivate<F, Fut>(&mut self, callback: F) -> Result<(), FacetError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(), FacetError>> + Send + 'static,
    {
        self.lifecycle
            .own(Box::new(move || Box::pin(callback()) as FacetFuture))
    }
}

impl std::fmt::Debug for FacetEnvironment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FacetEnvironment")
            .field("facet_id", &self.facet_id)
            .finish()
    }
}

/// The facet host phase, mirroring upstream `GenerationPhase`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostPhase {
    /// Created, before `activate`.
    Setup,
    /// Resolving requirements and providers.
    Assembling,
    /// Binding service implementations.
    Connecting,
    /// Running activation callbacks.
    Activating,
    /// Fully activated.
    Active,
    /// Replacing a generation.
    Reloading,
    /// Tearing down.
    Disposing,
    /// Fully disposed.
    Dead,
}

impl HostPhase {
    /// The upstream spelling used in error messages.
    pub fn as_str(self) -> &'static str {
        match self {
            HostPhase::Setup => "setup",
            HostPhase::Assembling => "assembling",
            HostPhase::Connecting => "connecting",
            HostPhase::Activating => "activating",
            HostPhase::Active => "active",
            HostPhase::Reloading => "reloading",
            HostPhase::Disposing => "disposing",
            HostPhase::Dead => "dead",
        }
    }
}

impl std::fmt::Display for HostPhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The composed facet host: one active generation plus the service directory it populates.
pub struct FacetHost {
    directory: Arc<FacetServiceDirectory>,
    facets: HashMap<String, FacetRuntime>,
    activation_order: Vec<String>,
    phase: HostPhase,
    on_error: Option<Arc<dyn Fn(FacetError) + Send + Sync>>,
}

impl FacetHost {
    /// The service directory of the active generation.
    pub fn services(&self) -> &FacetServiceDirectory {
        &self.directory
    }

    /// The host phase.
    pub fn phase(&self) -> HostPhase {
        self.phase
    }

    /// The facet ids of the active generation, in declaration order.
    pub fn facet_ids(&self) -> Vec<String> {
        let mut ids: Vec<&String> = self.facets.keys().collect();
        ids.sort();
        ids.into_iter().cloned().collect()
    }

    /// The activation order used by the active generation.
    pub fn activation_order(&self) -> &[String] {
        &self.activation_order
    }

    /// Resolves a singleton service from the host's own scope.
    ///
    /// Upstream `host.services.use(service)`.
    pub fn use_service<T: Send + Sync + 'static>(
        &self,
        service: &Service<T>,
    ) -> Result<Arc<T>, FacetError> {
        if self.phase != HostPhase::Active {
            return Err(FacetError::new(format!(
                "Facet host service handles cannot be used while {}",
                self.phase
            )));
        }
        self.directory
            .singleton_slot(service.id())
            .get::<T>()
            .ok_or_else(|| FacetError::new(format!("Service {} is not provided", service.id())))
    }

    fn create_runtime(&self, facet_id: &str) -> FacetRuntime {
        FacetRuntime {
            id: facet_id.to_string(),
            state: Arc::new(FacetRuntimeState::default()),
            lifecycle: Arc::new(FacetLifecycle::new(facet_id)),
        }
    }

    fn setup_facet(&self, facet: &Arc<dyn Facet>, record: &FacetRuntime) -> Result<(), FacetError> {
        let mut env = FacetEnvironment::new(
            record.id.clone(),
            Arc::clone(&record.lifecycle),
            Arc::clone(&self.directory),
            Arc::clone(&record.state),
            self.on_error.clone(),
        );
        facet.setup(&mut env)?;
        record.lifecycle.prepared()
    }

    /// Installs the singleton provisions and attaches the keyed provisions of a generation.
    fn bind_provisions(&self, records: &[&FacetRuntime]) -> Vec<Box<dyn FnOnce() + Send>> {
        let mut keyed = Vec::new();
        for record in records {
            for provision in record.state.take_provisions() {
                match provision {
                    Provision::Singleton { install, .. } => install(&self.directory),
                    Provision::Keyed { attach, .. } => keyed.push(attach),
                }
            }
        }
        keyed
    }

    async fn terminate(&mut self, extra: Vec<FacetRuntime>) -> Vec<FacetError> {
        self.phase = HostPhase::Disposing;
        for record in self.facets.values() {
            record.lifecycle.revoke();
        }
        for record in &extra {
            record.lifecycle.revoke();
        }
        let order: Vec<String> = if self.activation_order.is_empty() {
            let mut ids: Vec<String> = self.facets.keys().cloned().collect();
            ids.sort();
            ids
        } else {
            self.activation_order.clone()
        };
        let mut errors = Vec::new();
        for id in order.iter().rev() {
            if let Some(record) = self.facets.remove(id) {
                if let Err(error) = record.lifecycle.dispose().await {
                    errors.push(error);
                }
            }
        }
        for record in self.facets.drain().map(|(_, record)| record) {
            if let Err(error) = record.lifecycle.dispose().await {
                errors.push(error);
            }
        }
        for record in extra.into_iter().rev() {
            if let Err(error) = record.lifecycle.dispose().await {
                errors.push(error);
            }
        }
        self.directory.clear();
        self.activation_order.clear();
        self.phase = HostPhase::Dead;
        errors
    }

    async fn abort(&mut self, extra: Vec<FacetRuntime>) -> Vec<FacetError> {
        self.terminate(extra).await
    }
}

impl std::fmt::Debug for FacetHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FacetHost")
            .field("phase", &self.phase)
            .field("facet_ids", &self.facet_ids())
            .finish()
    }
}

/// Activates a generation of facets (upstream `createFacetHost`).
///
/// The returned host owns the generation. On failure the partially started generation is disposed
/// before the error is returned; if that cleanup also fails, the failures are aggregated under
/// `Facet generation startup and cleanup failed`.
pub async fn create_facet_host(options: FacetOptions) -> Result<FacetHost, FacetError> {
    let facets: Vec<Arc<dyn Facet>> = options.facets().to_vec();
    let ids: Vec<&str> = facets.iter().map(|facet| facet.id()).collect();
    if ids.iter().any(|id| id.is_empty()) {
        return Err(FacetError::new("Facet ID must not be empty"));
    }
    let unique: HashSet<&str> = ids.iter().copied().collect();
    if unique.len() != ids.len() {
        return Err(FacetError::new(
            "Facet IDs must be unique within a generation",
        ));
    }

    let mut host = FacetHost {
        directory: Arc::new(FacetServiceDirectory::new()),
        facets: HashMap::new(),
        activation_order: Vec::new(),
        phase: HostPhase::Setup,
        on_error: options.on_error().cloned(),
    };
    match host.start(&facets).await {
        Ok(()) => Ok(host),
        Err(error) => {
            let cleanup = host.abort(Vec::new()).await;
            if cleanup.is_empty() {
                Err(error)
            } else {
                let mut failures = vec![error];
                failures.extend(cleanup);
                Err(FacetError::aggregate(
                    "Facet generation startup and cleanup failed",
                    failures,
                ))
            }
        }
    }
}

impl FacetHost {
    async fn start(&mut self, facets: &[Arc<dyn Facet>]) -> Result<(), FacetError> {
        let mut records: Vec<FacetRuntime> = Vec::new();
        for facet in facets {
            let record = self.create_runtime(facet.id());
            self.setup_facet(facet, &record)?;
            self.facets.insert(record.id.clone(), record.clone());
            records.push(record);
        }

        self.phase = HostPhase::Assembling;
        self.activation_order = validate_facets(&records)?;
        let lookup: HashMap<&str, &FacetRuntime> = records
            .iter()
            .map(|record| (record.id.as_str(), record))
            .collect();
        let ordered: Vec<&FacetRuntime> = self
            .activation_order
            .iter()
            .filter_map(|id| lookup.get(id.as_str()).copied())
            .collect();
        for attach in self.bind_provisions(&ordered) {
            attach();
        }

        self.phase = HostPhase::Connecting;
        self.phase = HostPhase::Activating;
        for id in self.activation_order.clone() {
            let record = self
                .facets
                .get(&id)
                .expect("activation order is derived from the facet map")
                .lifecycle
                .clone();
            record.activate().await?;
        }
        self.phase = HostPhase::Active;
        Ok(())
    }

    /// Replaces the active generation (upstream `FacetKernel.reload`).
    pub async fn reload(&mut self, facets: Vec<Arc<dyn Facet>>) -> Result<(), FacetError> {
        if self.phase != HostPhase::Active {
            return Err(FacetError::new(format!(
                "Facet host cannot reload while {}",
                self.phase
            )));
        }
        let ids: Vec<&str> = facets.iter().map(|facet| facet.id()).collect();
        if ids.iter().any(|id| id.is_empty()) {
            return Err(FacetError::new("Facet ID must not be empty"));
        }
        let unique: HashSet<&str> = ids.iter().copied().collect();
        if unique.len() != ids.len() {
            return Err(FacetError::new("Reloaded facet IDs must be unique"));
        }
        for id in &ids {
            if !self.facets.contains_key(*id) {
                return Err(FacetError::new(format!("Facet {id} is not active")));
            }
        }
        self.phase = HostPhase::Reloading;

        // Phase 1: set up candidates and check that every candidate preserves its shape.
        let mut staged: Vec<FacetRuntime> = Vec::new();
        let mut candidates: HashMap<String, FacetRuntime> = HashMap::new();
        for facet in &facets {
            let record = self.create_runtime(facet.id());
            let setup = self.setup_facet(facet, &record);
            let shape = setup.and_then(|()| {
                let previous = self
                    .facets
                    .get(facet.id())
                    .expect("every reload id was checked to be active");
                if !same_facet_shape(previous, &record) {
                    return Err(FacetError::new(format!(
                        "Reloaded facet {} must preserve its service requirements and provisions",
                        facet.id()
                    )));
                }
                Ok(())
            });
            staged.push(record);
            if let Err(error) = shape {
                let cleanup = dispose_records(reverse_take(&mut staged)).await;
                if cleanup.is_empty() {
                    self.phase = HostPhase::Active;
                    return Err(error);
                }
                let abort = self.abort(Vec::new()).await;
                let mut failures = vec![error];
                failures.extend(cleanup);
                failures.extend(abort);
                return Err(FacetError::aggregate(
                    "Facet reload setup and cleanup failed",
                    failures,
                ));
            }
            let record = staged.pop().expect("the candidate was just staged");
            candidates.insert(record.id.clone(), record);
        }

        // Phase 2: activate the candidates in the previous generation's relative order.
        let candidate_order: Vec<String> = self
            .activation_order
            .iter()
            .filter(|id| candidates.contains_key(*id))
            .cloned()
            .collect();
        for id in &candidate_order {
            let outcome = candidates
                .get(id)
                .expect("candidate order is derived from the candidate set")
                .lifecycle
                .activate()
                .await;
            if let Err(error) = outcome {
                let disposed: Vec<FacetRuntime> = candidate_order
                    .iter()
                    .map(|id| candidates.remove(id).expect("candidate exists"))
                    .collect();
                let cleanup = dispose_records(disposed).await;
                if cleanup.is_empty() {
                    self.phase = HostPhase::Active;
                    return Err(error);
                }
                let abort = self.abort(Vec::new()).await;
                let mut failures = vec![error];
                failures.extend(cleanup);
                failures.extend(abort);
                return Err(FacetError::aggregate(
                    "Facet reload activation and cleanup failed",
                    failures,
                ));
            }
        }

        // Phase 3: cut over. Swap in the candidates, rebind singletons, retire the outgoing
        // generation, then attach the keyed spawners of the new generation.
        let previous: Vec<FacetRuntime> = candidate_order
            .iter()
            .map(|id| {
                self.facets
                    .insert(id.clone(), candidates.remove(id).expect("candidate exists"))
                    .expect("the outgoing generation had this facet")
            })
            .collect();
        let candidate_refs: Vec<&FacetRuntime> = candidate_order
            .iter()
            .filter_map(|id| self.facets.get(id))
            .collect();
        let keyed = self.bind_provisions(&candidate_refs);

        let mut retirement = previous;
        retirement.reverse();
        let mut retirement_errors = dispose_records(retirement).await;
        if !retirement_errors.is_empty() {
            let error = if retirement_errors.len() == 1 {
                retirement_errors.remove(0)
            } else {
                FacetError::with_causes("Failed to retire replaced facets", retirement_errors)
            };
            let abort = self.abort(Vec::new()).await;
            let mut failures = vec![error];
            failures.extend(abort);
            return Err(FacetError::aggregate(
                "Facet reload failed after cutover",
                failures,
            ));
        }
        for attach in keyed {
            attach();
        }
        self.phase = HostPhase::Active;
        Ok(())
    }

    /// Disposes the active generation (upstream `FacetKernel.dispose`).
    pub async fn dispose(&mut self) -> Result<(), FacetError> {
        if self.phase == HostPhase::Dead {
            return Ok(());
        }
        if self.phase != HostPhase::Active {
            return Err(FacetError::new(format!(
                "Facet host cannot be disposed while {}",
                self.phase
            )));
        }
        let mut errors = self.terminate(Vec::new()).await;
        match errors.len() {
            0 => Ok(()),
            1 => Err(errors.remove(0)),
            _ => Err(FacetError::with_causes(
                "Failed to dispose facet generation",
                errors,
            )),
        }
    }
}

fn reverse_take(records: &mut Vec<FacetRuntime>) -> Vec<FacetRuntime> {
    let mut taken = std::mem::take(records);
    taken.reverse();
    taken
}

async fn dispose_records(records: Vec<FacetRuntime>) -> Vec<FacetError> {
    let mut errors = Vec::new();
    for record in records {
        if let Err(error) = record.lifecycle.dispose().await {
            errors.push(error);
        }
    }
    errors
}

fn same_facet_shape(left: &FacetRuntime, right: &FacetRuntime) -> bool {
    same_references(&left.state.requires.lock(), &right.state.requires.lock())
        && same_references(&left.state.provides.lock(), &right.state.provides.lock())
}

fn same_references(left: &[ServiceReference], right: &[ServiceReference]) -> bool {
    left.len() == right.len()
        && left.iter().all(|reference| {
            right.iter().any(|other| {
                other.service_id == reference.service_id && other.mode == reference.mode
            })
        })
}

/// Validates a generation and returns its activation order (upstream `validateFacets`).
///
/// Fails on a duplicate provision, a mode mismatch, a requirement nothing provides, or a
/// dependency cycle.
pub(crate) fn validate_facets(records: &[FacetRuntime]) -> Result<Vec<String>, FacetError> {
    let mut providers: HashMap<String, (Option<String>, ServiceMode)> = HashMap::new();
    for record in records {
        for provision in record.state.provides.lock().iter() {
            if let Some((facet_id, mode)) = providers.get(&provision.service_id) {
                if *mode != provision.mode {
                    return Err(FacetError::new(format!(
                        "Service {} is provided as both {} and {}",
                        provision.service_id,
                        mode.as_str(),
                        provision.mode.as_str()
                    )));
                }
                let provider = facet_id.clone().unwrap_or_else(|| "the host".to_string());
                return Err(FacetError::new(format!(
                    "Service {} is provided by both {provider} and {}",
                    provision.service_id, record.id
                )));
            }
            providers.insert(
                provision.service_id.clone(),
                (Some(record.id.clone()), provision.mode),
            );
        }
    }

    let mut dependencies: HashMap<&str, HashSet<&str>> = records
        .iter()
        .map(|record| (record.id.as_str(), HashSet::new()))
        .collect();
    let mut dependents: HashMap<&str, HashSet<&str>> = records
        .iter()
        .map(|record| (record.id.as_str(), HashSet::new()))
        .collect();
    for record in records {
        for requirement in record.state.requires.lock().iter() {
            let Some((facet_id, mode)) = providers.get(&requirement.service_id) else {
                return Err(FacetError::new(format!(
                    "Facet {} requires local/{}/{}, but no facet provides it",
                    record.id,
                    requirement.service_id,
                    requirement.mode.as_str()
                )));
            };
            if *mode != requirement.mode {
                return Err(FacetError::new(format!(
                    "Facet {} requires {} as {}, but {} provides it as {}",
                    record.id,
                    requirement.service_id,
                    requirement.mode.as_str(),
                    facet_id.as_deref().unwrap_or("the host"),
                    mode.as_str()
                )));
            }
            let Some(provider_id) = facet_id else {
                continue;
            };
            if *provider_id == record.id {
                continue;
            }
            if let Some(set) = dependencies.get_mut(record.id.as_str()) {
                set.insert(provider_id.as_str());
            }
            if let Some(set) = dependents.get_mut(provider_id.as_str()) {
                set.insert(record.id.as_str());
            }
        }
    }

    let mut remaining: HashMap<&str, usize> = dependencies
        .iter()
        .map(|(id, values)| (*id, values.len()))
        .collect();
    let mut ready: Vec<&str> = records
        .iter()
        .map(|record| record.id.as_str())
        .filter(|id| remaining.get(id) == Some(&0))
        .collect();
    let mut order: Vec<String> = Vec::new();
    while !ready.is_empty() {
        let id = ready.remove(0);
        order.push(id.to_string());
        let mut newly_ready: Vec<&str> = Vec::new();
        if let Some(set) = dependents.get(id) {
            for dependent in set.iter() {
                if let Some(count) = remaining.get_mut(*dependent) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        newly_ready.push(dependent);
                    }
                }
            }
        }
        newly_ready.sort();
        ready.extend(newly_ready);
    }
    if order.len() != records.len() {
        let cycle: Vec<&str> = records
            .iter()
            .map(|record| record.id.as_str())
            .filter(|id| remaining.get(id).copied().unwrap_or(0) > 0)
            .collect();
        return Err(FacetError::new(format!(
            "Facet dependency cycle: {}",
            cycle.join(", ")
        )));
    }
    Ok(order)
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_string()
    }
}
