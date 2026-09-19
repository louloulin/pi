//! OpenAI Responses provider — the modern `POST /v1/responses` endpoint.
//!
//! [`Api::OpenAiResponses`](pi_protocol::Api::OpenAiResponses) is a
//! different wire protocol from Chat Completions, not a variant of it:
//!
//! | concern        | Chat Completions                     | Responses                                        |
//! |----------------|--------------------------------------|--------------------------------------------------|
//! | conversation   | `messages[]` with `role` + `content` | `input[]` of typed items (`message`, `function_call`, `function_call_output`) |
//! | tools          | nested `{type:"function", function:{…}}` | flat `{type:"function", name, description, parameters}` |
//! | tool call      | assistant `tool_calls[]` + `role:"tool"` turn | `function_call` item + separate `function_call_output` item |
//! | system prompt  | `role:"system"` message               | `role:"system"`/`"developer"` *item* in `input[]` |
//! | streaming      | `choices[].delta` chunks              | one event per change (`response.output_text.delta`, …) |
//! | reasoning      | none                                  | `response.reasoning_summary_text.delta`           |
//! | usage          | `usage.prompt_tokens`                 | `response.usage.input_tokens`                     |
//!
//! The upstream TypeScript port has both adapters
//! (`packages/ai/src/api/openai-responses.ts` and
//! `openai-responses-shared.ts`); this module closes the Rust gap where
//! `pi-coding-agent`'s router returned "no streaming adapter" for that
//! [`Api`](pi_protocol::Api).
//!
//! # Mapping table
//!
//! | wire event                                            | Rust event                                        |
//! |-------------------------------------------------------|---------------------------------------------------|
//! | *first* chunk of any kind                             | [`AssistantMessageEvent::Start`]                  |
//! | `response.output_text.delta`                          | [`AssistantMessageEvent::TextDelta`]              |
//! | `response.refusal.delta`                              | [`AssistantMessageEvent::TextDelta`]              |
//! | `response.reasoning_summary_text.delta`               | [`AssistantMessageEvent::ThinkingDelta`]          |
//! | `response.reasoning_text.delta`                       | [`AssistantMessageEvent::ThinkingDelta`]          |
//! | `response.output_item.added` (`function_call`)        | [`AssistantMessageEvent::ToolCallDelta`] (id/name) |
//! | `response.function_call_arguments.delta`              | [`AssistantMessageEvent::ToolCallDelta`] (args)   |
//! | `response.completed` / `response.incomplete`          | [`AssistantMessageEvent::Done`]                   |
//! | `response.failed` / `error`                           | `Err(`[`StreamError::Malformed`]`)`               |
//! | `data: [DONE]`                                        | *(ignored — the `response.completed` event ends the stream)* |
//!
//! # Deliberate simplifications vs. the TS port
//!
//! * Thinking text is streamed as
//!   [`ThinkingDelta`](AssistantMessageEvent::ThinkingDelta) but **not
//!   stored**: the `pi-protocol` [`Content`] enum has no `Thinking`
//!   variant yet, so reasoning output cannot appear in the final `Done`
//!   content. Same trade-off as the Anthropic and Google adapters.
//! * Reasoning items are not echoed back on request rebuilds (the
//!   upstream preserves an encrypted reasoning payload); our conversation
//!   history only carries text + tool calls.
//! * `store: false` is always sent (upstream default), and no
//!   `prompt_cache_key` / `service_tier` / `reasoning` parameters are
//!   emitted: the Rust [`Model`] descriptor and
//!   [`SimpleStreamOptions`] have no fields for them yet.
//! * Tool-call ids are the provider's `call_id` only. Upstream encodes
//!   `call_id|item_id` so a request rebuild can echo the `fc_…` item id;
//!   the Responses request builder accepts a bare `call_id`, so the
//!   Rust adapter does not need the second half.
//! * The system prompt is emitted as a `system` role item (upstream
//!   upgrades to `developer` only for reasoning models; the Rust
//!   [`Model`] has no `reasoning` flag to key off).
//! * SSE `event:` names are ignored — every frame's JSON `type` field is
//!   authoritative and always present, so dispatch reads that instead of
//!   keeping a second state machine in sync with the header line.
//! * `max_output_tokens` is clamped to [`MIN_OUTPUT_TOKENS`] because the
//!   Responses endpoint rejects smaller values.
//!
//! Native targets use `reqwest`; `wasm32-unknown-unknown` has no usable
//! HTTP client in this stage and returns [`StreamError::Malformed`] from
//! every call (same as the other HTTP adapters).

// Wire-format structs below are documented inline via the upstream
// OpenAI reference rather than per-field Rustdoc, mirroring `openai.rs`.
#![allow(missing_docs)]

