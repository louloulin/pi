//! Google Gemini provider (Generative Language API).
//!
//! Implements [`StreamFn`] against the Gemini
//! `generativelanguage.googleapis.com` REST surface:
//!
//! ```text
//! POST {base_url}/models/{model}:streamGenerateContent?alt=sse&key={api_key}
//! ```
//!
//! The default `base_url` is
//! [`DEFAULT_BASE_URL`] (`https://generativelanguage.googleapis.com/v1beta`),
//! matching `packages/ai/src/providers/google.ts`. With `alt=sse` the
//! endpoint emits one SSE `data:` event per `GenerateContentResponse`
//! chunk; this adapter parses the byte stream into
//! [`AssistantMessageEvent`]s and accumulates the final
//! [`AssistantMessage`] into a single trailing
//! [`Done`](AssistantMessageEvent::Done) event.
//!
//! # Field mapping (Rust ↔ TS `google-generative-ai`)
//!
//! | Gemini wire field                                | Rust field / event                     |
//! | ------------------------------------------------ | -------------------------------------- |
//! | `candidates[].content.parts[].text`              | `AssistantMessageEvent::TextDelta`     |
//! | `candidates[].content.parts[].thought = true`    | `AssistantMessageEvent::ThinkingDelta` |
//! | `candidates[].content.parts[].functionCall`      | `Content::ToolCall` + `ToolCallDelta`  |
//! | `candidates[].finishReason = "STOP"`             | `StopReason::Stop` (→ `ToolUse`)       |
//! | `candidates[].finishReason = "MAX_TOKENS"`       | `StopReason::MaxTokens`                |
//! | `candidates[].finishReason = "SAFETY"` / …       | `StopReason::Error`                    |
//! | `usageMetadata.promptTokenCount`                 | `Usage::input` (− cached)              |
//! | `usageMetadata.candidatesTokenCount`             | `Usage::output` (+ thoughts)           |
//! | `usageMetadata.cachedContentTokenCount`          | `Usage::cache_read`                    |
//! | `usageMetadata.totalTokenCount`                  | `Usage::total`                         |
//! | `modelVersion`                                   | `AssistantMessage::model`              |
//!
//! Thought *signatures* (`thoughtSignature`) are not yet part of the
//! Rust wire types, so they are accepted and dropped — mirroring the
//! Anthropic adapter's handling of thinking signatures.
//!
//! # Native vs. WASM
//!
//! Native targets use `reqwest`; the `wasm32-unknown-unknown` target has
//! no usable HTTP client and returns [`StreamError::Malformed`] from
//! every call. The module itself compiles on wasm so the provider is
//! registered in the shared catalog without pulling `reqwest` in.

// Wire-format structs (the `Google*` / `GenerateContent*` types below)
// are exposed for inspection and fixture tests; their fields are
// documented inline via the upstream Gemini reference rather than via
// Rustdoc. Keep the allow in scope until each struct gets its own doc
// comment.
#![allow(missing_docs)]

