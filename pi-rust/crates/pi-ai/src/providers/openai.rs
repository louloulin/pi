//! OpenAI Chat Completions provider.
//!
//! Implements the [`StreamFn`] trait against the OpenAI Chat Completions
//! HTTP API (`POST /v1/chat/completions`). The streaming endpoint emits
//! `data: {...}` Server-Sent Events; this adapter parses the byte stream
//! into [`AssistantMessageEvent`]s and accumulates the final
//! [`AssistantMessage`] into a single trailing
//! [`Done`](AssistantMessageEvent::Done).
//!
//! native targets use `reqwest`; the `wasm32-unknown-unknown` target has
//! no usable HTTP client in Stage 1 and returns
//! [`StreamError::Malformed`] from every call. Stage 4 (browser host)
//! will replace this with a `fetch`-based adapter.

// Wire-format structs (the `Chat*` types below) are exposed for
// inspection and fixture tests; their fields are documented inline via
// the upstream OpenAI reference rather than via Rustdoc. Keep the allow
// in scope until each struct gets its own doc comment.
#![allow(missing_docs)]

use async_trait::async_trait;
use bytes::Bytes;
use futures::{stream, TryStreamExt};
use pi_protocol::{
    AssistantMessage, AssistantMessageEvent, Content, Context, Message, Model, Role, StopReason,
    TextContent, ToolCall, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::stream::AssistantMessageEventStream;
use crate::types::{SimpleStreamOptions, StreamError};
use crate::StreamFn;

/// Default base URL for OpenAI Chat Completions.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// OpenAI Chat Completions provider.
///
/// Construct with [`OpenAiProvider::new`] for the production endpoint, or
/// use [`OpenAiProvider::with_base_url`] to point at a compatible mirror
/// (Azure deployment, local `vllm`, `Ollama`'s OpenAI shim, …).
#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    /// Bearer token sent in the `Authorization` header.
    pub api_key: String,
    /// Base URL with no trailing slash. Must include the `/v1` prefix.
    pub base_url: String,
}

impl OpenAiProvider {
    /// Create a provider pointing at the production OpenAI endpoint.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Create a provider pointing at a custom base URL (Azure, local mirror, …).
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
        }
    }

    /// Build the request body for the chat-completions endpoint.
    pub fn build_request(
        model: &Model,
        ctx: &Context,
        options: &SimpleStreamOptions,
        stream: bool,
    ) -> Result<ChatRequest, StreamError> {
        let mut messages = Vec::with_capacity(ctx.messages.len() + 1);
        if !ctx.system_prompt.is_empty() {
            messages.push(ChatMessage {
                role: "system".into(),
                content: Some(ctx.system_prompt.clone()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }
        for msg in &ctx.messages {
            messages.push(chat_message_from(msg)?);
        }
        let tools = if ctx.tools.is_empty() {
            None
        } else {
            Some(
                ctx.tools
                    .iter()
                    .map(|t| ChatTool {
                        kind: "function".into(),
                        function: ChatToolFunction {
                            name: t.name.clone(),
                            description: t.description.clone(),
                            parameters: t.parameters.clone(),
                        },
                    })
                    .collect(),
            )
        };
        Ok(ChatRequest {
            model: model.id.clone(),
            messages,
            tools,
            stream,
            temperature: options.temperature,
            max_tokens: options.max_tokens,
            stream_options: if stream {
                Some(StreamOptions {
                    include_usage: true,
                })
            } else {
                None
            },
        })
    }

    /// POST `/chat/completions` with `stream: true` and pipe the SSE
    /// byte stream through [`parse_sse`].
    #[cfg(not(target_arch = "wasm32"))]
    async fn send_streaming(
        &self,
        body: &ChatRequest,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
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

    /// POST `/chat/completions` with `stream: false` and wrap the
    /// response into a single Done event.
    #[cfg(not(target_arch = "wasm32"))]
    async fn send_once(&self, body: &ChatRequest) -> Result<ChatResponse, StreamError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(StreamError::Provider {
                status: status.as_u16(),
                body: truncate_body(&body),
            });
        }
        Ok(response.json::<ChatResponse>().await?)
    }
}

#[async_trait]
impl StreamFn for OpenAiProvider {
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
                // Some upstream proxies reject `stream: true`. Fall back
                // to a one-shot request so callers still get a Done event.
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
                "OpenAiProvider is not yet implemented for wasm32-unknown-unknown".into(),
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

/// Chat Completions request body.
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    /// Model identifier (`gpt-4o-mini`, `o3-mini`, …).
    pub model: String,
    /// Conversation messages.
    pub messages: Vec<ChatMessage>,
    /// Tool descriptors. Skipped when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ChatTool>>,
    /// Whether to stream the response.
    pub stream: bool,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Maximum output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// `include_usage: true` so the SSE stream ends with a usage chunk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamOptions {
    /// Include a final chunk with the prompt + completion token usage.
    pub include_usage: bool,
}

/// One chat message in OpenAI wire format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// `system` | `user` | `assistant` | `tool`.
    pub role: String,
    /// String content or null for tool-call-only assistant turns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Assistant tool calls (only present on assistant turns).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ChatToolCall>>,
    /// Tool result identifier (only present on tool turns).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Tool/function name (only present on tool turns).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// One tool descriptor in OpenAI wire format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTool {
    /// Tool type — always `function` today.
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ChatToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolFunction {
    pub name: String,
    pub description: String,
    /// JSON Schema describing the parameters object.
    pub parameters: Value,
}

/// In-flight tool call assembled from streaming deltas.
#[derive(Debug, Clone, Default)]
struct PendingToolCall {
    id: Option<String>,
    name: Option<String>,
    /// Arguments fragment, accumulated verbatim.
    arguments: String,
}

/// A single tool call in OpenAI wire format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolCall {
    pub index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub function: ChatToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolCallFunction {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Arguments as a (possibly partial) JSON string.
    #[serde(default)]
    pub arguments: String,
}

/// Chat completion response (non-streaming).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub id: String,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    #[serde(default)]
    pub usage: Option<ChatUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatMessage,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
}

