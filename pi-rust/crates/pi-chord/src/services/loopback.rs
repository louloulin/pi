//! In-process transport that connects a binding directly to a provider, ported from
//! `packages/chord/src/services/loopback.ts`.
//!
//! This changes no remote service semantics — it is the same call path a real transport takes, minus
//! the encoding. It is what a host uses when the provider lives in the same process, which is also
//! how the tests exercise the whole boundary without a wire.
//!
//! Upstream wraps the provider subscription in a fresh object whose `activate`/`close` forward to
//! the provider; here the provider subscription already implements
//! [`ServiceSubscription`](super::provider::ServiceSubscription), so it is returned directly.

use std::sync::Arc;

use crate::context::Context;
use crate::json::JsonValue;
use crate::types::ServiceMode;

use super::consumer::RemoteServiceTransport;
use super::errors::ServiceError;
use super::provider::{RemoteServiceProvider, ServiceProviderListener, ServiceSubscription};
use super::wire::ServiceCall;

/// A loopback implementation of [`RemoteServiceTransport`].
pub struct LoopbackServiceTransport {
    provider: Arc<RemoteServiceProvider>,
}

impl std::fmt::Debug for LoopbackServiceTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoopbackServiceTransport")
            .field("disposed", &self.provider.is_disposed())
            .finish()
    }
}

impl LoopbackServiceTransport {
    /// Creates a transport over `provider`.
    pub fn new(provider: Arc<RemoteServiceProvider>) -> Self {
        Self { provider }
    }

    /// The wrapped provider.
    pub fn provider(&self) -> &Arc<RemoteServiceProvider> {
        &self.provider
    }
}

impl RemoteServiceTransport for LoopbackServiceTransport {
    fn invoke(&self, call: &ServiceCall, context: &Context) -> Result<JsonValue, ServiceError> {
        self.provider.invoke(call, context)
    }

    fn subscribe(
        &self,
        service_id: &str,
        mode: ServiceMode,
        listener: ServiceProviderListener,
    ) -> Result<Arc<dyn ServiceSubscription>, ServiceError> {
        self.provider.subscribe(service_id, mode, listener)
    }
}

/// Connects a provider to a binding without changing remote service semantics.
pub fn create_loopback_service_transport(
    provider: Arc<RemoteServiceProvider>,
) -> Arc<LoopbackServiceTransport> {
    Arc::new(LoopbackServiceTransport::new(provider))
}
