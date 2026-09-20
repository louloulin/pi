//! Telemetry instrumentation for the agent loop.
//!
//! The contract itself lives in [`pi_telemetry`] (the Rust port of
//! `packages/telemetry`); this module owns the *agent-side vocabulary* the
//! loop emits and the glue that turns domain errors into span statuses. It is
//! the Rust counterpart of `packages/agent/src/harness/telemetry.ts`.
//!
//! Instrumentation is strictly opt-in: [`AgentConfig::telemetry`] defaults to
//! `None`, in which case the loop records nothing and takes no clones on the
//! hot path. When a [`TelemetryContext`] is installed the loop emits:
//!
//! | Span | Covers | Parent |
//! |------|--------|--------|
//! | `pi.harness.run` | one [`AgentLoop::run`](crate::AgentLoop::run) invocation | root or external |
//! | `pi.harness.turn` | one assistant response plus its tool batch | `pi.harness.run` |
//! | `pi.ai.request` | one logical provider request | `pi.harness.turn` |
//! | `pi.harness.tool` | one tool execution | `pi.harness.turn` |
//!
//! Span and attribute names mirror the upstream schema so an exporter
//! configured for the TypeScript harness keeps working with the Rust port.
//! Attributes never carry prompts, completions, tool arguments or tool
//! output — only identifiers, provider metadata, usage counters and error
//! classes, matching the data policy documented on
//! [`AttributeValue`](pi_telemetry::AttributeValue).

use pi_protocol::{Api, AssistantMessage, StopReason, ToolResult};
use pi_telemetry::{IntoTelemetryError, SpanAttributes, SpanError};

use crate::agent_loop::AgentError;

/// Dotted span names emitted by the agent loop.
pub mod span_name {
    /// One `AgentLoop::run` invocation.
    pub const HARNESS_RUN: &str = "pi.harness.run";
    /// One assistant response and its tool batch.
    pub const HARNESS_TURN: &str = "pi.harness.turn";
    /// One tool execution.
    pub const HARNESS_TOOL: &str = "pi.harness.tool";
    /// One logical request to an AI provider.
    pub const AI_REQUEST: &str = "pi.ai.request";
}

/// Attribute keys recorded on the spans above.
pub mod attribute_name {
    /// Logical provider operation (`stream`).
    pub const AI_OPERATION: &str = "pi.ai.operation";
    /// Selected provider id.
    pub const AI_PROVIDER: &str = "pi.ai.provider";
    /// Requested model id.
    pub const AI_MODEL: &str = "pi.ai.model";
    /// Provider API family.
    pub const AI_API: &str = "pi.ai.api";
    /// Whether the operation returns a stream.
    pub const AI_STREAMING: &str = "pi.ai.streaming";
    /// Concrete response model reported by the provider.
    pub const AI_RESPONSE_MODEL: &str = "pi.ai.response.model";
    /// Normalized terminal stop reason.
    pub const AI_RESPONSE_STOP_REASON: &str = "pi.ai.response.stop_reason";
    /// Provider / transport error class.
    pub const AI_ERROR_TYPE: &str = "pi.ai.error.type";
    /// Reported input tokens.
    pub const AI_USAGE_INPUT_TOKENS: &str = "pi.ai.usage.input_tokens";
    /// Reported output tokens.
    pub const AI_USAGE_OUTPUT_TOKENS: &str = "pi.ai.usage.output_tokens";
    /// Reported cache-read tokens.
    pub const AI_USAGE_CACHE_READ_TOKENS: &str = "pi.ai.usage.cache_read_tokens";
    /// Reported cache-write tokens.
    pub const AI_USAGE_CACHE_WRITE_TOKENS: &str = "pi.ai.usage.cache_write_tokens";
    /// Reported total tokens.
    pub const AI_USAGE_TOTAL_TOKENS: &str = "pi.ai.usage.total_tokens";
    /// Run operation kind (`run`).
    pub const OPERATION_KIND: &str = "pi.operation.kind";
    /// Run invocation outcome (`completed` / `failed`).
    pub const OPERATION_OUTCOME: &str = "pi.operation.outcome";
    /// Invocation-local turn id (one-based, as a string).
    pub const TURN_ID: &str = "pi.turn.id";
    /// Tool name.
    pub const TOOL_NAME: &str = "pi.tool.name";
    /// Tool call id.
    pub const TOOL_CALL_ID: &str = "pi.tool.call_id";
    /// Whether the tool execution produced an error result.
    pub const TOOL_IS_ERROR: &str = "pi.tool.is_error";
}

