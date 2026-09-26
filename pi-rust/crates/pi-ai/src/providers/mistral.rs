//! Mistral native Chat Completions provider.
//!
//! Ports `packages/ai/src/api/mistral-conversations.ts`: `POST
//! {base_url}/v1/chat/completions` with `stream: true` and `data: {...}`
//! Server-Sent Events. This adapter parses the byte stream into
//! [`AssistantMessageEvent`]s and accumulates the final content into a
//! single trailing [`Done`](AssistantMessageEvent::Done).
//!
//! # Event mapping
//!
//! | Mistral wire shape | Rust event |
//! | --- | --- |
//! | first chunk | [`AssistantMessageEvent::Start`] |
//! | `delta.content` string / `{type:"text"}` | [`AssistantMessageEvent::TextDelta`] |
//! | `delta.content[].type = "thinking"` | [`AssistantMessageEvent::ThinkingDelta`] |
//! | `delta.tool_calls[]` | [`AssistantMessageEvent::ToolCallDelta`] |
//! | `data: [DONE]` / EOF | [`AssistantMessageEvent::Done`] |
//! | `finish_reason` = `error` or unknown | `Error` event + `Done { stop_reason: Error }` |
//!
//! # Deliberate differences from the TypeScript port
//!
//! * **Tool-call ids are normalized to 9 alphanumeric characters** exactly
//!   like upstream ([`derive_mistral_tool_call_id`], including the
//!   `shortHash` fallback), but the normalizer runs while *encoding* the
//!   request too: upstream applies it inside `transformMessages`, and the
//!   Rust port has no `transform_messages` yet, so this adapter keeps a
//!   single normalizer per request so replayed assistant tool calls and
//!   their tool results still agree.
//! * **Thinking is streamed, not stored.** `pi-protocol::Content` has no
//!   `Thinking` variant yet (same trade-off as the Anthropic / Google
//!   adapters), so `thinking` deltas surface as events only and are never
//!   replayed on a subsequent request.
//! * **No reasoning controls.** Upstream's `streamSimple` derives
//!   `prompt_mode` / `reasoning_effort` from `model.reasoning` and
//!   `thinkingLevelMap`; the Rust [`Model`] descriptor carries neither, so
//!   neither field is ever sent.
//! * **No `tool_choice`.** [`SimpleStreamOptions`] has no `toolChoice`
//!   field, so the payload never carries one.
//! * **No prompt-caching affinity.** `prompt_cache_key` and the
//!   `x-affinity` header both need `options.sessionId` / `cacheRetention`,
//!   which the Rust options type does not model yet.
//! * **Images are always inlined.** Upstream drops image chunks for models
//!   whose `input` does not include `"image"`; the Rust [`Model`] has no
//!   modality list, so every image block is sent as a `data:` URL.
//! * **`strict` is always `false`.** Upstream resolves it from the tool's
//!   `constrainedSampling` hint; Rust [`pi_protocol::ToolDefinition`] has no
//!   such field, so the JSON-schema-strict sampling branch is inert.
//! * **No non-streaming fallback.** Unlike the OpenAI adapter this one has
//!   none, matching upstream Mistral (which always streams).
//! * **`x-affinity` / `responseId` / `rawStopReason` / `timestamp` /
//!   per-message cost** are not carried: the Rust protocol types have no
//!   slot for them (`Usage` has no `cost`, `AssistantMessage` has no
//!   `responseId`).
//!
//! native targets use `reqwest`; the `wasm32-unknown-unknown` target has no
//! usable HTTP client and returns [`StreamError::Malformed`] from every
//! call. [`parse_sse`] itself is target-agnostic and is what the fixture
//! tests exercise.

// Wire-format structs (the `Mistral*` types below) are documented against
// the upstream API reference inline rather than via Rustdoc on every field.
#![allow(missing_docs)]

use std::collections::HashMap;

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use pi_protocol::{
    AssistantMessageEvent, Content, Context, Message, Model, Role, StopReason, TextContent,
    ToolCall, ToolDefinition, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::json_parse::parse_streaming_json;
use crate::stream::AssistantMessageEventStream;
use crate::types::{SimpleStreamOptions, StreamError};
#[cfg(not(target_arch = "wasm32"))]
use crate::utils::error_body::truncate_provider_error_body;
use crate::StreamFn;

/// Default base URL. The `/v1/chat/completions` path is appended by
/// [`MistralProvider::build_url`] — mirroring upstream, where `baseUrl`
/// is the bare host and the adapter appends the version prefix.
pub const DEFAULT_BASE_URL: &str = "https://api.mistral.ai";

/// Mistral ids must be 9 alphanumeric characters.
pub const MISTRAL_TOOL_CALL_ID_LENGTH: usize = 9;

/// Mistral native Chat Completions provider.
#[derive(Debug, Clone)]
pub struct MistralProvider {
    /// Bearer token sent in the `Authorization` header.
    pub api_key: String,
    /// Base URL with no trailing slash (no `/v1` suffix).
    pub base_url: String,
}

impl MistralProvider {
    /// Create a provider pointing at the production Mistral endpoint.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Create a provider pointing at a custom base URL (proxy, mirror, …).
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
        }
    }

    /// `{base_url}/v1/chat/completions`, trailing slashes collapsed —
    /// the same URL upstream's `requestMistralStream` builds.
    pub fn build_url(&self) -> String {
        format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        )
    }

    /// Build the streaming request body for a context.
    pub fn build_request(
        model: &Model,
        ctx: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<MistralChatRequest, StreamError> {
        let mut normalizer = MistralToolCallIdNormalizer::new();
        let mut messages = to_chat_messages(&ctx.messages, &mut normalizer);
        if !ctx.system_prompt.is_empty() {
            messages.insert(
                0,
                MistralChatMessage {
                    role: "system".into(),
                    content: Some(MistralContent::Text(ctx.system_prompt.clone())),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    prefix: None,
                },
            );
        }
        let tools = if ctx.tools.is_empty() {
            None
        } else {
            Some(ctx.tools.iter().map(to_function_tool).collect())
        };
        Ok(MistralChatRequest {
            model: model.id.clone(),
            stream: true,
            messages,
            tools,
            temperature: options.temperature,
            max_tokens: options.max_tokens,
        })
    }

    /// POST the streaming endpoint and pipe the SSE bytes through
    /// [`parse_sse`].
    #[cfg(not(target_arch = "wasm32"))]
    async fn send_streaming(
        &self,
        body: &MistralChatRequest,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        let client = reqwest::Client::new();
        let response = client
            .post(self.build_url())
            .bearer_auth(&self.api_key)
            .header("accept", "text/event-stream")
            .json(body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let hint = crate::retry::retry_hint_from_headers(response.headers());
            let body = response.text().await.unwrap_or_default();
            return Err(StreamError::provider_with_hint(
                status.as_u16(),
                truncate_provider_error_body(&body),
                hint,
            ));
        }
        let model_id = body.model.clone();
        let byte_stream = response.bytes_stream();
        let mapped = futures::TryStreamExt::map_err(byte_stream, StreamError::Transport);
        Ok(parse_sse(Box::pin(mapped), model_id))
    }
}