use async_trait::async_trait;
use bytes::Bytes;
#[cfg(not(target_arch = "wasm32"))]
use futures::TryStreamExt;
use pi_protocol::{
    Api, AssistantMessage, AssistantMessageEvent, Content, Context, Model, ProviderId, Role,
    StopReason, TextContent, ToolCall, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::stream::AssistantMessageEventStream;
use crate::types::{SimpleStreamOptions, StreamError};
use crate::StreamFn;

/// Default base URL for the Gemini Generative Language API.
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Google Gemini provider.
///
/// Construct with [`GoogleProvider::new`] for the production endpoint,
/// or use [`GoogleProvider::with_base_url`] to point at a proxy / mirror
/// (the base URL must include the API version segment, e.g. `/v1beta`).
#[derive(Debug, Clone)]
pub struct GoogleProvider {
    /// API key. Sent both as the `?key=` query parameter (the form the
    /// upstream TS adapter documents) and the `x-goog-api-key` header
    /// the official SDK uses.
    pub api_key: String,
    /// Base URL with no trailing slash, including the API version
    /// (e.g. `https://generativelanguage.googleapis.com/v1beta`).
    pub base_url: String,
}

impl GoogleProvider {
    /// Create a provider pointing at the production Gemini endpoint.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Create a provider pointing at a custom base URL.
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
        }
    }

    /// Build the request body for `:streamGenerateContent`.
    ///
    /// Public so callers (and tests) can inspect the wire payload
    /// without making a network call.
    pub fn build_request(
        model: &Model,
        ctx: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<GenerateContentRequest, StreamError> {
        let contents = convert_contents(model, ctx)?;
        let system_instruction = if ctx.system_prompt.is_empty() {
            None
        } else {
            Some(SystemInstruction {
                parts: vec![GooglePart::text(&ctx.system_prompt)],
            })
        };
        let generation_config = if options.temperature.is_some() || options.max_tokens.is_some() {
            Some(GenerationConfig {
                temperature: options.temperature,
                max_output_tokens: options.max_tokens,
            })
        } else {
            None
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
        Ok(GenerateContentRequest {
            contents,
            system_instruction,
            generation_config,
            tools,
        })
    }

    /// POST `:streamGenerateContent?alt=sse` and pipe the SSE byte
    /// stream through [`parse_sse`].
    ///
    /// HTTP status errors map to [`StreamError::Provider`]; transport
    /// errors map to [`StreamError::Transport`]. The rate-limit case
    /// (HTTP 429) is preserved so callers can implement retry/back-off.
    #[cfg(not(target_arch = "wasm32"))]
    async fn send_streaming(
        &self,
        model: &Model,
        body: &GenerateContentRequest,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        let url = format!(
            "{}/models/{}:streamGenerateContent",
            self.base_url.trim_end_matches('/'),
            model.id
        );
        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .query(&[("alt", "sse"), ("key", self.api_key.as_str())])
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
            return Err(StreamError::Provider {
                status: status_code,
                body: truncate_body(&body),
            });
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
// Wire types — request
// ---------------------------------------------------------------------------

/// `generateContent` / `streamGenerateContent` request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateContentRequest {
    /// Conversation turns (user / model).
    pub contents: Vec<GoogleContent>,
    /// System prompt. Gemini carries it as a top-level field rather than
    /// a message in `contents`.
    #[serde(
        rename = "systemInstruction",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub system_instruction: Option<SystemInstruction>,
    /// Sampling / output-limit configuration.
    #[serde(
        rename = "generationConfig",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub generation_config: Option<GenerationConfig>,
    /// Tool declarations. Skipped when empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<GoogleToolGroup>>,
}

/// System instruction wrapper — a parts list with no `role`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInstruction {
    pub parts: Vec<GooglePart>,
}

/// Generation configuration subset the Rust port maps from
/// [`SimpleStreamOptions`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(rename = "maxOutputTokens", skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

/// One tool group — `{ "functionDeclarations": [ … ] }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleToolGroup {
    #[serde(rename = "functionDeclarations")]
    pub function_declarations: Vec<FunctionDeclaration>,
}

/// One function declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDeclaration {
    pub name: String,
    pub description: String,
    /// JSON Schema describing the parameters object. Gemini's v1beta
    /// endpoint accepts full JSON Schema under this key (the legacy
    /// `parameters` key only accepts the OpenAPI 3.0 subset).
    #[serde(rename = "parametersJsonSchema")]
    pub parameters_json_schema: Value,
}

/// One conversation turn in Gemini wire format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleContent {
    /// `"user"` or `"model"`. Absent on system instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Content parts.
    #[serde(default)]
    pub parts: Vec<GooglePart>,
}

/// One content part. The same shape covers requests (text / inlineData /
/// functionCall / functionResponse) and responses (text / thought /
/// thoughtSignature / functionCall).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GooglePart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Response-only: marks a text part as a thought summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought: Option<bool>,
    /// Thought signature — accepted and dropped (not in the Rust wire
    /// types yet).
    #[serde(
        rename = "thoughtSignature",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub thought_signature: Option<String>,
    #[serde(
        rename = "inlineData",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub inline_data: Option<InlineData>,
    #[serde(
        rename = "functionCall",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub function_call: Option<FunctionCall>,
    #[serde(
        rename = "functionResponse",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub function_response: Option<FunctionResponse>,
}

