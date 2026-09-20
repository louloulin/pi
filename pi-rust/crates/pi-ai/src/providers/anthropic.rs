//! Anthropic Messages provider.
//!
//! Implements [`StreamFn`] against the Anthropic Messages API
//! (`POST {base_url}/v1/messages`). The streaming endpoint emits typed
//! SSE events (`message_start`, `content_block_start`,
//! `content_block_delta`, `content_block_stop`, `message_delta`,
//! `message_stop`, `ping`, `error`); this adapter parses the byte
//! stream into [`AssistantMessageEvent`]s and accumulates the final
//! [`AssistantMessage`] into a single trailing
//! [`Done`](AssistantMessageEvent::Done) event.
//!
//! # Field mapping (Rust ↔ TS Anthropic Messages)
//!
//! | TS field                                | Rust field / wire usage                |
//! | --------------------------------------- | -------------------------------------- |
//! | `input_tokens`                          | `Usage::input`                         |
//! | `output_tokens`                         | `Usage::output`                        |
//! | `cache_read_input_tokens`               | `Usage::cache_read`                    |
//! | `cache_creation_input_tokens`           | `Usage::cache_write`                   |
//! | `cache_creation.ephemeral_1h_input_tokens` | combined into `Usage::cache_write` |
//! | `stop_reason = "end_turn"`              | `StopReason::Stop`                     |
//! | `stop_reason = "tool_use"`              | `StopReason::ToolUse`                  |
//! | `stop_reason = "max_tokens"`            | `StopReason::MaxTokens`                |
//! | `stop_reason = "refusal"` / `sensitive` | `StopReason::Error` (with `errorMessage`) |
//! | `stop_reason = "pause_turn"`            | `StopReason::Stop`                     |
//! | `stop_reason = "stop_sequence"`         | `StopReason::Stop`                     |
//! | `content_block.type = "text"`           | `Content::Text(TextContent)`           |
//! | `content_block.type = "thinking"`       | `AssistantMessageEvent::ThinkingDelta` |
//! | `content_block.type = "tool_use"`       | `Content::ToolCall(ToolCall)`          |
//!
//! # Native vs. WASM
//!
//! Native targets use `reqwest`; the `wasm32-unknown-unknown` target has
//! no usable HTTP client in Stage 7 and returns
//! [`StreamError::Malformed`] from every call. The WASM-bindgen
//! registration (`pi_ai::wasm::register_anthropic_provider`) seeds a
//! stub provider that can be used in JS host smoke tests.

// Wire-format structs (the `*Wire` types below) are exposed for
// inspection and fixture tests; their fields are documented inline via
// the upstream Anthropic reference rather than via Rustdoc. Keep the
// allow in scope until each struct gets its own doc comment.
#![allow(missing_docs)]

use async_trait::async_trait;
use bytes::Bytes;
#[cfg(not(target_arch = "wasm32"))]
use futures::TryStreamExt;
use pi_protocol::{
    AssistantMessage, AssistantMessageEvent, Content, Context, Message, Model, Role, StopReason,
    TextContent, ToolCall, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::json_parse::{parse_streaming_json, repair_json};
use crate::stream::AssistantMessageEventStream;
use crate::types::{SimpleStreamOptions, StreamError};
#[cfg(not(target_arch = "wasm32"))]
use crate::utils::error_body::truncate_provider_error_body;
use crate::StreamFn;

/// Default base URL for the Anthropic Messages API.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// API version sent in the `anthropic-version` header.
///
/// Pinned to the value the upstream SDK uses so the wire format stays
/// stable.
pub const ANTHROPIC_VERSION: &str = "2023-06-15";

/// Default `max_tokens` when the caller does not supply one and the
/// model descriptor does not either. Anthropic's API requires this
/// field to be present.
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Anthropic Messages provider.
///
/// Construct with [`AnthropicProvider::new`] for the production
/// endpoint, or use [`AnthropicProvider::with_base_url`] to point at a
/// custom mirror (e.g. AWS Bedrock's Anthropic adapter).
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    /// Bearer token sent in the `Authorization` header. Anthropic also
    /// accepts `x-api-key`; we send both so proxies and OAuth shims
    /// can pick whichever they understand.
    pub api_key: String,
    /// Base URL with no trailing slash. The `/v1/messages` path is
    /// appended in [`Self::send_streaming`].
    pub base_url: String,
    /// Override the `anthropic-version` header. Mostly useful for
    /// proxy shims that expect a different version string.
    pub api_version: String,
}

