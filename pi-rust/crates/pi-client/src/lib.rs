//! Remote client SDK for the Pi server protocol — Rust port of
//! `packages/client`.
//!
//! The upstream package is 1,135 lines across `client.ts` (the public facade),
//! `connection.ts` (the connection state machine), `unix.ts` (Node's
//! `AF_UNIX` transport and local discovery), `transport.ts` / `types.ts` (the
//! vocabulary) and `errors.ts` / `promise.ts` (error classes and deferred
//! helpers). This crate keeps the same split:
//!
//! | Upstream | Rust |
//! | --- | --- |
//! | `client.ts` | [`Client`] / [`ClientOptions`] |
//! | `connection.ts` | `connection` module (`pub(crate)`) |
//! | `transport.ts` | [`ByteTransport`] / [`ByteTransportFactory`] |
//! | `types.ts` | `types` module plus [`ServiceSubscription`] |
//! | `errors.ts` | [`ClientError`] |
//! | `promise.ts` | `tokio::sync::oneshot` |
//! | `unix.ts` | [`unix`] (Unix only) |
//!
//! # Deliberate divergences
//!
//! * `Client.connect(options)` cannot coexist with the instance method in Rust,
//!   so the static constructor is [`Client::connect_with`].
//! * Upstream's `AbortSignal` parameter becomes an explicit
//!   [`RequestCancel`] token.
//! * `createClientServiceTransport` is **not** ported: Chord's
//!   `RemoteServiceTransport` is a synchronous trait
//!   (`pi_chord::services::RemoteServiceTransport`), so an async client cannot
//!   implement it without blocking a runtime thread. Hosts that need the bridge
//!   should adapt [`Client::request`] and [`Client::subscribe_service`] directly.
//! * Options that upstream validates by throwing a `TypeError` report
//!   [`ClientError::Configuration`] instead.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use pi_client::{
//!     unix::{create_unix_transport_factory, UnixTransportOptions},
//!     Client, ClientOptions,
//! };
//!
//! # async fn run() -> Result<(), pi_client::ClientError> {
//! let factory = create_unix_transport_factory(UnixTransportOptions::new("/tmp/pi.sock"))?;
//! let client = Client::connect_with(ClientOptions::new(
//!     factory,
//!     "00000000-0000-4000-8000-000000000001",
//! ))
//! .await?;
//! let catalogue = client
//!     .service_catalogue(pi_client::RpcTarget::Server(pi_client::ServerTarget {
//!         server_id: client.server_id().to_owned(),
//!     }), None)
//!     .await?;
//! println!("{} services", catalogue.len());
//! client.dispose().await;
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

mod cancel;
mod client;
mod connection;
mod errors;
mod subscription;
mod transport;
mod types;

pub mod testing;

#[cfg(unix)]
pub mod unix;

pub use cancel::RequestCancel;
pub use client::{Client, ClientOptions};
pub use errors::{to_disconnected, ClientError};
pub use subscription::{service_listener, ServiceSubscription, ServiceUpdateListener};
pub use transport::{
    ByteTransport, ByteTransportFactory, ByteTransportHandlers, ClosedTransport, ConnectFuture,
    FactoryFn,
};
pub use types::{
    AttachmentChangeListener, ConnectionState, ConnectionStateChange, ConnectionStateListener,
    ListenerErrorHandler, ServiceListener, Unsubscribe,
};

pub use pi_chord::services::{
    ProviderUpdate, ServiceCall, ServiceProviderUpdate, ServiceSubscriptionSnapshot,
    SubscriptionSnapshot,
};
pub use pi_chord::types::{ServiceCatalogueEntry, ServiceMode};
pub use pi_protocol::rpc::{RpcTarget, ServerHello, ServerTarget, SessionTarget, PROTOCOL_VERSION};

#[cfg(unix)]
pub use unix::{
    create_unix_transport_factory, discover_unix_servers, discover_unix_servers_with_timeout,
    UnixServerRoute, UnixTransportOptions,
};