impl GooglePart {
    /// Construct a text part.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            ..Self::default()
        }
    }

    /// Construct an inline (base64) image part.
    pub fn inline_data(mime_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            inline_data: Some(InlineData {
                mime_type: mime_type.into(),
                data: data.into(),
            }),
            ..Self::default()
        }
    }

    /// Construct a function-call part.
    pub fn function_call(name: impl Into<String>, args: Value, id: Option<String>) -> Self {
        Self {
            function_call: Some(FunctionCall {
                name: name.into(),
                args: Some(args),
                id,
            }),
            ..Self::default()
        }
    }

    /// Construct a function-response part.
    pub fn function_response(name: impl Into<String>, response: Value, id: Option<String>) -> Self {
        Self {
            function_response: Some(FunctionResponse {
                name: name.into(),
                response: Some(response),
                id,
            }),
            ..Self::default()
        }
    }

    /// True when the part is a function response (used to group tool
    /// results into a single Gemini user turn).
    fn is_function_response(&self) -> bool {
        self.function_response.is_some()
    }
}

/// Inline image payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineData {
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub data: String,
}

/// Function call emitted by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    /// Echoed back for Gemini 3 / Claude / gpt-oss models only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// Function result echoed back to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionResponse {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

// ---------------------------------------------------------------------------
// Wire types — response
// ---------------------------------------------------------------------------

/// One streaming `GenerateContentResponse` chunk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenerateContentResponse {
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    #[serde(default, rename = "usageMetadata")]
    pub usage_metadata: Option<UsageMetadata>,
    #[serde(default, rename = "modelVersion")]
    pub model_version: Option<String>,
    #[serde(default, rename = "promptFeedback")]
    pub prompt_feedback: Option<PromptFeedback>,
    #[serde(default, rename = "responseId")]
    pub response_id: Option<String>,
    /// Populated when the HTTP body is an error envelope rather than a
    /// candidate list.
    #[serde(default)]
    pub error: Option<GoogleError>,
}

/// One generation candidate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Candidate {
    #[serde(default)]
    pub content: Option<GoogleContent>,
    #[serde(default, rename = "finishReason")]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub index: Option<u32>,
}

/// Token usage metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageMetadata {
    #[serde(default, rename = "promptTokenCount")]
    pub prompt_token_count: u32,
    #[serde(default, rename = "candidatesTokenCount")]
    pub candidates_token_count: u32,
    #[serde(default, rename = "thoughtsTokenCount")]
    pub thoughts_token_count: u32,
    #[serde(default, rename = "cachedContentTokenCount")]
    pub cached_content_token_count: u32,
    #[serde(default, rename = "toolUsePromptTokenCount")]
    pub tool_use_prompt_token_count: u32,
    #[serde(default, rename = "totalTokenCount")]
    pub total_token_count: u32,
}

/// Prompt-level feedback (safety blocking and friends).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptFeedback {
    #[serde(default, rename = "blockReason")]
    pub block_reason: Option<String>,
}

/// Error envelope returned by the Generative Language API.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoogleError {
    #[serde(default)]
    pub code: Option<u32>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

// ---------------------------------------------------------------------------
// Message conversion
// ---------------------------------------------------------------------------

