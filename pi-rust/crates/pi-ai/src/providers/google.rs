//! Google Gemini provider (Generative Language API).
//!
//! Implements [`StreamFn`] against the Google Generative Language API
//! (`POST {base_url}/models/{model}:streamGenerateContent?alt=sse`).
//! The `alt=sse` variant emits one JSON object per `data:` frame; this
//! adapter parses those frames into [`AssistantMessageEvent`]s and
//! accumulates the final content into a single trailing
//! [`Done`](AssistantMessageEvent::Done) event.
//!
//! This is the Rust port of `packages/ai/src/api/google-generative-ai.ts`
//! plus `packages/ai/src/api/google-shared.ts`.
//!
//! # Field mapping (Rust ↔ TS Google Generative AI)
//!
//! | TS field                                | Rust field / wire usage                |
//! | --------------------------------------- | -------------------------------------- |
//! | `usageMetadata.promptTokenCount`        | `Usage::input` (minus cache read)      |
//! | `usageMetadata.cachedContentTokenCount` | `Usage::cache_read`                    |
//! | `usageMetadata.candidatesTokenCount`    | `Usage::output` (plus thoughts)        |
//! | `usageMetadata.thoughtsTokenCount`      | folded into `Usage::output`            |
//! | `usageMetadata.totalTokenCount`         | `Usage::total`                         |
//! | `finishReason = "STOP"`                 | `StopReason::Stop` (→ `ToolUse` if a tool call was seen) |
//! | `finishReason = "MAX_TOKENS"`           | `StopReason::MaxTokens`                |
//! | safety / malformed / unspecified        | `StopReason::Error`                    |
//! | `parts[].text`                          | `AssistantMessageEvent::TextDelta`     |
//! | `parts[].thought = true` + `text`       | `AssistantMessageEvent::ThinkingDelta` |
//! | `parts[].functionCall`                  | `Content::ToolCall` + `ToolCallDelta`  |
//! | `parts[].inlineData` (request)          | `Content::Image`                       |
//!
//! # Deliberate simplifications vs. the TS port
//!
//! * Thinking content is streamed as [`ThinkingDelta`](AssistantMessageEvent::ThinkingDelta)
//!   but not stored: the `pi-protocol` `Content` enum has no
//!   `Thinking` variant yet (same trade-off as the Anthropic adapter).
//! * Thought signatures (`thoughtSignature`) are accepted on the wire
//!   but dropped on request rebuilds — again awaiting a protocol increment.
//! * `thinkingConfig` is not sent; the Rust [`Model`] descriptor has no
//!   `reasoning` flag yet, so the upstream "disable thinking when the
//!   caller did not ask for reasoning" branch has nothing to key off.
//!
//! # Native vs. WASM
//!
//! Native targets use `reqwest`; the `wasm32-unknown-unknown` target has
//! no usable outbound HTTP client in this stage and returns
//! [`StreamError::Malformed`] from every call. [`parse_sse`] itself is
//! target-agnostic and is what the fixture tests exercise.

// Wire-format structs are documented inline against the upstream API
// reference rather than via Rustdoc on every generated field.
#![allow(missing_docs)]

use std::collections::HashMap;

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

use crate::stream::AssistantMessageEventStream;
use crate::types::{SimpleStreamOptions, StreamError};
use crate::StreamFn;

/// Default base URL for the Google Generative Language API.
///
/// Includes the `/v1beta` version prefix, matching the upstream SDK's
/// `baseUrl` (the SDK appends the empty api version when a custom
/// `baseUrl` is supplied).
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Google Gemini provider.
///
/// Construct with [`GoogleProvider::new`] for the production endpoint,
/// or [`GoogleProvider::with_base_url`] to point at a local gateway.
#[derive(Debug, Clone)]
pub struct GoogleProvider {
    /// API key sent in the `x-goog-api-key` header.
    pub api_key: String,
    /// Base URL with no trailing slash. The
    /// `/{model}:streamGenerateContent` path is appended in
    /// [`Self::send_streaming`].
    pub base_url: String,
}

