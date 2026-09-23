//! Streaming API surface.
//!
//! Stage 0 only declares the trait and the event-stream wrapper. Stage 1
//! fills in the OpenAI / Anthropic adapters and the faux provider used by
//! `pi-agent-core` tests.

use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use pi_protocol::{AssistantMessageEvent, Context, Model};
use std::pin::Pin;

use crate::types::{SimpleStreamOptions, StreamError};

/// Boxed async stream of assistant message events.
pub type AssistantMessageEventStream =
    Pin<Box<dyn Stream<Item = Result<AssistantMessageEvent, StreamError>> + Send>>;

/// Streaming trait used by `pi-agent-core`.
///
/// Mirrors `StreamFn` in `packages/agent/src/types.ts`. Implementors must:
///
/// * Never panic for request/model/runtime failures — encode them in the
///   returned stream as `AssistantMessageEvent::Error` followed by
///   `AssistantMessageEvent::Done { stop_reason: Error, .. }`.
/// * Honour `SimpleStreamOptions::signal` and emit `Done { stop_reason:
///   Aborted, .. }` promptly after cancellation.
#[async_trait]
pub trait StreamFn: Send + Sync {
    /// Stream a single assistant turn.
    async fn stream_simple(
        &self,
        model: &Model,
        ctx: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, StreamError>;
}

/// Convenience alias for an `Arc<dyn StreamFn>`.
pub type SharedStreamFn = Arc<dyn StreamFn>;

/// `Arc<T>` is itself a [`StreamFn`], so decorators such as
/// [`crate::retry::RetryStreamFn`] can wrap a [`SharedStreamFn`] the same way
/// they wrap a concrete adapter.
#[async_trait]
impl<T: StreamFn + ?Sized> StreamFn for Arc<T> {
    async fn stream_simple(
        &self,
        model: &Model,
        ctx: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        (**self).stream_simple(model, ctx, options).await
    }
}