impl ChatResponse {
    /// Convert into the final [`AssistantMessage`].
    fn into_assistant_message(self, fallback_model: &str) -> Result<AssistantMessage, StreamError> {
        let mut content = Vec::new();
        let mut stop_reason = StopReason::Stop;
        for choice in self.choices.into_iter() {
            if let Some(text) = choice.message.content.as_deref() {
                if !text.is_empty() {
                    content.push(Content::Text(TextContent {
                        text: text.to_string(),
                    }));
                }
            }
            if let Some(calls) = choice.message.tool_calls {
                for call in calls {
                    content.push(Content::ToolCall(into_tool_call(call)?));
                }
            }
            match choice.finish_reason.as_deref() {
                Some("stop") => stop_reason = StopReason::Stop,
                Some("length") => stop_reason = StopReason::MaxTokens,
                Some("tool_calls") => stop_reason = StopReason::ToolUse,
                Some("content_filter") | Some("error") => stop_reason = StopReason::Error,
                _ => {}
            }
        }
        let usage = self
            .usage
            .map(|u| Usage {
                input: u.prompt_tokens,
                output: u.completion_tokens,
                total: u.total_tokens,
                ..Usage::default()
            })
            .unwrap_or_default();
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

fn into_tool_call(call: ChatToolCall) -> Result<ToolCall, StreamError> {
    let id = call
        .id
        .ok_or_else(|| StreamError::Malformed("tool_call missing id".into()))?;
    let name = call
        .function
        .name
        .ok_or_else(|| StreamError::Malformed("tool_call missing function.name".into()))?;
    let arguments = if call.function.arguments.is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_str(&call.function.arguments)
            .map_err(|e| StreamError::Malformed(format!("tool_call arguments JSON: {e}")))?
    };
    Ok(ToolCall {
        id,
        name,
        arguments,
    })
}

fn chat_message_from(msg: &Message) -> Result<ChatMessage, StreamError> {
    match msg.role {
        Role::System | Role::User => {
            let text = msg
                .content
                .iter()
                .filter_map(|c| match c {
                    Content::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            Ok(ChatMessage {
                role: match msg.role {
                    Role::System => "system",
                    Role::User => "user",
                    _ => unreachable!(),
                }
                .into(),
                content: Some(text),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            })
        }
        Role::Assistant => {
            let mut text = String::new();
            let mut tool_calls = Vec::new();
            for c in &msg.content {
                match c {
                    Content::Text(t) => text.push_str(&t.text),
                    Content::ToolCall(call) => {
                        tool_calls.push(ChatToolCall {
                            index: tool_calls.len() as u32,
                            id: Some(call.id.clone()),
                            kind: Some("function".into()),
                            function: ChatToolCallFunction {
                                name: Some(call.name.clone()),
                                arguments: serde_json::to_string(&call.arguments)
                                    .map_err(|e| StreamError::Malformed(e.to_string()))?,
                            },
                        });
                    }
                    _ => {
                        return Err(StreamError::Malformed(
                            "assistant message may only contain text or tool calls".into(),
                        ));
                    }
                }
            }
            Ok(ChatMessage {
                role: "assistant".into(),
                content: if text.is_empty() { None } else { Some(text) },
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
                tool_call_id: None,
                name: None,
            })
        }
        Role::Tool => {
            let tool_call_id = msg
                .content
                .iter()
                .find_map(|c| match c {
                    Content::ToolResult(r) => Some(r.tool_call_id.clone()),
                    _ => None,
                })
                .ok_or_else(|| StreamError::Malformed("tool message missing tool result".into()))?;
            // The agent loop wraps every tool result in
            // `Content::ToolResult`, so only scanning bare `Content::Text`
            // blocks would send the model an empty `content` string: the
            // tool would run but the model could never react to its output.
            // Bare text blocks are still honoured for hand-built contexts.
            let mut text = String::new();
            for c in &msg.content {
                match c {
                    Content::Text(t) => text.push_str(&t.text),
                    Content::ToolResult(r) => match &*r.content {
                        Content::Text(t) => text.push_str(&t.text),
                        other => text.push_str(&serde_json::to_string(other).unwrap_or_default()),
                    },
                    _ => {}
                }
            }
            Ok(ChatMessage {
                role: "tool".into(),
                content: Some(text),
                tool_calls: None,
                tool_call_id: Some(tool_call_id),
                name: None,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// SSE parser
// ---------------------------------------------------------------------------

/// Parse an OpenAI SSE byte stream into an [`AssistantMessageEventStream`].
///
/// Each event payload is one JSON object shaped like:
///
/// ```text
/// data: {"id":"…","choices":[{"index":0,"delta":{"role":"assistant","content":"hi"}}]}
/// ```
///
/// Lines that don't start with `data:` are ignored (event names, ids,
/// empty heartbeats). A `data: [DONE]` sentinel ends the stream.
/// Malformed JSON or unexpected shapes surface as
/// [`StreamError::Malformed`].
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
    /// Assistant message content accumulated so far.
    content: Vec<Content>,
    /// In-flight tool calls keyed by their `index` field.
    pending_tool_calls: std::collections::HashMap<u32, PendingToolCall>,
    /// Running usage totals.
    usage: Usage,
    /// Stop reason set by the last `finish_reason`.
    stop_reason: Option<StopReason>,
}

impl SseStream {
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
        // Split into lines on `\n` or `\r\n`. Carriage returns without
        // a following newline terminate a line too (per the SSE spec).
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
        match field {
            b"data" => {
                // OpenAI's SSE encodes the literal text on a single
                // line — multiple `data:` lines in one event are joined
                // with newlines (per the spec). We follow that rule.
                let value = String::from_utf8_lossy(value).into_owned();
                self.data_lines.push(value);
            }
            b"event" | b"id" | b"retry" => {
                // We only care about `data:`.
            }
            _ => {}
        }
        Ok(())
    }

    fn dispatch_event(&mut self) -> Result<(), StreamError> {
        if self.data_lines.is_empty() {
            return Ok(());
        }
        let payload = self.data_lines.join("\n");
        self.data_lines.clear();
        if payload.trim() == "[DONE]" {
            // End-of-stream sentinel — nothing to parse, just leave the
            // state alone so the next poll finalises the stream.
            return Ok(());
        }
        self.handle_chunk(&payload)
    }

    fn handle_chunk(&mut self, data: &str) -> Result<(), StreamError> {
        let chunk: StreamChunk = serde_json::from_str(data)
            .map_err(|e| StreamError::Malformed(format!("SSE JSON: {e}: {data}")))?;

        if !self.started {
            self.started = true;
            self.pending.push_back(Ok(AssistantMessageEvent::Start {
                model: self.model_id.clone(),
            }));
        }

        if let Some(usage) = chunk.usage {
            self.state.usage = Usage {
                input: usage.prompt_tokens,
                output: usage.completion_tokens,
                total: usage.total_tokens,
                ..Usage::default()
            };
        }

        for choice in chunk.choices {
            if let Some(reason) = choice.finish_reason.as_deref() {
                self.state.stop_reason = Some(match reason {
                    "stop" => StopReason::Stop,
                    "length" => StopReason::MaxTokens,
                    "tool_calls" => StopReason::ToolUse,
                    "content_filter" | "error" => StopReason::Error,
                    other => {
                        return Err(StreamError::Malformed(format!(
                            "unknown finish_reason: {other}"
                        )));
                    }
                });
            }
            if let Some(delta) = choice.delta {
                if let Some(content) = delta.content.as_deref() {
                    if !content.is_empty() {
                        self.push_text(content);
                    }
                }
                if let Some(calls) = delta.tool_calls {
                    for call in calls {
                        self.handle_tool_delta(call);
                    }
                }
            }
        }
        Ok(())
    }

    fn push_text(&mut self, delta: &str) {
        self.pending.push_back(Ok(AssistantMessageEvent::TextDelta {
            delta: delta.to_string(),
        }));
        if let Some(Content::Text(t)) = self.state.content.last_mut() {
            t.text.push_str(delta);
            return;
        }
        self.state.content.push(Content::Text(TextContent {
            text: delta.to_string(),
        }));
    }

    fn handle_tool_delta(&mut self, call: ChatToolCall) {
        let entry = self.state.pending_tool_calls.entry(call.index).or_default();
        if let Some(id) = call.id.clone() {
            entry.id = Some(id);
        }
        if let Some(name) = call.function.name.clone() {
            entry.name = Some(name);
        }
        if !call.function.arguments.is_empty() {
            entry.arguments.push_str(&call.function.arguments);
        }
        let arguments_delta = if call.function.arguments.is_empty() {
            None
        } else {
            Some(call.function.arguments)
        };
        self.pending
            .push_back(Ok(AssistantMessageEvent::ToolCallDelta {
                index: call.index,
                id: call.id,
                name: call.function.name,
                arguments_delta,
            }));
    }

    fn finalize(&mut self) {
        // Materialise pending tool calls into the content list so the
        // final Done event sees them in order.
        let mut pending: Vec<_> = self.state.pending_tool_calls.drain().collect();
        pending.sort_by_key(|(idx, _)| *idx);
        for (_idx, call) in pending {
            let id = call.id.unwrap_or_default();
            let name = call.name.unwrap_or_default();
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
        let stop_reason = self.state.stop_reason.unwrap_or(
            if self.state.content.iter().any(Content::is_tool_call) {
                StopReason::ToolUse
            } else {
                StopReason::Stop
            },
        );
        let content = std::mem::take(&mut self.state.content);
        let usage = self.state.usage;
        self.pending.push_back(Ok(AssistantMessageEvent::Done {
            content,
            stop_reason,
            usage,
        }));
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

#[derive(Debug, Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    #[serde(default)]
    finish_reason: Option<String>,
    #[serde(default)]
    delta: Option<StreamDelta>,
}

#[derive(Debug, Default, Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChatToolCall>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{stream, StreamExt};
    use pi_protocol::ToolDefinition;
    use serde_json::json;

    fn model() -> Model {
        Model {
            provider: pi_protocol::ProviderId::new("openai"),
            id: "gpt-4o-mini".into(),
            api: pi_protocol::Api::OpenAiChatCompletions,
            label: None,
            context_window: 128_000,
            max_output_tokens: 4096,
        }
    }

    #[test]
    fn tool_result_content_reaches_the_model() {
        // The agent loop wraps every tool result in `Content::ToolResult`,
        // so reading only bare `Content::Text` blocks sends the model an
        // empty `content` string and the agent can never react to real
        // tool output.
        let mut ctx = Context::new("you are pi");
        ctx.messages.push(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(pi_protocol::ToolResult {
                tool_call_id: "call_1".into(),
                content: Box::new(Content::text("72F and sunny")),
                is_error: false,
                details: None,
            })],
            model: None,
        });
        let req =
            OpenAiProvider::build_request(&model(), &ctx, &SimpleStreamOptions::default(), true)
                .expect("build request");
        let v = serde_json::to_value(&req).expect("serialize request");
        let message = tool_message(&v);
        assert_eq!(message["tool_call_id"], "call_1");
        assert_eq!(
            message["content"], "72F and sunny",
            "the tool output must be serialized, not dropped"
        );
    }

    #[test]
    fn tool_result_accepts_bare_text_blocks() {
        // Hand-built contexts (extensions, tests) may still push a plain
        // text block under `Role::Tool`; that must keep working.
        let mut ctx = Context::new("you are pi");
        ctx.messages.push(Message {
            role: Role::Tool,
            content: vec![
                Content::ToolResult(pi_protocol::ToolResult {
                    tool_call_id: "call_2".into(),
                    content: Box::new(Content::text("first")),
                    is_error: true,
                    details: None,
                }),
                Content::text("second"),
            ],
            model: None,
        });
        let req =
            OpenAiProvider::build_request(&model(), &ctx, &SimpleStreamOptions::default(), true)
                .expect("build request");
        let v = serde_json::to_value(&req).expect("serialize request");
        assert_eq!(tool_message(&v)["content"], "firstsecond");
    }

    /// The serialized `Role::Tool` message (the request leads with the
    /// system prompt, so the tool entry is not `messages[0]`).
    fn tool_message(request: &serde_json::Value) -> &serde_json::Value {
        request["messages"]
            .as_array()
            .expect("messages array")
            .iter()
            .find(|message| message["role"] == "tool")
            .expect("a tool message")
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
    fn request_body_serializes_with_tools() {
        let ctx = ctx_with_tool();
        let req = OpenAiProvider::build_request(
            &model(),
            &ctx,
            &SimpleStreamOptions {
                temperature: Some(0.2),
                max_tokens: Some(256),
                ..Default::default()
            },
            true,
        )
        .expect("build request");
        let v = serde_json::to_value(&req).expect("serialize request");
        assert_eq!(v["model"], "gpt-4o-mini");
        assert_eq!(v["stream"], true);
        let temperature = v["temperature"].as_f64().expect("temperature number");
        assert!((temperature - 0.2).abs() < 1e-6, "got {temperature}");
        assert_eq!(v["max_tokens"], 256);
        assert_eq!(v["stream_options"]["include_usage"], true);

        let messages = v["messages"].as_array().expect("messages array");
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "you are pi");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "what's the weather in SF?");

        let tools = v["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
    }

    #[test]
    fn request_body_serializes_without_tools() {
        let mut ctx = Context::new("you are pi");
        ctx.messages.push(Message {
            role: Role::User,
            content: vec![Content::text("hi")],
            model: None,
        });
        let req =
            OpenAiProvider::build_request(&model(), &ctx, &SimpleStreamOptions::default(), true)
                .expect("build request");
        let v = serde_json::to_value(&req).expect("serialize");
        assert!(v.get("tools").is_none());
        assert!(v.get("temperature").is_none());
    }

    #[tokio::test]
    async fn sse_parser_emits_text_deltas_then_done() {
        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/openai_chat_text.sse");
        let bytes =
            std::fs::read(&fixture).unwrap_or_else(|e| panic!("read fixture {fixture:?}: {e}"));
        let chunks = stream::iter(vec![Ok::<bytes::Bytes, StreamError>(Bytes::from(bytes))]);
        let mut s = parse_sse(chunks, "gpt-4o-mini".to_string());
        let mut events = Vec::new();
        while let Some(ev) = s.next().await {
            events.push(ev.expect("stream event"));
        }
        // Expect: Start, TextDelta("hello"), TextDelta(" world"), Done.
        assert!(
            events.len() >= 4,
            "got {} events: {:?}",
            events.len(),
            events
        );
        assert!(matches!(events[0], AssistantMessageEvent::Start { .. }));
        match &events[1] {
            AssistantMessageEvent::TextDelta { delta } => assert_eq!(delta, "hello"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
        match &events[2] {
            AssistantMessageEvent::TextDelta { delta } => assert_eq!(delta, " world"),
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
                assert_eq!(text, "hello world");
                assert_eq!(usage.input, 17);
                assert_eq!(usage.output, 4);
            }
            other => panic!("expected done, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sse_parser_emits_tool_call_deltas_then_done() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/openai_chat_tool_call.sse");
        let bytes =
            std::fs::read(&fixture).unwrap_or_else(|e| panic!("read fixture {fixture:?}: {e}"));
        let chunks = stream::iter(vec![Ok::<bytes::Bytes, StreamError>(Bytes::from(bytes))]);
        let mut s = parse_sse(chunks, "gpt-4o-mini".to_string());
        let mut events = Vec::new();
        while let Some(ev) = s.next().await {
            events.push(ev.expect("stream event"));
        }
        // Look for the ToolCallDelta + final Done.
        let tool_deltas: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AssistantMessageEvent::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments_delta,
                } => Some((*index, id.clone(), name.clone(), arguments_delta.clone())),
                _ => None,
            })
            .collect();
        assert!(!tool_deltas.is_empty(), "expected at least one tool delta");
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
        assert_eq!(tc.name, "get_weather");
        assert_eq!(tc.arguments["city"], "San Francisco");
    }
}
