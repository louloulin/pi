//! Facet, service and replicated-state type vocabulary.
//!
//! This module mirrors the type surface of the upstream `packages/chord/src/types.ts` and
//! `services/state.ts` for the parts that belong to the runtime-agnostic core layer. The
//! remote/wire vocabulary lives in [`crate::services`] (`parse_service_call`,
//! `ServiceSubscriptionSnapshot`, `ServiceProviderUpdate`, ...).

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use crate::context::Context;
use crate::delta::DeltaError;
use crate::facets::FacetEnvironment;

/// A boxed, sendable future with no output, used for disposals and lifecycle effects.
pub type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// A boxed, sendable fallible future returned by facet lifecycle callbacks.
pub type FacetFuture = Pin<Box<dyn Future<Output = Result<(), FacetError>> + Send>>;

/// The error type used across the facet kernel.
///
/// Upstream throws JavaScript values and aggregates cleanup failures with `AggregateError`. In
/// Rust every failure becomes a [`FacetError`] carrying a human-readable message plus the optional
/// list of nested failure messages. [`fmt::Display`] renders the message and appends the causes, so
/// assertions can still match on the upstream wording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FacetError {
    message: String,
    causes: Vec<String>,
}

impl FacetError {
    /// Creates an error with the given message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            causes: Vec::new(),
        }
    }

    /// Creates an error with the given message and nested failures.
    pub fn with_causes(message: impl Into<String>, causes: Vec<FacetError>) -> Self {
        FacetError::aggregate(message, causes)
    }

    /// Wraps `failures` under a single aggregated message.
    ///
    /// This is the Rust counterpart of `new AggregateError(errors, message)`: the aggregate message
    /// is kept and every failure contributes its rendered form to [`FacetError::causes`].
    pub fn aggregate(
        message: impl Into<String>,
        failures: impl IntoIterator<Item = FacetError>,
    ) -> FacetError {
        let causes: Vec<String> = failures
            .into_iter()
            .map(|error| error.to_string())
            .collect();
        Self {
            message: message.into(),
            causes,
        }
    }

    /// The error's own message, without nested causes.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The nested failure messages, in the order they were collected.
    pub fn causes(&self) -> &[String] {
        &self.causes
    }

    /// Returns the same error with `context` prepended to the message.
    pub fn context(mut self, context: impl AsRef<str>) -> Self {
        self.message = format!("{}: {}", context.as_ref(), self.message);
        self
    }
}

impl fmt::Display for FacetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)?;
        if !self.causes.is_empty() {
            formatter.write_str(": ")?;
            formatter.write_str(&self.causes.join("; "))?;
        }
        Ok(())
    }
}

impl Error for FacetError {}

impl From<&str> for FacetError {
    fn from(message: &str) -> Self {
        FacetError::new(message)
    }
}

impl From<String> for FacetError {
    fn from(message: String) -> Self {
        FacetError::new(message)
    }
}

impl From<DeltaError> for FacetError {
    fn from(error: DeltaError) -> Self {
        FacetError::new(error.to_string())
    }
}

/// Whether a service is a single instance per generation or a keyed collection of instances.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServiceMode {
    /// One implementation shared by every consumer, bound by `provide`.
    Singleton,
    /// Many implementations addressed by a string key, spawned by `provide_many`.
    Keyed,
}

impl ServiceMode {
    /// The wire/display spelling (`"singleton"` or `"keyed"`).
    pub fn as_str(self) -> &'static str {
        match self {
            ServiceMode::Singleton => "singleton",
            ServiceMode::Keyed => "keyed",
        }
    }
}

impl fmt::Display for ServiceMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A typed service identity: a stable string id plus the mode it is provided with.
///
/// The type parameter carries the value type a consumer receives, so a facet can only bind an
/// implementation that matches the handle it will read from. `T` is normally a concrete struct
/// describing the service contract; wrapping a trait object (`Box<dyn Contract>`) works as well.
pub struct Service<T> {
    id: String,
    local: bool,
    marker: PhantomData<fn() -> T>,
}