/// Convert the internal [`Context`] into Gemini `contents`.
fn convert_contents(model: &Model, ctx: &Context) -> Result<Vec<GoogleContent>, StreamError> {
    // Gemini's `functionResponse` needs the function *name*, but the
    // internal `ToolResult` only carries the originating call id. Build
    // an id → name map from the assistant turns first.
    let mut tool_names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for msg in &ctx.messages {
        for block in &msg.content {
            if let Content::ToolCall(call) = block {
                tool_names.insert(call.id.clone(), call.name.clone());
            }
        }
    }

    let normalize = requires_tool_call_id(&model.id);
    let mut contents: Vec<GoogleContent> = Vec::new();

    for msg in &ctx.messages {
        match msg.role {
            // The system prompt travels in `systemInstruction`, so a
            // stray system message in the array is dropped.
            Role::System => {}
            Role::User => {
                let mut parts = Vec::with_capacity(msg.content.len());
                for block in &msg.content {
                    match block {
                        Content::Text(t) => parts.push(GooglePart::text(&t.text)),
                        Content::Image(img) => {
                            parts.push(GooglePart::inline_data(&img.mime_type, &img.data));
                        }
                        _ => {}
                    }
                }
                if parts.is_empty() {
                    continue;
                }
                contents.push(GoogleContent {
                    role: Some("user".into()),
                    parts,
                });
            }
            Role::Assistant => {
                let mut parts = Vec::with_capacity(msg.content.len());
                for block in &msg.content {
                    match block {
                        Content::Text(t) => parts.push(GooglePart::text(&t.text)),
                        Content::ToolCall(call) => parts.push(GooglePart::function_call(
                            &call.name,
                            call.arguments.clone(),
                            if normalize {
                                Some(call.id.clone())
                            } else {
                                None
                            },
                        )),
                        _ => {
                            return Err(StreamError::Malformed(
                                "assistant message may only contain text or tool calls".into(),
                            ));
                        }
                    }
                }
                if parts.is_empty() {
                    continue;
                }
                contents.push(GoogleContent {
                    role: Some("model".into()),
                    parts,
                });
            }
            Role::Tool => {
                let mut result: Option<&pi_protocol::ToolResult> = None;
                let mut text = String::new();
                for block in &msg.content {
                    match block {
                        Content::ToolResult(r) => {
                            if let Content::Text(t) = &*r.content {
                                text.push_str(&t.text);
                            }
                            result = Some(r);
                        }
                        Content::Text(t) => text.push_str(&t.text),
                        _ => {}
                    }
                }
                let Some(result) = result else {
                    return Err(StreamError::Malformed(
                        "tool message missing tool result".into(),
                    ));
                };
                let name = tool_names
                    .get(&result.tool_call_id)
                    .cloned()
                    .unwrap_or_else(|| result.tool_call_id.clone());
                let response = if result.is_error {
                    serde_json::json!({ "error": text })
                } else {
                    serde_json::json!({ "output": text })
                };
                let part = GooglePart::function_response(
                    name,
                    response,
                    if normalize {
                        Some(result.tool_call_id.clone())
                    } else {
                        None
                    },
                );

                // The API requires all function responses for a turn to
                // be grouped in a single user message — merge with the
                // previous user turn when it already carries responses.
                match contents.last_mut() {
                    Some(last)
                        if last.role.as_deref() == Some("user")
                            && last.parts.iter().any(GooglePart::is_function_response) =>
                    {
                        last.parts.push(part);
                    }
                    _ => contents.push(GoogleContent {
                        role: Some("user".into()),
                        parts: vec![part],
                    }),
                }
            }
        }
    }

    Ok(contents)
}

/// Models that require explicit tool-call ids in function calls /
/// responses (Gemini 3+, Claude and gpt-oss served through Google).
fn requires_tool_call_id(model_id: &str) -> bool {
    let lower = model_id.to_lowercase();
    if lower.starts_with("claude-") || lower.starts_with("gpt-oss-") {
        return true;
    }
    gemini_major_version(&lower).is_some_and(|major| major >= 3)
}