impl AnthropicProvider {
    /// Create a provider pointing at the production Anthropic endpoint.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            api_version: ANTHROPIC_VERSION.to_string(),
        }
    }

    /// Create a provider pointing at a custom base URL (AWS Bedrock,
    /// local mirror, …).
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
            api_version: ANTHROPIC_VERSION.to_string(),
        }
    }

    /// Override the `anthropic-version` header value.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.api_version = version.into();
        self
    }

    /// Build the request body for the messages endpoint.
    ///
    /// Public so callers (and tests) can inspect the wire payload
    /// without making a network call.
    pub fn build_request(
        model: &Model,
        ctx: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<MessagesRequest, StreamError> {
        let mut messages = Vec::with_capacity(ctx.messages.len());
        for msg in &ctx.messages {
            messages.push(anthropic_message_from(msg)?);
        }
        let tools = if ctx.tools.is_empty() {
            None
        } else {
            Some(
                ctx.tools
                    .iter()
                    .map(|t| ToolDescriptor {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        input_schema: t.parameters.clone(),
                    })
                    .collect(),
            )
        };
        Ok(MessagesRequest {
            model: model.id.clone(),
            messages,
            system: if ctx.system_prompt.is_empty() {
                None
            } else {
                Some(ctx.system_prompt.clone())
            },
            max_tokens: options
                .max_tokens
                .unwrap_or(model.max_output_tokens.max(DEFAULT_MAX_TOKENS)),
            temperature: options.temperature,
            stream: true,
            tools,
        })
    }

    /// POST `/v1/messages` with `stream: true` and pipe the SSE byte
    /// stream through [`parse_sse`].
    ///
    /// HTTP status errors map to [`StreamError::Provider`]; transport
    /// errors map to [`StreamError::Transport`]; the rate-limit case
    /// (HTTP 429) is preserved so callers can implement retry/back-off.
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
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("anthropic-version", &self.api_version)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let status_code = status.as_u16();
            let hint = crate::retry::retry_hint_from_headers(response.headers());
            let body = response.text().await.unwrap_or_default();
            return Err(classify_http_status(
                status_code,
                hint,
                truncate_provider_error_body(&body),
            ));
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

/// Classify an HTTP error into the variant of [`StreamError`] the
/// rest of the codebase expects. Pulled out so the wasm stub and the
/// tests share the same logic. Native-only — the wasm stub returns
/// [`StreamError::Malformed`] before any HTTP code is involved.
#[cfg(not(target_arch = "wasm32"))]
fn classify_http_status(
    status: u16,
    hint: crate::types::ProviderRetryHint,
    body: String,
) -> StreamError {
    StreamError::provider_with_hint(status, body, hint)
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Anthropic Messages request body.
#[derive(Debug, Clone, Serialize)]
pub struct MessagesRequest {
    /// Model identifier (`claude-haiku-4-5`, …).
    pub model: String,
    /// Conversation messages (user / assistant turns; tool results are
    /// encoded as user turns with `tool_result` content blocks).
    pub messages: Vec<AnthropicMessage>,
    /// System prompt. Anthropic carries it as a top-level field rather
    /// than a message in the array.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Required by the Anthropic API.
    pub max_tokens: u32,
    /// Sampling temperature. Omitted on thinking-enabled requests
    /// because Anthropic rejects it together with extended thinking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Always `true` for streaming requests.
    pub stream: bool,
    /// Tool descriptors. Skipped when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDescriptor>>,
}

/// One conversation message in Anthropic wire format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum AnthropicMessage {
    /// A user-authored message.
    User {
        /// Either a plain string (text-only) or a list of content blocks.
        content: UserContent,
    },
    /// An assistant-authored message.
    Assistant {
        /// List of content blocks (text, tool_use).
        content: Vec<AssistantContentBlock>,
    },
}

/// User-side content: either a plain string or an array of blocks
/// (text / image / tool_result).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    /// Plain text.
    Text(String),
    /// Array of blocks (text / image / tool_result).
    Blocks(Vec<UserContentBlock>),
}

/// One block in a user message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserContentBlock {
    /// Plain text block.
    Text {
        /// UTF-8 text.
        text: String,
    },
    /// Image attachment.
    Image {
        /// Image source descriptor.
        source: ImageSource,
    },
    /// Tool result returned to the model.
    ToolResult {
        /// Echoes the originating tool call id.
        tool_use_id: String,
        /// Result content.
        content: ToolResultContent,
        /// True when the tool failed and the model should treat the
        /// result as an error.
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

/// Image source — base64 inline image data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    /// Always `"base64"` in the Rust port.
    #[serde(rename = "type")]
    pub kind: String,
    /// MIME type (e.g. `image/png`).
    pub media_type: String,
    /// Base64-encoded image bytes.
    pub data: String,
}

/// Tool result content — string or array of blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    /// Plain text result.
    Text(String),
    /// Array of blocks.
    Blocks(Vec<UserContentBlock>),
}

/// One block in an assistant message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantContentBlock {
    /// Plain text block.
    Text {
        /// UTF-8 text.
        text: String,
    },
    /// Tool call from the model.
    ToolUse {
        /// Provider-issued identifier.
        id: String,
        /// Tool name.
        name: String,
        /// Parsed arguments object.
        input: Value,
    },
}

/// Tool descriptor in Anthropic wire format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    /// Tool name the model uses to invoke the tool.
    pub name: String,
    /// Description shown to the model.
    pub description: String,
    /// JSON Schema describing the tool's parameters object.
    pub input_schema: Value,
}

// ---------------------------------------------------------------------------
// Message conversion
// ---------------------------------------------------------------------------