/// Stable string form of [`Api`], used for the `pi.ai.api` attribute.
pub fn api_name(api: Api) -> &'static str {
    match api {
        Api::OpenAiChatCompletions => "openai_chat_completions",
        Api::OpenAiResponses => "openai_responses",
        Api::AnthropicMessages => "anthropic_messages",
        Api::GoogleGenerativeAi => "google_generative_ai",
        Api::BedrockConverse => "bedrock_converse",
        Api::CohereV2 => "cohere_v2",
        Api::MistralConversations => "mistral_conversations",
        Api::Faux => "faux",
    }
}

/// Stable string form of [`StopReason`], used for `pi.ai.response.stop_reason`.
///
/// The vocabulary matches the upstream `pi.ai.response.stop_reason` values
/// where they overlap (`stop`, `length`, `tool_use`, `error`, `aborted`).
pub fn stop_reason_name(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Stop => "stop",
        StopReason::ToolUse => "tool_use",
        StopReason::MaxTokens => "length",
        StopReason::Aborted => "aborted",
        StopReason::Error => "error",
        StopReason::Empty => "empty",
    }
}

/// Error class recorded on `pi.ai.error.type` and in the span status.
pub fn agent_error_type(error: &AgentError) -> &'static str {
    match error {
        AgentError::Stream(_) => "stream",
        AgentError::Tool { .. } => "tool",
        AgentError::Provider(_) => "provider",
    }
}

/// End attributes for a completed `pi.ai.request` span.
pub fn response_attributes(message: &AssistantMessage) -> SpanAttributes {
    let mut attributes = SpanAttributes::new();
    attributes.insert(
        attribute_name::AI_RESPONSE_MODEL.to_owned(),
        message.model.clone().into(),
    );
    attributes.insert(
        attribute_name::AI_RESPONSE_STOP_REASON.to_owned(),
        stop_reason_name(message.stop_reason).into(),
    );
    attributes.insert(
        attribute_name::AI_USAGE_INPUT_TOKENS.to_owned(),
        message.usage.input.into(),
    );
    attributes.insert(
        attribute_name::AI_USAGE_OUTPUT_TOKENS.to_owned(),
        message.usage.output.into(),
    );
    attributes.insert(
        attribute_name::AI_USAGE_CACHE_READ_TOKENS.to_owned(),
        message.usage.cache_read.into(),
    );
    attributes.insert(
        attribute_name::AI_USAGE_CACHE_WRITE_TOKENS.to_owned(),
        message.usage.cache_write.into(),
    );
    attributes.insert(
        attribute_name::AI_USAGE_TOTAL_TOKENS.to_owned(),
        message.usage.total.into(),
    );
    attributes
}

/// End attributes for a failed provider request. The span status itself is
/// set by the automatic `Err` handling in `start_span_with`.
pub fn request_error_attributes(error: &AgentError) -> SpanAttributes {
    let mut attributes = SpanAttributes::new();
    attributes.insert(
        attribute_name::AI_ERROR_TYPE.to_owned(),
        agent_error_type(error).into(),
    );
    attributes
}

/// End attributes for a tool execution.
pub fn tool_attributes(result: &ToolResult) -> SpanAttributes {
    let mut attributes = SpanAttributes::new();
    attributes.insert(
        attribute_name::TOOL_IS_ERROR.to_owned(),
        result.is_error.into(),
    );
    attributes
}

impl IntoTelemetryError for AgentError {
    fn telemetry_error(&self) -> SpanError {
        SpanError::new(agent_error_type(self), self.to_string())
    }
}