use std::collections::BTreeMap;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{stream, TryStreamExt};
use pi_protocol::{
    AssistantMessage, AssistantMessageEvent, Content, Context, Message, Model, Role, StopReason,
    TextContent, ToolCall, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::json_parse::parse_streaming_json;
use crate::stream::AssistantMessageEventStream;
use crate::types::{SimpleStreamOptions, StreamError};
use crate::StreamFn;

/// Default base URL for the OpenAI Responses API.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Smallest `max_output_tokens` the Responses endpoint accepts.
pub const MIN_OUTPUT_TOKENS: u32 = 16;

/// OpenAI Responses API provider.
///
/// Construct with [`OpenAiResponsesProvider::new`] for the production
/// endpoint, or [`with_base_url`](OpenAiResponsesProvider::with_base_url)
/// for an Azure deployment or a local gateway.
#[derive(Debug, Clone)]
pub struct OpenAiResponsesProvider {
    /// Bearer token sent in the `Authorization` header.
    pub api_key: String,
    /// Base URL with no trailing slash. Must include the `/v1` prefix.
    pub base_url: String,
}

impl OpenAiResponsesProvider {
    /// Create a provider pointing at the production OpenAI endpoint.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Create a provider pointing at a custom base URL (Azure, proxy, …).
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
        }
    }

    /// Build the request body for the `/responses` endpoint.
    ///
    /// `stream` only flips the `stream` flag; the caller decides whether
    /// to POST it through [`send_streaming`](Self::send_streaming) or
    /// [`send_once`](Self::send_once).
    pub fn build_request(
        model: &Model,
        ctx: &Context,
        options: &SimpleStreamOptions,
        stream: bool,
    ) -> Result<ResponsesRequest, StreamError> {
        let mut input: Vec<ResponsesInputItem> = Vec::with_capacity(ctx.messages.len() + 1);
        if !ctx.system_prompt.is_empty() {
            input.push(ResponsesInputItem::Message {
                role: "system".into(),
                content: ctx.system_prompt.clone(),
            });
        }
        for msg in &ctx.messages {
            append_message(&mut input, msg)?;
        }
        let tools = if ctx.tools.is_empty() {
            None
        } else {
            Some(
                ctx.tools
                    .iter()
                    .map(|t| ResponsesTool::Function {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: t.parameters.clone(),
                    })
                    .collect(),
            )
        };
        Ok(ResponsesRequest {
            model: model.id.clone(),
            input,
            tools,
            stream,
            store: false,
            temperature: options.temperature,
            max_output_tokens: options.max_tokens.map(|m| m.max(MIN_OUTPUT_TOKENS)),
        })
    }

    /// POST `/responses` with `stream: true` and pipe the SSE byte stream
    /// through [`parse_sse`].
    #[cfg(not(target_arch = "wasm32"))]
    async fn send_streaming(
        &self,
        body: &ResponsesRequest,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));
        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let hint = crate::retry::retry_hint_from_headers(response.headers());
            let body = response.text().await.unwrap_or_default();
            return Err(StreamError::provider_with_hint(
                status.as_u16(),
                truncate_body(&body),
                hint,
            ));
        }
        let model_id = body.model.clone();
        let byte_stream = response.bytes_stream();
        let mapped = byte_stream.map_err(StreamError::Transport);
        Ok(parse_sse(Box::pin(mapped), model_id))
    }

    /// POST `/responses` with `stream: false` and wrap the response into
    /// a single `Done` event.
    #[cfg(not(target_arch = "wasm32"))]
    async fn send_once(&self, body: &ResponsesRequest) -> Result<ResponsesResponse, StreamError> {
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));
        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let hint = crate::retry::retry_hint_from_headers(response.headers());
            let body = response.text().await.unwrap_or_default();
            return Err(StreamError::provider_with_hint(
                status.as_u16(),
                truncate_body(&body),
                hint,
            ));
        }
        Ok(response.json::<ResponsesResponse>().await?)
    }
}

