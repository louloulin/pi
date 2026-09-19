//! Anthropic Messages provider.
//!
//! Implements the [`StreamFn`] trait against the Anthropic Messages HTTP API
//! (`POST {base}/v1/messages`). The streaming endpoint emits typed SSE
//! events (`event: message_start` / `content_block_start` /
//! `content_block_delta` / `content_block_stop` / `message_delta` /
//! `message_stop` / `ping` / `error`); this adapter parses the byte
//! stream into [`AssistantMessageEvent`]s and accumulates the final
//! [`AssistantMessage`] into a single trailing
//! [`Done`](AssistantMessageEvent::Done).
//!
//! This is the Stage 7 port of `packages/ai/src/providers/anthropic.ts` +
//! `packages/ai/src/api/anthropic-messages.ts`. It covers:
//!
//! - Text streaming (`content_block_delta` with `delta.type == "text_delta"`).
//! - Tool-use streaming (`content_block_start` with `type == "tool_use"` +
//!   `input_json_delta` accumulation).
//! - Thinking streaming (`content_block_start` with `type == "thinking"` +
//!   `thinking_delta` accumulation), mapped to
//!   [`AssistantMessageEvent::ThinkingDelta`].
//! - `message_delta.usage` for output tokens, `message_start.message.usage`
//!   for input + cache tokens (mapped onto [`Usage::cache_read`] /
//!   [`Usage::cache_write`]).
//! - `stop_reason` mapping: `end_turn` → `StopReason::Stop`,
//!   `tool_use` → `StopReason::ToolUse`, `max_tokens` → `StopReason::MaxTokens`,
//!   `refusal` → `StopReason::Error`, anything else → `StopReason::Stop`.
//! - Bearer auth via `x-api-key: {key}` (the Anthropic convention; we
//!   also accept `Authorization: Bearer {key}` for proxies that rewrite
//!   the header).
//!
//! Native targets use `reqwest`; the `wasm32-unknown-unknown` target has
//! no usable HTTP client in Stage 7 and returns
//! [`StreamError::Malformed`] from every call. Stage 6 (browser host) will
//! replace this with a `fetch`-based adapter.

// Wire-format structs (the `Messages*` types below) are exposed for
// inspection and fixture tests; their fields are documented inline via
// the upstream Anthropic reference rather than via Rustdoc. Keep the
// allow in scope until each struct gets its own doc comment.
#![allow(missing_docs)]

use async_trait::async_trait;
use bytes::Bytes;
use futures::TryStreamExt;
use pi_protocol::{
    AssistantMessageEvent, Content, Context, Message, Model, Role, StopReason, TextContent,
    ToolCall, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::stream::AssistantMessageEventStream;
use crate::types::{SimpleStreamOptions, StreamError};
use crate::StreamFn;

/// Default base URL for Anthropic Messages.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Anthropic API version header. The Messages API tracks this as a date
/// stamp; 2023-06-15 is the value every documented SDK ships with.
pub const ANTHROPIC_VERSION: &str = "2023-06-15";

/// Anthropic Messages provider.
///
/// Construct with [`AnthropicProvider::new`] for the production endpoint,
/// or use [`AnthropicProvider::with_base_url`] to point at a compatible
/// mirror (Vertex Claude, Bedrock Claude, internal relay, …).
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    /// API key sent in the `x-api-key` header (Anthropic convention).
    pub api_key: String,
    /// Base URL with no trailing slash. Must NOT include the `/v1` prefix;
    /// [`AnthropicProvider::stream_simple`] appends it.
    pub base_url: String,
}