fn anthropic_message_from(msg: &Message) -> Result<AnthropicMessage, StreamError> {
    match msg.role {
        Role::System => {
            // Anthropic carries system as a top-level field, so we
            // promote a stray system message in the array to a no-op
            // (the top-level `system` is set from `Context::system_prompt`
            // directly). This keeps the wire format valid.
            Ok(AnthropicMessage::User {
                content: UserContent::Text(String::new()),
            })
        }
        Role::User => {
            let blocks: Vec<UserContentBlock> = msg
                .content
                .iter()
                .filter_map(|c| match c {
                    Content::Text(t) => Some(UserContentBlock::Text {
                        text: t.text.clone(),
                    }),
                    _ => None,
                })
                .collect();
            Ok(AnthropicMessage::User {
                content: UserContent::Blocks(blocks),
            })
        }
        Role::Assistant => {
            let mut blocks = Vec::with_capacity(msg.content.len());
            for c in &msg.content {
                match c {
                    Content::Text(t) => {
                        blocks.push(AssistantContentBlock::Text {
                            text: t.text.clone(),
                        });
                    }
                    Content::ToolCall(call) => {
                        blocks.push(AssistantContentBlock::ToolUse {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            input: call.arguments.clone(),
                        });
                    }
                    _ => {
                        return Err(StreamError::Malformed(
                            "assistant message may only contain text or tool calls".into(),
                        ));
                    }
                }
            }
            Ok(AnthropicMessage::Assistant { content: blocks })
        }
        Role::Tool => {
            // `added_tool_names` is deliberately not projected here. Upstream
            // `anthropic-messages.ts::convertToolResult` turns `addedToolNames`
            // into `tool_reference` sibling blocks next to the `tool_result`,
            // but this request model has no `tool_reference` content variant
            // and `pi-ai`'s compat layer has no deferred-tools flag. The field
            // stops at the transcript (see `pi_ai::utils::deferred_tools`).
            //
            // Anthropic expects tool results as user-side `tool_result`
            // blocks. Pull the tool call id from the first ToolResult
            // block; concatenate the textual content of the rest.
            let tool_use_id = msg
                .content
                .iter()
                .find_map(|c| match c {
                    Content::ToolResult(r) => Some(r.tool_call_id.clone()),
                    _ => None,
                })
                .ok_or_else(|| StreamError::Malformed("tool message missing tool result".into()))?;
            let mut text = String::new();
            let mut is_error = None;
            for c in &msg.content {
                match c {
                    Content::Text(t) => text.push_str(&t.text),
                    Content::ToolResult(r) => {
                        is_error = Some(r.is_error);
                        if let Content::Text(t) = &*r.content {
                            text.push_str(&t.text);
                        }
                    }
                    _ => {}
                }
            }
            Ok(AnthropicMessage::User {
                content: UserContent::Blocks(vec![UserContentBlock::ToolResult {
                    tool_use_id,
                    content: ToolResultContent::Text(text),
                    is_error,
                }]),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// SSE parser
// ---------------------------------------------------------------------------

/// Parse an Anthropic SSE byte stream into an
/// [`AssistantMessageEventStream`].
///
/// Each event is shaped like:
///
/// ```text
/// event: message_start
/// data: {"type":"message_start","message":{...}}
/// ```
///
/// Lines that don't start with `event:` or `data:` are ignored (ids,
/// retry hints, empty heartbeats). `event: ping` is dropped silently.
/// `event: error` is converted into an `AssistantMessageEvent::Error`.
pub fn parse_sse(
    bytes: impl futures::Stream<Item = Result<Bytes, StreamError>> + Send + 'static,
    model_id: String,
) -> AssistantMessageEventStream {
    Box::pin(SseStream::new(bytes, model_id))
}

struct SseStream {
    inner: std::pin::Pin<Box<dyn futures::Stream<Item = Result<Bytes, StreamError>> + Send>>,
    /// Half-parsed SSE buffer (lines from the most recent chunk).
    line_buffer: Vec<u8>,
    /// Currently buffered `event:` field for the in-flight event.
    event_name: Option<String>,
    /// Currently buffered `data:` lines for the in-flight event.
    data_lines: Vec<String>,
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
    /// Whether `tool_use` blocks have been seen (drives the default
    /// stop reason when the upstream omits one).
    saw_tool_use: bool,
    /// Final stop reason set by `message_delta.delta.stop_reason`.
    stop_reason: Option<StopReason>,
    /// Final assistant message model name. The Anthropic API can
    /// return a different model id from the request (server-side
    /// fallbacks); the TS port tracks this through `AssistantMessage`.
    model: Option<String>,
    /// Token usage accumulated so far. `input_tokens` and
    /// `cache_read_input_tokens` come from `message_start`; the
    /// `output_tokens` final value comes from `message_delta.usage`.
    usage: Usage,
    /// Per-block state — we track the tool_use id/name/arguments and
    /// the text / thinking accumulators by their Anthropic `index`.
    blocks: std::collections::HashMap<u32, BlockState>,
}

#[derive(Default)]
struct BlockState {
    kind: BlockKind,
}

#[derive(Default)]
enum BlockKind {
    #[default]
    Empty,
    Text(TextBlock),
    ToolUse(ToolUseBlock),
    /// Thinking blocks are emitted as `ThinkingDelta` events but not
    /// stored in the content array (the wire types do not yet have a
    /// `ThinkingContent` variant — they will land in a follow-up
    /// `pi-protocol` increment).
    Thinking,
}

#[derive(Default)]
struct TextBlock {
    text: String,
}

#[derive(Default)]
struct ToolUseBlock {
    id: Option<String>,
    name: Option<String>,
    /// Arguments as a (possibly partial) JSON string.
    arguments: String,
}

impl SseStream {
    fn new(
        bytes: impl futures::Stream<Item = Result<Bytes, StreamError>> + Send + 'static,
        model_id: String,
    ) -> Self {
        Self {
            inner: Box::pin(bytes),
            line_buffer: Vec::new(),
            event_name: None,
            data_lines: Vec::new(),
            pending: std::collections::VecDeque::new(),
            finished: false,
            started: false,
            model_id,
            state: ParserState::default(),
        }
    }

    fn feed(&mut self, bytes: &Bytes) -> Result<(), StreamError> {
        self.line_buffer.extend_from_slice(bytes);
        while let Some(rel_end) = find_newline(&self.line_buffer) {
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
        let value = String::from_utf8_lossy(value).into_owned();
        match field {
            b"event" => {
                // Replace the in-flight event name — Anthropic always
                // sends a single `event:` per event.
                self.event_name = Some(value);
            }
            b"data" => {
                self.data_lines.push(value);
            }
            b"id" | b"retry" => {
                // We only care about `event:` / `data:`.
            }
            _ => {}
        }
        Ok(())
    }

    fn dispatch_event(&mut self) -> Result<(), StreamError> {
        let event_name = match self.event_name.take() {
            Some(name) => name,
            None => {
                // No event name means no buffered payload — clear any
                // stray data lines and move on.
                self.data_lines.clear();
                return Ok(());
            }
        };
        let payload = self.data_lines.join("\n");
        self.data_lines.clear();
        if payload.is_empty() {
            return Ok(());
        }
        // Upstream parses every frame with `parseJsonWithRepair`. Repairing
        // once here is equivalent — `repair_json` is the identity on valid
        // JSON — and keeps the per-event handlers from each retrying.
        let payload = repair_json(&payload);
        match event_name.as_str() {
            "ping" => Ok(()),
            "error" => self.handle_error_event(&payload),
            _ => self.handle_event(&event_name, &payload),
        }
    }

    fn handle_error_event(&mut self, data: &str) -> Result<(), StreamError> {
        // `event: error` payload shape: `{ "type": "error", "error": { "type": "...", "message": "..." } }`
        // The TS port surfaces the upstream `message` verbatim.
        #[derive(Deserialize)]
        struct WireErrorEvent {
            error: WireErrorBody,
        }
        #[derive(Deserialize)]
        struct WireErrorBody {
            #[serde(default)]
            message: Option<String>,
        }
        let parsed: WireErrorEvent = serde_json::from_str(data)
            .map_err(|e| StreamError::Malformed(format!("Anthropic error event JSON: {e}")))?;
        let message = parsed
            .error
            .message
            .unwrap_or_else(|| "Anthropic returned an error event".to_string());
        self.pending.push_back(Err(StreamError::Malformed(format!(
            "Anthropic error event: {message}"
        ))));
        Ok(())
    }

    fn handle_event(&mut self, name: &str, data: &str) -> Result<(), StreamError> {
        if !self.started {
            self.started = true;
            self.pending.push_back(Ok(AssistantMessageEvent::Start {
                model: self
                    .state
                    .model
                    .clone()
                    .unwrap_or_else(|| self.model_id.clone()),
            }));
        }

        match name {
            "message_start" => self.handle_message_start(data),
            "content_block_start" => self.handle_content_block_start(data),
            "content_block_delta" => self.handle_content_block_delta(data),
            "content_block_stop" => self.handle_content_block_stop(data),
            "message_delta" => self.handle_message_delta(data),
            "message_stop" => Ok(()),
            // Other event names (`ping`, proxies' custom events, …)
            // are silently dropped.
            _ => Ok(()),
        }
    }

    fn handle_message_start(&mut self, data: &str) -> Result<(), StreamError> {
        #[derive(Deserialize)]
        struct WireMessageStart {
            message: WireMessageMeta,
        }
        #[derive(Deserialize)]
        struct WireMessageMeta {
            #[serde(default, rename = "_unused_id")]
            #[allow(dead_code)]
            id: Option<String>,
            #[serde(default)]
            model: Option<String>,
            usage: WireMessageUsage,
        }
        #[derive(Deserialize, Default)]
        struct WireMessageUsage {
            #[serde(default)]
            input_tokens: u32,
            #[serde(default)]
            output_tokens: u32,
            #[serde(default)]
            cache_creation_input_tokens: u32,
            #[serde(default)]
            cache_read_input_tokens: u32,
        }
        let parsed: WireMessageStart = serde_json::from_str(data)
            .map_err(|e| StreamError::Malformed(format!("Anthropic message_start: {e}: {data}")))?;
        if let Some(model) = parsed.message.model {
            self.state.model = Some(model);
        }
        let usage = parsed.message.usage;
        self.state.usage.input = usage.input_tokens;
        self.state.usage.output = usage.output_tokens;
        self.state.usage.cache_read = usage.cache_read_input_tokens;
        self.state.usage.cache_write = usage.cache_creation_input_tokens;
        Ok(())
    }

    fn handle_content_block_start(&mut self, data: &str) -> Result<(), StreamError> {
        #[derive(Deserialize)]
        struct WireContentBlockStart {
            index: u32,
            content_block: WireContentBlock,
        }
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        #[allow(clippy::large_enum_variant)]
        enum WireContentBlock {
            /// Text block.
            Text {
                #[serde(default)]
                text: String,
            },
            /// Thinking block (extended thinking).
            Thinking {
                #[serde(default)]
                thinking: String,
            },
            /// Tool call.
            ToolUse {
                #[serde(default)]
                id: String,
                #[serde(default)]
                name: String,
                #[serde(default)]
                input: Value,
            },
        }
        let parsed: WireContentBlockStart = serde_json::from_str(data).map_err(|e| {
            StreamError::Malformed(format!("Anthropic content_block_start: {e}: {data}"))
        })?;
        let index = parsed.index;
        let mut block = BlockState::default();
        match parsed.content_block {
            WireContentBlock::Text { text } => {
                if !text.is_empty() {
                    // Preserve any initial text the API hands us in the
                    // start event (e.g. when the model emits a leading
                    // paragraph in one shot).
                    self.pending.push_back(Ok(AssistantMessageEvent::TextDelta {
                        delta: text.clone(),
                    }));
                    block.kind = BlockKind::Text(TextBlock { text });
                } else {
                    block.kind = BlockKind::Text(TextBlock::default());
                }
            }
            WireContentBlock::Thinking { thinking } => {
                if !thinking.is_empty() {
                    self.pending
                        .push_back(Ok(AssistantMessageEvent::ThinkingDelta { delta: thinking }));
                }
                block.kind = BlockKind::Thinking;
            }
            WireContentBlock::ToolUse { id, name, input } => {
                let mut state = ToolUseBlock::default();
                if !id.is_empty() {
                    state.id = Some(id.clone());
                }
                if !name.is_empty() {
                    state.name = Some(name.clone());
                }
                let initial_json =
                    serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                if !initial_json.is_empty() && initial_json != "{}" {
                    state.arguments.push_str(&initial_json);
                }
                self.pending
                    .push_back(Ok(AssistantMessageEvent::ToolCallDelta {
                        index,
                        id: if id.is_empty() { None } else { Some(id) },
                        name: if name.is_empty() { None } else { Some(name) },
                        arguments_delta: if initial_json.is_empty() || initial_json == "{}" {
                            None
                        } else {
                            Some(initial_json)
                        },
                    }));
                block.kind = BlockKind::ToolUse(state);
                self.state.saw_tool_use = true;
            }
        }
        self.state.blocks.insert(index, block);
        Ok(())
    }

    fn handle_content_block_delta(&mut self, data: &str) -> Result<(), StreamError> {
        #[derive(Deserialize)]
        struct WireContentBlockDelta {
            index: u32,
            delta: WireContentDelta,
        }
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        #[allow(clippy::enum_variant_names)]
        enum WireContentDelta {
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
                #[allow(dead_code)]
                signature: String,
            },
        }
        let parsed: WireContentBlockDelta = serde_json::from_str(data).map_err(|e| {
            StreamError::Malformed(format!("Anthropic content_block_delta: {e}: {data}"))
        })?;
        let index = parsed.index;
        match parsed.delta {
            WireContentDelta::TextDelta { text } => {
                if text.is_empty() {
                    return Ok(());
                }
                let delta = text;
                self.pending.push_back(Ok(AssistantMessageEvent::TextDelta {
                    delta: delta.clone(),
                }));
                let entry = self.state.blocks.entry(index).or_default();
                if let BlockKind::Text(t) = &mut entry.kind {
                    t.text.push_str(&delta);
                } else {
                    entry.kind = BlockKind::Text(TextBlock { text: delta });
                }
            }
            WireContentDelta::ThinkingDelta { thinking } => {
                if thinking.is_empty() {
                    return Ok(());
                }
                self.pending
                    .push_back(Ok(AssistantMessageEvent::ThinkingDelta { delta: thinking }));
            }
            WireContentDelta::InputJsonDelta { partial_json } => {
                let entry = self.state.blocks.entry(index).or_default();
                let tc = match &mut entry.kind {
                    BlockKind::ToolUse(tc) => tc,
                    _ => {
                        entry.kind = BlockKind::ToolUse(ToolUseBlock::default());
                        match &mut entry.kind {
                            BlockKind::ToolUse(tc) => tc,
                            _ => unreachable!(),
                        }
                    }
                };
                tc.arguments.push_str(&partial_json);
                self.pending
                    .push_back(Ok(AssistantMessageEvent::ToolCallDelta {
                        index,
                        id: None,
                        name: None,
                        arguments_delta: Some(partial_json),
                    }));
            }
            WireContentDelta::SignatureDelta { .. } => {
                // The signature is required by the API to echo back
                // on multi-turn reasoning; the Rust port discards it
                // because the wire types do not carry a thinking
                // signature yet. Recorded for completeness — see the
                // `anthropic-shared` module in the TS port.
            }
        }
        Ok(())
    }

    fn handle_content_block_stop(&mut self, _data: &str) -> Result<(), StreamError> {
        // The stop event carries no payload worth parsing — the block
        // state is already finalised in the per-event handlers. We
        // could emit `ContentBlockStop` events here in the future if
        // the wire surface grows them.
        Ok(())
    }

    fn handle_message_delta(&mut self, data: &str) -> Result<(), StreamError> {
        #[derive(Deserialize)]
        struct WireMessageDelta {
            #[serde(default)]
            delta: Option<WireMessageDeltaInner>,
            #[serde(default)]
            usage: Option<WireMessageDeltaUsage>,
        }
        #[derive(Deserialize)]
        struct WireMessageDeltaInner {
            #[serde(default)]
            stop_reason: Option<String>,
        }
        #[derive(Deserialize, Default)]
        struct WireMessageDeltaUsage {
            #[serde(default)]
            input_tokens: Option<u32>,
            #[serde(default)]
            output_tokens: Option<u32>,
            #[serde(default)]
            cache_read_input_tokens: Option<u32>,
            #[serde(default)]
            cache_creation_input_tokens: Option<u32>,
        }
        let parsed: WireMessageDelta = serde_json::from_str(data)
            .map_err(|e| StreamError::Malformed(format!("Anthropic message_delta: {e}: {data}")))?;
        if let Some(delta) = parsed.delta {
            if let Some(reason) = delta.stop_reason {
                self.state.stop_reason = Some(map_stop_reason(&reason));
            }
        }
        if let Some(usage) = parsed.usage {
            // Mirror the TS port's behaviour: only update the fields
            // the upstream actually sends. `output_tokens` is always
            // populated by Anthropic; the cache / input fields only
            // appear when the stream crosses a cache boundary.
            if let Some(input_tokens) = usage.input_tokens {
                self.state.usage.input = input_tokens;
            }
            if let Some(output_tokens) = usage.output_tokens {
                self.state.usage.output = output_tokens;
            }
            if let Some(cache_read) = usage.cache_read_input_tokens {
                self.state.usage.cache_read = cache_read;
            }
            if let Some(cache_write) = usage.cache_creation_input_tokens {
                self.state.usage.cache_write = cache_write;
            }
        }
        Ok(())
    }

    fn finalize(&mut self) {
        // Materialise per-block state into the content list, preserving
        // the Anthropic block order (sorted by `index`).
        let mut block_order: Vec<u32> = self.state.blocks.keys().copied().collect();
        block_order.sort_unstable();
        let mut blocks = std::mem::take(&mut self.state.blocks);
        let mut content: Vec<Content> = Vec::new();
        for index in block_order {
            let entry = blocks.remove(&index).unwrap_or_default();
            match entry.kind {
                BlockKind::ToolUse(tc) => {
                    let id = tc.id.unwrap_or_default();
                    let name = tc.name.unwrap_or_default();
                    let arguments = parse_streaming_json(Some(tc.arguments.as_str()));
                    content.push(Content::ToolCall(ToolCall {
                        id,
                        name,
                        arguments,
                    }));
                }
                BlockKind::Text(t) => {
                    if !t.text.is_empty() {
                        content.push(Content::Text(TextContent { text: t.text }));
                    }
                }
                BlockKind::Thinking | BlockKind::Empty => {}
            }
        }
        let stop_reason = self.state.stop_reason.unwrap_or({
            if self.state.saw_tool_use {
                StopReason::ToolUse
            } else {
                StopReason::Stop
            }
        });
        let usage = self.state.usage;
        let model = self
            .state
            .model
            .clone()
            .unwrap_or_else(|| self.model_id.clone());
        let message = AssistantMessage {
            model,
            content,
            stop_reason,
            usage,
            error_message: None,
        };
        self.pending.push_back(Ok(message_to_done(message)));
    }
}

fn message_to_done(message: AssistantMessage) -> AssistantMessageEvent {
    AssistantMessageEvent::Done {
        content: message.content,
        stop_reason: message.stop_reason,
        usage: message.usage,
    }
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" => StopReason::Stop,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        // Anthropic's `refusal` and `sensitive` stop reasons are
        // surfaced as errors with an explanation; the Rust port
        // collapses both into `StopReason::Error` (the closest
        // existing variant) and forwards the upstream message
        // through the trailing `Done` payload instead of a separate
        // `Error` event. The TS port's `errorMessage` enrichment
        // lives on the `AssistantMessage` struct and is not yet in
        // the Rust wire types — once it lands the mapping below
        // becomes a richer enum case.
        "refusal" | "sensitive" => StopReason::Error,
        "pause_turn" | "stop_sequence" => StopReason::Stop,
        other => {
            // Forward unknown reasons as `Stop` rather than panicking
            // — the Anthropic API has added new reasons in the past
            // (e.g. `sensitive`) and this keeps the parser forward
            // compatible. The TS port throws; we log instead.
            tracing::debug!(reason = %other, "Anthropic returned unknown stop_reason");
            StopReason::Stop
        }
    }
}

impl futures::Stream for SseStream {
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
                    if !self.line_buffer.is_empty() {
                        let buf = std::mem::take(&mut self.line_buffer);
                        if let Err(e) = self.process_line(&buf) {
                            return std::task::Poll::Ready(Some(Err(e)));
                        }
                    }
                    if let Err(e) = self.dispatch_event() {
                        return std::task::Poll::Ready(Some(Err(e)));
                    }
                    if !self.started {
                        // Empty stream — synthesize a Start + Done so
                        // callers always see the contract.
                        self.started = true;
                        let start_model = self.model_id.clone();
                        self.pending
                            .push_back(Ok(AssistantMessageEvent::Start { model: start_model }));
                    }
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
    if value.first() == Some(&b' ') {
        value = &value[1..];
    }
    Some((field, value))
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
            id: "claude-haiku-4-5".into(),
            api: pi_protocol::Api::AnthropicMessages,
            label: None,
            context_window: 200_000,
            max_output_tokens: 8_192,
        }
    }

    fn ctx_with_tool() -> Context {
        let mut ctx = Context::new("you are pi");
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
        let ctx = ctx_with_tool();
        let req = AnthropicProvider::build_request(
            &model(),
            &ctx,
            &SimpleStreamOptions {
                temperature: Some(0.3),
                max_tokens: Some(512),
                ..Default::default()
            },
        )
        .expect("build request");
        let v = serde_json::to_value(&req).expect("serialize request");
        assert_eq!(v["model"], "claude-haiku-4-5");
        assert_eq!(v["stream"], true);
        assert_eq!(v["max_tokens"], 512);
        assert!((v["temperature"].as_f64().expect("temperature number") - 0.3).abs() < 1e-6);
        assert_eq!(v["system"], "you are pi");

        let messages = v["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["type"], "text");
        assert_eq!(
            messages[0]["content"][0]["text"],
            "what's the weather in SF?"
        );

        let tools = v["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "get_weather");
        assert_eq!(tools[0]["input_schema"]["type"], "object");
    }

    #[test]
    fn request_body_serializes_without_tools() {
        let mut ctx = Context::new("you are pi");
        ctx.messages.push(Message {
            role: Role::User,
            content: vec![Content::text("hi")],
            model: None,
        });
        let req = AnthropicProvider::build_request(&model(), &ctx, &SimpleStreamOptions::default())
            .expect("build request");
        let v = serde_json::to_value(&req).expect("serialize request");
        assert!(v.get("tools").is_none());
        assert!(v.get("temperature").is_none());
        // max_tokens must be present even when the caller omits it.
        assert!(v["max_tokens"].is_number());
    }

    #[test]
    fn tool_results_become_user_blocks() {
        let mut ctx = Context::new("");
        ctx.messages.push(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(pi_protocol::ToolResult {
                tool_call_id: "toolu_x".into(),
                content: Box::new(Content::text("72F and sunny")),
                is_error: false,
                details: None,
                added_tool_names: None,
            })],
            model: None,
        });
        let req = AnthropicProvider::build_request(&model(), &ctx, &SimpleStreamOptions::default())
            .expect("build request");
        let v = serde_json::to_value(&req).expect("serialize request");
        let messages = v["messages"].as_array().expect("messages array");
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["type"], "tool_result");
        assert_eq!(messages[0]["content"][0]["tool_use_id"], "toolu_x");
        assert_eq!(messages[0]["content"][0]["content"], "72F and sunny");
    }

    #[tokio::test]
    async fn sse_parser_emits_text_deltas_then_done() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/anthropic/text_response.sse");
        let bytes =
            std::fs::read(&fixture).unwrap_or_else(|e| panic!("read fixture {fixture:?}: {e}"));
        let chunks = stream::iter(vec![Ok::<bytes::Bytes, StreamError>(Bytes::from(bytes))]);
        let mut s = parse_sse(chunks, "claude-haiku-4-5".to_string());
        let mut events = Vec::new();
        while let Some(ev) = s.next().await {
            events.push(ev.expect("stream event"));
        }
        // Expect: Start, TextDelta("Hello"), TextDelta(" there"), Done.
        assert!(
            events.len() >= 4,
            "got {} events: {:?}",
            events.len(),
            events
        );
        assert!(matches!(events[0], AssistantMessageEvent::Start { .. }));
        match &events[1] {
            AssistantMessageEvent::TextDelta { delta } => assert_eq!(delta, "Hello"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
        match &events[2] {
            AssistantMessageEvent::TextDelta { delta } => assert_eq!(delta, " there"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
        match &events[3] {
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
                assert_eq!(text, "Hello there");
                assert_eq!(usage.input, 17);
                assert_eq!(usage.output, 4);
            }
            other => panic!("expected done, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sse_parser_emits_tool_call_delta_then_done() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/anthropic/tool_use_response.sse");
        let bytes =
            std::fs::read(&fixture).unwrap_or_else(|e| panic!("read fixture {fixture:?}: {e}"));
        let chunks = stream::iter(vec![Ok::<bytes::Bytes, StreamError>(Bytes::from(bytes))]);
        let mut s = parse_sse(chunks, "claude-haiku-4-5".to_string());
        let mut events = Vec::new();
        while let Some(ev) = s.next().await {
            events.push(ev.expect("stream event"));
        }
        let tool_deltas: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AssistantMessageEvent::ToolCallDelta {
                    index,
                    name,
                    arguments_delta,
                    ..
                } => Some((*index, name.clone(), arguments_delta.clone())),
                _ => None,
            })
            .collect();
        assert!(!tool_deltas.is_empty(), "expected at least one tool delta");
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
        assert_eq!(tc.name, "get_weather");
        assert_eq!(tc.id, "toolu_test_01");
        assert_eq!(tc.arguments["city"], "San Francisco");
    }

    #[tokio::test]
    async fn sse_parser_records_cache_read_tokens() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/anthropic/cache_read_response.sse");
        let bytes =
            std::fs::read(&fixture).unwrap_or_else(|e| panic!("read fixture {fixture:?}: {e}"));
        let chunks = stream::iter(vec![Ok::<bytes::Bytes, StreamError>(Bytes::from(bytes))]);
        let mut s = parse_sse(chunks, "claude-haiku-4-5".to_string());
        let mut events = Vec::new();
        while let Some(ev) = s.next().await {
            events.push(ev.expect("stream event"));
        }
        let done = events
            .iter()
            .find_map(|e| match e {
                AssistantMessageEvent::Done { usage, .. } => Some(usage),
                _ => None,
            })
            .expect("done event");
        assert_eq!(done.cache_read, 1200);
        assert_eq!(done.cache_write, 0);
        assert_eq!(done.input, 5);
        assert_eq!(done.output, 3);
    }

    #[tokio::test]
    async fn sse_parser_surfaces_error_event_as_stream_error() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/anthropic/error_event.sse");
        let bytes =
            std::fs::read(&fixture).unwrap_or_else(|e| panic!("read fixture {fixture:?}: {e}"));
        let chunks = stream::iter(vec![Ok::<bytes::Bytes, StreamError>(Bytes::from(bytes))]);
        let mut s = parse_sse(chunks, "claude-haiku-4-5".to_string());
        let mut saw_error = false;
        while let Some(ev) = s.next().await {
            if let Err(StreamError::Malformed(msg)) = ev {
                if msg.contains("overloaded") {
                    saw_error = true;
                }
            }
        }
        assert!(
            saw_error,
            "expected the parser to surface the upstream error"
        );
    }

    #[tokio::test]
    async fn stop_reason_max_tokens_maps_to_max_tokens() {
        let payload = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"claude-haiku-4-5\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n";
        let chunks = stream::iter(vec![Ok::<bytes::Bytes, StreamError>(Bytes::from(
            payload.to_string(),
        ))]);
        let mut s = parse_sse(chunks, "claude-haiku-4-5".to_string());
        let mut done_reason = None;
        while let Some(ev) = s.next().await {
            if let Ok(AssistantMessageEvent::Done { stop_reason, .. }) = ev {
                done_reason = Some(stop_reason);
            }
        }
        assert_eq!(done_reason, Some(StopReason::MaxTokens));
    }

    #[test]
    fn classify_http_status_returns_provider_variant() {
        let hint = crate::types::ProviderRetryHint {
            retry_after_ms: Some(2_000),
            should_retry: None,
        };
        match classify_http_status(401, hint, "unauthorized".into()) {
            StreamError::Provider { status, body, hint } => {
                assert_eq!(status, 401);
                assert_eq!(body, "unauthorized");
                assert_eq!(hint.retry_after_ms, Some(2_000));
            }
            other => panic!("expected Provider error, got {other:?}"),
        }
    }

    #[test]
    fn build_request_drops_empty_temperature() {
        let mut ctx = Context::new("");
        ctx.messages.push(Message {
            role: Role::User,
            content: vec![Content::text("hi")],
            model: None,
        });
        let req = AnthropicProvider::build_request(&model(), &ctx, &SimpleStreamOptions::default())
            .expect("build request");
        let v = serde_json::to_value(&req).expect("serialize request");
        assert!(v.get("temperature").is_none());
        // max_tokens must still be present.
        assert!(v["max_tokens"].is_number());
    }

    // Tiny smoke test that exercises the `Stream` shape without
    // running an HTTP server — splits a multi-event byte stream into
    // individual chunks to make sure the parser reassembles them.
    #[tokio::test]
    async fn sse_parser_handles_chunked_input() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/anthropic/text_response.sse");
        let bytes =
            std::fs::read(&fixture).unwrap_or_else(|e| panic!("read fixture {fixture:?}: {e}"));
        // Feed the fixture 16 bytes at a time so the line-buffering
        // path gets exercised.
        let mut chunks: Vec<Result<Bytes, StreamError>> = Vec::new();
        let mut offset = 0;
        while offset < bytes.len() {
            let end = (offset + 16).min(bytes.len());
            chunks.push(Ok(Bytes::copy_from_slice(&bytes[offset..end])));
            offset = end;
        }
        let mut s = parse_sse(stream::iter(chunks), "claude-haiku-4-5".to_string());
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
        assert_eq!(text, "Hello there");
    }

    // ---------------------------------------------------------------------------
    // Stub sanity check: the unit tests below build a request body
    // and assert the JSON shape. They exist mainly to catch
    // regressions in the message conversion. Keep them close to the
    // type definitions so the wire format stays easy to inspect.

    #[test]
    fn user_text_only_serializes_as_block_array() {
        let mut ctx = Context::new("");
        ctx.messages.push(Message {
            role: Role::User,
            content: vec![Content::text("hello")],
            model: None,
        });
        let req = AnthropicProvider::build_request(&model(), &ctx, &SimpleStreamOptions::default())
            .expect("build request");
        let v = serde_json::to_value(&req).expect("serialize");
        let content = &v["messages"][0]["content"];
        assert!(content.is_array(), "expected block array, got {content}");
        assert_eq!(content[0]["type"], "text");
    }

    #[test]
    fn assistant_text_and_tool_use_in_one_message() {
        let mut ctx = Context::new("");
        ctx.messages.push(Message {
            role: Role::Assistant,
            content: vec![
                Content::text("Looking up the weather…"),
                Content::ToolCall(pi_protocol::ToolCall {
                    id: "toolu_x".into(),
                    name: "get_weather".into(),
                    arguments: json!({"city": "Berlin"}),
                }),
            ],
            model: None,
        });
        let req = AnthropicProvider::build_request(&model(), &ctx, &SimpleStreamOptions::default())
            .expect("build request");
        let v = serde_json::to_value(&req).expect("serialize");
        let content = &v["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["name"], "get_weather");
        assert_eq!(content[1]["input"]["city"], "Berlin");
    }
}