#[async_trait]
impl StreamFn for OpenAiResponsesProvider {
    async fn stream_simple(
        &self,
        model: &Model,
        ctx: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        let body = Self::build_request(model, ctx, options, true)?;

        #[cfg(not(target_arch = "wasm32"))]
        {
            match self.send_streaming(&body).await {
                Ok(stream) => Ok(stream),
                // Some gateways reject `stream: true`. Fall back to a
                // one-shot request so callers still get a Done event.
                Err(StreamError::Provider { status: 400, .. }) => {
                    let model_id = body.model.clone();
                    let non_streaming = Self::build_request(model, ctx, options, false)?;
                    let resp = self.send_once(&non_streaming).await?;
                    let message = resp.into_assistant_message(&model_id)?;
                    Ok(Box::pin(stream::once(async move {
                        Ok(AssistantMessageEvent::Done {
                            content: message.content,
                            stop_reason: message.stop_reason,
                            usage: message.usage,
                        })
                    })))
                }
                Err(e) => Err(e),
            }
        }

        #[cfg(target_arch = "wasm32")]
        {
            let _ = body;
            Err(StreamError::Malformed(
                "OpenAiResponsesProvider is not yet implemented for wasm32-unknown-unknown".into(),
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
// Request: `pi_protocol` context -> Responses `input[]`
// ---------------------------------------------------------------------------

/// Append the items a single [`Message`] turns into.
///
/// A `Role::Assistant` message can hold both text and tool calls; upstream
/// emits them in block order, this port emits the text message first and
/// the `function_call` items after it. The model sees the same content
/// either way, and the order is stable across request rebuilds.
fn append_message(
    input: &mut Vec<ResponsesInputItem>,
    msg: &Message,
) -> Result<(), StreamError> {
    match msg.role {
        Role::System | Role::User => {
            let role = match msg.role {
                Role::System => "system",
                Role::User => "user",
                _ => unreachable!(),
            };
            input.push(ResponsesInputItem::Message {
                role: role.into(),
                content: join_text(msg),
            });
            Ok(())
        }
        Role::Assistant => {
            let text = join_text(msg);
            if !text.is_empty() {
                input.push(ResponsesInputItem::Message {
                    role: "assistant".into(),
                    content: text,
                });
            }
            for block in &msg.content {
                match block {
                    Content::ToolCall(call) => {
                        input.push(ResponsesInputItem::FunctionCall {
                            call_id: call.id.clone(),
                            name: call.name.clone(),
                            arguments: serde_json::to_string(&call.arguments)
                                .map_err(|e| StreamError::Malformed(e.to_string()))?,
                        });
                    }
                    Content::Text(_) => {}
                    _ => {
                        return Err(StreamError::Malformed(
                            "assistant message may only contain text or tool calls".into(),
                        ));
                    }
                }
            }
            Ok(())
        }
        Role::Tool => {
            let mut call_id: Option<String> = None;
            let mut output = String::new();
            for block in &msg.content {
                match block {
                    Content::ToolResult(result) => {
                        if call_id.is_none() {
                            call_id = Some(result.tool_call_id.clone());
                        }
                        match &*result.content {
                            Content::Text(t) => output.push_str(&t.text),
                            other => output
                                .push_str(&serde_json::to_string(other).unwrap_or_default()),
                        }
                    }
                    Content::Text(t) => output.push_str(&t.text),
                    _ => {}
                }
            }
            let call_id = call_id
                .ok_or_else(|| StreamError::Malformed("tool message missing tool result".into()))?;
            input.push(ResponsesInputItem::FunctionCallOutput {
                call_id,
                output: if output.is_empty() {
                    "(no tool output)".to_string()
                } else {
                    output
                },
            });
            Ok(())
        }
    }
}

/// Concatenate the bare text blocks of a message.
fn join_text(msg: &Message) -> String {
    msg.content
        .iter()
        .filter_map(|c| match c {
            Content::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

// ---------------------------------------------------------------------------
// Wire types — request
// ---------------------------------------------------------------------------

/// Responses request body.
#[derive(Debug, Clone, Serialize)]
pub struct ResponsesRequest {
    /// Model identifier (`gpt-5`, `o4-mini`, …).
    pub model: String,
    /// Conversation items, in order.
    pub input: Vec<ResponsesInputItem>,
    /// Flat tool descriptors. Skipped when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ResponsesTool>>,
    /// Whether the server should stream events.
    pub stream: bool,
    /// Always `false`: pi keeps conversation state client-side.
    pub store: bool,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Output-token cap (never below [`MIN_OUTPUT_TOKENS`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

/// One entry of the `input[]` array.
///
/// `type` is explicit on every variant: the endpoint infers `message`
/// from the presence of `role`, but being explicit keeps the request
/// self-describing and the round-trip unambiguous.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsesInputItem {
    /// A `system` / `developer` / `user` / `assistant` turn.
    Message {
        /// `system`, `developer`, `user` or `assistant`.
        role: String,
        /// Text content.
        content: String,
    },
    /// A tool invocation the model asked for, echoed back on later turns.
    FunctionCall {
        /// Provider call identifier, matched by the `function_call_output`.
        call_id: String,
        /// Tool name.
        name: String,
        /// JSON-encoded arguments string.
        arguments: String,
    },
    /// The result of a [`FunctionCall`](Self::FunctionCall).
    FunctionCallOutput {
        /// The `call_id` of the invocation being answered.
        call_id: String,
        /// Tool output as text.
        output: String,
    },
}

/// Flat tool descriptor (Responses does not nest it like Chat Completions).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsesTool {
    /// A function tool.
    Function {
        /// Tool name.
        name: String,
        /// Human-readable description.
        description: String,
        /// JSON Schema for the arguments object.
        parameters: Value,
    },
}

// ---------------------------------------------------------------------------
// Wire types — non-streaming response
// ---------------------------------------------------------------------------

/// Non-streaming `/responses` response body (only the fields pi consumes).
#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesResponse {
    /// Model that produced the response.
    #[serde(default)]
    pub model: String,
    /// `completed` | `incomplete` | `failed` | …
    #[serde(default)]
    pub status: Option<String>,
    /// Output items in order.
    #[serde(default)]
    pub output: Vec<ResponsesOutputItem>,
    /// Token usage.
    #[serde(default)]
    pub usage: Option<ResponsesUsage>,
    /// Present when `status == "incomplete"`.
    #[serde(default)]
    pub incomplete_details: Option<ResponsesIncompleteDetails>,
    /// Present when `status == "failed"`.
    #[serde(default)]
    pub error: Option<ResponsesErrorBody>,
}

/// One output item.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsesOutputItem {
    /// An assistant message.
    Message {
        /// Content parts.
        #[serde(default)]
        content: Vec<ResponsesOutputContent>,
    },
    /// A reasoning item (metadata only; not stored in `Content`).
    Reasoning {
        /// Summary parts.
        #[serde(default)]
        summary: Vec<ResponsesSummaryPart>,
    },
    /// A tool call.
    FunctionCall {
        /// `fc_…` item id.
        #[serde(default)]
        id: Option<String>,
        /// Provider call identifier.
        #[serde(default)]
        call_id: Option<String>,
        /// Tool name.
        #[serde(default)]
        name: Option<String>,
        /// JSON-encoded arguments.
        #[serde(default)]
        arguments: String,
    },
    /// Any item this port does not model (image, web search, …).
    #[serde(other)]
    Other,
}

/// One content part of an output message.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsesOutputContent {
    /// Assistant text.
    OutputText {
        /// The text.
        #[serde(default)]
        text: String,
    },
    /// A refusal.
    Refusal {
        /// The refusal text.
        #[serde(default)]
        refusal: String,
    },
    /// Unmodelled part.
    #[serde(other)]
    Other,
}

/// One reasoning-summary part.
#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesSummaryPart {
    /// Summary text.
    #[serde(default)]
    pub text: String,
}

/// Token usage as reported by the Responses endpoint.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ResponsesUsage {
    /// Prompt tokens.
    #[serde(default)]
    pub input_tokens: u32,
    /// Generated tokens (includes reasoning tokens).
    #[serde(default)]
    pub output_tokens: u32,
    /// Total tokens.
    #[serde(default)]
    pub total_tokens: u32,
    /// Breakdown details.
    #[serde(default)]
    pub input_tokens_details: Option<ResponsesInputTokensDetails>,
}

/// Cached-token breakdown of [`ResponsesUsage`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ResponsesInputTokensDetails {
    /// Input tokens served from the prompt cache.
    #[serde(default)]
    pub cached_tokens: u32,
}

/// Why a response was truncated.
#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesIncompleteDetails {
    /// e.g. `max_output_tokens`.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Failure body of a response (or of an `error` stream event).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ResponsesErrorBody {
    /// Error message.
    #[serde(default)]
    pub message: Option<String>,
}

impl ResponsesUsage {
    /// Fold the wire shape into the protocol [`Usage`].
    ///
    /// `cached_tokens` maps to [`Usage::cache_read`]; `total` is taken
    /// from the wire when present, otherwise derived.
    fn into_usage(self) -> Usage {
        let cache_read = self
            .input_tokens_details
            .map(|d| d.cached_tokens)
            .unwrap_or_default();
        let total = if self.total_tokens == 0 {
            self.input_tokens.saturating_add(self.output_tokens)
        } else {
            self.total_tokens
        };
        Usage {
            input: self.input_tokens,
            output: self.output_tokens,
            cache_read,
            total,
            ..Usage::default()
        }
    }
}

/// Map an `incomplete_details.reason` to a stop reason.
fn stop_reason_from_incomplete(reason: Option<&str>) -> StopReason {
    match reason {
        Some("max_output_tokens") => StopReason::MaxTokens,
        Some("content_filter") => StopReason::Error,
        _ => StopReason::Stop,
    }
}

impl ResponsesResponse {
    /// Convert into the final [`AssistantMessage`].
    fn into_assistant_message(self, fallback_model: &str) -> Result<AssistantMessage, StreamError> {
        if self.status.as_deref() == Some("failed") {
            let message = self
                .error
                .and_then(|e| e.message)
                .unwrap_or_else(|| "OpenAI returned a failed response".to_string());
            return Err(StreamError::Malformed(format!(
                "OpenAI Responses error: {message}"
            )));
        }
        let mut content = Vec::new();
        let mut saw_tool_call = false;
        for item in self.output {
            match item {
                ResponsesOutputItem::Message { content: parts } => {
                    let text: String = parts
                        .iter()
                        .map(|p| match p {
                            ResponsesOutputContent::OutputText { text } => text.clone(),
                            ResponsesOutputContent::Refusal { refusal } => refusal.clone(),
                            ResponsesOutputContent::Other => String::new(),
                        })
                        .collect();
                    if !text.is_empty() {
                        content.push(Content::Text(TextContent { text }));
                    }
                }
                ResponsesOutputItem::FunctionCall {
                    call_id,
                    name,
                    arguments,
                    ..
                } => {
                    saw_tool_call = true;
                    content.push(Content::ToolCall(ToolCall {
                        id: call_id.unwrap_or_default(),
                        name: name.unwrap_or_default(),
                        arguments: parse_arguments(&arguments),
                    }));
                }
                ResponsesOutputItem::Reasoning { .. } | ResponsesOutputItem::Other => {}
            }
        }
        let stop_reason = match self.status.as_deref() {
            Some("incomplete") => stop_reason_from_incomplete(
                self.incomplete_details.as_ref().and_then(|d| d.reason.as_deref()),
            ),
            _ if saw_tool_call => StopReason::ToolUse,
            _ => StopReason::Stop,
        };
        let usage = self.usage.map(ResponsesUsage::into_usage).unwrap_or_default();
        Ok(AssistantMessage {
            model: if self.model.is_empty() {
                fallback_model.to_string()
            } else {
                self.model
            },
            content,
            stop_reason,
            usage,
        })
    }
}

/// Parse an accumulated arguments string, tolerating partial or malformed JSON.
fn parse_arguments(arguments: &str) -> Value {
    parse_streaming_json(Some(arguments))
}

// ---------------------------------------------------------------------------
// SSE parser
// ---------------------------------------------------------------------------

/// Parse an OpenAI Responses SSE byte stream into an
/// [`AssistantMessageEventStream`].
///
/// Each frame is one JSON object carrying its own `type`:
///
/// ```text
/// event: response.output_text.delta
/// data: {"type":"response.output_text.delta","output_index":0,"delta":"hi"}
/// ```
///
/// Lines that don't start with `data:` are ignored, as is the
/// `data: [DONE]` sentinel. Malformed JSON surfaces as
/// [`StreamError::Malformed`].
pub fn parse_sse(
    bytes: impl futures::Stream<Item = Result<Bytes, StreamError>> + Send + 'static,
    model_id: String,
) -> AssistantMessageEventStream {
    Box::pin(ResponsesSseStream::new(bytes, model_id))
}

/// In-flight tool call assembled from streaming deltas.
#[derive(Debug, Clone, Default)]
struct PendingToolCall {
    call_id: Option<String>,
    name: Option<String>,
    /// Arguments fragment, accumulated verbatim.
    arguments: String,
}

/// Accumulated output, keyed by `output_index` so the final `Done`
/// content can be materialised in provider order regardless of arrival
/// order. Text and tool calls live in separate maps because one
/// `output_index` names exactly one output item: a text delta that
/// arrives for an index already holding a tool call (a protocol
/// violation) then costs nothing and drops neither block.
#[derive(Default)]
struct ParserState {
    /// Text blocks by `output_index`.
    text_blocks: BTreeMap<u32, String>,
    /// In-flight tool calls by `output_index`.
    tool_calls: BTreeMap<u32, PendingToolCall>,
    /// Indices seen as reasoning items: streamed as deltas, not stored
    /// (no `Content::Thinking` variant in the protocol yet).
    thinking: std::collections::BTreeSet<u32>,
    /// Token usage from the terminal event.
    usage: Usage,
    /// Stop reason set by the terminal event.
    stop_reason: Option<StopReason>,
}

impl ParserState {
    fn text_slot(&mut self, index: u32) -> &mut String {
        self.text_blocks.entry(index).or_default()
    }

    fn tool_slot(&mut self, index: u32) -> &mut PendingToolCall {
        self.tool_calls.entry(index).or_default()
    }

    fn ensure_thinking(&mut self, index: u32) {
        self.thinking.insert(index);
    }

    /// Materialise the accumulated blocks into `Done` content.
    fn content(&self) -> Vec<Content> {
        let mut content = Vec::with_capacity(self.text_blocks.len() + self.tool_calls.len());
        let mut indices: Vec<u32> = self
            .text_blocks
            .keys()
            .chain(self.thinking.iter())
            .chain(self.tool_calls.keys())
            .copied()
            .collect();
        indices.sort_unstable();
        indices.dedup();
        for index in indices {
            if let Some(text) = self.text_blocks.get(&index) {
                if !text.is_empty() {
                    content.push(Content::Text(TextContent { text: text.clone() }));
                }
            }
            if let Some(call) = self.tool_calls.get(&index) {
                content.push(Content::ToolCall(ToolCall {
                    id: call.call_id.clone().unwrap_or_default(),
                    name: call.name.clone().unwrap_or_default(),
                    arguments: parse_arguments(&call.arguments),
                }));
            }
        }
        content
    }

    fn has_tool_call(&self) -> bool {
        !self.tool_calls.is_empty()
    }
}

struct ResponsesSseStream {
    inner: std::pin::Pin<Box<dyn futures::Stream<Item = Result<Bytes, StreamError>> + Send>>,
    /// Half-parsed SSE buffer (bytes from the most recent chunk).
    line_buffer: Vec<u8>,
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

impl ResponsesSseStream {
    fn new(
        bytes: impl futures::Stream<Item = Result<Bytes, StreamError>> + Send + 'static,
        model_id: String,
    ) -> Self {
        Self {
            inner: Box::pin(bytes),
            line_buffer: Vec::new(),
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
            if !self.line_buffer.is_empty() {
                self.line_buffer.remove(0);
            }
            self.process_line(&raw)?;
        }
        Ok(())
    }

    fn process_line(&mut self, raw: &[u8]) -> Result<(), StreamError> {
        let line = if raw.starts_with(b"\xEF\xBB\xBF") {
            &raw[3..]
        } else {
            raw
        };
        if line.is_empty() {
            return self.dispatch_event();
        }
        if line.first() == Some(&b':') {
            return Ok(());
        }
        let (field, value) = match split_field(line) {
            Some(parts) => parts,
            None => return Ok(()),
        };
        if field == b"data" {
            self.data_lines.push(String::from_utf8_lossy(value).into_owned());
        }
        // `event:` / `id:` / `retry:` carry no information we need: the
        // JSON payload's `type` field is authoritative.
        Ok(())
    }

    fn dispatch_event(&mut self) -> Result<(), StreamError> {
        if self.data_lines.is_empty() {
            return Ok(());
        }
        let payload = self.data_lines.join("\n");
        self.data_lines.clear();
        if payload.trim() == "[DONE]" {
            return Ok(());
        }
        self.handle_event(&payload)
    }

    fn handle_event(&mut self, data: &str) -> Result<(), StreamError> {
        let event: StreamEvent = serde_json::from_str(data)
            .map_err(|e| StreamError::Malformed(format!("Responses SSE JSON: {e}: {data}")))?;
        // Copy the discriminator out so the arms below can move `event`.
        let kind = event.kind.clone();

        if !self.started {
            self.started = true;
            self.pending.push_back(Ok(AssistantMessageEvent::Start {
                model: self.model_id.clone(),
            }));
        }

        match kind.as_str() {
            "response.created" | "response.in_progress" | "response.queued" => {}
            "response.output_item.added" => self.handle_item_added(event),
            "response.output_item.done" => self.handle_item_done(event)?,
            "response.output_text.delta" => {
                if let Some(delta) = event.delta.as_deref().filter(|d| !d.is_empty()) {
                    let index = event.output_index.unwrap_or_default();
                    self.state.text_slot(index).push_str(delta);
                    self.pending.push_back(Ok(AssistantMessageEvent::TextDelta {
                        delta: delta.to_string(),
                    }));
                }
            }
            // A refusal is assistant-visible text; the protocol has no
            // separate variant for it.
            "response.refusal.delta" => {
                if let Some(delta) = event.delta.as_deref().filter(|d| !d.is_empty()) {
                    let index = event.output_index.unwrap_or_default();
                    self.state.text_slot(index).push_str(delta);
                    self.pending.push_back(Ok(AssistantMessageEvent::TextDelta {
                        delta: delta.to_string(),
                    }));
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(delta) = event.delta.as_deref().filter(|d| !d.is_empty()) {
                    self.state.ensure_thinking(event.output_index.unwrap_or_default());
                    self.pending
                        .push_back(Ok(AssistantMessageEvent::ThinkingDelta {
                            delta: delta.to_string(),
                        }));
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(delta) = event.delta.as_deref().filter(|d| !d.is_empty()) {
                    let index = event.output_index.unwrap_or_default();
                    self.state.tool_slot(index).arguments.push_str(delta);
                    self.pending
                        .push_back(Ok(AssistantMessageEvent::ToolCallDelta {
                            index,
                            id: None,
                            name: None,
                            arguments_delta: Some(delta.to_string()),
                        }));
                }
            }
            // The `…arguments.done` event repeats the full argument
            // string; treat it as authoritative so a server that only
            // sends `done` (no deltas) still yields complete arguments.
            "response.function_call_arguments.done" => {
                if let Some(arguments) = event.arguments.as_deref() {
                    let index = event.output_index.unwrap_or_default();
                    self.state.tool_slot(index).arguments = arguments.to_string();
                }
            }
            "response.completed" => self.handle_terminal(event, false),
            "response.incomplete" => self.handle_terminal(event, true),
            "response.failed" => {
                let message = event
                    .response
                    .as_ref()
                    .and_then(|r| r.error.as_ref())
                    .and_then(|e| e.message.clone())
                    .or_else(|| event.message.clone())
                    .unwrap_or_else(|| "OpenAI returned a failed response".to_string());
                self.pending.push_back(Err(StreamError::Malformed(format!(
                    "OpenAI Responses error: {message}"
                ))));
                // Terminal: do not synthesise a `Done` after the failure.
                self.finished = true;
            }
            "error" => {
                let message = event
                    .message
                    .clone()
                    .or_else(|| event.error.as_ref().and_then(|e| e.message.clone()))
                    .unwrap_or_else(|| "OpenAI returned an error event".to_string());
                self.pending.push_back(Err(StreamError::Malformed(format!(
                    "OpenAI Responses error: {message}"
                ))));
                self.finished = true;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_item_added(&mut self, event: StreamEvent) {
        let index = event.output_index.unwrap_or_default();
        let Some(item) = event.item else {
            return;
        };
        match item.kind.as_str() {
            "function_call" => {
                let arguments = item.arguments.unwrap_or_default();
                let slot = self.state.tool_slot(index);
                if let Some(id) = item.call_id.as_ref().or(item.id.as_ref()) {
                    slot.call_id = Some(id.clone());
                }
                if let Some(name) = item.name.as_ref() {
                    slot.name = Some(name.clone());
                }
                if !arguments.is_empty() {
                    slot.arguments.push_str(&arguments);
                }
                self.pending
                    .push_back(Ok(AssistantMessageEvent::ToolCallDelta {
                        index,
                        id: slot.call_id.clone(),
                        name: slot.name.clone(),
                        arguments_delta: if arguments.is_empty() {
                            None
                        } else {
                            Some(arguments)
                        },
                    }));
            }
            "message" => {
                let _ = self.state.text_slot(index);
            }
            "reasoning" => self.state.ensure_thinking(index),
            _ => {}
        }
    }

    fn handle_item_done(&mut self, event: StreamEvent) -> Result<(), StreamError> {
        let index = event.output_index.unwrap_or_default();
        let Some(item) = event.item else {
            return Ok(());
        };
        match item.kind.as_str() {
            "function_call" => {
                let slot = self.state.tool_slot(index);
                if let Some(id) = item.call_id.as_ref().or(item.id.as_ref()) {
                    slot.call_id = Some(id.clone());
                }
                if let Some(name) = item.name.as_ref() {
                    slot.name = Some(name.clone());
                }
                if let Some(arguments) = item.arguments.as_ref() {
                    slot.arguments = arguments.clone();
                }
            }
            "message" => {
                let text: String = item
                    .content
                    .unwrap_or_default()
                    .iter()
                    .map(|part| match part {
                        ResponsesOutputContent::OutputText { text } => text.clone(),
                        ResponsesOutputContent::Refusal { refusal } => refusal.clone(),
                        ResponsesOutputContent::Other => String::new(),
                    })
                    .collect();
                if !text.is_empty() {
                    let slot = self.state.text_slot(index);
                    slot.clear();
                    slot.push_str(&text);
                }
            }
            "reasoning" => self.state.ensure_thinking(index),
            _ => {}
        }
        Ok(())
    }

    fn handle_terminal(&mut self, event: StreamEvent, incomplete: bool) {
        let response = event.response;
        if let Some(usage) = response.as_ref().and_then(|r| r.usage.clone()) {
            self.state.usage = usage.into_usage();
        }
        let reason = response
            .as_ref()
            .and_then(|r| r.incomplete_details.as_ref())
            .and_then(|d| d.reason.as_deref());
        self.state.stop_reason = Some(if incomplete {
            stop_reason_from_incomplete(reason)
        } else if self.state.has_tool_call() {
            StopReason::ToolUse
        } else {
            StopReason::Stop
        });
    }

    fn finalize(&mut self) {
        let stop_reason = self.state.stop_reason.unwrap_or(
            if self.state.has_tool_call() {
                StopReason::ToolUse
            } else {
                StopReason::Stop
            },
        );
        let content = self.state.content();
        let usage = self.state.usage;
        self.pending.push_back(Ok(AssistantMessageEvent::Done {
            content,
            stop_reason,
            usage,
        }));
    }
}

impl futures::Stream for ResponsesSseStream {
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

/// One SSE frame, flattened: the fields used by every event type live on
/// one struct so dispatch stays a single `match` on `kind` instead of a
/// per-event deserializer.
#[derive(Debug, Default, Deserialize)]
struct StreamEvent {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    output_index: Option<u32>,
    #[serde(default)]
    delta: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    error: Option<ResponsesErrorBody>,
    #[serde(default)]
    item: Option<StreamItem>,
    #[serde(default)]
    response: Option<StreamResponse>,
}

/// The `item` object of `response.output_item.added` / `.done`.
#[derive(Debug, Default, Deserialize)]
struct StreamItem {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
    #[serde(default)]
    content: Option<Vec<ResponsesOutputContent>>,
}

/// The `response` object of the terminal events.
#[derive(Debug, Default, Deserialize)]
struct StreamResponse {
    #[serde(default)]
    usage: Option<ResponsesUsage>,
    #[serde(default)]
    incomplete_details: Option<ResponsesIncompleteDetails>,
    #[serde(default)]
    error: Option<ResponsesErrorBody>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{stream, StreamExt};
    use pi_protocol::{Api, ProviderId, ToolDefinition};
    use serde_json::json;

    fn model() -> Model {
        Model {
            provider: ProviderId::new("openai-responses"),
            id: "gpt-5".into(),
            api: Api::OpenAiResponses,
            label: None,
            context_window: 400_000,
            max_output_tokens: 128_000,
        }
    }

    fn ctx() -> Context {
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
                "properties": {"city": {"type": "string"}},
                "required": ["city"],
            }),
            metadata: None,
        });
        ctx
    }

    fn replay(fixture: &str) -> Vec<Result<AssistantMessageEvent, StreamError>> {
        let bytes = fixture.as_bytes().to_vec();
        let chunks: Vec<Result<Bytes, StreamError>> = bytes
            .chunks(7)
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        let s = parse_sse(Box::pin(stream::iter(chunks)), "gpt-5".into());
        futures::executor::block_on(s.collect::<Vec<_>>())
    }

    fn done(events: &[Result<AssistantMessageEvent, StreamError>]) -> (Vec<Content>, StopReason, Usage) {
        match events.last() {
            Some(Ok(AssistantMessageEvent::Done {
                content,
                stop_reason,
                usage,
            })) => (content.clone(), *stop_reason, *usage),
            other => panic!("expected trailing Done, got {other:?}"),
        }
    }

    #[test]
    fn request_uses_the_responses_wire_shape() {
        let req =
            OpenAiResponsesProvider::build_request(&model(), &ctx(), &SimpleStreamOptions::default(), true)
                .expect("build request");
        let v = serde_json::to_value(&req).expect("serialize");
        assert_eq!(v["model"], "gpt-5");
        assert_eq!(v["stream"], true);
        assert_eq!(v["store"], false);
        // System prompt is a typed system item, not a `messages` entry.
        assert_eq!(v["input"][0]["type"], "message");
        assert_eq!(v["input"][0]["role"], "system");
        assert_eq!(v["input"][0]["content"], "you are pi");
        assert_eq!(v["input"][1]["role"], "user");
        // Tools are flat (no nested `function` object like Chat Completions).
        assert_eq!(v["tools"][0]["type"], "function");
        assert_eq!(v["tools"][0]["name"], "get_weather");
        assert!(v["tools"][0].get("function").is_none());
        assert!(v.get("messages").is_none());
    }

    #[test]
    fn max_output_tokens_is_clamped_to_the_endpoint_minimum() {
        let options = SimpleStreamOptions {
            max_tokens: Some(4),
            ..Default::default()
        };
        let req = OpenAiResponsesProvider::build_request(&model(), &ctx(), &options, false)
            .expect("build request");
        assert_eq!(req.max_output_tokens, Some(MIN_OUTPUT_TOKENS));
        // A larger value passes through untouched.
        let options = SimpleStreamOptions {
            max_tokens: Some(1024),
            ..Default::default()
        };
        let req = OpenAiResponsesProvider::build_request(&model(), &ctx(), &options, false)
            .expect("build request");
        assert_eq!(req.max_output_tokens, Some(1024));
    }

    #[test]
    fn tool_history_round_trips_as_function_call_items() {
        let mut ctx = ctx();
        ctx.messages.push(Message {
            role: Role::Assistant,
            content: vec![
                Content::text("checking"),
                Content::ToolCall(ToolCall {
                    id: "call_abc".into(),
                    name: "get_weather".into(),
                    arguments: json!({"city": "SF"}),
                }),
            ],
            model: None,
        });
        ctx.messages.push(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(pi_protocol::ToolResult {
                tool_call_id: "call_abc".into(),
                content: Box::new(Content::text("72F and sunny")),
                is_error: false,
                details: None,
            })],
            model: None,
        });
        let req =
            OpenAiResponsesProvider::build_request(&model(), &ctx, &SimpleStreamOptions::default(), true)
                .expect("build request");
        let v = serde_json::to_value(&req).expect("serialize");
        let items = v["input"].as_array().expect("input array");
        let call = items
            .iter()
            .find(|i| i["type"] == "function_call")
            .expect("a function_call item");
        assert_eq!(call["call_id"], "call_abc");
        assert_eq!(call["name"], "get_weather");
        assert_eq!(call["arguments"], "{\"city\":\"SF\"}");
        let result = items
            .iter()
            .find(|i| i["type"] == "function_call_output")
            .expect("a function_call_output item");
        assert_eq!(result["call_id"], "call_abc");
        assert_eq!(
            result["output"], "72F and sunny",
            "the tool output must be serialized, not dropped"
        );
        // The assistant text still travels as its own message item.
        assert!(items
            .iter()
            .any(|i| i["type"] == "message" && i["role"] == "assistant" && i["content"] == "checking"));
    }

    #[test]
    fn text_stream_emits_start_text_done() {
        let fixture = "\
event: response.created
data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5\"}}

event: response.output_item.added
data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"msg_1\",\"role\":\"assistant\",\"content\":[]}}

event: response.output_text.delta
data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"Hello\"}

event: response.output_text.delta
data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\" there\"}

event: response.completed
data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":12,\"output_tokens\":3,\"total_tokens\":15,\"input_tokens_details\":{\"cached_tokens\":4}}}}

";
        let events = replay(fixture);
        assert_eq!(
            events[0].as_ref().unwrap(),
            &AssistantMessageEvent::Start {
                model: "gpt-5".into()
            }
        );
        let deltas: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                Ok(AssistantMessageEvent::TextDelta { delta }) => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["Hello", " there"]);
        let (content, stop_reason, usage) = done(&events);
        assert_eq!(content, vec![Content::text("Hello there")]);
        assert_eq!(stop_reason, StopReason::Stop);
        assert_eq!(usage.input, 12);
        assert_eq!(usage.output, 3);
        assert_eq!(usage.cache_read, 4);
        assert_eq!(usage.total, 15);
    }

    #[test]
    fn tool_call_stream_assembles_arguments_from_deltas() {
        let fixture = "\
data: {\"type\":\"response.created\",\"response\":{\"model\":\"gpt-5\"}}

data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"get_weather\",\"arguments\":\"\"}}

data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"item_id\":\"fc_1\",\"delta\":\"{\\\"city\\\"\"}

data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"item_id\":\"fc_1\",\"delta\":\":\\\"SF\\\"}\"}

data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"SF\\\"}\"}}

data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":20,\"output_tokens\":9,\"total_tokens\":29}}}

";
        let events = replay(fixture);
        let first_tool_delta = events
            .iter()
            .find_map(|e| match e {
                Ok(AssistantMessageEvent::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments_delta,
                }) => Some((*index, id.clone(), name.clone(), arguments_delta.clone())),
                _ => None,
            })
            .expect("a tool call delta");
        assert_eq!(
            first_tool_delta,
            (0, Some("call_1".into()), Some("get_weather".into()), None)
        );
        let (content, stop_reason, usage) = done(&events);
        assert_eq!(
            content,
            vec![Content::ToolCall(ToolCall {
                id: "call_1".into(),
                name: "get_weather".into(),
                arguments: json!({"city": "SF"}),
            })]
        );
        assert_eq!(
            stop_reason,
            StopReason::ToolUse,
            "a stream carrying tool calls must not report a plain stop"
        );
        assert_eq!(usage.total, 29);
    }

    #[test]
    fn reasoning_deltas_stream_but_are_not_stored() {
        let fixture = "\
data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\",\"summary\":[]}}

data: {\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":0,\"delta\":\"thinking…\"}

data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"message\",\"role\":\"assistant\"}}

data: {\"type\":\"response.output_text.delta\",\"output_index\":1,\"delta\":\"42\"}

data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2,\"total_tokens\":7}}}

";
        let events = replay(fixture);
        assert!(events.iter().any(|e| matches!(
            e,
            Ok(AssistantMessageEvent::ThinkingDelta { delta }) if delta == "thinking…"
        )));
        let (content, stop_reason, _) = done(&events);
        assert_eq!(content, vec![Content::text("42")]);
        assert_eq!(stop_reason, StopReason::Stop);
    }

    #[test]
    fn incomplete_response_maps_max_output_tokens() {
        let fixture = "\
data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"partial\"}

data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"},\"usage\":{\"input_tokens\":1,\"output_tokens\":9,\"total_tokens\":10}}}

";
        let events = replay(fixture);
        let (content, stop_reason, _) = done(&events);
        assert_eq!(content, vec![Content::text("partial")]);
        assert_eq!(stop_reason, StopReason::MaxTokens);
    }

    #[test]
    fn failed_response_surfaces_the_provider_message() {
        let fixture = "\
data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"server_error\",\"message\":\"upstream exploded\"}}}

";
        let events = replay(fixture);
        let last = events.last().expect("at least one event").as_ref();
        let message = last.expect_err("failure must be an error").to_string();
        assert!(message.contains("upstream exploded"), "{message}");
    }

    #[test]
    fn error_event_surfaces_its_message() {
        let fixture = "\
data: {\"type\":\"error\",\"code\":\"rate_limit_exceeded\",\"message\":\"slow down\"}

";
        let events = replay(fixture);
        let message = events
            .last()
            .expect("at least one event")
            .as_ref()
            .expect_err("error event must be an error")
            .to_string();
        assert!(message.contains("slow down"), "{message}");
    }

    #[test]
    fn done_sentinel_does_not_end_the_stream_early() {
        // Azure deployments append `data: [DONE]`; the `response.completed`
        // event is what carries usage and must still be consumed.
        let fixture = "\
data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"hi\"}

data: [DONE]

data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}

";
        let events = replay(fixture);
        let (content, _, usage) = done(&events);
        assert_eq!(content, vec![Content::text("hi")]);
        assert_eq!(usage.total, 2);
    }

    #[test]
    fn malformed_json_is_reported_as_malformed() {
        let fixture = "data: {not json}\n\n";
        let events = replay(fixture);
        let message = events
            .last()
            .expect("at least one event")
            .as_ref()
            .expect_err("malformed frame must fail")
            .to_string();
        assert!(message.contains("Responses SSE JSON"), "{message}");
    }

    #[test]
    fn provider_url_and_object_safety() {
        let provider = OpenAiResponsesProvider::new("test-key");
        assert_eq!(provider.base_url, DEFAULT_BASE_URL);
        assert_eq!(provider.api_key, "test-key");
        let custom = OpenAiResponsesProvider::with_base_url("k", "http://localhost:8080/v1/");
        assert_eq!(custom.base_url, "http://localhost:8080/v1/");
        let _shared: std::sync::Arc<dyn StreamFn> = std::sync::Arc::new(provider);
    }
}