impl<T> Service<T> {
    /// The service id, e.g. `local/test.experimental.source`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether the service is provided in-process by a facet (upstream `Service.local`).
    pub fn is_local(&self) -> bool {
        self.local
    }
}

impl<T> Clone for Service<T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            local: self.local,
            marker: PhantomData,
        }
    }
}

impl<T> fmt::Debug for Service<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Service")
            .field("id", &self.id)
            .field("local", &self.local)
            .finish()
    }
}

impl<T> PartialEq for Service<T> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.local == other.local
    }
}

impl<T> Eq for Service<T> {}

fn validate_service_id(id: &str) -> Result<(), FacetError> {
    if id.is_empty() {
        return Err(FacetError::new("Service ID must not be empty"));
    }
    if id.starts_with("$chord.") {
        return Err(FacetError::new(
            "Service IDs beginning with $chord. are reserved",
        ));
    }
    Ok(())
}

/// Declares a remote-capable service (upstream `defineService`).
pub fn define_service<T>(id: &str) -> Result<Service<T>, FacetError> {
    validate_service_id(id)?;
    Ok(Service {
        id: id.to_string(),
        local: false,
        marker: PhantomData,
    })
}

/// Declares a service that must be provided in-process (upstream `defineLocalService`).
pub fn define_local_service<T>(id: &str) -> Result<Service<T>, FacetError> {
    validate_service_id(id)?;
    Ok(Service {
        id: id.to_string(),
        local: true,
        marker: PhantomData,
    })
}

/// A unit of setup that registers service requirements, provisions and lifecycle callbacks.
///
/// Upstream `Facet.setup(env): void` is synchronous and signals failure by throwing. The Rust
/// counterpart returns `Result`; returning `Err` aborts activation with the same semantics as a
/// thrown error. Setup is synchronous by construction — an `async fn` cannot satisfy this trait.
pub trait Facet: Send + Sync + 'static {
    /// The facet's unique id within a generation.
    fn id(&self) -> &str;

    /// Registers requirements, provisions and lifecycle callbacks.
    fn setup(&self, env: &mut FacetEnvironment) -> Result<(), FacetError>;
}

/// A facet built from a closure, as returned by [`define_facet`].
pub struct FnFacet<F> {
    id: String,
    setup: F,
}

impl<F> FnFacet<F> {
    /// Wraps `setup` under the facet id `id`.
    pub fn new(id: impl Into<String>, setup: F) -> Self {
        Self {
            id: id.into(),
            setup,
        }
    }
}

impl<F> Facet for FnFacet<F>
where
    F: Fn(&mut FacetEnvironment) -> Result<(), FacetError> + Send + Sync + 'static,
{
    fn id(&self) -> &str {
        &self.id
    }

    fn setup(&self, env: &mut FacetEnvironment) -> Result<(), FacetError> {
        (self.setup)(env)
    }
}

/// Defines a facet from a setup closure (upstream `defineFacet`).
pub fn define_facet<F>(id: &str, setup: F) -> Arc<dyn Facet>
where
    F: Fn(&mut FacetEnvironment) -> Result<(), FacetError> + Send + Sync + 'static,
{
    Arc::new(FnFacet::new(id, setup))
}

/// Options accepted by [`crate::create_facet_host`].
///
/// Upstream also takes `serviceSources`; remote service sources are Stage 18 and are absent here.
#[derive(Clone, Default)]
pub struct FacetOptions {
    facets: Vec<Arc<dyn Facet>>,
    on_error: Option<Arc<dyn Fn(FacetError) + Send + Sync>>,
}

impl FacetOptions {
    /// Creates options for the given generation.
    pub fn new(facets: Vec<Arc<dyn Facet>>) -> Self {
        Self {
            facets,
            on_error: None,
        }
    }

    /// Sets the observer called when a background facet failure surfaces (upstream `onError`).
    pub fn with_on_error(mut self, on_error: Arc<dyn Fn(FacetError) + Send + Sync>) -> Self {
        self.on_error = Some(on_error);
        self
    }