impl GoogleProvider {
    /// Create a provider pointing at the production Google endpoint.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Create a provider pointing at a custom base URL (a gateway,
    /// `google-vertex` shim, or a local mock server).
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
        }
    }

    /// Build the request body for `streamGenerateContent`.
    ///
    /// Public so callers (and tests) can inspect the wire payload
    /// without making a network call.
    pub fn build_request(
        model: &Model,
        ctx: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<GenerateContentRequest, StreamError> {
        let mut contents: Vec<GoogleContent> = Vec::with_capacity(ctx.messages.len());
        // Tool call id → tool name, so a `Role::Tool` message can fill in
        // the `functionResponse.name` field Google requires. The Rust
        // `Message` type does not carry a `toolName` (the TS one does),
        // so we resolve it from the preceding assistant turn.
        let mut tool_names: HashMap<String, String> = HashMap::new();

        for msg in &ctx.messages {
            match msg.role {
                Role::System => {
                    // Google carries the system prompt in the top-level
                    // `systemInstruction` field; a stray system message
                    // in the log is dropped.
                }
                Role::User => {
                    if let Some(content) = user_content(msg) {
                        contents.push(content);
                    }
                }
                Role::Assistant => {
                    for block in &msg.content {
                        if let Content::ToolCall(call) = block {
                            tool_names.insert(call.id.clone(), call.name.clone());
                        }
                    }
                    if let Some(content) = assistant_content(msg) {
                        contents.push(content);
                    }
                }
                Role::Tool => {
                    for part in function_response_parts(msg, &tool_names)? {
                        // The Cloud Code Assist API requires all function
                        // responses to be delivered in a single user turn,
                        // so merge into the trailing user turn when it
                        // already holds a function response.
                        match contents.last_mut() {
                            Some(last)
                                if last.role == "user"
                                    && last.parts.iter().any(|p| p.function_response.is_some()) =>
                            {
                                last.parts.push(part);
                            }
                            _ => contents.push(GoogleContent {
                                role: "user".to_string(),
                                parts: vec![part],
                            }),
                        }
                    }
                }
            }
        }

        let system_instruction = if ctx.system_prompt.is_empty() {
            None
        } else {
            Some(SystemInstruction {
                parts: vec![GooglePart::text(ctx.system_prompt.clone())],
            })
        };

        let tools = if ctx.tools.is_empty() {
            None
        } else {
            Some(vec![GoogleToolGroup {
                function_declarations: ctx
                    .tools
                    .iter()
                    .map(|t| FunctionDeclaration {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters_json_schema: t.parameters.clone(),
                    })
                    .collect(),
            }])
        };

        let mut generation_config = GenerationConfig::default();
        if let Some(temperature) = options.temperature {
            generation_config.temperature = Some(temperature);
        }
        let max_output_tokens = options.max_tokens.or({
            if model.max_output_tokens > 0 {
                Some(model.max_output_tokens)
            } else {
                None
            }
        });
        generation_config.max_output_tokens = max_output_tokens;
        let generation_config = if generation_config.temperature.is_none()
            && generation_config.max_output_tokens.is_none()
        {
            None
        } else {
            Some(generation_config)
        };

        Ok(GenerateContentRequest {
            contents,
            system_instruction,
            tools,
            generation_config,
        })
    }

    /// POST `/{model}:streamGenerateContent?alt=sse` and pipe the SSE
    /// byte stream through [`parse_sse`].
    #[cfg(not(target_arch = "wasm32"))]
    async fn send_streaming(
        &self,
        model: &Model,
        body: &GenerateContentRequest,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            self.base_url.trim_end_matches('/'),
            urlencode(&model.id),
        );
        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let status_code = status.as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(classify_http_status(status_code, truncate_body(&body)));
        }
        let model_id = model.id.clone();
        let byte_stream = response.bytes_stream();
        let mapped = byte_stream.map_err(StreamError::Transport);
        Ok(parse_sse(Box::pin(mapped), model_id))
    }
}

#[async_trait]
impl StreamFn for GoogleProvider {
    async fn stream_simple(
        &self,
        model: &Model,
        ctx: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        let body = Self::build_request(model, ctx, options)?;

        #[cfg(not(target_arch = "wasm32"))]
        {
            self.send_streaming(model, &body).await
        }

        #[cfg(target_arch = "wasm32")]
        {
            let _ = body;
            Err(StreamError::Malformed(
                "GoogleProvider is not yet implemented for wasm32-unknown-unknown".into(),
            ))
        }
    }
}

