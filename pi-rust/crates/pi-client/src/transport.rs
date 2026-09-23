//! The byte transport boundary — Rust port of `packages/client/src/transport.ts`.
//!
//! A transport is one ordered, reliable byte channel to a server. The client
//! never inspects a transport beyond `send`/`close`; connection setup,
//! authentication and framing are the transport's problem.
//!
//! Upstream models the two directions as an interface with three required
//! callbacks (`onData` / `onClose` / `onError`). Rust keeps the same contract
//! but hands the callbacks to the factory as one [`ByteTransportHandlers`]
//! object so a factory cannot forget one of them.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;

use crate::errors::ClientError;

/// One ordered, reliable byte channel to a server.
#[async_trait]
pub trait ByteTransport: Send + Sync {
    /// Sends one byte chunk. Calls must be delivered in invocation order.
    async fn send(&self, chunk: Vec<u8>) -> Result<(), ClientError>;

    /// Closes the transport. Repeated calls must be harmless.
    fn close(&self);
}

/// The callbacks a transport must deliver.
///
/// Exactly one terminal callback (`on_close` or `on_error`) is expected; the
/// connection ignores anything that arrives after it has moved on.
#[async_trait]
pub trait ByteTransportHandlers: Send + Sync {
    /// Delivers an arbitrary inbound byte chunk.
    async fn on_data(&self, chunk: Vec<u8>);

    /// Reports an orderly terminal close.
    async fn on_close(&self);

    /// Reports a terminal transport failure.
    async fn on_error(&self, error: ClientError);
}

/// Creates a fresh connected, authenticated transport.
#[async_trait]
pub trait ByteTransportFactory: Send + Sync {
    /// Opens one transport and wires its callbacks.
    async fn connect(
        &self,
        handlers: Arc<dyn ByteTransportHandlers>,
    ) -> Result<Arc<dyn ByteTransport>, ClientError>;
}

/// The boxed future a [`FactoryFn`] closure returns.
pub type ConnectFuture =
    Pin<Box<dyn Future<Output = Result<Arc<dyn ByteTransport>, ClientError>> + Send>>;

/// Adapts a closure to [`ByteTransportFactory`].
///
/// Upstream factories are plain functions; Rust's `async fn` in traits cannot
/// be implemented by closures, so this adapter gives the same ergonomics:
///
/// ```
/// use std::sync::Arc;
/// use pi_client::{ByteTransportFactory, ClientError, FactoryFn};
///
/// let factory: Arc<dyn ByteTransportFactory> = Arc::new(FactoryFn::new(|handlers| {
///     Box::pin(async move {
///         let _ = handlers;
///         Err(ClientError::disconnected("no transport in this example"))
///     })
/// }));
/// # let _ = factory;
/// ```
pub struct FactoryFn<F>(F);

impl<F> FactoryFn<F>
where
    F: Fn(Arc<dyn ByteTransportHandlers>) -> ConnectFuture + Send + Sync,
{
    /// Wraps `factory`.
    pub fn new(factory: F) -> Self {
        Self(factory)
    }
}

#[async_trait]
impl<F> ByteTransportFactory for FactoryFn<F>
where
    F: Fn(Arc<dyn ByteTransportHandlers>) -> ConnectFuture + Send + Sync,
{
    async fn connect(
        &self,
        handlers: Arc<dyn ByteTransportHandlers>,
    ) -> Result<Arc<dyn ByteTransport>, ClientError> {
        (self.0)(handlers).await
    }
}

/// A transport that is already closed and always fails to send.
///
/// Test doubles use this for "connect succeeded but the peer vanished"
/// cases without re-implementing the trait.
pub struct ClosedTransport;

#[async_trait]
impl ByteTransport for ClosedTransport {
    async fn send(&self, _chunk: Vec<u8>) -> Result<(), ClientError> {
        Err(ClientError::disconnected("Transport is closed"))
    }

    fn close(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn factory_fn_delegates_to_the_closure() {
        let factory = FactoryFn::new(|_handlers| {
            Box::pin(async {
                Err::<Arc<dyn ByteTransport>, ClientError>(ClientError::disconnected("boom"))
            })
        });
        let handlers: Arc<dyn ByteTransportHandlers> = Arc::new(NoopHandlers);
        let error = match factory.connect(handlers).await {
            Ok(_) => panic!("expected the factory to fail"),
            Err(error) => error,
        };
        assert_eq!(error, ClientError::disconnected("boom"));
    }

    struct NoopHandlers;

    #[async_trait]
    impl ByteTransportHandlers for NoopHandlers {
        async fn on_data(&self, _chunk: Vec<u8>) {}
        async fn on_close(&self) {}
        async fn on_error(&self, _error: ClientError) {}
    }
}
