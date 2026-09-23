//! Session entry assembly: in-memory messages → upstream `SessionEntry`
//! values, and JSONL session files → [`SessionData`].
//!
//! The HTML template is the upstream one, so the payload it receives has to
//! use the upstream session-entry spelling (`{"type":"message","id",…,
//! "message":{…}}`) even though the Rust agent stores a flatter
//! [`pi_protocol::Message`](pi_protocol::Message) list. This module owns that
//! translation in both directions:
//!
//! * [`session_data_from_messages`] converts the live TUI/print conversation,
//! * [`read_session_file`] parses a `.jsonl` file — accepting both the
//!   upstream format written by the TS port and the Rust
//!   [`pi_protocol::SessionEntry`](pi_protocol::SessionEntry) spelling that
//!   `pi session export` produces.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};
use pi_protocol::{Content, Message, Role, SessionEntry, StopReason, ToolDefinition};
use serde_json::{json, Value};

use super::{ExportError, SessionData, ToolInfo, CURRENT_SESSION_VERSION};

/// Milliseconds since the Unix epoch for `message.timestamp` fields.
fn unix_millis(now: DateTime<Utc>, offset: usize) -> i64 {
    now.timestamp_millis() + offset as i64
}

/// ISO-8601 timestamp for the entry's `timestamp` field.
fn iso_timestamp(now: DateTime<Utc>, offset: usize) -> String {
    (now + chrono::Duration::milliseconds(offset as i64))
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Monotonic entry-id + parent-chain builder.
#[derive(Debug, Default)]
struct EntryChain {
    counter: usize,
    parent: Option<String>,
}

impl EntryChain {
    /// Next generated entry id (8 hex chars, upstream's short-id shape).
    fn next_id(&mut self) -> String {
        self.counter += 1;
        format!("{:08x}", self.counter)
    }

    /// Push `message` as a `message` entry and advance the chain.
    fn push_message(
        &mut self,
        entries: &mut Vec<Value>,
        message: Value,
        now: DateTime<Utc>,
        offset: usize,
    ) {
        let id = self.next_id();
        entries.push(json!({
            "type": "message",
            "id": id,
            "parentId": self.parent,
            "timestamp": iso_timestamp(now, offset),
            "message": message,
        }));
        self.parent = Some(id);
    }
}

/// Assemble [`SessionData`] for a live session (upstream
/// `exportSessionToHtml`'s `SessionData` literal).
///
/// `messages` is the agent's in-memory message log in Rust's flat shape; it
/// is converted to the upstream entry tree with a linear `parentId` chain.
/// `theme_name` selects the palette the pre-rendered tool HTML (upstream
/// `preRenderCustomTools`) is painted with; pass the same name that is handed
/// to `generate_html` so both halves of the document agree.
pub fn session_data_from_messages(
    session_id: &str,
    cwd: &str,
    messages: &[Message],
    system_prompt: Option<&str>,
    tools: &[ToolDefinition],
    theme_name: Option<&str>,
) -> SessionData {
    let now = Utc::now();
    let mut chain = EntryChain::default();
    let mut entries: Vec<Value> = Vec::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();

    for (offset, message) in messages.iter().enumerate() {
        // Record tool names as soon as the assistant declares them so the
        // matching `toolResult` entry can carry `toolName`.
        if message.role == Role::Assistant {
            for block in &message.content {
                if let Content::ToolCall(call) = block {
                    tool_names.insert(call.id.clone(), call.name.clone());
                }
            }
        }
        if let Some(value) = message_to_upstream(message, &tool_names, unix_millis(now, offset)) {
            chain.push_message(&mut entries, value, now, offset);
        }
    }

    let leaf_id = entries
        .last()
        .and_then(|entry| entry.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let rendered_tools = super::pre_render_custom_tools(&entries, cwd, theme_name);

    SessionData {
        header: Some(json!({
            "type": "session",
            "version": CURRENT_SESSION_VERSION,
            "id": session_id,
            "timestamp": now.to_rfc3339_opts(SecondsFormat::Millis, true),
            "cwd": cwd,
        })),
        entries,
        leaf_id,
        system_prompt: system_prompt
            .filter(|prompt| !prompt.is_empty())
            .map(str::to_string),
        tools: (!tools.is_empty()).then(|| {
            tools
                .iter()
                .map(|tool| ToolInfo {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.parameters.clone(),
                })
                .collect()
        }),
        rendered_tools,
    }
}

/// Convert one Rust message into an upstream `AgentMessage` JSON object.
///
/// Returns `None` for system messages (the system prompt travels in its own
/// `SessionData` field) and for messages that carry no renderable content.
fn message_to_upstream(
    message: &Message,
    tool_names: &HashMap<String, String>,
    timestamp: i64,
) -> Option<Value> {
    match message.role {
        Role::System => None,
        Role::User => {
            let blocks: Vec<Value> = message.content.iter().filter_map(display_block).collect();
            if blocks.is_empty() {
                return None;
            }
            Some(json!({
                "role": "user",
                "content": blocks,
                "timestamp": timestamp,
            }))
        }
        Role::Assistant => {
            let content: Vec<Value> = message.content.iter().filter_map(assistant_block).collect();
            Some(json!({
                "role": "assistant",
                "content": content,
                "model": message.model.clone().unwrap_or_default(),
                "usage": zero_usage(),
                "stopReason": "stop",
                "timestamp": timestamp,
            }))
        }
        Role::Tool => {
            let result = message.content.iter().find_map(|block| match block {
                Content::ToolResult(result) => Some(result),
                _ => None,
            })?;
            let blocks: Vec<Value> = match result.content.as_ref() {
                Content::Text(text) => vec![json!({"type": "text", "text": text.text})],
                Content::Image(image) => vec![json!({
                    "type": "image",
                    "data": image.data,
                    "mimeType": image.mime_type,
                })],
                _ => Vec::new(),
            };
            Some(json!({
                "role": "toolResult",
                "toolCallId": result.tool_call_id,
                "toolName": tool_names
                    .get(&result.tool_call_id)
                    .cloned()
                    .unwrap_or_default(),
                "content": blocks,
                "isError": result.is_error,
                "details": result.details,
                "timestamp": timestamp,
            }))
        }
    }
}

/// Text / image blocks shown verbatim in user and tool-result messages.
fn display_block(block: &Content) -> Option<Value> {
    match block {
        Content::Text(text) => Some(json!({"type": "text", "text": text.text})),
        Content::Image(image) => Some(json!({
            "type": "image",
            "data": image.data,
            "mimeType": image.mime_type,
        })),
        _ => None,
    }
}

/// Text / thinking / tool-call blocks rendered inside an assistant message.
fn assistant_block(block: &Content) -> Option<Value> {
    match block {
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

fn zero_usage() -> Value {
    json!({
        "input": 0,
        "output": 0,
        "cacheRead": 0,
        "cacheWrite": 0,
        "totalTokens": 0,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0},
    })
}

fn usage_to_upstream(usage: &pi_protocol::Usage) -> Value {
    let total = if usage.total > 0 {
        usage.total
    } else {
        usage.input + usage.output
    };
    json!({
        "input": usage.input,
        "output": usage.output,
        "cacheRead": usage.cache_read,
        "cacheWrite": usage.cache_write,
        "totalTokens": total,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0},
    })
}

fn stop_reason_to_upstream(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Stop => "stop",
        StopReason::ToolUse => "toolUse",
        StopReason::MaxTokens => "length",
        StopReason::Aborted => "aborted",
        StopReason::Error => "error",
        StopReason::Empty => "stop",
    }
}

/// Read a JSONL session file and assemble the [`SessionData`] payload.
///
/// Accepts the upstream `{"type":"session",…}` header and the Rust
/// `{"type":"header",…}` spelling; entries may be upstream entries
/// (`message`, `compaction`, `custom`, …) or Rust
/// [`SessionEntry`](pi_protocol::SessionEntry) rows. Unparseable lines are
/// skipped, matching upstream `loadEntriesFromFile`.
pub fn read_session_file(path: &Path) -> Result<SessionData, ExportError> {
    let resolved = crate::paths::absolute(path);
    if !resolved.exists() {
        return Err(ExportError::FileNotFound(resolved.display().to_string()));
    }
    let raw = std::fs::read_to_string(&resolved).map_err(|error| ExportError::Io {
        path: resolved.display().to_string(),
        message: error.to_string(),
    })?;
    parse_session_jsonl(
        crate::paths::strip_bom(&raw),
        &resolved.display().to_string(),
    )
}

/// Parse the contents of a JSONL session file (see [`read_session_file`]).
pub fn parse_session_jsonl(content: &str, label: &str) -> Result<SessionData, ExportError> {
    let parsed: Vec<Value> = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect();

    let Some(first) = parsed.first() else {
        return Err(ExportError::InvalidSession(label.to_string()));
    };
    let header =
        header_from_value(first).ok_or_else(|| ExportError::InvalidSession(label.to_string()))?;

    let now = Utc::now();
    let mut chain = EntryChain::default();
    let mut entries: Vec<Value> = Vec::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();

    for (offset, value) in parsed.iter().skip(1).enumerate() {
        for entry in entry_from_value(value, &mut chain, &mut tool_names, now, offset) {
            entries.push(entry);
        }
    }

    // Only a linear chain keeps a usable leaf pointer; for upstream files the
    // last entry is the leaf, exactly like `SessionManager.getLeafId()`.
    let leaf_id = entries
        .last()
        .and_then(|entry| entry.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);

    // The CLI export has no live agent state, so the theme falls back to the
    // default one — the same theme `generate_html(…, None)` uses.
    let cwd = header
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let rendered_tools = super::pre_render_custom_tools(&entries, &cwd, None);

    Ok(SessionData {
        header: Some(header),
        entries,
        leaf_id,
        system_prompt: None,
        tools: None,
        rendered_tools,
    })
}

/// Normalise the first JSONL line into the upstream session header shape.
fn header_from_value(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let id = object.get("id")?.as_str()?;
    match object.get("type").and_then(Value::as_str) {
        Some("session") => Some(json!({
            "type": "session",
            "version": object.get("version").and_then(Value::as_u64).unwrap_or(CURRENT_SESSION_VERSION as u64),
            "id": id,
            "timestamp": object.get("timestamp").cloned().unwrap_or(Value::Null),
            "cwd": object.get("cwd").cloned().unwrap_or_else(|| Value::String(String::new())),
        })),
        // Rust spelling: `{"type":"header","id","created_at","version"}`.
        Some("header") => Some(json!({
            "type": "session",
            "version": CURRENT_SESSION_VERSION,
            "id": id,
            "timestamp": object.get("created_at").cloned().unwrap_or(Value::Null),
            "cwd": "",
        })),
        _ => None,
    }
}

/// Convert one JSONL entry into zero or more upstream entries.
fn entry_from_value(
    value: &Value,
    chain: &mut EntryChain,
    tool_names: &mut HashMap<String, String>,
    now: DateTime<Utc>,
    offset: usize,
) -> Vec<Value> {
    // Upstream entries always carry their own `id`; keep them untouched so
    // ids, `parentId`s and labels survive the round-trip. Rust
    // `SessionEntry` rows carry no id and are translated below.
    if value.get("id").and_then(Value::as_str).is_some() {
        return vec![value.clone()];
    }

    if let Ok(entry) = serde_json::from_value::<SessionEntry>(value.clone()) {
        return rust_entry_to_upstream(entry, chain, tool_names, now, offset);
    }

    Vec::new()
}

/// Translate one Rust session entry into upstream entries.
fn rust_entry_to_upstream(
    entry: SessionEntry,
    chain: &mut EntryChain,
    tool_names: &mut HashMap<String, String>,
    now: DateTime<Utc>,
    offset: usize,
) -> Vec<Value> {
    match entry {
        SessionEntry::Header { .. } => Vec::new(),
        SessionEntry::UserMessage(message) => {
            let mut entries = Vec::new();
            if let Some(value) = message_to_upstream(&message, tool_names, unix_millis(now, offset))
            {
                chain.push_message(&mut entries, value, now, offset);
            }
            entries
        }
        SessionEntry::AssistantMessage(message) => {
            for block in &message.content {
                if let Content::ToolCall(call) = block {
                    tool_names.insert(call.id.clone(), call.name.clone());
                }
            }
            let content: Vec<Value> = message.content.iter().filter_map(assistant_block).collect();
            let value = json!({
                "role": "assistant",
                "content": content,
                "model": message.model,
                "usage": usage_to_upstream(&message.usage),
                "stopReason": stop_reason_to_upstream(message.stop_reason),
                "errorMessage": message.error_message,
                "timestamp": unix_millis(now, offset),
            });
            let mut entries = Vec::new();
            chain.push_message(&mut entries, value, now, offset);
            entries
        }
        SessionEntry::ToolCall(call) => {
            tool_names.insert(call.id.clone(), call.name.clone());
            let value = json!({
                "role": "assistant",
                "content": [{
                    "type": "toolCall",
                    "id": call.id,
                    "name": call.name,
                    "arguments": call.arguments,
                }],
                "model": "",
                "usage": zero_usage(),
                "stopReason": "toolUse",
                "timestamp": unix_millis(now, offset),
            });
            let mut entries = Vec::new();
            chain.push_message(&mut entries, value, now, offset);
            entries
        }
        SessionEntry::ToolResult(result) => {
            let blocks = match result.content.as_ref() {
                Content::Text(text) => vec![json!({"type": "text", "text": text.text})],
                Content::Image(image) => vec![json!({
                    "type": "image",
                    "data": image.data,
                    "mimeType": image.mime_type,
                })],
                _ => Vec::new(),
            };
            let value = json!({
                "role": "toolResult",
                "toolCallId": result.tool_call_id,
                "toolName": tool_names
                    .get(&result.tool_call_id)
                    .cloned()
                    .unwrap_or_default(),
                "content": blocks,
                "isError": result.is_error,
                "details": result.details,
                "timestamp": unix_millis(now, offset),
            });
            let mut entries = Vec::new();
            chain.push_message(&mut entries, value, now, offset);
            entries
        }
        SessionEntry::Extension {
            extension,
            kind,
            payload,
        } => {
            let id = chain.next_id();
            let entry = json!({
                "type": "custom",
                "id": id,
                "parentId": chain.parent,
                "timestamp": iso_timestamp(now, offset),
                "customType": format!("{extension}:{kind}"),
                "data": payload,
            });
            chain.parent = Some(id);
            vec![entry]
        }
        SessionEntry::Compaction {
            summary,
            retained_tail,
            tokens_before,
            usage,
            details,
        } => {
            let id = chain.next_id();
            let entry = json!({
                "type": "compaction",
                "id": id,
                "parentId": chain.parent,
                "timestamp": iso_timestamp(now, offset),
                "summary": summary,
                "firstKeptEntryId": chain.parent.clone().unwrap_or_default(),
                "tokensBefore": tokens_before,
                "usage": usage.as_ref().map(usage_to_upstream),
                "details": details,
                "retainedTail": retained_tail,
            });
            chain.parent = Some(id);
            vec![entry]
        }
    }
}

/// A deterministic in-memory session used by the HTML tests.
///
/// Mirrors the shape a real export produces: a `session` header, a user
/// message, an assistant turn with a `bash` tool call and the matching
/// `toolResult`.
pub fn fixture_session_data() -> SessionData {
    let header = json!({
        "type": "session",
        "version": CURRENT_SESSION_VERSION,
        "id": "fixture-session",
        "timestamp": "2024-12-03T14:00:00.000Z",
        "cwd": "/tmp/project",
    });
    let entries = vec![
        json!({
            "type": "message",
            "id": "a1b2c3d4",
            "parentId": null,
            "timestamp": "2024-12-03T14:00:01.000Z",
            "message": {"role": "user", "content": "Hello", "timestamp": 1733234401000u64},
        }),
        json!({
            "type": "message",
            "id": "b2c3d4e5",
            "parentId": "a1b2c3d4",
            "timestamp": "2024-12-03T14:00:02.000Z",
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "Hi!"},
                    {"type": "toolCall", "id": "call_123", "name": "bash", "arguments": {"command": "ls"}},
                ],
                "api": "faux",
                "provider": "faux",
                "model": "faux-model",
                "usage": zero_usage(),
                "stopReason": "toolUse",
                "timestamp": 1733234402000u64,
            },
        }),
        json!({
            "type": "message",
            "id": "c3d4e5f6",
            "parentId": "b2c3d4e5",
            "timestamp": "2024-12-03T14:00:03.000Z",
            "message": {
                "role": "toolResult",
                "toolCallId": "call_123",
                "toolName": "bash",
                "content": [{"type": "text", "text": "a.txt"}],
                "isError": false,
                "timestamp": 1733234403000u64,
            },
        }),
    ];
    // Only `bash` is present, which `template.js` renders itself, so this is
    // always `None` — but it exercises the same wiring a real fixture would.
    let rendered_tools = super::pre_render_custom_tools(&entries, "/tmp/project", Some("dark"));
    SessionData {
        header: Some(header),
        entries,
        leaf_id: Some("c3d4e5f6".to_string()),
        system_prompt: Some("You are pi.".to_string()),
        tools: Some(vec![ToolInfo {
            name: "bash".to_string(),
            description: "Run a shell command".to_string(),
            parameters: json!({"type": "object"}),
        }]),
        rendered_tools,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![Content::text(text)],
            model: None,
        }
    }

    #[test]
    fn converts_in_memory_messages_to_a_linked_entry_tree() {
        let messages = vec![user("hello")];
        let data = session_data_from_messages(
            "session-1",
            "/tmp/project",
            &messages,
            Some("system"),
            &[],
            Some("dark"),
        );
        assert_eq!(data.entries.len(), 1);
        let entry = &data.entries[0];
        assert_eq!(entry["type"], "message");
        assert_eq!(entry["parentId"], Value::Null);
        assert_eq!(entry["message"]["role"], "user");
        assert_eq!(data.leaf_id.as_deref(), entry["id"].as_str());
        assert_eq!(data.system_prompt.as_deref(), Some("system"));
    }

    #[test]
    fn parses_upstream_session_files() {
        let fixture = fixture_session_data();
        let mut lines = vec![serde_json::to_string(fixture.header.as_ref().unwrap()).unwrap()];
        for entry in &fixture.entries {
            lines.push(serde_json::to_string(entry).unwrap());
        }
        let parsed = parse_session_jsonl(&lines.join("\n"), "fixture").expect("parses");
        assert_eq!(parsed.entries.len(), 3);
        assert_eq!(parsed.leaf_id.as_deref(), Some("c3d4e5f6"));
        assert_eq!(parsed.header.as_ref().unwrap()["id"], "fixture-session");
    }

    #[test]
    fn parses_rust_jsonl_entries() {
        let jsonl = concat!(
            "{\"type\":\"header\",\"id\":\"s1\",\"created_at\":\"2024-12-03T14:00:00Z\",\"version\":\"0.1.0\"}\n",
            "{\"type\":\"user_message\",\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}\n",
            "{\"type\":\"assistant_message\",\"model\":\"faux\",\"content\":[{\"type\":\"tool_call\",\"id\":\"c1\",\"name\":\"bash\",\"arguments\":{\"command\":\"ls\"}}],\"stop_reason\":\"tool_use\",\"usage\":{\"input\":1,\"output\":2,\"cache_read\":0,\"cache_write\":0,\"total\":3}}\n",
            "{\"type\":\"tool_result\",\"tool_call_id\":\"c1\",\"content\":{\"type\":\"text\",\"text\":\"a.txt\"},\"is_error\":false}\n",
        );
        let parsed = parse_session_jsonl(jsonl, "rust").expect("parses");
        assert_eq!(parsed.entries.len(), 3);
        assert_eq!(parsed.entries[0]["message"]["role"], "user");
        assert_eq!(
            parsed.entries[1]["message"]["content"][0]["type"],
            "toolCall"
        );
        assert_eq!(parsed.entries[2]["message"]["role"], "toolResult");
        assert_eq!(parsed.entries[2]["message"]["toolName"], "bash");
        assert_eq!(parsed.header.as_ref().unwrap()["type"], "session");
    }

    #[test]
    fn upstream_entries_with_ids_pass_through_untouched() {
        // A `compaction` entry shares its tag with a Rust `SessionEntry`
        // variant; the `id` must win so upstream fields (`firstKeptEntryId`,
        // `custom`) survive.
        let jsonl = concat!(
            "{\"type\":\"session\",\"id\":\"s1\",\"timestamp\":\"2024-12-03T14:00:00.000Z\",\"cwd\":\"/tmp\"}\n",
            "{\"type\":\"compaction\",\"id\":\"e1\",\"parentId\":null,\"timestamp\":\"2024-12-03T14:00:01.000Z\",\"summary\":\"sum\",\"firstKeptEntryId\":\"x\",\"tokensBefore\":42,\"custom\":\"kept\"}\n",
            "{\"type\":\"label\",\"id\":\"e2\",\"parentId\":\"e1\",\"timestamp\":\"2024-12-03T14:00:02.000Z\",\"label\":\"checkpoint\"}\n",
        );
        let parsed = parse_session_jsonl(jsonl, "upstream").expect("parses");
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[0]["firstKeptEntryId"], "x");
        assert_eq!(parsed.entries[0]["custom"], "kept");
        assert!(parsed.entries[0].get("retainedTail").is_none());
        assert_eq!(parsed.entries[1]["type"], "label");
        assert_eq!(parsed.leaf_id.as_deref(), Some("e2"));
    }

    #[test]
    fn rejects_files_without_a_session_header() {
        let err = parse_session_jsonl("{\"type\":\"message\"}", "bad").unwrap_err();
        assert!(matches!(err, ExportError::InvalidSession(_)), "{err}");
        let err = parse_session_jsonl("", "bad").unwrap_err();
        assert!(matches!(err, ExportError::InvalidSession(_)), "{err}");
    }

    #[test]
    fn missing_files_report_the_upstream_message() {
        let err = read_session_file(Path::new("/definitely/not/here/session.jsonl")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "File not found: /definitely/not/here/session.jsonl"
        );
    }
}