/// Percent-encode the characters that are unsafe in a path segment.
///
/// Model ids only ever contain `[A-Za-z0-9._-]`, but encoding the path
/// segment keeps a caller-supplied id from breaking the URL.
fn urlencode(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Classify an HTTP error into the variant of [`StreamError`] used
/// across `pi-ai`. `401` / `403` map to `Provider { status }` (the
/// `StreamError` enum currently has no dedicated auth variant), `429`
/// and `5xx` are preserved verbatim so callers can implement back-off.
#[cfg(not(target_arch = "wasm32"))]
fn classify_http_status(status: u16, body: String) -> StreamError {
    StreamError::Provider { status, body }
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
// Wire types (requests)
// ---------------------------------------------------------------------------

/// `POST /models/{model}:streamGenerateContent` request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateContentRequest {
    /// Conversation turns. Roles are `"user"` and `"model"`; function
    /// responses travel in a `"user"` turn.
    pub contents: Vec<GoogleContent>,
    /// System prompt, carried separately from the message list.
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<SystemInstruction>,
    /// Function declarations. Skipped when the context has no tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<GoogleToolGroup>>,
    /// Sampling controls. Skipped when neither temperature nor a token
    /// cap was requested.
    #[serde(rename = "generationConfig", skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GenerationConfig>,
}

/// System instruction wrapper — Google accepts `{"parts": [...]}` and
/// does not require a `role`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInstruction {
    /// Instruction parts (always a single text part in this port).
    pub parts: Vec<GooglePart>,
}

/// One conversation turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleContent {
    /// `"user"` or `"model"`.
    pub role: String,
    /// Content parts.
    pub parts: Vec<GooglePart>,
}

/// One content part — text, inline image, function call, or function
/// response. `None` fields are omitted on the wire.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GooglePart {
    /// Text payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `true` marks a thought summary (Gemini thinking output).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought: Option<bool>,
    /// Inline base64 image data.
    #[serde(rename = "inlineData", skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<InlineData>,
    /// Model-issued function call.
    #[serde(rename = "functionCall", skip_serializing_if = "Option::is_none")]
    pub function_call: Option<FunctionCall>,
    /// Tool result returned to the model.
    #[serde(rename = "functionResponse", skip_serializing_if = "Option::is_none")]
    pub function_response: Option<FunctionResponse>,
    /// Opaque thought signature the API asks to be echoed back.
    #[serde(rename = "thoughtSignature", skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

impl GooglePart {
    /// Convenience constructor for a text part.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            ..Default::default()
        }
    }
}

/// Inline image source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InlineData {
    /// MIME type (`image/png`, …).
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    /// Base64-encoded bytes.
    pub data: String,
}

/// A function call issued by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    /// Tool name.
    pub name: String,
    /// Parsed arguments object.
    #[serde(default)]
    pub args: Value,
    /// Provider-issued call id. Only sent back for models that require
    /// it (Gemini 3+, Claude / gpt-oss behind Cloud Code Assist).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// A function response sent back to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionResponse {
    /// Tool name the response belongs to.
    pub name: String,
    /// `{"output": ...}` on success, `{"error": ...}` on failure.
    pub response: Value,
    /// Provider-issued call id (see [`FunctionCall::id`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// Sampling controls.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GenerationConfig {
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Output token cap.
    #[serde(rename = "maxOutputTokens", skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

/// A group of function declarations (Google nests them under
/// `tools[].functionDeclarations`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoogleToolGroup {
    /// Declarations in this group.
    #[serde(rename = "functionDeclarations")]
    pub function_declarations: Vec<FunctionDeclaration>,
}

/// One function declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionDeclaration {
    /// Tool name.
    pub name: String,
    /// Description shown to the model.
    pub description: String,
    /// Full JSON Schema for the parameters object. Google's
    /// `parametersJsonSchema` accepts standard JSON Schema (unlike the
    /// legacy OpenAPI-subset `parameters` field).
    #[serde(rename = "parametersJsonSchema")]
    pub parameters_json_schema: Value,
}

// ---------------------------------------------------------------------------
// Message conversion
// ---------------------------------------------------------------------------