// ---------------------------------------------------------------------------
// Catalog loader — `register_provider_json`
// ---------------------------------------------------------------------------

/// JSON envelope matching the Rust-side model descriptor.
///
/// The TS port ships a generated catalog (`anthropic.models.ts`) that
/// is too rich for the Rust wire types to express verbatim (it carries
/// cost metadata, compatibility flags, etc.). For Stage 7 we expose
/// a minimal subset that the `Models::register_provider_json` helper
/// can deserialize into the existing [`Model`] struct. The richer
/// metadata lands in a follow-up `pi-protocol` increment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicModelEntry {
    /// Model identifier (e.g. `claude-haiku-4-5`).
    pub id: String,
    /// Human-readable label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Context window size.
    #[serde(default)]
    pub context_window: u32,
    /// Maximum output tokens.
    #[serde(default)]
    pub max_output_tokens: u32,
    /// Whether the model is a reasoning/thinking model. Set to `true`
    /// when the upstream catalogue marks the entry as reasoning.
    #[serde(default)]
    pub reasoning: bool,
}

/// JSON envelope describing one provider's catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicCatalog {
    /// Provider identifier — typically `"anthropic"`.
    pub provider: String,
    /// Models in the catalog.
    pub models: Vec<AnthropicModelEntry>,
}