fn gemini_major_version(model_id: &str) -> Option<u32> {
    let rest = model_id
        .strip_prefix("gemini-live-")
        .or_else(|| model_id.strip_prefix("gemini-"))?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

// ---------------------------------------------------------------------------
// SSE parser
// ---------------------------------------------------------------------------

/// Parse a Gemini SSE byte stream into an
/// [`AssistantMessageEventStream`].
///
/// Each event is shaped like:
///
/// ```text
/// data: {"candidates":[{"content":{"parts":[{"text":"Hi"}]}}]}
/// ```
///
/// Every `data:` payload is a full `GenerateContentResponse`; a payload
/// may also be a JSON array of responses (some proxies batch them).
/// `event:` lines, ids and comments are ignored. A payload carrying a
/// top-level `error` is surfaced as [`StreamError::Malformed`].
pub fn parse_sse(
    bytes: impl futures::Stream<Item = Result<Bytes, StreamError>> + Send + 'static,
    model_id: String,
) -> AssistantMessageEventStream {
    Box::pin(GoogleSseStream::new(bytes, model_id))
}

struct GoogleSseStream {
    inner: std::pin::Pin<Box<dyn futures::Stream<Item = Result<Bytes, StreamError>> + Send>>,
    /// Half-parsed SSE buffer (bytes from the most recent chunk).
    line_buffer: Vec<u8>,
    /// Buffered `data:` lines for the in-flight event.
    data_lines: Vec<String>,
    /// Pending events produced by the current chunk but not yet yielded.
    pending: std::collections::VecDeque<Result<AssistantMessageEvent, StreamError>>,
    /// True once the stream sent its final `Done`.
    finished: bool,
    /// True once we've emitted the initial `Start`.
    started: bool,
    model_id: String,
    state: GoogleState,
}

#[derive(Default)]
struct GoogleState {
    /// Finalised content blocks, in emission order.
    content: Vec<Content>,
    /// Text / thinking block currently being accumulated.
    current: Option<CurrentBlock>,
    /// Whether a `functionCall` part has been seen (drives the default
    /// stop reason when the upstream sends `STOP`).
    saw_tool_use: bool,
    /// Stop reason set by `finishReason` / `promptFeedback`.
    stop_reason: Option<StopReason>,
    /// Raw `finishReason` string, kept for diagnostics.
    raw_stop_reason: Option<String>,
    /// Final assistant message model name (`modelVersion`).
    model: Option<String>,
    /// Token usage accumulated so far.
    usage: Usage,
    /// Monotonic counter for synthesised tool-call ids.
    tool_call_counter: u64,
}

enum CurrentBlock {
    Text(String),
    Thinking(String),
}

impl CurrentBlock {
    fn push_str(&mut self, s: &str) {
        match self {
            CurrentBlock::Text(buf) | CurrentBlock::Thinking(buf) => buf.push_str(s),
        }
    }

    fn is_thinking(&self) -> bool {
        matches!(self, CurrentBlock::Thinking(_))
    }
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
            state: GoogleState::default(),
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
        if field == b"data" {
            self.data_lines.push(String::from_utf8_lossy(value).into_owned());
        }
        Ok(())
    }

    fn dispatch_event(&mut self) -> Result<(), StreamError> {
        let payload = self.data_lines.join("\n");
        self.data_lines.clear();
        if payload.is_empty() {
            return Ok(());
        }
        let trimmed = payload.trim_start();
        if trimmed.starts_with('[') {
            let chunks: Vec<GenerateContentResponse> = serde_json::from_str(&payload)
                .map_err(|e| StreamError::Malformed(format!("Google chunk array JSON: {e}")))?;
            for chunk in chunks {
                self.handle_chunk(chunk)?;
            }
        } else {
            let chunk: GenerateContentResponse = serde_json::from_str(&payload)
                .map_err(|e| StreamError::Malformed(format!("Google chunk JSON: {e}: {payload}")))?;
            self.handle_chunk(chunk)?;
        }
        Ok(())
    }

    fn ensure_started(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        let model = self
            .state
            .model
            .clone()
            .unwrap_or_else(|| self.model_id.clone());
        self.pending
            .push_back(Ok(AssistantMessageEvent::Start { model }));
    }

    fn handle_chunk(&mut self, chunk: GenerateContentResponse) -> Result<(), StreamError> {
        if let Some(error) = chunk.error {
            let message = error
                .message
                .unwrap_or_else(|| "Google returned an error event".to_string());
            self.pending.push_back(Err(StreamError::Malformed(format!(
                "Google error event: {message}"
            ))));
            return Ok(());
        }

        if let Some(version) = &chunk.model_version {
            self.state.model = Some(version.clone());
        }
        self.ensure_started();

        if let Some(feedback) = chunk.prompt_feedback {
            if let Some(reason) = feedback.block_reason {
                self.state.stop_reason = Some(StopReason::Error);
                self.state.raw_stop_reason = Some(reason);
            }
        }

        for candidate in chunk.candidates {
            if let Some(content) = candidate.content {
                for part in content.parts {
                    self.handle_part(part);
                }
            }
            if let Some(reason) = candidate.finish_reason {
                self.state.raw_stop_reason = Some(reason.clone());
                self.state.stop_reason = Some(map_finish_reason(&reason));
            }
        }

        if let Some(usage) = chunk.usage_metadata {
            self.apply_usage(usage);
        }
        Ok(())
    }

    fn handle_part(&mut self, part: GooglePart) {
        if let Some(text) = part.text {
            if text.is_empty() {
                return;
            }
            let is_thinking = part.thought.unwrap_or(false);
            self.ensure_block(is_thinking);
            if let Some(current) = self.state.current.as_mut() {
                current.push_str(&text);
            }
            let event = if is_thinking {
                AssistantMessageEvent::ThinkingDelta { delta: text }
            } else {
                AssistantMessageEvent::TextDelta { delta: text }
            };
            self.pending.push_back(Ok(event));
            return;
        }

        if let Some(call) = part.function_call {
            self.flush_current();
            let name = call.name;
            let args = call
                .args
                .unwrap_or_else(|| Value::Object(Default::default()));
            let id = self.next_tool_call_id(call.id.as_deref(), &name);
            let arguments_delta =
                serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string());
            let index = self.state.content.len() as u32;
            self.state.content.push(Content::ToolCall(ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: args,
            }));
            self.state.saw_tool_use = true;
            self.pending
                .push_back(Ok(AssistantMessageEvent::ToolCallDelta {
                    index,
                    id: Some(id),
                    name: Some(name),
                    arguments_delta: Some(arguments_delta),
                }));
        }
    }

    fn ensure_block(&mut self, thinking: bool) {
        let matches = match self.state.current.as_ref() {
            Some(current) => current.is_thinking() == thinking,
            None => false,
        };
        if !matches {
            self.flush_current();
            self.state.current = Some(if thinking {
                CurrentBlock::Thinking(String::new())
            } else {
                CurrentBlock::Text(String::new())
            });
        }
    }

    fn flush_current(&mut self) {
        match self.state.current.take() {
            Some(CurrentBlock::Text(text)) => {
                if !text.is_empty() {
                    self.state.content.push(Content::Text(TextContent { text }));
                }
            }
            // Thinking blocks carry no `Content` variant yet, so they are
            // streamed as deltas only — matching the Anthropic adapter.
            Some(CurrentBlock::Thinking(_)) | None => {}
        }
    }

    fn next_tool_call_id(&mut self, provided: Option<&str>, name: &str) -> String {
        if let Some(id) = provided {
            if !id.is_empty()
                && !self
                    .state
                    .content
                    .iter()
                    .any(|c| matches!(c, Content::ToolCall(t) if t.id == id))
            {
                return id.to_string();
            }
        }
        self.state.tool_call_counter += 1;
        format!("{name}_{}", self.state.tool_call_counter)
    }

    fn apply_usage(&mut self, usage: UsageMetadata) {
        let cached = usage.cached_content_token_count;
        // Mirror the TS adapter: input excludes cached tokens, output
        // folds in the thought-token count.
        let input = usage.prompt_token_count.saturating_sub(cached);
        let output = usage
            .candidates_token_count
            .saturating_add(usage.thoughts_token_count);
        let total = if usage.total_token_count > 0 {
            usage.total_token_count
        } else {
            input.saturating_add(output)
        };
        self.state.usage = Usage {
            input,
            output,
            cache_read: cached,
            cache_write: 0,
            total,
        };
    }

    fn finalize(&mut self) {
        self.flush_current();
        let mut stop_reason = self.state.stop_reason.unwrap_or({
            if self.state.saw_tool_use {
                StopReason::ToolUse
            } else if self.state.content.is_empty() {
                StopReason::Empty
            } else {
                StopReason::Stop
            }
        });
        // Gemini reports `STOP` for tool-call turns; the agent loop needs
        // `ToolUse` to know it must execute the call.
        if self.state.saw_tool_use && stop_reason == StopReason::Stop {
            stop_reason = StopReason::ToolUse;
        }
        let usage = self.state.usage;
        let model = self
            .state
            .model
            .clone()
            .unwrap_or_else(|| self.model_id.clone());
        let message = AssistantMessage {
            model,
            content: std::mem::take(&mut self.state.content),
            stop_reason,
            usage,
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

/// Map a Gemini `finishReason` string to the internal [`StopReason`].
///
/// Unknown reasons degrade to [`StopReason::Stop`] so new upstream
/// values stay forward compatible.
pub fn map_finish_reason(reason: &str) -> StopReason {
    match reason {
        "STOP" => StopReason::Stop,
        "MAX_TOKENS" => StopReason::MaxTokens,
        "SAFETY"
        | "RECITATION"
        | "BLOCKLIST"
        | "PROHIBITED_CONTENT"
        | "SPII"
        | "IMAGE_SAFETY"
        | "IMAGE_PROHIBITED_CONTENT"
        | "IMAGE_RECITATION"
        | "IMAGE_OTHER"
        | "LANGUAGE"
        | "MALFORMED_FUNCTION_CALL"
        | "UNEXPECTED_TOOL_CALL"
        | "NO_IMAGE"
        | "FINISH_REASON_UNSPECIFIED"
        | "OTHER" => StopReason::Error,
        other => {
            tracing::debug!(reason = %other, "Google returned unknown finishReason");
            StopReason::Stop
        }
    }
}

impl futures::Stream for GoogleSseStream {
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
                        self.ensure_started();
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
// Model catalog
// ---------------------------------------------------------------------------

/// Per-1M-token pricing for a Gemini model.
///
/// The shared [`Model`] descriptor does not carry cost fields yet, so the
/// catalog keeps them alongside each entry (mirrors the TS
/// `google.models.ts` cost metadata).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GooglePricing {
    /// USD per 1M input tokens.
    pub input: f64,
    /// USD per 1M output tokens.
    pub output: f64,
    /// USD per 1M cached input tokens.
    #[serde(default)]
    pub cache_read: f64,
    /// USD per 1M cache-write tokens.
    #[serde(default)]
    pub cache_write: f64,
}

/// One model in the Gemini catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleModelEntry {
    /// Model identifier (e.g. `gemini-2.5-flash`).
    pub id: String,
    /// Human-readable label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Context window size in tokens.
    #[serde(default)]
    pub context_window: u32,
    /// Maximum output tokens.
    #[serde(default)]
    pub max_output_tokens: u32,
    /// Whether the model supports thinking / reasoning.
    #[serde(default)]
    pub reasoning: bool,
    /// Per-1M-token pricing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<GooglePricing>,
}

impl GoogleModelEntry {
    fn into_model(self, provider: ProviderId) -> Model {
        Model {
            provider,
            id: self.id,
            api: Api::GoogleGenerativeAi,
            label: self.label,
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
        }
    }
}

/// JSON envelope describing the Gemini catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleCatalog {
    /// Provider identifier — typically `"google"`.
    pub provider: String,
    /// Models in the catalog.
    pub models: Vec<GoogleModelEntry>,
}

