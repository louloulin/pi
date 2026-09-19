//! The public entry points, mirroring `packages/chord/src/api.ts`.
//!
//! Everything exported here is the stable surface other crates should use; the
//! [`facets`](crate::facets), [`services`](crate::services), [`state`](crate::state) and
//! [`types`](crate::types) modules carry the individual pieces.

pub use crate::facets::loader::{
    combine_facet_loaders, create_static_facet_loader, dispose_loaded_facets, CombinedFacetLoader,
    CombinedLoadedFacets, LoadedFacetsImpl, StaticFacetLoader,
};
pub use crate::facets::{
    create_facet_host, FacetEnvironment, FacetHost, FacetLifecycle, FacetServiceDirectory,
    HostPhase, KeyedObserver, KeyedServiceSpawner, KeyedSlot, LifecyclePhase, ServiceHandle,
    SingletonSlot,
};
pub use crate::services::{
    create_loopback_service_transport, create_remote_service_binding, create_remote_service_endpoint,
    create_service_catalogue_call, create_service_subscribe_call, create_service_unsubscribe_call,
    decode_service_control_call, decode_wire_provider_update, decode_wire_subscription_snapshot,
    decoded_ops_from_json, decoded_ops_to_json, encode_subscription_snapshot,
    parse_service_call, parse_service_catalogue, parse_service_instance_address, parse_service_mode,
    parse_service_provider_update, parse_service_subscription_snapshot,
    parse_wire_service_provider_update, parse_wire_service_subscription_snapshot,
    service_catalogue_to_json, service_instance_address_to_json, wire_ops_from_json_value,
    wire_ops_to_json_value, AccessAssert, DirectoryErrorReporter, ErrorReporter, InstanceDirectory,
    InstanceDirectoryEntry, InstanceSnapshot, KeyedServiceProxy, LoopbackServiceTransport,
    MemberShape, MemberSnapshot, MemberSourceListener, ObserveHandle, ProviderUpdate, RemoteMethod,
    RemoteMethodHandle, RemoteObservation, RemoteServiceBinding, RemoteServiceBindingOptions,
    RemoteServiceEndpoint, RemoteServiceError, RemoteServiceErrorCode, RemoteServiceProxy,
    RemoteServiceProvider, RemoteServiceTransport, RemoteSpawnHandle, RemoteStateHandle,
    RemoteStateListener, ReplicatedStateMember, ServiceCall, ServiceControlCall, ServiceError,
    ServiceImplementation, ServiceInstanceSnapshot, ServiceMember, ServiceMemberKind,
    ServiceMemberSnapshot, ServiceProviderEntry, ServiceProviderListener, ServiceProviderUpdate,
    ServiceSlot,
    ServiceStateDecoder, ServiceStateEncoder, ServiceSubscription, ServiceSubscriptionSnapshot,
    ServiceUpdatePublisher, SubscriptionSnapshot, WireOpCodec, WireServiceInstanceSnapshot,
    WireServiceMemberSnapshot, WireServiceProviderUpdate, WireServiceSubscriptionSnapshot,
};
pub use crate::state::{
    replicated_state, MutableReplicatedState, ReplicatedState, StateReplica, StateSourceSubscription,
    StateSubscription,
};
pub use crate::types::{
    define_facet, define_local_service, define_service, BoxFuture, DeliveryKind, Facet, FacetError,
    FacetFuture, FacetLoader, FacetOptions, FnFacet, LoadResultFuture, LoadedFacets,
    ReplicatedStateDelivery, ReplicatedStateListener, ReplicatedStateSourceListener,
    Service, ServiceCatalogueEntry, ServiceInstanceAddress, ServiceMode,
};
