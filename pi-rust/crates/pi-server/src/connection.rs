//! The byte-transport boundary — Rust port of `packages/server/src/connection.ts`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::errors::ServerError;

/// An established, authorized, ordered byte connection.
///
/// `send` and `close` are asynchronous because a transport may apply
/// backpressure; the rest of the server keeps its synchronous service
/// dispatch (see the crate README) and enqueues frames through a writer task.
#[async_trait]
pub trait ByteConnection: Send + Sync {
    /// Whether the connection can no longer carry bytes.
    fn closed(&self) -> bool;

    /// Sends one complete frame.
    async fn send(&self, chunk: Vec<u8>) -> Result<(), ServerError>;

    /// Closes the connection, optionally after delivering a final frame.
    async fn close(&self, final_chunk: Option<Vec<u8>>) -> Result<(), ServerError>;
}

/// The server-side callbacks for one accepted connection.
pub trait ByteConnectionHandler: Send + Sync {
    /// A chunk of bytes arrived.
    fn on_data(&self, chunk: Vec<u8>);
    /// The transport closed.
    fn on_close(&self);
    /// The transport failed.
    fn on_error(&self, error: ServerError);
}

/// Creates a handler for an accepted connection.
pub type ByteConnectionAcceptor = Arc<dyn Fn(Arc<dyn ByteConnection>) -> Arc<dyn ByteConnectionHandler> + Send + Sync>;

/// The lifecycle stage of one accepted connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStage {
    /// Waiting for the client's `hello`.
    AwaitingHello,
    /// The `hello` arrived and the server is attaching services.
    Handshaking,
    /// The handshake completed; requests are accepted.
    Ready,
    /// The connection is being torn down.
    Closing,
    /// The connection is gone.
    Closed,
}

/// Whether the connection must no longer receive protocol messages.
pub fn is_terminal_connection(stage: ConnectionStage, disconnected: bool) -> bool {
    disconnected || matches!(stage, ConnectionStage::Closing | ConnectionStage::Closed)
}
