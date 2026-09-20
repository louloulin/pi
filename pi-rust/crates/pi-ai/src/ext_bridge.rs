//! Bridge between the JS `@earendil-works/pi-ai/compat` provider surface and
//! the Rust providers (LUM-1180).
//!
//! The extension host cannot run a provider itself:
//! `pi-extensions` does not depend on `pi-ai`, so the shim's built-in
//! provider factories (`anthropicMessagesApi` / `openAIResponsesApi` plus,
//! since LUM-1204, `openAICompletionsApi` / `googleGenerativeAIApi` /
//! `azureOpenAIResponsesApi`) hand a request to an injected runner, which
//! lives in `pi-coding-agent` where the providers do.
//!
//! This module holds the parts that are pure `pi-ai`:
//!
//! * [`model_from_js`] / [`context_from_js`] / [`stream_options_from_js`]
//!   translate the upstream `Model` / `Context` / `SimpleStreamOptions` JSON
//!   shapes the shim serialises into the [`pi_protocol`] types the providers
//!   consume;
//! * [`JsEventEncoder`] and [`JsAssistantEventStream`] translate the Rust
//!   [`AssistantMessageEvent`] stream back into the upstream
//!   `AssistantMessageEvent` shapes the shim's
//!   [`AssistantMessageEventStream`] collects.
//!
//! [`model_from_js`] accepts the [`BRIDGED_APIS`] families; anything else is
//! rejected with [`unbridged_api_error`], which names the remaining gaps
//! ([`UNBRIDGED_API_GAPS`]).
//!
//! Divergences from upstream are recorded in
//! `crates/pi-extensions/docs/SDK_MODULES.md`; the most visible one is that
//! `pi_protocol::Content` has no thinking block, so thinking text survives in
//! the streamed `*_delta` events but the authoritative `done.message` content
//! is rebuilt from the Rust event (thinking is carried over from the
//! accumulated partial).

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

use futures::Stream;
use pi_protocol::{
    Api, AssistantMessageEvent, Content, Context, ImageContent, Message, Model, ProviderId, Role,
    StopReason, ToolCall, ToolDefinition, ToolResult, Usage,
};
use serde_json::{json, Value};

use crate::types::SimpleStreamOptions;
use crate::AssistantMessageEventStream;

/// API families the `@earendil-works/pi-ai/compat` bridge routes to a Rust
/// adapter, in the order the shim's `__pi_sdk_builtin_api_apis` lists them.
///
/// The shim exposes more factories than this (see [`UNBRIDGED_API_GAPS`]);
/// an `api` outside this list is rejected by [`model_from_js`] before any
/// HTTP request is built.
pub const BRIDGED_APIS: &[&str] = &[
    "anthropic-messages",
    "openai-responses",
    "openai-completions",
    "google-generative-ai",
    "azure-openai-responses",
];

/// API families the shim still reports as gaps, paired with the capability
/// the host is missing.
///
/// Kept here so the two places that phrase the rejection —
/// [`model_from_js`] and `BuiltinPiAiStreamRunner`'s adapter fallback — share
/// one list instead of drifting. Mirrors the `__pi_sdk_stream_gaps` reasons
/// in `pi-ext-shim.mjs`.
pub const UNBRIDGED_API_GAPS: &[(&str, &str)] = &[
    (
        "google-vertex",
        "Vertex needs a GCP project plus location and ADC/access-token credentials",
    ),
    (
        "bedrock-converse-stream",
        "Bedrock needs AWS SigV4 credentials and a region",
    ),
    (
        "openai-codex-responses",
        "the Codex Responses dialect is authenticated with a ChatGPT account token, not an API key",
    ),
    (
        "pi-messages",
        "the first-party pi gateway protocol has no endpoint or credential in this build",
    ),
    (
        "mistral-conversations",
        "the adapter exists but this bridge does not route to it yet",
    ),
];

