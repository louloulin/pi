//! The listener boundary — Rust port of `packages/server/src/listener.ts`.

use async_trait::async_trait;

use crate::connection::ByteConnectionAcceptor;
use crate::errors::ServerError;

/// Supplies established byte connections after any required transport
/// authentication.
#[async_trait]
pub trait ServerListener: Send + Sync {
    /// Starts listening and passes authorized connections to `accept`.
    async fn start(&self, accept: ByteConnectionAcceptor) -> Result<(), ServerError>;

    /// Stops listening and closes every connection it owns.
    async fn close(&self) -> Result<(), ServerError>;
}
