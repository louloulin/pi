//! OpenAI provider — Stage 0 placeholder, real adapter lands in Stage 1.

use async_trait::async_trait;
use pi_protocol::{Context, Model};

use crate::stream::AssistantMessageEventStream;
use crate::types::{SimpleStreamOptions, StreamError};
use crate::StreamFn;

/// OpenAI provider — Stage 0 placeholder.
#[derive(Debug, Default, Clone)]
pub struct OpenAiProvider;

#[async_trait]
impl StreamFn for OpenAiProvider {
    async fn stream_simple(
        &self,
        _model: &Model,
        _ctx: &Context,
        _options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        Err(StreamError::Malformed(
            "OpenAiProvider is a Stage 0 stub; Stage 1 wires the real adapter".into(),
        ))
    }
}