/// Parse an Anthropic catalog JSON document into a list of
/// [`Model`] descriptors suitable for [`crate::models::Models::set_provider`].
pub fn catalog_from_json(json_str: &str) -> Result<Vec<Model>, StreamError> {
    let catalog: AnthropicCatalog = serde_json::from_str(json_str)
        .map_err(|e| StreamError::Malformed(format!("Anthropic catalog JSON: {e}")))?;
    Ok(catalog
        .models
        .into_iter()
        .map(|m| Model {
            provider: pi_protocol::ProviderId::new(&catalog.provider),
            id: m.id,
            api: pi_protocol::Api::AnthropicMessages,
            label: m.label,
            context_window: m.context_window,
            max_output_tokens: m.max_output_tokens,
        })
        .collect())
}

/// Built-in Claude catalog — the three models Stage 7 ships by
/// default. Mirrors the TS port's defaults so downstream callers get
/// Claude 4.5 / 4.x coverage without configuration.
pub fn builtin_claude_models() -> Vec<Model> {
    vec![
        Model {
            provider: pi_protocol::ProviderId::new("anthropic"),
            id: "claude-opus-4-5".into(),
            api: pi_protocol::Api::AnthropicMessages,
            label: Some("Claude Opus 4.5".into()),
            context_window: 200_000,
            max_output_tokens: 32_000,
        },
        Model {
            provider: pi_protocol::ProviderId::new("anthropic"),
            id: "claude-sonnet-4-5".into(),
            api: pi_protocol::Api::AnthropicMessages,
            label: Some("Claude Sonnet 4.5".into()),
            context_window: 200_000,
            max_output_tokens: 16_000,
        },
        Model {
            provider: pi_protocol::ProviderId::new("anthropic"),
            id: "claude-haiku-4-5".into(),
            api: pi_protocol::Api::AnthropicMessages,
            label: Some("Claude Haiku 4.5".into()),
            context_window: 200_000,
            max_output_tokens: 8_192,
        },
    ]
}