/// Error text for an `api` the extension bridge does not dispatch.
///
/// Names every family it *does* serve and the capability each remaining gap
/// is missing, so the failure is actionable instead of a flat "unsupported".
/// [`model_from_js`] and the runner's adapter fallback both go through this,
/// so a caller sees the same message wherever the family is rejected.
pub fn unbridged_api_error(api: &str) -> String {
    let bridged = BRIDGED_APIS
        .iter()
        .map(|api| format!("`{api}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let gaps = UNBRIDGED_API_GAPS
        .iter()
        .map(|(name, reason)| format!("`{name}` ({reason})"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("the built-in provider bridge serves {bridged}, not `{api}`; still a gap: {gaps}")
}

/// Parse the upstream `Model` JSON the shim sends into a [`Model`].
///
/// Only the API families in [`BRIDGED_APIS`] are accepted; any other `api`
/// is rejected with [`unbridged_api_error`], because the runner has no
/// adapter for it. `contextWindow` and `maxTokens` are optional and default
/// to `0` (the providers then leave the request uncapped where the wire
/// protocol allows it).
pub fn model_from_js(value: &Value) -> Result<Model, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "model must be a JSON object".to_string())?;
    let provider = object
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("extension");
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "model.id must be a string".to_string())?;
    let api = match object.get("api").and_then(Value::as_str) {
        Some(id) if BRIDGED_APIS.contains(&id) => {
            Api::from_api_id(id).expect("every bridged api id maps back to an `Api`")
        }
        other => return Err(unbridged_api_error(other.unwrap_or("<missing api>"))),
    };
    Ok(Model {
        provider: ProviderId::new(provider),
        id: id.to_string(),
        api,
        label: object
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string),
        context_window: object
            .get("contextWindow")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        max_output_tokens: object.get("maxTokens").and_then(Value::as_u64).unwrap_or(0) as u32,
    })
}

/// Parse the upstream `Context` JSON into a [`Context`].
///
/// `systemPrompt` becomes the top-level system prompt; `messages` and `tools`
/// are optional. Unknown message roles are skipped so a newer upstream shape
/// degrades to a usable request instead of failing the whole stream.
pub fn context_from_js(value: &Value) -> Result<Context, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "context must be a JSON object".to_string())?;
    let system_prompt = object
        .get("systemPrompt")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut messages = Vec::new();
    for raw in object
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        if let Some(message) = message_from_js(raw) {
            messages.push(message);
        }
    }
    let mut tools = Vec::new();
    for raw in object
        .get("tools")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        if let Some(tool) = tool_from_js(raw) {
            tools.push(tool);
        }
    }
    Ok(Context {
        system_prompt,
        messages,
        tools,
    })
}

/// Parse the provider request options the shim serialises.
///
/// Upstream `StreamOptions` carry `apiKey`, `baseUrl`, `maxTokens` and
/// `temperature`; only those reach the runner. `signal` is carried
/// out-of-band as the host-side [`cancellation token`](crate::AbortSignal),
/// so it is intentionally ignored here.
pub fn stream_options_from_js(value: &Value) -> SimpleStreamOptions {
    let Some(object) = value.as_object() else {
        return SimpleStreamOptions::default();
    };
    SimpleStreamOptions {
        temperature: object
            .get("temperature")
            .and_then(Value::as_f64)
            .map(|value| value as f32),
        max_tokens: object
            .get("maxTokens")
            .and_then(Value::as_u64)
            .map(|value| value as u32),
        signal: None,
    }
}

/// `options.apiKey` from the shim request, when present.
pub fn api_key_from_js(value: &Value) -> Option<String> {
    value
        .get("apiKey")
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
}

/// `options.baseUrl` from the shim request, when present.
///
/// `model.baseUrl` is the upstream fallback; the caller decides which wins.
pub fn base_url_from_js(value: &Value) -> Option<String> {
    value
        .get("baseUrl")
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty())
        .map(str::to_string)
}