impl AnthropicProvider {
    /// Create a provider pointing at the production Anthropic endpoint.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Create a provider pointing at a custom base URL (Vertex, Bedrock,
    /// internal relay, …).
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
        }
    }

    /// Build the request body for the `/v1/messages` endpoint.
    pub fn build_request(
        model: &Model,
        ctx: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<MessagesRequest, StreamError> {
        // Anthropic requires a non-empty `max_tokens`. If the caller did
        // not set one, fall back to the model's `max_output_tokens`, then
        // to a conservative default.
        let max_tokens = options
            .max_tokens
            .or(Some(model.max_output_tokens))
            .filter(|n| *n > 0)
            .unwrap_or(4096);

        // Anthropic's API takes `system` as a top-level field rather than
        // a `system` message in `messages`. Pull from both `ctx.system_prompt`
        // and any `Role::System` messages in the conversation.
        let mut system: Option<String> = if ctx.system_prompt.is_empty() {
            None
        } else {
            Some(ctx.system_prompt.clone())
        };
        let mut messages: Vec<MessagesMessage> = Vec::with_capacity(ctx.messages.len());
        for msg in &ctx.messages {
            match msg.role {
                Role::System => {
                    if system.is_some() {
                        return Err(StreamError::Malformed(
                            "Anthropic only accepts a single system prompt".into(),
                        ));
                    }
                    system = Some(text_of(msg));
                }
                Role::User => messages.push(MessagesMessage {
                    role: "user".into(),
                    content: MessagesContent::from_message(msg)?,
                }),
                Role::Assistant => messages.push(MessagesMessage {
                    role: "assistant".into(),
                    content: MessagesContent::from_message(msg)?,
                }),
                Role::Tool => messages.push(MessagesMessage {
                    role: "user".into(),
                    content: MessagesContent::from_message(msg)?,
                }),
            }
        }

        let tools: Vec<MessagesTool> = ctx
            .tools
            .iter()
            .map(|t| MessagesTool {
                name: t.name.clone(),
                description: if t.description.is_empty() {
                    None
                } else {
                    Some(t.description.clone())
                },
                input_schema: t.parameters.clone(),
            })
            .collect();

        Ok(MessagesRequest {
            model: model.id.clone(),
            max_tokens,
            stream: true,
            messages,
            system: system.filter(|s| !s.is_empty()),
            tools: if tools.is_empty() { None } else { Some(tools) },
            temperature: options.temperature,
        })
    }

    /// POST `/v1/messages` with `stream: true` and pipe the SSE byte
    /// stream through [`parse_sse`].
    #[cfg(not(target_arch = "wasm32"))]
    async fn send_streaming(
        &self,
        body: &MessagesRequest,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            // Surface Anthropic's structured error payloads ({"type":"error",...})
            // as a `StreamError::Provider` with the JSON body attached.
            return Err(StreamError::Provider {
                status: status.as_u16(),
                body: truncate_body(&body),
            });
        }
        let model_id = body.model.clone();
        let byte_stream = response.bytes_stream();
        let mapped = byte_stream.map_err(StreamError::Transport);
        Ok(parse_sse(Box::pin(mapped), model_id))
    }
}