fn user_content(msg: &Message) -> Option<GoogleContent> {
    let mut parts = Vec::with_capacity(msg.content.len());
    for block in &msg.content {
        match block {
            Content::Text(t) if !t.text.is_empty() => parts.push(GooglePart::text(t.text.clone())),
            Content::Image(image) => parts.push(GooglePart {
                inline_data: Some(InlineData {
                    mime_type: image.mime_type.clone(),
                    data: image.data.clone(),
                }),
                ..Default::default()
            }),
            _ => {}
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(GoogleContent {
            role: "user".to_string(),
            parts,
        })
    }
}

fn assistant_content(msg: &Message) -> Option<GoogleContent> {
    let mut parts = Vec::with_capacity(msg.content.len());
    for block in &msg.content {
        match block {
            Content::Text(t) if !t.text.is_empty() => parts.push(GooglePart::text(t.text.clone())),
            Content::ToolCall(call) => parts.push(GooglePart {
                function_call: Some(FunctionCall {
                    name: call.name.clone(),
                    args: call.arguments.clone(),
                    id: Some(call.id.clone()),
                }),
                ..Default::default()
            }),
            _ => {}
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(GoogleContent {
            role: "model".to_string(),
            parts,
        })
    }
}

/// Convert a `Role::Tool` message into one or more `functionResponse`
/// parts. Google keys responses by tool *name*, which the Rust
/// [`Message`] type does not carry — resolve it from the tool calls
/// seen earlier in the conversation, falling back to the call id.
fn function_response_parts(
    msg: &Message,
    tool_names: &HashMap<String, String>,
) -> Result<Vec<GooglePart>, StreamError> {
    let mut parts = Vec::new();
    let mut saw_result = false;
    for block in &msg.content {
        if let Content::ToolResult(result) = block {
            saw_result = true;
            let id = result.tool_call_id.clone();
            let name = tool_names.get(&id).cloned().unwrap_or_else(|| id.clone());
            let text = match &*result.content {
                Content::Text(t) => t.text.clone(),
                _ => String::new(),
            };
            let response = if result.is_error {
                serde_json::json!({ "error": text })
            } else {
                serde_json::json!({ "output": text })
            };
            parts.push(GooglePart {
                function_response: Some(FunctionResponse {
                    name,
                    response,
                    id: Some(id),
                }),
                ..Default::default()
            });
        }
    }
    if !saw_result {
        return Err(StreamError::Malformed(
            "tool message missing tool result".into(),
        ));
    }
    Ok(parts)
}

// ---------------------------------------------------------------------------
// SSE parser
// ---------------------------------------------------------------------------

/// Parse a Google `alt=sse` byte stream into an
/// [`AssistantMessageEventStream`].
///
/// Each frame is a `data:` line holding one `GenerateContentResponse`
/// JSON object; blank lines terminate a frame and `:` comments are
/// ignored. A frame that carries a top-level `error` object is surfaced
/// as [`StreamError::Malformed`].
pub fn parse_sse(
    bytes: impl futures::Stream<Item = Result<Bytes, StreamError>> + Send + 'static,
    model_id: String,
) -> AssistantMessageEventStream {
    Box::pin(GoogleSseStream::new(bytes, model_id))
}

struct GoogleSseStream {
    inner: std::pin::Pin<Box<dyn futures::Stream<Item = Result<Bytes, StreamError>> + Send>>,
    /// Partially-buffered line.
    line_buffer: Vec<u8>,
    /// Buffered `data:` lines for the frame in flight.
    data_lines: Vec<String>,
    /// Events produced by the current chunk but not yet yielded.
    pending: std::collections::VecDeque<Result<AssistantMessageEvent, StreamError>>,
    /// True once the terminal `Done` has been queued.
    finished: bool,
    /// True once `Start` has been emitted.
    started: bool,
    model_id: String,
    state: ParserState,
}

#[derive(Default)]
struct ParserState {
    usage: Usage,
    stop_reason: Option<StopReason>,
    /// Materialised content, in first-seen order.
    content: Vec<Content>,
    /// Index into `content` of the open text block, if the most recent
    /// part was text. Reset by thinking parts and tool calls so a later
    /// text run becomes its own block.
    open_text: Option<usize>,
    saw_tool_use: bool,
}

impl GoogleSseStream {
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
        while let Some(end) = find_newline(&self.line_buffer) {
            let raw = self.line_buffer.drain(..end).collect::<Vec<_>>();
            // Drop the line terminator (`\n`, `\r\n`, or a stray `\r`).
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
            return self.dispatch_frame();
        }
        // SSE comments start with `:`.
        if line.first() == Some(&b':') {
            return Ok(());
        }
        let Some((field, value)) = split_field(line) else {
            return Ok(());
        };
        if field == b"data" {
            self.data_lines
                .push(String::from_utf8_lossy(value).into_owned());
        }
        Ok(())
    }

    fn dispatch_frame(&mut self) -> Result<(), StreamError> {
        if self.data_lines.is_empty() {
            return Ok(());
        }
        let payload = self.data_lines.join("\n");
        self.data_lines.clear();
        if payload.trim().is_empty() {
            return Ok(());
        }
        let chunk: WireChunk = match serde_json::from_str(&payload) {
            Ok(chunk) => chunk,
            Err(e) => {
                self.pending.push_back(Err(StreamError::Malformed(format!(
                    "Google stream chunk JSON: {e}: {payload}"
                ))));
                return Ok(());
            }
        };
        if let Some(error) = chunk.error {
            let message = error
                .message
                .unwrap_or_else(|| "Google returned an error frame".to_string());
            self.pending.push_back(Err(StreamError::Malformed(format!(
                "Google stream error: {message}"
            ))));
            return Ok(());
        }
        if !self.started {
            self.started = true;
            self.pending
                .push_back(Ok(AssistantMessageEvent::Start {
                    model: self.model_id.clone(),
                }));
        }
        self.handle_chunk(chunk);
        Ok(())
    }

    fn handle_chunk(&mut self, chunk: WireChunk) {
        if let Some(candidate) = chunk.candidates.first() {
            if let Some(content) = &candidate.content {
                for part in &content.parts {
                    self.handle_part(part);
                }
            }
            if let Some(reason) = &candidate.finish_reason {
                let mut mapped = map_stop_reason(reason);
                // Google reports `STOP` even when the turn was a tool
                // call; prefer `ToolUse` in that case (mirrors TS).
                if mapped == StopReason::Stop && self.state.saw_tool_use {
                    mapped = StopReason::ToolUse;
                }
                self.state.stop_reason = Some(mapped);
            }
        }
        if let Some(usage) = chunk.usage_metadata {
            self.state.usage = usage.into_usage();
        }
    }

    fn handle_part(&mut self, part: &WirePart) {
        if let Some(text) = &part.text {
            if !text.is_empty() {
                if part.thought == Some(true) {
                    // Thinking blocks are streamed but not stored
                    // (`pi-protocol` has no `Thinking` content variant).
                    self.state.open_text = None;
                    self.pending
                        .push_back(Ok(AssistantMessageEvent::ThinkingDelta {
                            delta: text.clone(),
                        }));
                } else {
                    let index = match self.state.open_text {
                        Some(index) => index,
                        None => {
                            self.state
                                .content
                                .push(Content::Text(TextContent::default()));
                            let index = self.state.content.len() - 1;
                            self.state.open_text = Some(index);
                            index
                        }
                    };
                    if let Some(Content::Text(block)) = self.state.content.get_mut(index) {
                        block.text.push_str(text);
                    }
                    self.pending
                        .push_back(Ok(AssistantMessageEvent::TextDelta {
                            delta: text.clone(),
                        }));
                }
            }
        }

        if let Some(call) = &part.function_call {
            self.state.open_text = None;
            let index = self.state.content.len() as u32;
            let name = call.name.clone().unwrap_or_default();
            let id = match &call.id {
                Some(id) if !id.is_empty() => id.clone(),
                _ => next_tool_call_id(&name),
            };
            let arguments = if call.args.is_null() {
                serde_json::json!({})
            } else {
                call.args.clone()
            };
            self.state
                .content
                .push(Content::ToolCall(ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                }));
            self.state.saw_tool_use = true;
            self.pending
                .push_back(Ok(AssistantMessageEvent::ToolCallDelta {
                    index,
                    id: Some(id),
                    name: Some(name),
                    arguments_delta: Some(arguments.to_string()),
                }));
        }
    }

    fn finalize(&mut self) {
        let stop_reason = self.state.stop_reason.unwrap_or({
            if self.state.saw_tool_use {
                StopReason::ToolUse
            } else {
                StopReason::Stop
            }
        });
        let content = std::mem::take(&mut self.state.content);
        let usage = self.state.usage;
        let message = AssistantMessage {
            model: self.model_id.clone(),
            content,
            stop_reason,
            usage,
        };
        self.pending
            .push_back(Ok(AssistantMessageEvent::Done {
                content: message.content,
                stop_reason: message.stop_reason,
                usage: message.usage,
            }));
    }
}