    /// The facets to activate, in declaration order.
    pub fn facets(&self) -> &[Arc<dyn Facet>] {
        &self.facets
    }

    /// The background failure observer, if any.
    pub fn on_error(&self) -> Option<&Arc<dyn Fn(FacetError) + Send + Sync>> {
        self.on_error.as_ref()
    }
}

impl fmt::Debug for FacetOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FacetOptions")
            .field("facet_ids", &self.facet_ids())
            .field("has_on_error", &self.on_error.is_some())
            .finish()
    }
}

impl FacetOptions {
    fn facet_ids(&self) -> Vec<&str> {
        self.facets.iter().map(|facet| facet.id()).collect()
    }
}

/// A resolved generation of facets plus the disposals that own it.
///
/// Upstream `LoadedFacets` carries `facets` and `dispose()`; disposal is idempotent there. The Rust
/// counterpart is a trait object because the host must be able to dispose a generation it did not
/// construct itself.
pub trait LoadedFacets: Send + Sync {
    /// The facets of this generation, in declaration order.
    fn facets(&self) -> Vec<Arc<dyn Facet>>;

    /// Disposes everything the loader owns. Idempotent.
    fn dispose(&self) -> FacetFuture;
}

/// A lazily produced generation of facets (upstream `FacetLoader`).
pub trait FacetLoader: Send + Sync {
    /// Produces a generation. The future must be awaited on the host's executor.
    fn load(&self) -> LoadResultFuture;
}

/// The future returned by [`FacetLoader::load`].
pub type LoadResultFuture =
    Pin<Box<dyn Future<Output = Result<Box<dyn LoadedFacets>, FacetError>> + Send>>;

/// One entry in a service catalogue: what a source offers and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceCatalogueEntry {
    /// The service id.
    pub service_id: String,
    /// The mode the source provides it with.
    pub mode: ServiceMode,
}

impl ServiceCatalogueEntry {
    /// Creates a catalogue entry.
    pub fn new(service_id: impl Into<String>, mode: ServiceMode) -> Self {
        Self {
            service_id: service_id.into(),
            mode,
        }
    }
}

/// The address of one service instance: the key and generation of the instance.
///
/// Upstream's `ServiceInstanceAddress` is exactly `{ key, generation }` — the service id is
/// carried by the surrounding call, update or subscription. Matching that shape keeps the wire
/// validators honest.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ServiceInstanceAddress {
    /// The instance key (`""` is never used; singletons carry no address at all).
    pub key: String,
    /// The generation the instance was created in.
    pub generation: u64,
}

impl ServiceInstanceAddress {
    /// Creates an address.
    pub fn new(key: impl Into<String>, generation: u64) -> Self {
        Self {
            key: key.into(),
            generation,
        }
    }
}

/// Whether a replicated-state delivery is the initial snapshot or an incremental update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryKind {
    /// The listener is being handed the current value for the first time.
    Hydrate,
    /// The listener is being handed a new value produced by a publish.
    Update,
}

/// Describes one replicated-state delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplicatedStateDelivery {
    /// Hydrate or update.
    pub kind: DeliveryKind,
    /// The published sequence this delivery belongs to.
    pub sequence: u64,
}

impl ReplicatedStateDelivery {
    /// Creates a delivery descriptor.
    pub fn new(kind: DeliveryKind, sequence: u64) -> Self {
        Self { kind, sequence }
    }

    /// Whether this delivery is the initial snapshot.
    pub fn is_hydrate(&self) -> bool {
        self.kind == DeliveryKind::Hydrate
    }
}

/// A listener invoked with a replicated-state value.
pub type ReplicatedStateListener<T> =
    Arc<dyn Fn(&T, &Context, ReplicatedStateDelivery) + Send + Sync>;

/// A source listener invoked with the delta batch that produced a new value.
pub type ReplicatedStateSourceListener =
    Arc<dyn Fn(&[crate::delta::Op], u64, &Context) + Send + Sync>;