impl GoogleCatalog {
    /// Convert the catalog into the [`Model`] descriptors the shared
    /// [`crate::models::Models`] registry stores.
    pub fn into_models(self) -> Vec<Model> {
        let provider = ProviderId::new(self.provider.clone());
        self.models
            .into_iter()
            .map(|m| m.into_model(provider.clone()))
            .collect()
    }
}

/// Parse a Gemini catalog JSON document into a list of [`Model`]
/// descriptors suitable for [`crate::models::Models::set_provider`].
pub fn catalog_from_json(json_str: &str) -> Result<Vec<Model>, StreamError> {
    let catalog: GoogleCatalog = serde_json::from_str(json_str)
        .map_err(|e| StreamError::Malformed(format!("Google catalog JSON: {e}")))?;
    Ok(catalog.into_models())
}

/// Built-in Gemini catalog — the models Stage 13 ships by default, with
/// context window / max output / pricing metadata matching the upstream
/// generated catalog.
pub fn builtin_gemini_catalog() -> GoogleCatalog {
    GoogleCatalog {
        provider: "google".into(),
        models: vec![
            GoogleModelEntry {
                id: "gemini-2.5-pro".into(),
                label: Some("Gemini 2.5 Pro".into()),
                context_window: 1_048_576,
                max_output_tokens: 65_536,
                reasoning: true,
                pricing: Some(GooglePricing {
                    input: 1.25,
                    output: 10.0,
                    cache_read: 0.3125,
                    cache_write: 0.0,
                }),
            },
            GoogleModelEntry {
                id: "gemini-2.5-flash".into(),
                label: Some("Gemini 2.5 Flash".into()),
                context_window: 1_048_576,
                max_output_tokens: 65_536,
                reasoning: true,
                pricing: Some(GooglePricing {
                    input: 0.30,
                    output: 2.50,
                    cache_read: 0.075,
                    cache_write: 0.0,
                }),
            },
            GoogleModelEntry {
                id: "gemini-2.5-flash-lite".into(),
                label: Some("Gemini 2.5 Flash-Lite".into()),
                context_window: 1_048_576,
                max_output_tokens: 65_536,
                reasoning: true,
                pricing: Some(GooglePricing {
                    input: 0.10,
                    output: 0.40,
                    cache_read: 0.025,
                    cache_write: 0.0,
                }),
            },
            GoogleModelEntry {
                id: "gemini-2.0-flash".into(),
                label: Some("Gemini 2.0 Flash".into()),
                context_window: 1_048_576,
                max_output_tokens: 8_192,
                reasoning: false,
                pricing: Some(GooglePricing {
                    input: 0.10,
                    output: 0.40,
                    cache_read: 0.025,
                    cache_write: 0.0,
                }),
            },
        ],
    }
}