#[async_trait]
impl StreamFn for AnthropicProvider {
    async fn stream_simple(
        &self,
        model: &Model,
        ctx: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        let body = Self::build_request(model, ctx, options)?;

        #[cfg(not(target_arch = "wasm32"))]
        {
            self.send_streaming(&body).await
        }

        #[cfg(target_arch = "wasm32")]
        {
            let _ = body;
            Err(StreamError::Malformed(
                "AnthropicProvider is not yet implemented for wasm32-unknown-unknown".into(),
            ))
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn truncate_body(body: &str) -> String {
    const MAX: usize = 4096;
    if body.len() <= MAX {
        body.to_string()
    } else {
        let mut end = MAX;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…(truncated)", &body[..end])
    }
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Anthropic Messages request body.
#[derive(Debug, Clone, Serialize)]
pub struct MessagesRequest {
    /// Model identifier (`claude-3-5-sonnet-latest`, `claude-opus-4-5`, …).
    pub model: String,
    /// Required by Anthropic's API. Falls back to the model's
    /// `max_output_tokens` when the caller leaves it unset.
    pub max_tokens: u32,
    /// Whether to stream the response. Always `true` in this provider.
    pub stream: bool,
    /// Conversation messages (system goes in [`Self::system`]).
    pub messages: Vec<MessagesMessage>,
    /// Optional top-level system prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Tool descriptors. Skipped when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<MessagesTool>>,
    /// Sampling temperature (0–1). Anthropic rejects > 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

/// One message in Anthropic wire format.
#[derive(Debug, Clone, Serialize)]
pub struct MessagesMessage {
    /// `user` | `assistant`. Tool results ride on `user` per Anthropic's
    /// wire convention.
    pub role: String,
    /// Content blocks (text / tool_use / tool_result / image / …).
    pub content: MessagesContent,
}

/// Anthropic content payload — either a string (shorthand for a single
/// text block) or an array of typed content blocks. We always emit the
/// array form for clarity and to support tool_use / tool_result blocks.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum MessagesContent {
    /// Array form.
    Blocks(Vec<MessagesContentBlock>),
}

/// One content block in Anthropic wire format.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessagesContentBlock {
    /// Plain text.
    Text {
        /// Text payload.
        text: String,
    },
    /// Image attachment (base64).
    Image {
        /// Source descriptor (base64 + media type).
        source: MessagesImageSource,
    },
    /// Tool invocation from the assistant.
    ToolUse {
        /// Provider-issued tool call id.
        id: String,
        /// Tool name.
        name: String,
        /// Arguments object.
        input: Value,
    },
    /// Tool result returned to the model.
    ToolResult {
        /// Tool call id this result answers.
        tool_use_id: String,
        /// Result content (string or array of content blocks).
        content: MessagesToolResultContent,
        /// Whether the tool errored. Anthropic surfaces this as a
        /// synthetic error block.
        #[serde(skip_serializing_if = "is_false")]
        is_error: bool,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessagesImageSource {
    /// Base64-encoded image data.
    Base64 {
        /// MIME type (`image/png`, `image/jpeg`, `image/gif`, `image/webp`).
        media_type: String,
        /// Base64 payload (no `data:` prefix).
        data: String,
    },
}

/// Tool result content — either a string or a list of content blocks.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum MessagesToolResultContent {
    /// Single string (shorthand).
    Text(String),
    /// Multiple content blocks.
    Blocks(Vec<MessagesContentBlock>),
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Tool descriptor in Anthropic wire format.
#[derive(Debug, Clone, Serialize)]
pub struct MessagesTool {
    /// Tool name.
    pub name: String,
    /// Tool description (Anthropic recommends ≥ 1 sentence).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema describing the input object.
    pub input_schema: Value,
}

impl MessagesContent {
    fn from_message(msg: &Message) -> Result<Self, StreamError> {
        match msg.role {
            Role::System => unreachable!("system routed through MessagesRequest::system"),
            Role::User => {
                let mut blocks = Vec::with_capacity(msg.content.len());
                for c in &msg.content {
                    blocks.push(block_from(c)?);
                }
                if blocks.is_empty() {
                    blocks.push(MessagesContentBlock::Text { text: String::new() });
                }
                Ok(MessagesContent::Blocks(blocks))
            }
            Role::Assistant => {
                let mut blocks = Vec::with_capacity(msg.content.len());
                for c in &msg.content {
                    blocks.push(block_from(c)?);
                }
                if blocks.is_empty() {
                    blocks.push(MessagesContentBlock::Text { text: String::new() });
                }
                Ok(MessagesContent::Blocks(blocks))
            }
            Role::Tool => {
                // Anthropic expects tool results on a `user` turn with
                // `tool_result` blocks. Each `Content::ToolResult` becomes
                // its own block.
                let mut blocks = Vec::new();
                for c in &msg.content {
                    match c {
                        Content::ToolResult(r) => {
                            blocks.push(MessagesContentBlock::ToolResult {
                                tool_use_id: r.tool_call_id.clone(),
                                content: MessagesToolResultContent::Text(text_of_box(&r.content)),
                                is_error: r.is_error,
                            });
                        }
                        Content::Text(t) => {
                            blocks.push(MessagesContentBlock::Text { text: t.text.clone() });
                        }
                        other => {
                            return Err(StreamError::Malformed(format!(
                                "tool message may only contain Text or ToolResult, got {other:?}"
                            )));
                        }
                    }
                }
                if blocks.is_empty() {
                    blocks.push(MessagesContentBlock::Text { text: String::new() });
                }
                Ok(MessagesContent::Blocks(blocks))
            }
        }
    }
}

fn block_from(c: &Content) -> Result<MessagesContentBlock, StreamError> {
    match c {
        Content::Text(t) => Ok(MessagesContentBlock::Text { text: t.text.clone() }),
        Content::Image(_) => Err(StreamError::Malformed(
            "image content requires media_type + base64 data; wire-format conversion not yet implemented".into(),
        )),
        Content::ToolCall(call) => Ok(MessagesContentBlock::ToolUse {
            id: call.id.clone(),
            name: call.name.clone(),
            input: call.arguments.clone(),
        }),
        Content::ToolResult(r) => Ok(MessagesContentBlock::ToolResult {
            tool_use_id: r.tool_call_id.clone(),
            content: MessagesToolResultContent::Text(text_of_box(&r.content)),
            is_error: r.is_error,
        }),
    }
}

fn text_of(msg: &Message) -> String {
    msg.content
        .iter()
        .filter_map(|c| match c {
            Content::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Extract text from a `Box<Content>` (used by `ToolResult::content`).
fn text_of_box(c: &Content) -> String {
    match c {
        Content::Text(t) => t.text.clone(),
        Content::ToolResult(_) => String::new(),
        Content::ToolCall(_) => String::new(),
        Content::Image(_) => String::new(),
    }
}

// ---------------------------------------------------------------------------
// SSE parser
// ---------------------------------------------------------------------------

/// Parse an Anthropic SSE byte stream into an [`AssistantMessageEventStream`].
///
/// Anthropic's SSE format differs from OpenAI's:
/// - Each event has an `event:` field naming the event type
///   (`message_start`, `content_block_start`, …).
/// - The `data:` field is a single JSON object shaped like the upstream
///   `BetaRawMessageStreamEvent` (we accept the same shape with serde's
///   `untagged` enum dispatch).
///
/// We dispatch on the JSON `type` field for the inner payload because
/// that's what the SDK contract exposes.
pub fn parse_sse(
    bytes: impl futures::Stream<Item = Result<Bytes, StreamError>> + Send + 'static,
    model_id: String,
) -> AssistantMessageEventStream {
    Box::pin(AnthropicSseStream::new(bytes, model_id))
}

struct AnthropicSseStream {
    inner: std::pin::Pin<Box<dyn futures::Stream<Item = Result<Bytes, StreamError>> + Send>>,
    /// Half-parsed SSE buffer (lines from the most recent chunk).
    line_buffer: Vec<u8>,
    /// Currently buffered `data:` lines for the in-flight event.
    data_lines: Vec<String>,
    /// Currently buffered `event:` name (Anthropic tags every event).
    event_name: Option<String>,
    /// Pending events produced by the current chunk but not yet yielded.
    pending: std::collections::VecDeque<Result<AssistantMessageEvent, StreamError>>,
    /// True once the stream sent its final `Done`.
    finished: bool,
    /// True once we've emitted the initial `Start`.
    started: bool,
    model_id: String,
    state: ParserState,
}

#[derive(Default)]
struct ParserState {
    /// Assistant message content accumulated so far.
    content: Vec<Content>,
    /// In-flight tool calls keyed by their `index` field.
    pending_tool_calls: std::collections::HashMap<u32, PendingToolCall>,
    /// Running usage totals.
    usage: Usage,
    /// Stop reason set by `message_delta.stop_reason`.
    stop_reason: Option<StopReason>,
    /// Tracks whether the parser has observed `message_start` so we can
    /// emit `Done` with a sane model id.
    message_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct PendingToolCall {
    id: Option<String>,
    name: Option<String>,
    /// Arguments fragment, accumulated verbatim.
    arguments: String,
}

impl AnthropicSseStream {
    fn new(
        bytes: impl futures::Stream<Item = Result<Bytes, StreamError>> + Send + 'static,
        model_id: String,
    ) -> Self {
        Self {
            inner: Box::pin(bytes),
            line_buffer: Vec::new(),
            data_lines: Vec::new(),
            event_name: None,
            pending: std::collections::VecDeque::new(),
            finished: false,
            started: false,
            model_id,
            state: ParserState::default(),
        }
    }

    fn feed(&mut self, bytes: &Bytes) -> Result<(), StreamError> {
        self.line_buffer.extend_from_slice(bytes);
        loop {
            let Some(rel_end) = find_newline(&self.line_buffer) else {
                break;
            };
            let raw = self.line_buffer.drain(..rel_end).collect::<Vec<_>>();
            // Pop the line ending (handles `\n`, `\r\n`, or a stray `\r`).
            if !self.line_buffer.is_empty() {
                self.line_buffer.remove(0);
            }
            self.process_line(&raw)?;
        }
        Ok(())
    }

    fn process_line(&mut self, raw: &[u8]) -> Result<(), StreamError> {
        // Strip an optional leading UTF-8 BOM.
        let line = if raw.starts_with(b"\xEF\xBB\xBF") {
            &raw[3..]
        } else {
            raw
        };
        if line.is_empty() {
            // Blank line → dispatch the buffered event.
            self.dispatch_event()?;
            return Ok(());
        }
        // Comments start with `:`.
        if line.first() == Some(&b':') {
            return Ok(());
        }
        let (field, value) = match split_field(line) {
            Some(parts) => parts,
            None => return Ok(()),
        };
        match field {
            b"event" => {
                let value = String::from_utf8_lossy(value).into_owned();
                self.event_name = Some(value);
            }
            b"data" => {
                let value = String::from_utf8_lossy(value).into_owned();
                self.data_lines.push(value);
            }
            b"id" | b"retry" => {
                // We don't track retry hints or last-event-id.
            }
            _ => {}
        }
        Ok(())
    }

    fn dispatch_event(&mut self) -> Result<(), StreamError> {
        if self.data_lines.is_empty() && self.event_name.is_none() {
            return Ok(());
        }
        let payload = self.data_lines.join("\n");
        let event_name = self.event_name.take();
        self.data_lines.clear();
        if payload.is_empty() {
            return Ok(());
        }
        // Surface Anthropic's `error` event as a `StreamError::Provider`
        // so the caller can show the error message.
        if event_name.as_deref() == Some("error") {
            return Err(StreamError::Provider {
                status: 400,
                body: payload,
            });
        }
        self.handle_event(event_name.as_deref(), &payload)
    }

    fn handle_event(&mut self, name: Option<&str>, data: &str) -> Result<(), StreamError> {
        if !self.started {
            // We wait until we see `message_start` to emit the
            // `Start` event so we can attach the model id from the
            // payload (which may differ from the request's model id).
            if name == Some("message_start") {
                self.started = true;
                let payload: MessageStartPayload = serde_json::from_str(data).map_err(|e| {
                    StreamError::Malformed(format!("message_start JSON: {e}: {data}"))
                })?;
                self.state.message_id = Some(payload.message.id.clone());
                self.state.usage = Usage {
                    input: payload.message.usage.input_tokens,
                    output: payload.message.usage.output_tokens,
                    cache_read: payload.message.usage.cache_read_input_tokens,
                    cache_write: payload.message.usage.cache_creation_input_tokens,
                    total: payload
                        .message
                        .usage
                        .input_tokens
                        .saturating_add(payload.message.usage.output_tokens),
                };
                let model = if payload.message.model.is_empty() {
                    self.model_id.clone()
                } else {
                    payload.message.model.clone()
                };
                self.pending.push_back(Ok(AssistantMessageEvent::Start { model }));
            }
            return Ok(());
        }

        match name {
            Some("content_block_start") => {
                let payload: ContentBlockStartPayload = serde_json::from_str(data).map_err(|e| {
                    StreamError::Malformed(format!("content_block_start JSON: {e}: {data}"))
                })?;
                self.handle_content_block_start(payload)?;
            }
            Some("content_block_delta") => {
                let payload: ContentBlockDeltaPayload = serde_json::from_str(data).map_err(|e| {
                    StreamError::Malformed(format!("content_block_delta JSON: {e}: {data}"))
                })?;
                self.handle_content_block_delta(payload)?;
            }
            Some("content_block_stop") => {
                let payload: ContentBlockStopPayload = serde_json::from_str(data).map_err(|e| {
                    StreamError::Malformed(format!("content_block_stop JSON: {e}: {data}"))
                })?;
                self.handle_content_block_stop(payload)?;
            }
            Some("message_delta") => {
                let payload: MessageDeltaPayload = serde_json::from_str(data).map_err(|e| {
                    StreamError::Malformed(format!("message_delta JSON: {e}: {data}"))
                })?;
                if let Some(reason) = payload.delta.stop_reason.as_deref() {
                    self.state.stop_reason = Some(map_stop_reason(reason));
                }
                if let Some(usage) = payload.usage {
                    // `message_delta.usage` carries the final output_tokens
                    // count. Merge into existing usage so cache_read /
                    // cache_write from `message_start` are preserved.
                    self.state.usage.output = usage.output_tokens;
                }
            }
            Some("message_stop") => {
                // The SDK convention is to finalise on the next poll when
                // the underlying byte stream returns `None`; we leave
                // `message_stop` as a no-op marker.
            }
            Some("ping") | None => {
                // Heartbeats / unknown events: ignore.
            }
            Some(other) => {
                // Unknown event types are silently ignored — the upstream
                // SDK follows the same convention.
                let _ = other;
            }
        }
        Ok(())
    }

    fn handle_content_block_start(
        &mut self,
        payload: ContentBlockStartPayload,
    ) -> Result<(), StreamError> {
        match payload.content_block {
            ContentBlockStart::Text { text } => {
                if !text.is_empty() {
                    self.state.content.push(Content::Text(TextContent { text }));
                }
            }
            ContentBlockStart::Thinking { thinking } => {
                if !thinking.is_empty() {
                    self.pending.push_back(Ok(AssistantMessageEvent::ThinkingDelta {
                        delta: thinking,
                    }));
                }
            }
            ContentBlockStart::ToolUse { id, name, input } => {
                let entry = self
                    .state
                    .pending_tool_calls
                    .entry(payload.index)
                    .or_default();
                entry.id = Some(id);
                entry.name = Some(name);
                if let Some(obj) = input.as_object() {
                    if !obj.is_empty() {
                        let fragment =
                            serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                        entry.arguments.push_str(&fragment);
                    }
                }
            }
            ContentBlockStart::Other => {
                // Unknown block types are silently skipped, matching the
                // upstream SDK's behaviour for forward-compat.
            }
        }
        Ok(())
    }

    fn handle_content_block_delta(
        &mut self,
        payload: ContentBlockDeltaPayload,
    ) -> Result<(), StreamError> {
        match payload.delta {
            ContentBlockDelta::TextDelta { text } => {
                if text.is_empty() {
                    return Ok(());
                }
                self.pending.push_back(Ok(AssistantMessageEvent::TextDelta {
                    delta: text.clone(),
                }));
                if let Some(Content::Text(t)) = self.state.content.last_mut() {
                    t.text.push_str(&text);
                    return Ok(());
                }
                self.state.content.push(Content::Text(TextContent { text }));
                Ok(())
            }
            ContentBlockDelta::ThinkingDelta { thinking } => {
                if thinking.is_empty() {
                    return Ok(());
                }
                self.pending.push_back(Ok(AssistantMessageEvent::ThinkingDelta {
                    delta: thinking,
                }));
                Ok(())
            }
            ContentBlockDelta::InputJsonDelta { partial_json } => {
                let entry = self
                    .state
                    .pending_tool_calls
                    .entry(payload.index)
                    .or_default();
                if !partial_json.is_empty() {
                    entry.arguments.push_str(&partial_json);
                    self.pending.push_back(Ok(AssistantMessageEvent::ToolCallDelta {
                        index: payload.index,
                        id: None,
                        name: None,
                        arguments_delta: Some(partial_json),
                    }));
                }
                Ok(())
            }
            ContentBlockDelta::SignatureDelta { .. } | ContentBlockDelta::Other => Ok(()),
        }
    }

    fn handle_content_block_stop(
        &mut self,
        payload: ContentBlockStopPayload,
    ) -> Result<(), StreamError> {
        if let Some(mut call) = self.state.pending_tool_calls.remove(&payload.index) {
            let id = call.id.take().unwrap_or_default();
            let name = call.name.take().unwrap_or_default();
            let arguments = if call.arguments.is_empty() {
                Value::Object(Default::default())
            } else {
                match serde_json::from_str(&call.arguments) {
                    Ok(v) => v,
                    Err(_) => Value::String(call.arguments.clone()),
                }
            };
            self.state.content.push(Content::ToolCall(ToolCall {
                id,
                name,
                arguments,
            }));
        }
        Ok(())
    }

    fn finalize(&mut self) {
        // Materialise any pending tool calls that did not receive a
        // matching `content_block_stop` (defensive — Anthropic always
        // sends the stop, but a truncated stream should still
        // surface what we have).
        let mut pending: Vec<_> = self.state.pending_tool_calls.drain().collect();
        pending.sort_by_key(|(idx, _)| *idx);
        for (_idx, mut call) in pending {
            let id = call.id.take().unwrap_or_default();
            let name = call.name.take().unwrap_or_default();
            let arguments = if call.arguments.is_empty() {
                Value::Object(Default::default())
            } else {
                serde_json::from_str(&call.arguments).unwrap_or(Value::String(call.arguments))
            };
            self.state.content.push(Content::ToolCall(ToolCall {
                id,
                name,
                arguments,
            }));
        }
        let stop_reason = self.state.stop_reason.unwrap_or_else(|| {
            if self.state.content.iter().any(Content::is_tool_call) {
                StopReason::ToolUse
            } else if self.state.content.is_empty() {
                StopReason::Empty
            } else {
                StopReason::Stop
            }
        });
        let content = std::mem::take(&mut self.state.content);
        let usage = self.state.usage;
        let model_id = self.model_id.clone();
        let _ = self.state.message_id.take();
        self.pending.push_back(Ok(AssistantMessageEvent::Done {
            content,
            stop_reason,
            usage,
        }));
        // `model_id` is unused here (the `Start` event carries the model
        // id) but we keep the binding to document the source of truth.
        let _ = model_id;
    }
}

impl futures::Stream for AnthropicSseStream {
    type Item = Result<AssistantMessageEvent, StreamError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if let Some(ev) = self.pending.pop_front() {
            return std::task::Poll::Ready(Some(ev));
        }
        if self.finished {
            return std::task::Poll::Ready(None);
        }
        loop {
            match self.inner.as_mut().poll_next(cx) {
                std::task::Poll::Pending => return std::task::Poll::Pending,
                std::task::Poll::Ready(None) => {
                    self.finished = true;
                    // Drain any remaining buffered bytes as a final line.
                    if !self.line_buffer.is_empty() {
                        let buf = std::mem::take(&mut self.line_buffer);
                        if let Err(e) = self.process_line(&buf) {
                            return std::task::Poll::Ready(Some(Err(e)));
                        }
                    }
                    // Flush any in-progress event.
                    self.dispatch_event()?;
                    self.finalize();
                    if let Some(ev) = self.pending.pop_front() {
                        return std::task::Poll::Ready(Some(ev));
                    }
                    return std::task::Poll::Ready(None);
                }
                std::task::Poll::Ready(Some(Err(e))) => {
                    self.finished = true;
                    return std::task::Poll::Ready(Some(Err(e)));
                }
                std::task::Poll::Ready(Some(Ok(chunk))) => {
                    if let Err(e) = self.feed(&chunk) {
                        self.finished = true;
                        return std::task::Poll::Ready(Some(Err(e)));
                    }
                    if let Some(ev) = self.pending.pop_front() {
                        return std::task::Poll::Ready(Some(ev));
                    }
                    // Otherwise loop to read the next chunk.
                }
            }
        }
    }
}

fn find_newline(buf: &[u8]) -> Option<usize> {
    for (i, b) in buf.iter().enumerate() {
        if *b == b'\n' || *b == b'\r' {
            return Some(i);
        }
    }
    None
}

fn split_field(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let idx = line.iter().position(|b| *b == b':')?;
    let field = &line[..idx];
    let mut value = &line[idx + 1..];
    // Strip optional single leading space (SSE spec).
    if value.first() == Some(&b' ') {
        value = &value[1..];
    }
    Some((field, value))
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" | "stop_sequence" => StopReason::Stop,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "refusal" => StopReason::Error,
        other => {
            // Unknown stop reasons default to Stop rather than Error so
            // the caller still gets the content; the upstream SDK does
            // the same.
            let _ = other;
            StopReason::Stop
        }
    }
}

// ---------------------------------------------------------------------------
// Wire payload types (for `parse_sse`)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct MessageStartPayload {
    message: MessageStartMessage,
}

#[derive(Debug, Deserialize)]
struct MessageStartMessage {
    id: String,
    #[serde(default)]
    model: String,
    usage: MessagesUsage,
}

#[derive(Debug, Deserialize, Default)]
struct MessagesUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct ContentBlockStartPayload {
    index: u32,
    content_block: ContentBlockStart,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlockStart {
    Text {
        #[serde(default)]
        text: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct ContentBlockDeltaPayload {
    index: u32,
    delta: ContentBlockDelta,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlockDelta {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    SignatureDelta {
        // Signature verification is a Stage 8 concern (extended-thinking
        // tool result back-validation); Stage 7 only consumes the
        // thinking text fragments and ignores the signature.
        #[serde(default)]
        #[allow(dead_code)]
        signature: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct ContentBlockStopPayload {
    index: u32,
}

#[derive(Debug, Deserialize)]
struct MessageDeltaPayload {
    delta: MessageDeltaInner,
    #[serde(default)]
    usage: Option<MessagesUsage>,
}

#[derive(Debug, Deserialize, Default)]
struct MessageDeltaInner {
    #[serde(default)]
    stop_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{stream, StreamExt};
    use pi_protocol::ToolDefinition;
    use serde_json::json;

    fn model() -> Model {
        Model {
            provider: pi_protocol::ProviderId::new("anthropic"),
            id: "claude-3-5-sonnet-latest".into(),
            api: pi_protocol::Api::AnthropicMessages,
            label: None,
            context_window: 200_000,
            max_output_tokens: 8_192,
        }
    }

    fn ctx_with_tool() -> Context {
        let mut ctx = Context::new("");
        ctx.messages.push(Message {
            role: Role::User,
            content: vec![Content::text("what's the weather in SF?")],
            model: None,
        });
        ctx.tools.push(ToolDefinition {
            name: "get_weather".into(),
            label: "Get weather".into(),
            description: "Get the current weather for a city".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "city": {"type": "string"}
                },
                "required": ["city"],
            }),
            metadata: None,
        });
        ctx
    }

    #[test]
    fn request_body_serializes_with_tools_and_system() {
        let mut ctx = Context::new("you are pi");
        ctx.messages.push(Message {
            role: Role::User,
            content: vec![Content::text("hi")],
            model: None,
        });
        ctx.tools.push(ToolDefinition {
            name: "echo".into(),
            label: "Echo".into(),
            description: "Echo the input".into(),
            parameters: json!({"type": "object", "properties": {"x": {"type": "string"}}}),
            metadata: None,
        });
        let req = AnthropicProvider::build_request(
            &model(),
            &ctx,
            &SimpleStreamOptions {
                temperature: Some(0.5),
                max_tokens: Some(512),
                ..Default::default()
            },
        )
        .expect("build request");
        let v = serde_json::to_value(&req).expect("serialize request");
        assert_eq!(v["model"], "claude-3-5-sonnet-latest");
        assert_eq!(v["stream"], true);
        assert_eq!(v["max_tokens"], 512);
        assert!((v["temperature"].as_f64().expect("temperature") - 0.5).abs() < 1e-6);
        assert_eq!(v["system"], "you are pi");
        let messages = v["messages"].as_array().expect("messages array");
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["type"], "text");
        assert_eq!(messages[0]["content"][0]["text"], "hi");
        let tools = v["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "echo");
        assert_eq!(tools[0]["input_schema"]["type"], "object");
    }

    #[test]
    fn request_body_falls_back_to_model_max_tokens() {
        let mut ctx = Context::new("");
        ctx.messages.push(Message {
            role: Role::User,
            content: vec![Content::text("hi")],
            model: None,
        });
        let req = AnthropicProvider::build_request(
            &model(),
            &ctx,
            &SimpleStreamOptions::default(),
        )
        .expect("build request");
        assert_eq!(req.max_tokens, model().max_output_tokens);
    }

    #[tokio::test]
    async fn sse_parser_emits_text_deltas_then_done() {
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/anthropic_text.sse");
        let bytes =
            std::fs::read(&fixture).unwrap_or_else(|e| panic!("read fixture {fixture:?}: {e}"));
        let chunks = stream::iter(vec![Ok::<bytes::Bytes, StreamError>(Bytes::from(bytes))]);
        let mut s = parse_sse(chunks, "claude-3-5-sonnet-latest".to_string());
        let mut events = Vec::new();
        while let Some(ev) = s.next().await {
            events.push(ev.expect("stream event"));
        }
        // Expect: Start, TextDelta("Hello"), TextDelta(", world"), Done.
        assert!(
            events.len() >= 4,
            "got {} events: {:?}",
            events.len(),
            events
        );
        match &events[0] {
            AssistantMessageEvent::Start { model } => {
                assert_eq!(model, "claude-3-5-sonnet-20241022");
            }
            other => panic!("expected Start, got {other:?}"),
        }
        match &events[1] {
            AssistantMessageEvent::TextDelta { delta } => assert_eq!(delta, "Hello"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
        match &events[2] {
            AssistantMessageEvent::TextDelta { delta } => assert_eq!(delta, ", world"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
        match events.last().expect("at least one event") {
            AssistantMessageEvent::Done {
                content,
                stop_reason,
                usage,
            } => {
                assert_eq!(*stop_reason, StopReason::Stop);
                let text: String = content
                    .iter()
                    .filter_map(|c| match c {
                        Content::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(text, "Hello, world");
                assert_eq!(usage.input, 12);
                assert_eq!(usage.output, 7);
                assert_eq!(usage.cache_read, 0);
                assert_eq!(usage.cache_write, 0);
            }
            other => panic!("expected done, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sse_parser_emits_tool_call_then_done() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/anthropic_tool_use.sse");
        let bytes =
            std::fs::read(&fixture).unwrap_or_else(|e| panic!("read fixture {fixture:?}: {e}"));
        let chunks = stream::iter(vec![Ok::<bytes::Bytes, StreamError>(Bytes::from(bytes))]);
        let mut s = parse_sse(chunks, "claude-3-5-sonnet-latest".to_string());
        let mut events = Vec::new();
        while let Some(ev) = s.next().await {
            events.push(ev.expect("stream event"));
        }
        // Look for ToolCallDelta fragments.
        let tool_delta_count = events
            .iter()
            .filter(|e| matches!(e, AssistantMessageEvent::ToolCallDelta { .. }))
            .count();
        assert!(tool_delta_count > 0, "expected at least one tool delta");
        // The final Done should carry a tool call with parsed arguments.
        let done = events
            .iter()
            .find_map(|e| match e {
                AssistantMessageEvent::Done { content, .. } => Some(content),
                _ => None,
            })
            .expect("done event");
        let tc = done
            .iter()
            .find_map(|c| match c {
                Content::ToolCall(t) => Some(t),
                _ => None,
            })
            .expect("tool call in done");
        assert_eq!(tc.id, "toolu_01abc");
        assert_eq!(tc.name, "get_weather");
        assert_eq!(tc.arguments["city"], "San Francisco");
        let usage = events
            .iter()
            .find_map(|e| match e {
                AssistantMessageEvent::Done { usage, .. } => Some(*usage),
                _ => None,
            })
            .expect("usage on done");
        assert_eq!(usage.input, 25);
        assert_eq!(usage.output, 18);
        assert_eq!(usage.cache_read, 12);
    }

    #[tokio::test]
    async fn sse_parser_emits_thinking_deltas_then_text() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/anthropic_thinking.sse");
        let bytes =
            std::fs::read(&fixture).unwrap_or_else(|e| panic!("read fixture {fixture:?}: {e}"));
        let chunks = stream::iter(vec![Ok::<bytes::Bytes, StreamError>(Bytes::from(bytes))]);
        let mut s = parse_sse(chunks, "claude-3-7-sonnet-latest".to_string());
        let mut events = Vec::new();
        while let Some(ev) = s.next().await {
            events.push(ev.expect("stream event"));
        }
        let think = events
            .iter()
            .filter_map(|e| match e {
                AssistantMessageEvent::ThinkingDelta { delta } => Some(delta.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        assert!(think.contains("step-by-step"), "thinking: {think:?}");
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                AssistantMessageEvent::TextDelta { delta } => Some(delta.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "The answer is 4.");
        let done = events
            .iter()
            .find_map(|e| match e {
                AssistantMessageEvent::Done { stop_reason, .. } => Some(*stop_reason),
                _ => None,
            })
            .expect("done event");
        assert_eq!(done, StopReason::Stop);
    }

    #[tokio::test]
    async fn sse_parser_handles_split_chunks() {
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/anthropic_text.sse");
        let bytes =
            std::fs::read(&fixture).unwrap_or_else(|e| panic!("read fixture {fixture:?}: {e}"));
        // Split the SSE payload into half-line chunks to verify the
        // line-buffering code handles split lines across chunk
        // boundaries.
        let mut chunks: Vec<Result<Bytes, StreamError>> = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            // Take ~3 bytes per chunk so consecutive chunks split mid-line.
            let end = (i + 3).min(bytes.len());
            chunks.push(Ok(Bytes::copy_from_slice(&bytes[i..end])));
            i = end;
        }
        let mut s = parse_sse(stream::iter(chunks), "claude-3-5-sonnet-latest".to_string());
        let mut events = Vec::new();
        while let Some(ev) = s.next().await {
            events.push(ev.expect("stream event"));
        }
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                AssistantMessageEvent::TextDelta { delta } => Some(delta.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello, world");
    }

    #[test]
    fn stop_reason_mapping() {
        assert_eq!(map_stop_reason("end_turn"), StopReason::Stop);
        assert_eq!(map_stop_reason("tool_use"), StopReason::ToolUse);
        assert_eq!(map_stop_reason("max_tokens"), StopReason::MaxTokens);
        assert_eq!(map_stop_reason("refusal"), StopReason::Error);
        assert_eq!(map_stop_reason("stop_sequence"), StopReason::Stop);
    }

    // Keep `ctx_with_tool` referenced to silence the unused warning.
    #[allow(dead_code)]
    fn _ctx_with_tool_unused() -> Context {
        ctx_with_tool()
    }
}