impl futures::Stream for GoogleSseStream {
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
                        let buf = std::mem::take(&mut self.line_buffer);
                        if let Err(e) = self.process_line(&buf) {
                            return std::task::Poll::Ready(Some(Err(e)));
                        }
                    }
                    if let Err(e) = self.dispatch_frame() {
                        return std::task::Poll::Ready(Some(Err(e)));
                    }
                    if !self.started {
                        // Empty stream — still honour the Start/Done
                        // contract so callers do not have to special-case it.
                        self.started = true;
                        let model = self.model_id.clone();
                        self.pending
                            .push_back(Ok(AssistantMessageEvent::Start { model }));
                    }
                    self.finalize();
                    if let Some(event) = self.pending.pop_front() {
                        return std::task::Poll::Ready(Some(event));
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
                    if let Some(event) = self.pending.pop_front() {
                        return std::task::Poll::Ready(Some(event));
                    }
                }
            }
        }
    }
}

fn next_tool_call_id(name: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{name}_call_{seq}")
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "STOP" => StopReason::Stop,
        "MAX_TOKENS" => StopReason::MaxTokens,
        // Safety / blocked / malformed completions all surface as
        // errors, matching `mapStopReason` in `google-shared.ts`.
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" | "IMAGE_SAFETY"
        | "IMAGE_PROHIBITED_CONTENT" | "IMAGE_RECITATION" | "IMAGE_OTHER" | "LANGUAGE"
        | "MALFORMED_FUNCTION_CALL" | "UNEXPECTED_TOOL_CALL" | "NO_IMAGE" | "OTHER"
        | "FINISH_REASON_UNSPECIFIED" => StopReason::Error,
        other => {
            tracing::debug!(reason = %other, "Google returned unknown finishReason");
            StopReason::Error
        }
    }
}

