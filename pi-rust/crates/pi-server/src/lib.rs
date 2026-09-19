//! Agent sessions exposed as a server.
//!
//! `pi-server` is the Rust port of the upstream TypeScript `packages/server`
//! package: it accepts byte connections, performs the versioned handshake,
//! routes RPC requests to either a server-wide service host or one attached
//! session, forwards service state updates, and maps host failures onto the
//! bounded wire error vocabulary from [`pi_protocol::rpc`].
//!
//! # Architecture
//!
//! Service dispatch is **synchronous** because the hosted layer (`pi-chord`
//! service providers, `pi-agent-core` sessions) is synchronous. The async
//! boundary is confined to transport I/O:
//!
//! ```text
//! transport task          server connection          blocking pool
//! ──────────────          ─────────────────          ─────────────
//! read bytes  ──data──▶  decode + validate
//!                        handshake (sync)
//!                        route request  ──spawn_blocking──▶  ServiceCall
//!                        ◀──── response ───────────────────  Result
//! write frame ◀─queue──  encode
//! ```
//!
//! # Handshake
//!
//! The first frame must be `hello` carrying [`pi_protocol::rpc::PROTOCOL_VERSION`].
//! A mismatch or a non-`hello` first frame produces `hello_error` and closes
//! the connection; a client that never sends `hello` is dropped after the
//! configured handshake timeout.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use pi_server::{Server, ServerHost, ServerOptions, SessionId};
//! use pi_server::transports::MemoryListener;
//!
//! # async fn run(host: Arc<dyn ServerHost<SessionId>>) -> Result<(), pi_server::ServerError> {
//! let listener = MemoryListener::new();
//! let server_id = "3f1d2c9a-7b4e-4a1f-8c2d-9e5b6a7c8d90";
//! let options = ServerOptions::new(server_id, vec![listener.clone()]);
//! let server = Server::new(host, options)?;
//! server.start().await?;
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

pub mod connection;
pub mod errors;
pub mod listener;
pub mod server;
pub mod session_router;
pub mod testing;
pub mod transports;
pub mod types;

pub use connection::{
    is_terminal_connection, ByteConnection, ByteConnectionAcceptor, ByteConnectionHandler,
    ConnectionStage,
};
pub use errors::{ServerError, ServerErrorCode, INTERNAL_SERVER_ERROR_MESSAGE};
pub use listener::ServerListener;
pub use server::Server;
pub use session_router::SessionRouter;
pub use types::{
    target_session, AttachmentSink, ConnectionCountObserver, ErrorObserver,
    RoutedServerPresentation, RoutedServerServiceAttachment, RoutedServerServiceHost,
    RoutedSessionAttachment, RoutedSessionHandle, ServerHost, ServerOptions, ServicePublish,
    SessionId, SessionMetadata, TerminationFuture,
};

/// The upstream package this crate ports.
pub const UPSTREAM_PACKAGE: &str = "packages/server";