#[async_trait]
impl StreamFn for MistralProvider {
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
                "mistral streaming is unavailable on wasm32".into(),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Request payload
// ---------------------------------------------------------------------------

/// Streaming request body for `POST /v1/chat/completions`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MistralChatRequest {
    pub model: String,
    pub stream: bool,
    pub messages: Vec<MistralChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<MistralFunctionTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Upstream spells this `maxTokens` and remaps it to `max_tokens`.
    #[serde(rename = "max_tokens", skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

/// One outbound message. `content` is a bare string when the message has
/// no image, and a chunk array otherwise — the same choice upstream makes.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MistralChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<MistralContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<MistralRequestToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<bool>,
}

/// Message content: text-only messages go as a string, mixed content as
/// an array of chunks (`string | MistralContentChunk[]`).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(untagged)]
pub enum MistralContent {
    Text(String),
    Chunks(Vec<MistralContentChunk>),
}

/// One inbound content chunk.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MistralContentChunk {
    Text {
        text: String,
    },
    /// Upstream spells the field `imageUrl` and remaps it to `image_url`.
    ImageUrl {
        image_url: String,
    },
}

/// A replayed assistant tool call.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MistralRequestToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: MistralRequestToolCallFunction,
    pub index: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MistralRequestToolCallFunction {
    pub name: String,
    /// JSON-encoded arguments object, as a string (Mistral's wire shape).
    pub arguments: String,
}

/// A function tool declaration. `strict` comes from upstream's
/// `resolveJsonSchemaStrictSampling`, which is always `undefined` in the
/// Rust port (no `constrainedSampling` hint on tools) → serialized as
/// `false`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MistralFunctionTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: MistralFunctionToolSpec,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MistralFunctionToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub strict: bool,
}

fn to_function_tool(tool: &ToolDefinition) -> MistralFunctionTool {
    MistralFunctionTool {
        kind: "function".into(),
        function: MistralFunctionToolSpec {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.parameters.clone(),
            strict: false,
        },
    }
}

/// Convert protocol messages into Mistral wire messages.
///
/// `normalizer` is applied to every tool-call id so an assistant call and
/// the tool result that answers it keep matching after Mistral's 9-char
/// constraint is enforced (upstream does this in `transformMessages`).
fn to_chat_messages(
    messages: &[Message],
    normalizer: &mut MistralToolCallIdNormalizer,
) -> Vec<MistralChatMessage> {
    // Mistral sends the tool name alongside every tool result. Rust tool
    // results carry only the id, so look the name up from the assistant
    // tool call that produced it.
    let mut tool_names: HashMap<&str, &str> = HashMap::new();
    for msg in messages {
        for block in &msg.content {
            if let Content::ToolCall(call) = block {
                tool_names.insert(call.id.as_str(), call.name.as_str());
            }
        }
    }

    let mut result = Vec::with_capacity(messages.len());
    for msg in messages {
        match msg.role {
            Role::User | Role::System => {
                if let Some(message) = user_message(msg) {
                    result.push(message);
                }
            }
            Role::Assistant => {
                if let Some(message) = assistant_message(msg, normalizer) {
                    result.push(message);
                }
            }
            Role::Tool => {
                if let Some(message) = tool_message(msg, normalizer, &tool_names) {
                    result.push(message);
                }
            }
        }
    }
    result
}