fn message_from_js(value: &Value) -> Option<Message> {
    let object = value.as_object()?;
    let role = match object.get("role").and_then(Value::as_str)? {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "toolResult" | "tool" => Role::Tool,
        "system" => Role::System,
        _ => return None,
    };
    if role == Role::Tool {
        let content = object.get("content").map(text_only).unwrap_or_default();
        let result = ToolResult {
            tool_call_id: object
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            content: Box::new(Content::text(content)),
            is_error: object
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            details: object.get("details").cloned(),
            added_tool_names: object
                .get("addedToolNames")
                .and_then(Value::as_array)
                .map(|names| {
                    names
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                }),
        };
        return Some(Message {
            role,
            content: vec![Content::ToolResult(result)],
            model: None,
        });
    }
    let content = match object.get("content") {
        Some(Value::String(text)) => vec![Content::text(text.clone())],
        Some(Value::Array(blocks)) => blocks.iter().filter_map(content_from_js).collect(),
        _ => Vec::new(),
    };
    Some(Message {
        role,
        content,
        model: object
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Flatten a tool-result content payload into one text block. Images are
/// dropped: `pi_protocol::ToolResult` carries a single content block and the
/// Rust providers only replay text results.
fn text_only(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    block.get("text").and_then(Value::as_str)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn content_from_js(value: &Value) -> Option<Content> {
    let object = value.as_object()?;
    match object.get("type").and_then(Value::as_str)? {
        "text" => Some(Content::text(
            object
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )),
        "image" => Some(Content::Image(ImageContent {
            mime_type: object
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("image/png")
                .to_string(),
            data: object
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })),
        "toolCall" => Some(Content::ToolCall(ToolCall {
            id: object
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            name: object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            arguments: object
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({})),
        })),
        // Thinking blocks have no `pi_protocol::Content` variant yet; the
        // provider cannot replay their signature, so they are dropped.
        _ => None,
    }
}

fn tool_from_js(value: &Value) -> Option<ToolDefinition> {
    let object = value.as_object()?;
    let name = object.get("name").and_then(Value::as_str)?.to_string();
    Some(ToolDefinition {
        label: object
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or(&name)
            .to_string(),
        description: object
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        parameters: object
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
        name,
        metadata: object.get("metadata").cloned(),
    })
}

/// Accumulates Rust provider events into the upstream `AssistantMessage`
/// and encodes the JS events a `for await` consumer sees.
///
/// Upstream emits `start` before any partial, interleaves
/// `{text,thinking,toolcall}_{start,delta,end}` for each content block and
/// terminates with `done` or `error`. The Rust protocol only carries deltas
/// plus a `Done { content, .. }`, so the encoder synthesises the block
/// `*_start` / `*_end` events and keeps a live `partial` snapshot exactly the
/// way upstream's provider adapters do.
pub struct JsEventEncoder {
    model: String,
    partial: Value,
    /// Raw JSON argument fragments per open tool-call content index, used to
    /// rebuild `arguments` until the authoritative `Done` arrives.
    tool_arguments: HashMap<usize, String>,
    /// Rust tool-call `index` → content index, so repeated deltas for the
    /// same call append to one block.
    tool_blocks: HashMap<u32, usize>,
    terminal: bool,
}

impl JsEventEncoder {
    /// Create an encoder for one request.
    ///
    /// `api` / `provider` / `model` seed the `partial` message before the
    /// provider's `Start` event arrives; the event's model id wins when it
    /// differs (the Anthropic API can report a server-side fallback).
    pub fn new(
        api: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let api = api.into();
        let provider = provider.into();
        let model = model.into();
        let partial = json!({
            "role": "assistant",
            "content": [],
            "api": api,
            "provider": provider,
            "model": model,
            "usage": zero_usage(),
            "stopReason": "pending",
            "timestamp": now_millis(),
        });
        Self {
            model,
            partial,
            tool_arguments: HashMap::new(),
            tool_blocks: HashMap::new(),
            terminal: false,
        }
    }

    /// The live `AssistantMessage`-so-far, matching the `partial` field of
    /// the most recently emitted event.
    pub fn partial(&self) -> &Value {
        &self.partial
    }

    /// True once a terminal event (`done` / `error`) has been produced.
    pub fn is_terminal(&self) -> bool {
        self.terminal
    }

    /// Encode one provider event into zero or more JS events.
    pub fn encode(&mut self, event: &AssistantMessageEvent) -> Vec<Value> {
        if self.terminal {
            return Vec::new();
        }
        match event {
            AssistantMessageEvent::Start { model } => {
                if !model.is_empty() {
                    self.model = model.clone();
                    self.partial["model"] = json!(self.model);
                }
                vec![self.event("start", json!({}))]
            }
            AssistantMessageEvent::TextDelta { delta } => {
                let (index, started) = self.open_text_block();
                let mut events = Vec::new();
                if started {
                    events.push(self.event("text_start", json!({"contentIndex": index})));
                }
                if let Some(block) = self.partial["content"].get_mut(index) {
                    let text = block["text"].as_str().unwrap_or_default().to_string();
                    block["text"] = json!(format!("{text}{delta}"));
                }
                events
                    .push(self.event("text_delta", json!({"contentIndex": index, "delta": delta})));
                events
            }
            AssistantMessageEvent::ThinkingDelta { delta } => {
                let (index, started) = self.open_thinking_block();
                let mut events = Vec::new();
                if started {
                    events.push(self.event("thinking_start", json!({"contentIndex": index})));
                }
                if let Some(block) = self.partial["content"].get_mut(index) {
                    let thinking = block["thinking"].as_str().unwrap_or_default().to_string();
                    block["thinking"] = json!(format!("{thinking}{delta}"));
                }
                events.push(self.event(
                    "thinking_delta",
                    json!({"contentIndex": index, "delta": delta}),
                ));
                events
            }
            AssistantMessageEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => {
                let (content_index, started) = self.open_tool_call_block(*index);
                let mut events = Vec::new();
                if started {
                    events
                        .push(self.event("toolcall_start", json!({"contentIndex": content_index})));
                }
                if let Some(block) = self.partial["content"].get_mut(content_index) {
                    if let Some(id) = id.as_deref().filter(|id| !id.is_empty()) {
                        block["id"] = json!(id);
                    }
                    if let Some(name) = name.as_deref().filter(|name| !name.is_empty()) {
                        block["name"] = json!(name);
                    }
                    if let Some(delta) = arguments_delta.as_deref() {
                        let buffer = self.tool_arguments.entry(content_index).or_default();
                        buffer.push_str(delta);
                        if let Ok(parsed) = serde_json::from_str::<Value>(buffer) {
                            block["arguments"] = parsed;
                        }
                    }
                }
                if let Some(delta) = arguments_delta.as_deref().filter(|delta| !delta.is_empty()) {
                    events.push(self.event(
                        "toolcall_delta",
                        json!({"contentIndex": content_index, "delta": delta}),
                    ));
                }
                events
            }
            AssistantMessageEvent::Done {
                content,
                stop_reason,
                usage,
            } => {
                let mut events = self.close_open_block();
                self.apply_usage(usage);
                self.merge_final_content(content);
                match stop_reason {
                    StopReason::Aborted => {
                        self.partial["stopReason"] = json!("aborted");
                        events.push(self.error_event("aborted"));
                    }
                    StopReason::Error => {
                        let message = self.error_message().unwrap_or_default();
                        self.partial["stopReason"] = json!("error");
                        self.partial["errorMessage"] = json!(message.clone());
                        events.push(self.error_event("error"));
                    }
                    other => {
                        self.partial["stopReason"] = json!(stop_reason_js(*other));
                        events.push(self.event(
                            "done",
                            json!({"reason": done_reason_js(*other), "message": self.partial.clone()}),
                        ));
                    }
                }
                self.terminal = true;
                events
            }
            AssistantMessageEvent::Aborted => {
                let mut events = self.close_open_block();
                self.partial["stopReason"] = json!("aborted");
                if self.partial.get("errorMessage").is_none() {
                    self.partial["errorMessage"] = json!("aborted");
                }
                events.push(self.error_event("aborted"));
                self.terminal = true;
                events
            }
            AssistantMessageEvent::Error { message } => {
                let mut events = self.close_open_block();
                self.partial["stopReason"] = json!("error");
                self.partial["errorMessage"] = json!(message.clone());
                events.push(self.error_event("error"));
                self.terminal = true;
                events
            }
        }
    }

    /// Encode a transport-level failure (a `StreamError` item, or a source
    /// stream that ended without a terminal event) as a terminal error.
    pub fn encode_transport_error(&mut self, message: &str) -> Vec<Value> {
        if self.terminal {
            return Vec::new();
        }
        let mut events = self.close_open_block();
        self.partial["stopReason"] = json!("error");
        self.partial["errorMessage"] = json!(message);
        events.push(self.error_event("error"));
        self.terminal = true;
        events
    }

    fn error_message(&self) -> Option<String> {
        self.partial
            .get("errorMessage")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn event(&self, kind: &str, extra: Value) -> Value {
        let mut event = json!({"type": kind});
        if let Some(object) = extra.as_object() {
            for (key, value) in object {
                event[key] = value.clone();
            }
        }
        event["partial"] = self.partial.clone();
        event
    }

    fn error_event(&self, reason: &str) -> Value {
        json!({
            "type": "error",
            "reason": reason,
            "error": self.partial.clone(),
        })
    }

    fn apply_usage(&mut self, usage: &Usage) {
        self.partial["usage"] = usage_to_js(usage);
    }

    fn merge_final_content(&mut self, content: &[Content]) {
        let authoritative: Vec<Value> = content.iter().filter_map(content_to_js).collect();
        let Some(blocks) = self.partial["content"].as_array_mut() else {
            self.partial["content"] = json!(authoritative);
            return;
        };
        let mut next = 0usize;
        for block in blocks.iter_mut() {
            if block.get("type").and_then(Value::as_str) == Some("thinking") {
                continue;
            }
            if let Some(value) = authoritative.get(next) {
                *block = value.clone();
                next += 1;
            }
        }
        while next < authoritative.len() {
            blocks.push(authoritative[next].clone());
            next += 1;
        }
    }

    /// Index of the trailing text block, opening a new one when the last
    /// block is a different kind. Returns `(index, started)`.
    fn open_text_block(&mut self) -> (usize, bool) {
        let last = self.partial["content"]
            .as_array()
            .and_then(|blocks| blocks.last())
            .and_then(|block| block.get("type"))
            .and_then(Value::as_str);
        if last == Some("text") {
            let index = self.partial["content"].as_array().map_or(0, Vec::len) - 1;
            return (index, false);
        }
        self.close_open_block();
        let index = self.push_block(json!({"type": "text", "text": ""}));
        (index, true)
    }

    fn open_thinking_block(&mut self) -> (usize, bool) {
        let last = self.partial["content"]
            .as_array()
            .and_then(|blocks| blocks.last())
            .and_then(|block| block.get("type"))
            .and_then(Value::as_str);
        if last == Some("thinking") {
            let index = self.partial["content"].as_array().map_or(0, Vec::len) - 1;
            return (index, false);
        }
        self.close_open_block();
        let index = self.push_block(json!({"type": "thinking", "thinking": ""}));
        (index, true)
    }

    fn open_tool_call_block(&mut self, tool_index: u32) -> (usize, bool) {
        if let Some(index) = self.tool_blocks.get(&tool_index).copied() {
            return (index, false);
        }
        self.close_open_block();
        let index = self.push_block(json!({
            "type": "toolCall",
            "id": "",
            "name": "",
            "arguments": {},
        }));
        self.tool_blocks.insert(tool_index, index);
        (index, true)
    }

    fn push_block(&mut self, block: Value) -> usize {
        if let Some(blocks) = self.partial["content"].as_array_mut() {
            blocks.push(block);
            blocks.len() - 1
        } else {
            self.partial["content"] = json!([block]);
            0
        }
    }

    /// Emit the `*_end` event for the trailing block, if any.
    fn close_open_block(&mut self) -> Vec<Value> {
        let Some(blocks) = self.partial["content"].as_array() else {
            return Vec::new();
        };
        let Some(last) = blocks.last() else {
            return Vec::new();
        };
        let index = blocks.len() - 1;
        match last.get("type").and_then(Value::as_str) {
            Some("text") => {
                let content = last["text"].as_str().unwrap_or_default().to_string();
                vec![self.event(
                    "text_end",
                    json!({"contentIndex": index, "content": content}),
                )]
            }
            Some("thinking") => {
                let content = last["thinking"].as_str().unwrap_or_default().to_string();
                vec![self.event(
                    "thinking_end",
                    json!({"contentIndex": index, "content": content}),
                )]
            }
            Some("toolCall") => {
                let mut tool_call = last.clone();
                if let Some(raw) = self.tool_arguments.get(&index) {
                    if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
                        tool_call["arguments"] = parsed;
                    }
                }
                if let Some(block) = self.partial["content"].get_mut(index) {
                    block["arguments"] = tool_call["arguments"].clone();
                }
                vec![self.event(
                    "toolcall_end",
                    json!({"contentIndex": index, "toolCall": tool_call}),
                )]
            }
            _ => Vec::new(),
        }
    }
}

fn content_to_js(content: &Content) -> Option<Value> {
    match content {
        Content::Text(text) => Some(json!({"type": "text", "text": text.text})),
        Content::Image(image) => Some(json!({
            "type": "image",
            "data": image.data,
            "mimeType": image.mime_type,
        })),
        Content::ToolCall(call) => Some(json!({
            "type": "toolCall",
            "id": call.id,
            "name": call.name,
            "arguments": call.arguments,
        })),
        Content::ToolResult(_) => None,
    }
}

fn usage_to_js(usage: &Usage) -> Value {
    let total = if usage.total > 0 {
        usage.total
    } else {
        usage.input.saturating_add(usage.output)
    };
    json!({
        "input": usage.input,
        "output": usage.output,
        "cacheRead": usage.cache_read,
        "cacheWrite": usage.cache_write,
        "totalTokens": total,
        "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0},
    })
}

fn zero_usage() -> Value {
    usage_to_js(&Usage::default())
}

fn stop_reason_js(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Stop | StopReason::Empty => "stop",
        StopReason::ToolUse => "toolUse",
        StopReason::MaxTokens => "length",
        StopReason::Aborted => "aborted",
        StopReason::Error => "error",
    }
}

fn done_reason_js(reason: StopReason) -> &'static str {
    match reason {
        StopReason::ToolUse => "toolUse",
        StopReason::MaxTokens => "length",
        _ => "stop",
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// A [`Stream`] of upstream-shaped JS event objects produced by a Rust
/// provider stream.
///
/// The source is a [`pi_ai`](crate) [`AssistantMessageEventStream`]; every
/// [`AssistantMessageEvent`] is expanded through a [`JsEventEncoder`] and
/// the resulting JSON values are yielded in order. A [`StreamError`]
/// (transport failure) or a source that ends without a terminal event is
/// reported as a terminal `error` event, so the JS pump always observes a
/// `done` / `error` and never hangs.
///
/// [`StreamError`]: crate::StreamError
pub struct JsAssistantEventStream {
    source: AssistantMessageEventStream,
    encoder: JsEventEncoder,
    pending: VecDeque<Value>,
    finished: bool,
}

impl JsAssistantEventStream {
    /// Wrap a provider stream with an encoder.
    pub fn new(source: AssistantMessageEventStream, encoder: JsEventEncoder) -> Self {
        Self {
            source,
            encoder,
            pending: VecDeque::new(),
            finished: false,
        }
    }
}

impl Stream for JsAssistantEventStream {
    type Item = Value;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Value>> {
        let this = self.get_mut();
        loop {
            if let Some(value) = this.pending.pop_front() {
                return Poll::Ready(Some(value));
            }
            if this.finished {
                return Poll::Ready(None);
            }
            match this.source.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(event))) => {
                    let events = this.encoder.encode(&event);
                    this.pending.extend(events);
                    if this.encoder.is_terminal() {
                        this.finished = true;
                    }
                }
                Poll::Ready(Some(Err(error))) => {
                    let events = this.encoder.encode_transport_error(&error.to_string());
                    this.pending.extend(events);
                    this.finished = true;
                }
                Poll::Ready(None) => {
                    if !this.encoder.is_terminal() {
                        let events = this.encoder.encode_transport_error(
                            "provider stream ended without a terminal event",
                        );
                        this.pending.extend(events);
                    }
                    this.finished = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Value {
        json!({
            "id": "claude-haiku-4-5",
            "name": "Claude Haiku 4.5",
            "provider": "anthropic",
            "api": "anthropic-messages",
            "baseUrl": "https://api.anthropic.com",
            "contextWindow": 200000,
            "maxTokens": 8192,
        })
    }

    #[test]
    fn model_from_js_maps_the_upstream_fields() {
        let parsed = model_from_js(&model()).expect("model");
        assert_eq!(parsed.id, "claude-haiku-4-5");
        assert_eq!(parsed.provider.0, "anthropic");
        assert_eq!(parsed.api, Api::AnthropicMessages);
        assert_eq!(parsed.label.as_deref(), Some("Claude Haiku 4.5"));
        assert_eq!(parsed.context_window, 200000);
        assert_eq!(parsed.max_output_tokens, 8192);
    }

    #[test]
    fn model_from_js_maps_every_bridged_api_id() {
        for (id, api) in [
            ("anthropic-messages", Api::AnthropicMessages),
            ("openai-responses", Api::OpenAiResponses),
            ("openai-completions", Api::OpenAiChatCompletions),
            ("google-generative-ai", Api::GoogleGenerativeAi),
            ("azure-openai-responses", Api::AzureOpenAiResponses),
        ] {
            let mut value = model();
            value["api"] = json!(id);
            let parsed = model_from_js(&value).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert_eq!(parsed.api, api, "{id}");
        }
    }

    #[test]
    fn model_from_js_rejects_unbridged_apis_with_the_shared_gap_list() {
        // `mistral-conversations` has a Rust adapter but is not routed by the
        // bridge, so it is the sharpest negative case.
        let mut value = model();
        value["api"] = json!("mistral-conversations");
        let error = model_from_js(&value).expect_err("rejected");
        assert!(error.contains("`mistral-conversations`"), "{error}");
        for bridged in BRIDGED_APIS {
            assert!(error.contains(&format!("`{bridged}`")), "{error}");
        }
        for (gap, _) in UNBRIDGED_API_GAPS {
            assert!(error.contains(&format!("`{gap}`")), "{error}");
        }

        // An id from no family at all is rejected the same way.
        value["api"] = json!("not-a-real-api");
        let error = model_from_js(&value).expect_err("rejected");
        assert!(error.contains("`not-a-real-api`"), "{error}");
    }

    #[test]
    fn context_from_js_carries_messages_and_tools() {
        let value = json!({
            "systemPrompt": "be brief",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "hello"},
                    {"type": "toolCall", "id": "t1", "name": "read", "arguments": {"path": "a"}},
                ]},
                {"role": "toolResult", "toolCallId": "t1", "toolName": "read",
                 "content": [{"type": "text", "text": "file body"}], "isError": false},
            ],
            "tools": [{"name": "read", "description": "read a file",
                       "parameters": {"type": "object"}}],
        });
        let parsed = context_from_js(&value).expect("context");
        assert_eq!(parsed.system_prompt, "be brief");
        assert_eq!(parsed.messages.len(), 3);
        assert_eq!(parsed.messages[0].role, Role::User);
        assert_eq!(parsed.messages[1].content.len(), 2);
        assert!(parsed.messages[1].content[1].is_tool_call());
        assert_eq!(parsed.messages[2].role, Role::Tool);
        assert_eq!(parsed.tools.len(), 1);
        assert_eq!(parsed.tools[0].name, "read");
    }

    #[test]
    fn encoder_synthesises_block_events_around_deltas() {
        let mut encoder = JsEventEncoder::new("anthropic-messages", "anthropic", "claude");
        let mut events = Vec::new();
        events.extend(encoder.encode(&AssistantMessageEvent::Start {
            model: "claude-haiku-4-5".into(),
        }));
        events.extend(encoder.encode(&AssistantMessageEvent::TextDelta { delta: "he".into() }));
        events.extend(encoder.encode(&AssistantMessageEvent::TextDelta {
            delta: "llo".into(),
        }));
        events.extend(encoder.encode(&AssistantMessageEvent::Done {
            content: vec![Content::text("hello")],
            stop_reason: StopReason::Stop,
            usage: Usage {
                input: 3,
                output: 2,
                ..Usage::default()
            },
        }));

        let kinds: Vec<&str> = events
            .iter()
            .map(|event| event["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "start",
                "text_start",
                "text_delta",
                "text_delta",
                "text_end",
                "done"
            ]
        );
        assert_eq!(events[2]["delta"], "he");
        assert_eq!(events[2]["partial"]["content"][0]["text"], "he");
        let done = events.last().unwrap();
        assert_eq!(done["message"]["content"][0]["text"], "hello");
        assert_eq!(done["message"]["stopReason"], "stop");
        assert_eq!(done["message"]["usage"]["totalTokens"], 5);
        assert!(encoder.is_terminal());
    }

    #[test]
    fn encoder_maps_aborted_and_error_terminals() {
        let mut aborted = JsEventEncoder::new("openai-responses", "openai", "gpt-5");
        let events = aborted.encode(&AssistantMessageEvent::Aborted);
        assert_eq!(events[0]["type"], "error");
        assert_eq!(events[0]["reason"], "aborted");
        assert_eq!(events[0]["error"]["stopReason"], "aborted");

        let mut errored = JsEventEncoder::new("openai-responses", "openai", "gpt-5");
        let events = errored.encode(&AssistantMessageEvent::Error {
            message: "boom".into(),
        });
        assert_eq!(events[0]["type"], "error");
        assert_eq!(events[0]["reason"], "error");
        assert_eq!(events[0]["error"]["errorMessage"], "boom");
    }

    #[test]
    fn encoder_keeps_thinking_blocks_in_the_final_message() {
        let mut encoder = JsEventEncoder::new("anthropic-messages", "anthropic", "claude");
        encoder.encode(&AssistantMessageEvent::Start {
            model: "claude".into(),
        });
        encoder.encode(&AssistantMessageEvent::ThinkingDelta {
            delta: "why".into(),
        });
        encoder.encode(&AssistantMessageEvent::TextDelta {
            delta: "answer".into(),
        });
        let events = encoder.encode(&AssistantMessageEvent::Done {
            content: vec![Content::text("answer")],
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
        });
        let message = &events.last().unwrap()["message"];
        assert_eq!(message["content"][0]["type"], "thinking");
        assert_eq!(message["content"][0]["thinking"], "why");
        assert_eq!(message["content"][1]["type"], "text");
        assert_eq!(message["content"][1]["text"], "answer");
    }

    #[test]
    fn stream_reports_a_source_that_ends_without_a_terminal_event() {
        use futures::StreamExt;

        let source: AssistantMessageEventStream = Box::pin(futures::stream::iter(vec![Ok(
            AssistantMessageEvent::Start {
                model: "claude".into(),
            },
        )]));
        let mut stream = JsAssistantEventStream::new(
            source,
            JsEventEncoder::new("anthropic-messages", "anthropic", "claude"),
        );
        let collected = futures::executor::block_on(async {
            let mut out = Vec::new();
            while let Some(value) = stream.next().await {
                out.push(value["type"].as_str().unwrap().to_string());
            }
            out
        });
        assert_eq!(collected, vec!["start", "error"]);
    }
}
