//! Faux provider — emits scripted events for tests.
//!
//! Stage 1 wires the script loader. The Stage 0 stub returns a single
//! `Done` event so the event loop can be exercised end-to-end.

use async_trait::async_trait;
use futures::stream;
use pi_protocol::{AssistantMessageEvent, Context, Model, StopReason, Usage};

use crate::stream::AssistantMessageEventStream;
use crate::types::{SimpleStreamOptions, StreamError};
use crate::StreamFn;

/// Faux provider — always returns a one-shot "ok" reply.
#[derive(Debug, Default, Clone)]
pub struct FauxProvider;

#[async_trait]
impl StreamFn for FauxProvider {
    async fn stream_simple(
        &self,
        model: &Model,
        _ctx: &Context,
        _options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        let model_id = model.id.clone();
        let event = AssistantMessageEvent::Start { model: model_id };
        let done = AssistantMessageEvent::Done {
            content: vec![pi_protocol::Content::text("(faux) hello")],
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
        };
        Ok(Box::pin(stream::iter(vec![Ok(event), Ok(done)])))
    }
}