fn user_message(msg: &Message) -> Option<MistralChatMessage> {
    // A text-only message goes as a bare string (upstream sends the raw
    // string for the common case); mixed content becomes a chunk array.
    let mut text = String::new();
    let mut chunks: Vec<MistralContentChunk> = Vec::new();
    for block in &msg.content {
        match block {
            Content::Text(t) => {
                if t.text.is_empty() {
                    continue;
                }
                text.push_str(&t.text);
                chunks.push(MistralContentChunk::Text {
                    text: t.text.clone(),
                });
            }
            Content::Image(image) => chunks.push(MistralContentChunk::ImageUrl {
                image_url: format!("data:{};base64,{}", image.mime_type, image.data),
            }),
            _ => {}
        }
    }
    let has_image = chunks
        .iter()
        .any(|chunk| matches!(chunk, MistralContentChunk::ImageUrl { .. }));
    if !has_image {
        if text.is_empty() {
            return None;
        }
        return Some(MistralChatMessage {
            role: "user".into(),
            content: Some(MistralContent::Text(text)),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            prefix: None,
        });
    }
    Some(MistralChatMessage {
        role: "user".into(),
        content: Some(MistralContent::Chunks(chunks)),
        tool_calls: None,
        tool_call_id: None,
        name: None,
        prefix: None,
    })
}

