//! Facets: the typed service graph and its lifecycle.
//!
//! Upstream mapping (`packages/chord/src/facets/`):
//!
//! | Rust module | Upstream | Notes |
//! | --- | --- | --- |
//! | [`lifecycle`] | `host.ts` (`FacetLifecycle`) | effects, observations, service-access gate |
//! | [`registry`] | `services/handle.ts` + `services/instances.ts` (local half) | singleton slots, keyed instances, observers |
//! | [`host`] | `host.ts` (`FacetKernel`, `FacetHostImpl`) | setup, validation, assembly, activation, reload, disposal |
//! | [`loader`] | `facets/loader.ts` + `api.ts` (`combineFacetLoaders`) | generation loading and disposal |
//!
//! The remote half of `host.ts` (service sources, `RemoteServiceProvider`, loopback transport,
//! generation addressing) is Stage 18. This module therefore provides a *local* service directory:
//! singleton provisions and a keyed instance registry with observers, addressed by service id
//! rather than by generation. See the crate README for the consequences.

pub mod host;
pub mod lifecycle;
pub mod loader;
pub mod registry;

pub use host::{create_facet_host, FacetEnvironment, FacetHost, HostPhase};
pub use lifecycle::{Effect, FacetLifecycle, LifecyclePhase, ObservationStart};
pub use loader::{
    combine_facet_loaders, create_static_facet_loader, dispose_loaded_facets, CombinedFacetLoader,
    CombinedLoadedFacets, LoadedFacetsImpl, StaticFacetLoader,
};
pub use registry::{
    Erased, FacetServiceDirectory, KeyedObserver, KeyedServiceSpawner, KeyedSlot, ServiceHandle,
    SingletonSlot, TypedObserver,
};
