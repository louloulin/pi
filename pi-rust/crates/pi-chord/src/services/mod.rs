//! The remote service boundary: provider, consumer, wire codec and the loopback transport.
//!
//! Upstream `packages/chord/src/services/**` is roughly 2000 lines of TypeScript that answer one
//! question: how does a service that a facet declares reach a facet that requires it when the
//! provider may live in another process? The answer has four parts, and this module ports each of
//! them:
//!
//! | Module | Upstream | Contents |
//! | --- | --- | --- |
//! | [`wire`](self) | `wire.ts` | the strict JSON form of calls, snapshots, updates and control calls |
//! | [`errors`] | `errors.ts` | the eight `RemoteServiceErrorCode`s and [`ServiceError`] |
//! | [`state`] | `state.ts` | the provider-side view of a replicated state member |
//! | [`state_codec`] | `state-codec.ts` | `(instance, member)`-keyed op encoding for the wire |
//! | [`instances`] | `instances.ts` | keyed instance lifetime plus cancellable observations |
//! | [`handle`] | `handle.ts` | the host-owned rebindable slot a consumer reads through |
//! | [`provider`] | `provider.ts` | the allowlisted catalogue, live instances and subscriptions |
//! | [`consumer`] | `consumer.ts` | the binding, stable facades and keyed observations |
//! | [`loopback`] | `loopback.ts` | a transport that connects a binding to a provider in-process |
//!
//! # The one shape change
//!
//! Upstream groups the "remote service support" into `services/`, and the *call* abstraction it uses
//! (`RemoteServiceTransport`) lives in `types.ts`. This port keeps both, but the transport is
//! declared in [`consumer`] next to the only thing that consumes it; [`RemoteServiceTransport`] is
//! re-exported here so hosts import it from one place.
//!
//! # Two deliberate omissions
//!
//! * `services/paths.ts` and `services/registry.ts` do not exist upstream; the modules listed above
//!   are the whole directory other than `state-internals.ts`, whose 20 lines are folded into
//!   [`state`].
//! * `node/{manifest,package,bundle,bundle-loader}.ts` is not ported. Bundling a JavaScript service
//!   graph has no Rust equivalent — the Rust rewrite composes facets in-process — so there is
//!   nothing to preserve. `docs/FEATURE_PI_RS_STATUS.md` records this as out of scope.

pub mod consumer;
pub mod errors;
pub mod handle;
pub mod instances;
pub mod loopback;
pub mod provider;
pub mod state;
#[path = "state-codec.rs"]
pub mod state_codec;
pub mod wire;

pub use consumer::{
    create_remote_service_binding, AccessAssert, ErrorReporter, KeyedServiceProxy,
    RemoteMethodHandle, RemoteObservation, RemoteServiceBinding, RemoteServiceBindingOptions,
    RemoteServiceProxy, RemoteServiceTransport, RemoteStateHandle, RemoteStateListener,
};
pub use errors::{RemoteServiceError, RemoteServiceErrorCode, ServiceError};
pub use handle::ServiceSlot;
pub use instances::{
    DirectoryErrorReporter, InstanceDirectory, InstanceDirectoryEntry, ObserveHandle,
};
pub use loopback::{create_loopback_service_transport, LoopbackServiceTransport};
pub use provider::{
    create_remote_service_endpoint, service_catalogue_call, validate_remote_service_implementation,
    MemberShape, RemoteMethod, RemoteServiceEndpoint, RemoteServiceProvider, RemoteSpawnHandle,
    ServiceImplementation, ServiceMember, ServiceProviderEntry, ServiceProviderListener,
    ServiceSubscription, ServiceUpdatePublisher,
};
pub use state::{MemberSourceListener, ReplicatedStateMember};
pub use state_codec::{
    decode_wire_provider_update, decode_wire_subscription_snapshot, encode_subscription_snapshot,
    ServiceStateDecoder, ServiceStateEncoder,
};
pub use wire::{
    create_service_catalogue_call, create_service_subscribe_call, create_service_unsubscribe_call,
    decode_service_control_call, decoded_ops_from_json, decoded_ops_to_json, parse_service_call,
    parse_service_catalogue, parse_service_instance_address, parse_service_mode,
    parse_service_provider_update, parse_service_subscription_snapshot,
    parse_wire_service_provider_update, parse_wire_service_subscription_snapshot,
    service_catalogue_to_json, service_instance_address_to_json, wire_ops_from_json_value,
    wire_ops_to_json_value, InstanceSnapshot, MemberSnapshot, ProviderUpdate, ServiceCall,
    ServiceControlCall, ServiceInstanceSnapshot, ServiceMemberKind, ServiceMemberSnapshot,
    ServiceProviderUpdate, ServiceSubscriptionSnapshot, SubscriptionSnapshot, WireOpCodec,
    WireServiceInstanceSnapshot, WireServiceMemberSnapshot, WireServiceProviderUpdate,
    WireServiceSubscriptionSnapshot, SERVICE_CATALOGUE_MEMBER, SERVICE_CONTROL_ID,
    SERVICE_SUBSCRIBE_MEMBER, SERVICE_UNSUBSCRIBE_MEMBER,
};