/// Built-in Gemini models as [`Model`] descriptors. Used by the CLI to
/// seed `--model google/gemini-2.5-flash`.
pub fn builtin_gemini_models() -> Vec<Model> {
    builtin_gemini_catalog().into_models()
}

// ---------------------------------------------------------------------------
// Tests — private helper coverage (end-to-end tests live in tests/google.rs)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use pi_protocol::Message;

    #[test]
    fn requires_tool_call_id_matches_upstream_rules() {
        assert!(!requires_tool_call_id("gemini-2.5-flash"));
        assert!(requires_tool_call_id("gemini-3-pro"));
        assert!(requires_tool_call_id("claude-sonnet-4-5"));
        assert!(requires_tool_call_id("gpt-oss-120b"));
    }

    #[test]
    fn gemini_major_version_parses_ids() {
        assert_eq!(gemini_major_version("gemini-2.5-flash"), Some(2));
        assert_eq!(gemini_major_version("gemini-3-pro"), Some(3));
        assert_eq!(gemini_major_version("gemini-live-2.5-flash"), Some(2));
        assert_eq!(gemini_major_version("claude-sonnet-4-5"), None);
    }

    #[test]
    fn finish_reason_mapping_covers_error_families() {
        assert_eq!(map_finish_reason("STOP"), StopReason::Stop);
        assert_eq!(map_finish_reason("MAX_TOKENS"), StopReason::MaxTokens);
        assert_eq!(map_finish_reason("SAFETY"), StopReason::Error);
        assert_eq!(map_finish_reason("MALFORMED_FUNCTION_CALL"), StopReason::Error);
        assert_eq!(map_finish_reason("SOMETHING_NEW"), StopReason::Stop);
    }

    #[test]
    fn tool_result_resolves_function_name_from_prior_call() {
        let model = Model {
            provider: ProviderId::new("google"),
            id: "gemini-2.5-flash".into(),
            api: Api::GoogleGenerativeAi,
            label: None,
            context_window: 1_048_576,
            max_output_tokens: 65_536,
        };
        let mut ctx = Context::new("");
        ctx.messages.push(Message {
            role: Role::Assistant,
            content: vec![Content::ToolCall(ToolCall {
                id: "call_1".into(),
                name: "get_weather".into(),
                arguments: serde_json::json!({"city": "SF"}),
            })],
            model: None,
        });
        ctx.messages.push(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(pi_protocol::ToolResult {
                tool_call_id: "call_1".into(),
                content: Box::new(Content::text("72F")),
                is_error: false,
                details: None,
            })],
            model: None,
        });
        let contents = convert_contents(&model, &ctx).expect("convert");
        let last = contents.last().expect("tool turn");
        let fr = last.parts[0]
            .function_response
            .as_ref()
            .expect("function response");
        assert_eq!(fr.name, "get_weather");
        assert_eq!(fr.response.as_ref().unwrap()["output"], "72F");
        assert!(fr.id.is_none(), "gemini-2.5 does not require call ids");
    }
}
