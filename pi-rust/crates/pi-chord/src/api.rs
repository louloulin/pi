//! The public entry points, mirroring `packages/chord/src/api.ts`.
//!
//! Everything exported here is the stable surface other crates should use; the
//! [`facets`](crate::facets), [`state`](crate::state) and [`types`](crate::types) modules carry the
//! individual pieces. Remote service bindings (`createRemoteServiceBinding`) are Stage 18.

pub use crate::facets::loader::{
    combine_facet_loaders, create_static_facet_loader, dispose_loaded_facets, CombinedFacetLoader,
    CombinedLoadedFacets, LoadedFacetsImpl, StaticFacetLoader,
};
pub use crate::facets::{
    create_facet_host, FacetEnvironment, FacetHost, FacetLifecycle, FacetServiceDirectory,
    HostPhase, KeyedObserver, KeyedServiceSpawner, KeyedSlot, LifecyclePhase, ServiceHandle,
    SingletonSlot,
};
pub use crate::state::{
    replicated_state, MutableReplicatedState, ReplicatedState, StateReplica, StateSourceSubscription,
    StateSubscription,
};
pub use crate::types::{
    define_facet, define_local_service, define_service, BoxFuture, DeliveryKind, Facet, FacetError,
    FacetFuture, FacetLoader, FacetOptions, FnFacet, LoadResultFuture, LoadedFacets,
    ReplicatedStateDelivery, ReplicatedStateListener, ReplicatedStateSourceListener,
    Service, ServiceCatalogueEntry, ServiceInstanceAddress, ServiceInstanceSnapshot,
    ServiceMemberSnapshot, ServiceMode, ServiceProviderUpdate, ServiceSubscriptionSnapshot,
};