fn find_newline(buf: &[u8]) -> Option<usize> {
    buf.iter().position(|b| *b == b'\n' || *b == b'\r')
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
// Wire types (responses)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct WireChunk {
    #[serde(default)]
    candidates: Vec<WireCandidate>,
    #[serde(default, rename = "usageMetadata")]
    usage_metadata: Option<WireUsage>,
    #[serde(default)]
    error: Option<WireError>,
}

#[derive(Debug, Deserialize)]
struct WireCandidate {
    #[serde(default)]
    content: Option<WireContent>,
    #[serde(default, rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireContent {
    #[serde(default)]
    parts: Vec<WirePart>,
}

#[derive(Debug, Deserialize)]
struct WirePart {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thought: Option<bool>,
    #[serde(default, rename = "functionCall")]
    function_call: Option<WireFunctionCall>,
}

#[derive(Debug, Deserialize)]
struct WireFunctionCall {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    args: Value,
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct WireUsage {
    #[serde(default, rename = "promptTokenCount")]
    prompt_token_count: u32,
    #[serde(default, rename = "candidatesTokenCount")]
    candidates_token_count: u32,
    #[serde(default, rename = "cachedContentTokenCount")]
    cached_content_token_count: u32,
    #[serde(default, rename = "thoughtsTokenCount")]
    thoughts_token_count: u32,
    #[serde(default, rename = "totalTokenCount")]
    total_token_count: u32,
}

impl WireUsage {
    fn into_usage(self) -> Usage {
        Usage {
            // Google's `promptTokenCount` includes cache reads; the TS
            // port subtracts them so `input` is the uncached portion.
            input: self
                .prompt_token_count
                .saturating_sub(self.cached_content_token_count),
            output: self
                .candidates_token_count
                .saturating_add(self.thoughts_token_count),
            cache_read: self.cached_content_token_count,
            cache_write: 0,
            total: self.total_token_count,
        }
    }
}

#[derive(Debug, Deserialize)]
struct WireError {
    #[serde(default)]
    message: Option<String>,
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

/// Built-in Gemini catalog — the three models this provider ships by
/// default. Mirrors the TS port's Google defaults closely enough for
/// `Models` lookups; the generated upstream catalog carries richer
/// cost / capability metadata that lands with a `pi-protocol` increment.
pub fn builtin_google_models() -> Vec<Model> {
    vec![
        Model {
            provider: pi_protocol::ProviderId::new("google"),
            id: "gemini-2.5-pro".into(),
            api: pi_protocol::Api::GoogleGenerativeAi,
            label: Some("Gemini 2.5 Pro".into()),
            context_window: 1_048_576,
            max_output_tokens: 65_536,
        },
        Model {
            provider: pi_protocol::ProviderId::new("google"),
            id: "gemini-2.5-flash".into(),
            api: pi_protocol::Api::GoogleGenerativeAi,
            label: Some("Gemini 2.5 Flash".into()),
            context_window: 1_048_576,
            max_output_tokens: 65_536,
        },
        Model {
            provider: pi_protocol::ProviderId::new("google"),
            id: "gemini-2.5-flash-lite".into(),
            api: pi_protocol::Api::GoogleGenerativeAi,
            label: Some("Gemini 2.5 Flash Lite".into()),
            context_window: 1_048_576,
            max_output_tokens: 65_536,
        },
    ]
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use pi_protocol::{Message, ToolDefinition};
    use serde_json::json;

    fn model() -> Model {
        Model {
            provider: pi_protocol::ProviderId::new("google"),
            id: "gemini-2.5-flash".into(),
            api: pi_protocol::Api::GoogleGenerativeAi,
            label: None,
            context_window: 1_048_576,
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
                "properties": {"city": {"type": "string"}},
                "required": ["city"],
            }),
            metadata: None,
        });
        ctx
    }

    #[test]
    fn request_body_serializes_system_tools_and_generation_config() {
        let req = GoogleProvider::build_request(
            &model(),
            &ctx_with_tool(),
            &SimpleStreamOptions {
                temperature: Some(0.3),
                max_tokens: Some(512),
                ..Default::default()
            },
        )
        .expect("build request");
        let v = serde_json::to_value(&req).expect("serialize");

        assert_eq!(v["systemInstruction"]["parts"][0]["text"], "you are pi");
        assert_eq!(v["contents"][0]["role"], "user");
        assert_eq!(v["contents"][0]["parts"][0]["text"], "what's the weather in SF?");
        assert_eq!(v["generationConfig"]["maxOutputTokens"], 512);

        let decls = v["tools"][0]["functionDeclarations"]
            .as_array()
            .expect("declarations");
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0]["name"], "get_weather");
        assert_eq!(decls[0]["parametersJsonSchema"]["type"], "object");
        // The legacy OpenAPI-subset field must not be used.
        assert!(decls[0].get("parameters").is_none());
    }

    #[test]
    fn request_body_omits_generation_config_without_options() {
        let mut ctx = Context::new("");
        ctx.messages.push(Message {
            role: Role::User,
            content: vec![Content::text("hi")],
            model: None,
        });
        let mut m = model();
        m.max_output_tokens = 0;
        let req =
            GoogleProvider::build_request(&m, &ctx, &SimpleStreamOptions::default()).expect("build");
        let v = serde_json::to_value(&req).expect("serialize");
        assert!(v.get("systemInstruction").is_none());
        assert!(v.get("generationConfig").is_none());
        assert!(v.get("tools").is_none());
    }

    #[test]
    fn assistant_tool_call_becomes_function_call() {
        let mut ctx = Context::new("");
        ctx.messages.push(Message {
            role: Role::Assistant,
            content: vec![
                Content::text("Looking it up."),
                Content::ToolCall(ToolCall {
                    id: "call_abc".into(),
                    name: "get_weather".into(),
                    arguments: json!({"city": "Berlin"}),
                }),
            ],
            model: None,
        });
        let req = GoogleProvider::build_request(&model(), &ctx, &SimpleStreamOptions::default())
            .expect("build");
        let v = serde_json::to_value(&req).expect("serialize");
        assert_eq!(v["contents"][0]["role"], "model");
        assert_eq!(v["contents"][0]["parts"][0]["text"], "Looking it up.");
        assert_eq!(v["contents"][0]["parts"][1]["functionCall"]["name"], "get_weather");
        assert_eq!(
            v["contents"][0]["parts"][1]["functionCall"]["args"]["city"],
            "Berlin"
        );
        assert_eq!(v["contents"][0]["parts"][1]["functionCall"]["id"], "call_abc");
    }

    #[test]
    fn tool_results_merge_into_one_user_turn_and_resolve_names() {
        let mut ctx = Context::new("");
        ctx.messages.push(Message {
            role: Role::Assistant,
            content: vec![
                Content::ToolCall(ToolCall {
                    id: "call_a".into(),
                    name: "get_weather".into(),
                    arguments: json!({"city": "SF"}),
                }),
                Content::ToolCall(ToolCall {
                    id: "call_b".into(),
                    name: "get_time".into(),
                    arguments: json!({"zone": "UTC"}),
                }),
            ],
            model: None,
        });
        for (id, text) in [("call_a", "72F and sunny"), ("call_b", "09:00")] {
            ctx.messages.push(Message {
                role: Role::Tool,
                content: vec![Content::ToolResult(pi_protocol::ToolResult {
                    tool_call_id: id.into(),
                    content: Box::new(Content::text(text)),
                    is_error: false,
                    details: None,
                })],
                model: None,
            });
        }
        let req = GoogleProvider::build_request(&model(), &ctx, &SimpleStreamOptions::default())
            .expect("build");
        let v = serde_json::to_value(&req).expect("serialize");
        let contents = v["contents"].as_array().expect("contents");
        // Assistant turn + a single merged user turn holding both responses.
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[1]["role"], "user");
        let parts = contents[1]["parts"].as_array().expect("parts");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["functionResponse"]["name"], "get_weather");
        assert_eq!(parts[0]["functionResponse"]["response"]["output"], "72F and sunny");
        assert_eq!(parts[0]["functionResponse"]["id"], "call_a");
        assert_eq!(parts[1]["functionResponse"]["name"], "get_time");
        assert_eq!(parts[1]["functionResponse"]["response"]["output"], "09:00");
    }

    #[test]
    fn error_tool_result_uses_error_key() {
        let mut ctx = Context::new("");
        ctx.messages.push(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(pi_protocol::ToolResult {
                tool_call_id: "call_missing".into(),
                content: Box::new(Content::text("boom")),
                is_error: true,
                details: None,
            })],
            model: None,
        });
        let req = GoogleProvider::build_request(&model(), &ctx, &SimpleStreamOptions::default())
            .expect("build");
        let v = serde_json::to_value(&req).expect("serialize");
        let fr = &v["contents"][0]["parts"][0]["functionResponse"];
        // No prior assistant tool call → name falls back to the id.
        assert_eq!(fr["name"], "call_missing");
        assert_eq!(fr["response"]["error"], "boom");
    }

    #[test]
    fn user_image_becomes_inline_data() {
        let mut ctx = Context::new("");
        ctx.messages.push(Message {
            role: Role::User,
            content: vec![
                Content::text("look"),
                Content::Image(pi_protocol::ImageContent {
                    mime_type: "image/png".into(),
                    data: "AAAA".into(),
                }),
            ],
            model: None,
        });
        let req = GoogleProvider::build_request(&model(), &ctx, &SimpleStreamOptions::default())
            .expect("build");
        let v = serde_json::to_value(&req).expect("serialize");
        assert_eq!(v["contents"][0]["parts"][1]["inlineData"]["mimeType"], "image/png");
    }

    #[test]
    fn urlencode_escapes_path_segment() {
        assert_eq!(urlencode("gemini-2.5-flash"), "gemini-2.5-flash");
        assert_eq!(urlencode("a/b c"), "a%2Fb%20c");
    }

    #[test]
    fn builtin_catalog_has_three_models() {
        let models = builtin_google_models();
        assert_eq!(models.len(), 3);
        for m in &models {
            assert_eq!(m.api, pi_protocol::Api::GoogleGenerativeAi);
            assert_eq!(m.provider.0, "google");
            assert!(m.context_window > 0);
            assert!(m.max_output_tokens > 0);
        }
    }

    #[test]
    fn usage_mapping_folds_cache_and_thoughts() {
        let usage = WireUsage {
            prompt_token_count: 1000,
            candidates_token_count: 7,
            cached_content_token_count: 900,
            thoughts_token_count: 10,
            total_token_count: 1017,
        }
        .into_usage();
        assert_eq!(usage.input, 100);
        assert_eq!(usage.output, 17);
        assert_eq!(usage.cache_read, 900);
        assert_eq!(usage.cache_write, 0);
        assert_eq!(usage.total, 1017);
    }

    #[test]
    fn stop_reason_mapping() {
        assert_eq!(map_stop_reason("STOP"), StopReason::Stop);
        assert_eq!(map_stop_reason("MAX_TOKENS"), StopReason::MaxTokens);
        assert_eq!(map_stop_reason("SAFETY"), StopReason::Error);
        assert_eq!(map_stop_reason("SOMETHING_NEW"), StopReason::Error);
    }

    #[test]
    fn classify_http_status_returns_provider_variant() {
        match classify_http_status(429, "rate limited".into()) {
            StreamError::Provider { status, body } => {
                assert_eq!(status, 429);
                assert_eq!(body, "rate limited");
            }
            other => panic!("expected Provider error, got {other:?}"),
        }
    }
}