fn assistant_message(
    msg: &Message,
    normalizer: &mut MistralToolCallIdNormalizer,
) -> Option<MistralChatMessage> {
    let text = msg
        .content
        .iter()
        .filter_map(|c| match c {
            Content::Text(t) if !t.text.trim().is_empty() => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    let tool_calls = msg
        .content
        .iter()
        .filter_map(|c| match c {
            Content::ToolCall(call) => Some(MistralRequestToolCall {
                id: normalizer.normalize(&call.id),
                kind: "function".into(),
                function: MistralRequestToolCallFunction {
                    name: call.name.clone(),
                    arguments: serde_json::to_string(&call.arguments)
                        .unwrap_or_else(|_| "{}".into()),
                },
                index: 0,
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    if text.is_empty() && tool_calls.is_empty() {
        return None;
    }
    Some(MistralChatMessage {
        role: "assistant".into(),
        content: if text.is_empty() {
            None
        } else {
            Some(MistralContent::Text(text))
        },
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        tool_call_id: None,
        name: None,
        // Upstream always pins `prefix: false` on assistant messages so a
        // replayed turn is never treated as an assistant prefill.
        prefix: Some(false),
    })
}

fn tool_message(
    msg: &Message,
    normalizer: &mut MistralToolCallIdNormalizer,
    tool_names: &HashMap<&str, &str>,
) -> Option<MistralChatMessage> {
    // `added_tool_names` is not projected: upstream consumes it in
    // `transformMessages` only for compat modes that re-register deferred
    // tools; the native Mistral API has no such body field. The names stay on
    // the transcript (`pi_ai::utils::deferred_tools` reads them back).
    let result_block = msg.content.iter().find_map(|c| match c {
        Content::ToolResult(r) => Some(r),
        _ => None,
    });
    let (tool_call_id, is_error, text, has_image, image_url) = match result_block {
        Some(r) => {
            let (text, has_image, image_url) = match &*r.content {
                Content::Text(t) => (t.text.clone(), false, None),
                Content::Image(image) => (
                    String::new(),
                    true,
                    Some(format!("data:{};base64,{}", image.mime_type, image.data)),
                ),
                other => (
                    serde_json::to_string(other).unwrap_or_default(),
                    false,
                    None,
                ),
            };
            (
                r.tool_call_id.clone(),
                r.is_error,
                text,
                has_image,
                image_url,
            )
        }
        // Bare text blocks are honoured for hand-built contexts.
        None => {
            let text = msg
                .content
                .iter()
                .filter_map(|c| match c {
                    Content::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            (String::new(), false, text, false, None)
        }
    };

    let mut chunks = vec![MistralContentChunk::Text {
        text: build_tool_result_text(&text, has_image, is_error),
    }];
    if let Some(image_url) = image_url {
        chunks.push(MistralContentChunk::ImageUrl { image_url });
    }
    let name = tool_names
        .get(tool_call_id.as_str())
        .map(|n| (*n).to_string());
    Some(MistralChatMessage {
        role: "tool".into(),
        content: Some(MistralContent::Chunks(chunks)),
        tool_calls: None,
        tool_call_id: Some(normalizer.normalize(&tool_call_id)),
        name,
        prefix: None,
    })
}

/// Upstream `buildToolResultText` with the `supportsImages = true` branch:
/// the Rust [`Model`] has no modality list, so image results are never
/// reported as omitted.
fn build_tool_result_text(text: &str, has_image: bool, is_error: bool) -> String {
    let trimmed = text.trim();
    let error_prefix = if is_error { "[tool error] " } else { "" };
    if !trimmed.is_empty() {
        return format!("{error_prefix}{trimmed}");
    }
    if has_image {
        return if is_error {
            "[tool error] (see attached image)".into()
        } else {
            "(see attached image)".into()
        };
    }
    if is_error {
        "[tool error] (no tool output)".into()
    } else {
        "(no tool output)".into()
    }
}

// ---------------------------------------------------------------------------
// Tool-call id normalization
// ---------------------------------------------------------------------------

/// Maps arbitrary tool-call ids onto unique 9-char alphanumeric ids.
///
/// Port of upstream `createMistralToolCallIdNormalizer`: ids are stable
/// for the lifetime of the normalizer, and a hash collision is resolved by
/// salting with an attempt counter.
#[derive(Debug, Default)]
pub struct MistralToolCallIdNormalizer {
    ids: HashMap<String, String>,
    owners: HashMap<String, String>,
}

impl MistralToolCallIdNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stable 9-char id for `id`.
    ///
    /// The same input always maps to the same output; two different
    /// inputs never collide.
    pub fn normalize(&mut self, id: &str) -> String {
        if let Some(existing) = self.ids.get(id) {
            return existing.clone();
        }
        let mut attempt = 0u32;
        loop {
            let candidate = derive_mistral_tool_call_id(id, attempt);
            match self.owners.get(&candidate) {
                Some(owner) if owner != id => attempt += 1,
                _ => {
                    self.ids.insert(id.to_string(), candidate.clone());
                    self.owners.insert(candidate.clone(), id.to_string());
                    return candidate;
                }
            }
        }
    }
}

/// Port of upstream `deriveMistralToolCallId`.
pub fn derive_mistral_tool_call_id(id: &str, attempt: u32) -> String {
    let normalized: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if attempt == 0 && normalized.chars().count() == MISTRAL_TOOL_CALL_ID_LENGTH {
        return normalized;
    }
    let seed_base = if normalized.is_empty() {
        id.to_string()
    } else {
        normalized
    };
    let seed = if attempt == 0 {
        seed_base
    } else {
        format!("{seed_base}:{attempt}")
    };
    short_hash(&seed)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(MISTRAL_TOOL_CALL_ID_LENGTH)
        .collect()
}

/// Port of `packages/ai/src/utils/hash.ts::shortHash`.
///
/// `Math.imul` is a 32-bit signed multiply, and `charCodeAt` iterates
/// UTF-16 code units, so the port hashes `encode_utf16()` with
/// `wrapping_mul` and unsigned shifts.
pub fn short_hash(input: &str) -> String {
    const M1: i32 = 2_654_435_761u32 as i32;
    const M2: i32 = 1_597_334_677u32 as i32;
    const M3: i32 = 2_246_822_507u32 as i32;
    const M4: i32 = 3_266_489_909u32 as i32;

    let mut h1: i32 = 0xdead_beefu32 as i32;
    let mut h2: i32 = 0x41c6_ce57u32 as i32;
    for unit in input.encode_utf16() {
        let ch = unit as i32;
        h1 = (h1 ^ ch).wrapping_mul(M1);
        h2 = (h2 ^ ch).wrapping_mul(M2);
    }
    let h1_shift = ((h1 as u32) >> 16) as i32;
    let h2_shift13 = ((h2 as u32) >> 13) as i32;
    h1 = (h1 ^ h1_shift).wrapping_mul(M3) ^ (h2 ^ h2_shift13).wrapping_mul(M4);
    let h2_shift = ((h2 as u32) >> 16) as i32;
    let h1_shift13 = ((h1 as u32) >> 13) as i32;
    h2 = (h2 ^ h2_shift).wrapping_mul(M3) ^ (h1 ^ h1_shift13).wrapping_mul(M4);
    format!("{}{}", to_base36(h2 as u32), to_base36(h1 as u32))
}

fn to_base36(mut value: u32) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// SSE parsing
// ---------------------------------------------------------------------------

/// Parse a Mistral SSE byte stream into assistant events.
pub fn parse_sse(
    bytes: impl Stream<Item = Result<Bytes, StreamError>> + Send + 'static,
    model_id: String,
) -> AssistantMessageEventStream {
    Box::pin(MistralSseStream::new(bytes, model_id))
}

struct MistralSseStream {
    inner: std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, StreamError>> + Send>>,
    /// Half-parsed SSE buffer (bytes since the last line break).
    line_buffer: Vec<u8>,
    /// Buffered `data:` lines for the in-flight event.
    data_lines: Vec<String>,
    pending: std::collections::VecDeque<Result<AssistantMessageEvent, StreamError>>,
    finished: bool,
    started: bool,
    model_id: String,
    state: ParserState,
}

#[derive(Default)]
struct ParserState {
    /// Materialised content, in first-seen order.
    content: Vec<Content>,
    /// Index into `content` of the open text block, if the last delta was
    /// text. Closed by a thinking or tool-call delta so a later text run
    /// becomes its own block.
    open_text: Option<usize>,
    /// In-flight tool calls, in first-seen order.
    tool_calls: Vec<PendingToolCall>,
    usage: Usage,
    stop_reason: Option<StopReason>,
    error_message: Option<String>,
}

struct PendingToolCall {
    /// Wire `index` when present, otherwise the derived id.
    key: String,
    id: String,
    name: String,
    arguments: String,
}

impl MistralSseStream {
    fn new(
        bytes: impl Stream<Item = Result<Bytes, StreamError>> + Send + 'static,
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
        while let Some(rel_end) = find_line_end(&self.line_buffer) {
            let raw = self.line_buffer.drain(..rel_end).collect::<Vec<_>>();
            // `\r\n` is one line break, a lone `\r` or `\n` is one too.
            if self.line_buffer.first() == Some(&b'\r') {
                self.line_buffer.remove(0);
                if self.line_buffer.first() == Some(&b'\n') {
                    self.line_buffer.remove(0);
                }
            } else if !self.line_buffer.is_empty() {
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
            self.data_lines
                .push(String::from_utf8_lossy(value).into_owned());
        }
        Ok(())
    }

    fn dispatch_event(&mut self) -> Result<(), StreamError> {
        if self.data_lines.is_empty() {
            return Ok(());
        }
        let payload = self.data_lines.join("\n").trim().to_string();
        self.data_lines.clear();
        if payload.is_empty() {
            return Ok(());
        }
        if payload == "[DONE]" {
            return Ok(());
        }
        self.handle_chunk(&payload)
    }

    fn handle_chunk(&mut self, data: &str) -> Result<(), StreamError> {
        let event: MistralCompletionEvent = serde_json::from_str(data)
            .map_err(|e| StreamError::Malformed(format!("mistral SSE JSON: {e}: {data}")))?;

        if !self.started {
            self.started = true;
            self.pending.push_back(Ok(AssistantMessageEvent::Start {
                model: self.model_id.clone(),
            }));
        }

        if let Some(usage) = event.usage {
            let prompt = usage.prompt_tokens.unwrap_or(0);
            let cached = usage.cached_prompt_tokens(prompt);
            let input = prompt.saturating_sub(cached);
            let output = usage.completion_tokens.unwrap_or(0);
            let total = usage
                .total_tokens
                .unwrap_or_else(|| input + output + cached);
            self.state.usage = Usage {
                input,
                output,
                cache_read: cached,
                cache_write: 0,
                total,
            };
        }

        let Some(choice) = event.choices.into_iter().next() else {
            return Ok(());
        };

        if let Some(reason) = choice.finish_reason.as_deref() {
            let (stop_reason, error_message) = map_stop_reason(reason);
            self.state.stop_reason = Some(stop_reason);
            if let Some(message) = error_message {
                self.state.error_message = Some(message);
            }
        }

        if let Some(delta) = choice.delta {
            match delta.content {
                Some(DeltaContent::Text(text)) => self.push_text(&text),
                Some(DeltaContent::Chunks(chunks)) => {
                    for chunk in chunks {
                        self.push_chunk(chunk);
                    }
                }
                None => {}
            }
            for call in delta.tool_calls.unwrap_or_default() {
                self.handle_tool_delta(call);
            }
        }
        Ok(())
    }

    /// A bare-string `delta.content` is always text. Consecutive text
    /// deltas extend the open block; a thinking or tool-call delta closes
    /// it first (see [`Self::push_chunk`] / [`Self::handle_tool_delta`]).
    fn push_text(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        self.open_text_block(delta);
        self.pending.push_back(Ok(AssistantMessageEvent::TextDelta {
            delta: delta.to_string(),
        }));
    }

    fn push_chunk(&mut self, chunk: StreamContentChunk) {
        match chunk.kind.as_str() {
            "thinking" => {
                let delta: String = chunk
                    .thinking
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|part| part.text)
                    .collect();
                if delta.is_empty() {
                    return;
                }
                // Thinking is streamed but not stored: `Content` has no
                // `Thinking` variant yet.
                self.state.open_text = None;
                self.pending
                    .push_back(Ok(AssistantMessageEvent::ThinkingDelta { delta }));
            }
            // `text` is the only other chunk type upstream understands; an
            // unknown one contributes nothing.
            _ => {
                if let Some(text) = chunk.text {
                    self.push_text(&text);
                }
            }
        }
    }

    fn open_text_block(&mut self, delta: &str) {
        if let Some(index) = self.state.open_text {
            if let Some(Content::Text(text)) = self.state.content.get_mut(index) {
                text.text.push_str(delta);
                return;
            }
        }
        self.state.content.push(Content::Text(TextContent {
            text: delta.to_string(),
        }));
        self.state.open_text = Some(self.state.content.len() - 1);
    }

    fn handle_tool_delta(&mut self, call: StreamToolCall) {
        self.state.open_text = None;
        let index = call.index.unwrap_or(0);
        let key = match call.index {
            Some(index) => index.to_string(),
            None => call
                .id
                .clone()
                .filter(|id| !id.is_empty() && id != "null")
                .unwrap_or_else(|| derive_mistral_tool_call_id(&format!("toolcall:{index}"), 0)),
        };

        let position = match self
            .state
            .tool_calls
            .iter()
            .position(|entry| entry.key == key)
        {
            Some(position) => position,
            None => {
                let id = call
                    .id
                    .clone()
                    .filter(|id| !id.is_empty() && id != "null")
                    .unwrap_or_else(|| {
                        derive_mistral_tool_call_id(&format!("toolcall:{index}"), 0)
                    });
                self.state.tool_calls.push(PendingToolCall {
                    key,
                    id,
                    name: call.function.name.clone(),
                    arguments: String::new(),
                });
                self.state.tool_calls.len() - 1
            }
        };

        let arguments_delta = arguments_delta(&call.function.arguments);
        {
            let entry = &mut self.state.tool_calls[position];
            if let Some(id) = call.id.clone() {
                if !id.is_empty() && id != "null" {
                    entry.id = id;
                }
            }
            if !call.function.name.is_empty() {
                entry.name = call.function.name.clone();
            }
            if let Some(delta) = &arguments_delta {
                entry.arguments.push_str(delta);
            }
        }
        let (id, name) = {
            let entry = &self.state.tool_calls[position];
            (entry.id.clone(), entry.name.clone())
        };
        self.pending
            .push_back(Ok(AssistantMessageEvent::ToolCallDelta {
                index: position as u32,
                id: Some(id),
                name: Some(name),
                arguments_delta,
            }));
    }

    fn finalize(&mut self) {
        // Materialize tool calls after any text, in first-seen order.
        let pending = std::mem::take(&mut self.state.tool_calls);
        for entry in pending {
            let arguments = parse_streaming_json(Some(entry.arguments.as_str()));
            self.state.content.push(Content::ToolCall(ToolCall {
                id: entry.id,
                name: entry.name,
                arguments,
            }));
        }
        let content = std::mem::take(&mut self.state.content);
        let usage = self.state.usage;
        let stop_reason = match self.state.stop_reason.take() {
            Some(stop_reason) => stop_reason,
            None => {
                // Upstream raises "Mistral stream ended without a finish reason".
                if self.state.error_message.is_none() {
                    self.state.error_message =
                        Some("Mistral stream ended without a finish reason".into());
                }
                StopReason::Error
            }
        };
        if let Some(message) = self.state.error_message.take() {
            self.pending
                .push_back(Ok(AssistantMessageEvent::Error { message }));
        }
        self.pending.push_back(Ok(AssistantMessageEvent::Done {
            content,
            stop_reason,
            usage,
        }));
    }
}

impl Stream for MistralSseStream {
    type Item = Result<AssistantMessageEvent, StreamError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if let Some(event) = self.pending.pop_front() {
            return std::task::Poll::Ready(Some(event));
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
                        let buffer = std::mem::take(&mut self.line_buffer);
                        if let Err(error) = self.process_line(&buffer) {
                            return std::task::Poll::Ready(Some(Err(error)));
                        }
                    }
                    if let Err(error) = self.dispatch_event() {
                        return std::task::Poll::Ready(Some(Err(error)));
                    }
                    self.finalize();
                    if let Some(event) = self.pending.pop_front() {
                        return std::task::Poll::Ready(Some(event));
                    }
                    return std::task::Poll::Ready(None);
                }
                std::task::Poll::Ready(Some(Err(error))) => {
                    self.finished = true;
                    return std::task::Poll::Ready(Some(Err(error)));
                }
                std::task::Poll::Ready(Some(Ok(chunk))) => {
                    if let Err(error) = self.feed(&chunk) {
                        self.finished = true;
                        return std::task::Poll::Ready(Some(Err(error)));
                    }
                    if let Some(event) = self.pending.pop_front() {
                        return std::task::Poll::Ready(Some(event));
                    }
                }
            }
        }
    }
}

/// Normalize a tool-call argument fragment into the string form the Rust
/// event surface carries. Mistral occasionally sends a complete JSON
/// object instead of a fragment; upstream stringifies it.
fn arguments_delta(arguments: &StreamArguments) -> Option<String> {
    match arguments {
        StreamArguments::Missing => None,
        StreamArguments::Text(text) if text.is_empty() => None,
        StreamArguments::Text(text) => Some(text.clone()),
        StreamArguments::Object(value) => Some(value.to_string()),
    }
}

fn map_stop_reason(reason: &str) -> (StopReason, Option<String>) {
    match reason {
        "stop" => (StopReason::Stop, None),
        "length" | "model_length" => (StopReason::MaxTokens, None),
        "tool_calls" => (StopReason::ToolUse, None),
        "error" => (
            StopReason::Error,
            Some("Provider stopped with: error".into()),
        ),
        other => (
            StopReason::Error,
            Some(format!("Provider stopped with: {other}")),
        ),
    }
}

/// Index of the first line break in `buffer`.
///
/// Mistral streams end lines with `\n` or `\r\n` (upstream splits on
/// `/\r\n|\r|\n/`), so both count. A `\r` in the last position may be the
/// first half of a `\r\n` that has not arrived yet: waiting for the next
/// byte prevents a spurious empty line (and an early event dispatch).
fn find_line_end(buffer: &[u8]) -> Option<usize> {
    for (index, byte) in buffer.iter().enumerate() {
        match byte {
            b'\n' => return Some(index),
            b'\r' if index + 1 < buffer.len() => return Some(index),
            _ => {}
        }
    }
    None
}

fn split_field(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let index = line.iter().position(|byte| *byte == b':')?;
    let field = &line[..index];
    let mut value = &line[index + 1..];
    if value.first() == Some(&b' ') {
        value = &value[1..];
    }
    Some((field, value))
}

// ---------------------------------------------------------------------------
// Streaming wire types
// ---------------------------------------------------------------------------

/// One SSE payload. A missing `choices` array is a protocol error
/// (upstream's `Invalid Mistral streaming event`).
#[derive(Debug, Deserialize)]
pub struct MistralCompletionEvent {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub usage: Option<MistralUsage>,
    pub choices: Vec<MistralStreamChoice>,
}

#[derive(Debug, Deserialize)]
pub struct MistralStreamChoice {
    #[serde(default)]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub delta: Option<MistralStreamDelta>,
}

#[derive(Debug, Default, Deserialize)]
pub struct MistralStreamDelta {
    #[serde(default)]
    pub content: Option<DeltaContent>,
    #[serde(default)]
    pub tool_calls: Option<Vec<StreamToolCall>>,
}

/// `content` is either a plain string or an array of chunks.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum DeltaContent {
    Text(String),
    Chunks(Vec<StreamContentChunk>),
}

#[derive(Debug, Deserialize)]
pub struct StreamContentChunk {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub thinking: Option<Vec<StreamThinkingPart>>,
}

#[derive(Debug, Deserialize)]
pub struct StreamThinkingPart {
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StreamToolCall {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub index: Option<u32>,
    pub function: StreamToolCallFunction,
}

#[derive(Debug, Deserialize)]
pub struct StreamToolCallFunction {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub arguments: StreamArguments,
}

/// `arguments` arrives as a fragment string, a completed object, or not at
/// all.
#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
pub enum StreamArguments {
    /// Absent / `null`.
    #[default]
    Missing,
    Text(String),
    Object(Value),
}

#[derive(Debug, Default, Deserialize)]
pub struct MistralUsage {
    #[serde(default)]
    pub prompt_tokens: Option<u32>,
    #[serde(default)]
    pub completion_tokens: Option<u32>,
    #[serde(default)]
    pub total_tokens: Option<u32>,
    #[serde(default, rename = "promptTokensDetails")]
    pub prompt_tokens_details_camel: Option<CachedTokens>,
    #[serde(default)]
    pub prompt_tokens_details: Option<CachedTokens>,
    #[serde(default, rename = "promptTokenDetails")]
    pub prompt_token_details_camel: Option<CachedTokens>,
    #[serde(default, rename = "prompt_token_details")]
    pub prompt_token_details: Option<CachedTokens>,
    #[serde(default)]
    pub num_cached_tokens: Option<i64>,
    #[serde(default, rename = "numCachedTokens")]
    pub num_cached_tokens_camel: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct CachedTokens {
    #[serde(default)]
    pub cached_tokens: Option<i64>,
    #[serde(default, rename = "cachedTokens")]
    pub cached_tokens_camel: Option<i64>,
}

impl CachedTokens {
    fn value(&self) -> Option<i64> {
        self.cached_tokens.or(self.cached_tokens_camel)
    }
}

impl MistralUsage {
    /// Port of `getMistralCachedPromptTokens`: first present variant wins,
    /// clamped into `0..=prompt_tokens`.
    pub fn cached_prompt_tokens(&self, prompt_tokens: u32) -> u32 {
        let raw = self
            .prompt_tokens_details_camel
            .as_ref()
            .and_then(CachedTokens::value)
            .or_else(|| {
                self.prompt_tokens_details
                    .as_ref()
                    .and_then(CachedTokens::value)
            })
            .or_else(|| {
                self.prompt_token_details_camel
                    .as_ref()
                    .and_then(CachedTokens::value)
            })
            .or_else(|| {
                self.prompt_token_details
                    .as_ref()
                    .and_then(CachedTokens::value)
            })
            .or(self.num_cached_tokens)
            .or(self.num_cached_tokens_camel)
            .unwrap_or(0);
        let clamped = raw.max(0).min(prompt_tokens as i64);
        clamped as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_protocol::{ImageContent, ProviderId, ToolResult};
    use serde_json::json;

    fn model() -> Model {
        Model {
            provider: ProviderId::new("mistral"),
            id: "mistral-large-latest".into(),
            api: pi_protocol::Api::MistralConversations,
            label: Some("Mistral Large (latest)".into()),
            context_window: 262_144,
            max_output_tokens: 262_144,
        }
    }

    fn options() -> SimpleStreamOptions {
        SimpleStreamOptions {
            // 0.25 is exactly representable as `f32`, so the wire
            // assertion below stays exact.
            temperature: Some(0.25),
            max_tokens: Some(1024),
            signal: None,
        }
    }

    #[test]
    fn short_hash_matches_the_typescript_implementation() {
        // Expected values produced by the installed 0.85.1
        // `dist/utils/hash.js` (`shortHash`).
        assert_eq!(short_hash(""), "k4n83c7h0j2b");
        assert_eq!(short_hash("toolcall:0"), "1nlso9v7di2pi");
        assert_eq!(short_hash("call_abc"), "py36n51ekigv6");
        assert_eq!(short_hash("toolu_01ABCDEFxyz"), "wj476ktgdfvt");
        assert_eq!(short_hash("toolcall:1"), "so4jux1synmjc");
        // UTF-16 code units, not Unicode scalar values.
        assert_eq!(short_hash("call🙈x"), "2v91zgrfbunh");
    }

    #[test]
    fn derive_tool_call_id_matches_the_typescript_implementation() {
        assert_eq!(derive_mistral_tool_call_id("call_abc", 0), "18mdplhyx");
        assert_eq!(derive_mistral_tool_call_id("abcdefghi", 0), "abcdefghi");
        assert_eq!(derive_mistral_tool_call_id("abcdefghij", 0), "5vtivddm0");
        assert_eq!(derive_mistral_tool_call_id("toolcall:0", 0), "toolcall0");
        assert_eq!(derive_mistral_tool_call_id("🙈", 0), "kphsz0153");
        assert_eq!(derive_mistral_tool_call_id("!@#$%", 0), "83fvtmv73");
        assert_eq!(derive_mistral_tool_call_id("abcdefghi", 1), "mlj703uel");
        assert_eq!(derive_mistral_tool_call_id("call_abc", 1), "cqjnieh44");
        assert_eq!(
            derive_mistral_tool_call_id("toolu_01ABCDEFxyz", 0),
            "170ffo090"
        );
    }

    #[test]
    fn normalizer_is_stable_and_dedupes_collisions() {
        let mut normalizer = MistralToolCallIdNormalizer::new();
        let first = normalizer.normalize("call_1");
        assert_eq!(first, normalizer.normalize("call_1"));
        assert_eq!(first.len(), MISTRAL_TOOL_CALL_ID_LENGTH);
        assert!(first.chars().all(|c| c.is_ascii_alphanumeric()));
        // A 9-char alnum id is already valid and passes through untouched.
        assert_eq!(normalizer.normalize("abcdefghi"), "abcdefghi");
    }

    #[test]
    fn build_request_shape_matches_upstream() {
        let mut ctx = Context::new("be brief");
        ctx.messages.push(Message {
            role: Role::User,
            content: vec![Content::text("hello")],
            model: None,
        });
        ctx.messages.push(Message {
            role: Role::Assistant,
            content: vec![
                Content::text("calling"),
                Content::ToolCall(ToolCall {
                    id: "call_1".into(),
                    name: "read".into(),
                    arguments: json!({"path": "a.txt"}),
                }),
            ],
            model: None,
        });
        ctx.messages.push(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                tool_call_id: "call_1".into(),
                content: Box::new(Content::text("file body")),
                is_error: false,
                details: None,
                added_tool_names: None,
            images: Vec::new(),
            })],
            model: None,
        });
        ctx.tools.push(ToolDefinition {
            name: "read".into(),
            label: "Read".into(),
            description: "Read a file".into(),
            parameters: json!({"type": "object", "properties": {}}),
            metadata: None,
        });

        let request = MistralProvider::build_request(&model(), &ctx, &options()).unwrap();
        let wire = serde_json::to_value(&request).unwrap();

        assert_eq!(wire["model"], "mistral-large-latest");
        assert_eq!(wire["stream"], true);
        assert_eq!(wire["temperature"], 0.25);
        assert_eq!(wire["max_tokens"], 1024);
        // System prompt is unshifted onto the message list.
        assert_eq!(wire["messages"][0]["role"], "system");
        assert_eq!(wire["messages"][0]["content"], "be brief");
        assert_eq!(wire["messages"][1]["content"], "hello");
        // Assistant replayed with normalized id + JSON-string arguments.
        assert_eq!(wire["messages"][2]["prefix"], false);
        let id = wire["messages"][2]["tool_calls"][0]["id"].as_str().unwrap();
        assert_eq!(id.len(), MISTRAL_TOOL_CALL_ID_LENGTH);
        assert_eq!(wire["messages"][2]["tool_calls"][0]["type"], "function");
        assert_eq!(
            wire["messages"][2]["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"a.txt\"}"
        );
        // Tool result echoes the same normalized id and carries the name.
        assert_eq!(wire["messages"][3]["role"], "tool");
        assert_eq!(wire["messages"][3]["tool_call_id"], id);
        assert_eq!(wire["messages"][3]["name"], "read");
        assert_eq!(
            wire["messages"][3]["content"][0],
            json!({"type": "text", "text": "file body"})
        );
        // Tools carry the inert `strict: false` flag upstream defaults to.
        assert_eq!(wire["tools"][0]["function"]["strict"], false);
        assert_eq!(wire["tools"][0]["type"], "function");
    }

    #[test]
    fn image_blocks_become_data_urls_and_error_results_get_the_prefix() {
        let mut ctx = Context::new("");
        ctx.messages.push(Message {
            role: Role::User,
            content: vec![
                Content::text("what is this"),
                Content::Image(ImageContent {
                    mime_type: "image/png".into(),
                    data: "AAAA".into(),
                }),
            ],
            model: None,
        });
        ctx.messages.push(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                tool_call_id: "abcdefghi".into(),
                content: Box::new(Content::text("boom")),
                is_error: true,
                details: None,
                added_tool_names: None,
            images: Vec::new(),
            })],
            model: None,
        });

        let request = MistralProvider::build_request(&model(), &ctx, &options()).unwrap();
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(
            wire["messages"][0]["content"][1],
            json!({"type": "image_url", "image_url": "data:image/png;base64,AAAA"})
        );
        assert_eq!(
            wire["messages"][1]["content"][0],
            json!({"type": "text", "text": "[tool error] boom"})
        );
    }

    #[test]
    fn url_construction_matches_upstream() {
        let provider = MistralProvider::new("key");
        assert_eq!(
            provider.build_url(),
            "https://api.mistral.ai/v1/chat/completions"
        );
        let provider = MistralProvider::with_base_url("key", "https://proxy.test/");
        assert_eq!(
            provider.build_url(),
            "https://proxy.test/v1/chat/completions"
        );
    }

    #[test]
    fn cached_tokens_read_every_upstream_variant() {
        let cases = [
            json!({"prompt_tokens": 100, "promptTokensDetails": {"cachedTokens": 40}}),
            json!({"prompt_tokens": 100, "prompt_tokens_details": {"cached_tokens": 40}}),
            json!({"prompt_tokens": 100, "promptTokenDetails": {"cachedTokens": 40}}),
            json!({"prompt_tokens": 100, "prompt_token_details": {"cached_tokens": 40}}),
            json!({"prompt_tokens": 100, "num_cached_tokens": 40}),
            json!({"prompt_tokens": 100, "numCachedTokens": 40}),
        ];
        for case in cases {
            let usage: MistralUsage = serde_json::from_value(case.clone()).unwrap();
            assert_eq!(usage.cached_prompt_tokens(100), 40, "{case}");
        }
        // Clamped into `0..=prompt_tokens`.
        let usage: MistralUsage =
            serde_json::from_value(json!({"prompt_tokens": 10, "num_cached_tokens": 99})).unwrap();
        assert_eq!(usage.cached_prompt_tokens(10), 10);
        let usage: MistralUsage =
            serde_json::from_value(json!({"prompt_tokens": 10, "num_cached_tokens": -5})).unwrap();
        assert_eq!(usage.cached_prompt_tokens(10), 0);
    }

    #[test]
    fn stop_reasons_match_upstream() {
        assert_eq!(map_stop_reason("stop"), (StopReason::Stop, None));
        assert_eq!(map_stop_reason("length"), (StopReason::MaxTokens, None));
        assert_eq!(
            map_stop_reason("model_length"),
            (StopReason::MaxTokens, None)
        );
        assert_eq!(map_stop_reason("tool_calls"), (StopReason::ToolUse, None));
        assert_eq!(
            map_stop_reason("error"),
            (
                StopReason::Error,
                Some("Provider stopped with: error".to_string())
            )
        );
        assert_eq!(
            map_stop_reason("weird"),
            (
                StopReason::Error,
                Some("Provider stopped with: weird".to_string())
            )
        );
    }
}