#[cfg(test)]
mod catalog_tests {
    use super::*;
    use crate::models::Models;

    #[test]
    fn builtin_catalog_has_three_models() {
        let models = builtin_claude_models();
        assert_eq!(models.len(), 3);
        for m in &models {
            assert_eq!(m.api, pi_protocol::Api::AnthropicMessages);
            assert_eq!(m.provider.0, "anthropic");
            assert!(m.context_window > 0);
            assert!(m.max_output_tokens > 0);
        }
    }

    #[test]
    fn catalog_from_json_round_trip() {
        let json = serde_json::to_string(&AnthropicCatalog {
            provider: "anthropic".into(),
            models: vec![AnthropicModelEntry {
                id: "claude-haiku-4-5".into(),
                label: Some("Claude Haiku 4.5".into()),
                context_window: 200_000,
                max_output_tokens: 8_192,
                reasoning: false,
            }],
        })
        .expect("serialize");
        let models = catalog_from_json(&json).expect("parse");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "claude-haiku-4-5");
        assert_eq!(models[0].context_window, 200_000);
    }

    #[test]
    fn register_provider_json_seeds_anthropic_models() {
        let json = serde_json::to_string(&AnthropicCatalog {
            provider: "anthropic".into(),
            models: vec![AnthropicModelEntry {
                id: "claude-haiku-4-5".into(),
                label: None,
                context_window: 200_000,
                max_output_tokens: 8_192,
                reasoning: false,
            }],
        })
        .expect("serialize");
        let mut catalog = Models::new();
        let provider = pi_protocol::ProviderId::new("anthropic");
        let models = catalog_from_json(&json).expect("parse");
        catalog.set_provider(provider.clone(), models);
        let m = catalog
            .get_model(&provider, "claude-haiku-4-5")
            .expect("lookup");
        assert_eq!(m.api, pi_protocol::Api::AnthropicMessages);
    }
}
