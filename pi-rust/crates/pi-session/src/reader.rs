//! SQLite session reader.
//!
//! [`SessionReader::open`] probes the file's structure ([`SchemaLayout`])
//! and then reads either on-disk layout:
//!
//! * [`SchemaLayout::RustLegacy`] — the narrow Stage 5 layout this
//!   crate used to write: `entries.payload` is a zstd BLOB holding a
//!   serialized [`SessionEntry`](pi_protocol::SessionEntry). Read-only as
//!   of Stage 55 (convert with `pi session migrate`).
//! * [`SchemaLayout::UpstreamV4`] — the upstream TS
//!   `packages/session-backends/sqlite-node` format (AgentHarness storage
//!   format 4 / `storageVersion 1`): `entries.payload` is plain JSON text
//!   describing an `AgentHarness` entry (`message` / `compaction` /
//!   `branch_summary` / `custom`), which is mapped onto
//!   [`SessionEntry`](pi_protocol::SessionEntry) here.
//!
//! The two layouts share nothing but the file extension — see
//! [`crate::schema`] for the column-by-column comparison. Since Stage 55
//! [`SessionWriter`](crate::SessionWriter) writes the upstream layout and
//! refuses to append to a Rust legacy file
//! ([`SessionError::LegacyLayout`](crate::SessionError::LegacyLayout));
//! the reader keeps both paths so legacy files stay readable.
//!
//! # Upstream mapping
//!
//! | upstream | here |
//! | --- | --- |
//! | `sessions.created_at` (millis) | [`SessionRow::created_at`], [`SessionEntry::Header::created_at`] |
//! | `sessions.parent_session_id` | [`SessionRow::parent_session`] |
//! | `sessions.metadata` `.version` | [`SessionRow::version`] (empty when absent) |
//! | `entries.id` / `entries.parent_id` | [`DecodedEntry::entry_id`] / [`DecodedEntry::parent_entry_id`] |
//! | `entries.type = "message"` | `UserMessage` / `AssistantMessage` / `ToolResult` by `payload.message.role` |
//! | `entries.type = "compaction"` | [`SessionEntry::Compaction`] |
//! | `entries.type = "custom"` | [`SessionEntry::Extension`] (`extension = "custom"`, `kind = custom_type`) |
//! | `entries.type = "branch_summary"` | [`SessionEntry::Extension`] passthrough (`extension = "branch_summary"`) |
//!
//! Known degradations, all deliberate and all documented rather than
//! silent:
//!
//! * Upstream has no `sessions.cwd` and no `sessions.version` column, so
//!   [`SessionRow::cwd`] is always `None` and `version` is read out of
//!   `metadata.version` (empty string when the metadata has none).
//! * `branch_summary` has no Rust variant yet. [`SessionEntry`] is
//!   matched exhaustively by `pi-coding-agent`, so adding one would break
//!   that crate; the whole upstream payload is passed through inside an
//!   [`SessionEntry::Extension`] instead of being dropped.
//! * Assistant `thinking` content blocks have no representation in
//!   `pi_protocol::Content` yet. They are skipped with a `tracing::warn!`
//!   (and noted here) rather than being silently converted to text.
//! * [`pi_protocol::ToolResult::content`] holds a single block while
//!   upstream allows an array. A single block maps 1:1; several text
//!   blocks are joined with `\n`; a mix of text and images keeps the
//!   first block and warns about the rest.
//! * Upstream `terminate`, `fromHook`, `api`, `provider`, `responseId`,
//!   `diagnostics` and per-message `timestamp` fields have no Rust
//!   counterpart and are dropped. `MessageEntry.terminate` and the rest
//!   are preserved inside the `branch_summary` / unknown-type passthrough
//!   payloads.
//!
//! [`SessionEntry::Header::created_at`]: pi_protocol::SessionEntry::Header
//! [`SchemaLayout`]: crate::schema::SchemaLayout
//! [`DecodedEntry::entry_id`]: DecodedEntry::entry_id

use std::path::{Path, PathBuf};

use pi_protocol::{
    AssistantMessage, Content, ImageContent, Message, Role, SessionEntry, StopReason, TextContent,
    ToolCall, ToolResult, Usage,
};
use rusqlite::{Connection, Row};
use serde_json::Value;

use crate::error::{Result, SessionError};
use crate::schema::{self, EntryRow, SchemaLayout, SessionRow};

/// Stored row whose `payload` column has already been decoded.
#[derive(Debug, Clone)]
pub struct DecodedEntry {
    /// 1-based sequence number within the session. In the upstream layout
    /// this is `entries.seq` (the display order) — upstream's primary key
    /// is `(session_id, id)`, so `seq` is not unique by construction.
    pub seq: i64,
    /// Sequence number of the parent entry (for tree-shaped sessions).
    /// Always `None` for upstream rows, where the parent is identified by
    /// id, not by seq.
    pub parent_seq: Option<i64>,
    /// Logical entry id (e.g. a uuid stamped by the agent). Maps to the
    /// upstream `entries.id` column.
    pub entry_id: Option<String>,
    /// Logical id of the parent entry. Maps to the upstream
    /// `entries.parent_id` column.
    pub parent_entry_id: Option<String>,
    /// Entry type discriminator. For upstream rows this is the upstream
    /// `entries.type` (`message`, `compaction`, `custom`,
    /// `branch_summary`); for Rust rows it is the Rust tag
    /// (`user_message`, ...).
    pub type_: String,
    /// Wall-clock timestamp in milliseconds since the unix epoch.
    pub timestamp: i64,
    /// Decoded [`SessionEntry`](pi_protocol::SessionEntry) value.
    pub entry: SessionEntry,
}

impl DecodedEntry {
    /// Borrow the inner [`SessionEntry`](pi_protocol::SessionEntry).
    pub fn entry(&self) -> &SessionEntry {
        &self.entry
    }
}

/// Read-only handle over a session database file.
#[derive(Debug)]
pub struct SessionReader {
    conn: Connection,
    path: PathBuf,
    layout: SchemaLayout,
}

const LEGACY_SESSIONS_SQL: &str = "id, created_at, parent_session, cwd, version, metadata";
const UPSTREAM_SESSIONS_SQL: &str =
    "id, created_at, parent_session_id, storage_version, metadata, message_count, usage_payload, next_seq";

impl SessionReader {
    /// Open an existing session database. Returns
    /// [`SessionError::NotFound`](crate::SessionError::NotFound) when the
    /// file does not exist, and
    /// [`SessionError::Corrupt`](crate::SessionError::Corrupt) when the
    /// file is not a session database or uses a layout this reader does
    /// not recognise.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(SessionError::NotFound(path.to_path_buf()));
        }
        let (conn, layout) = schema::open_read_only_with_layout(path)?;
        Ok(Self {
            conn,
            path: path.to_path_buf(),
            layout,
        })
    }

    /// Path the reader is bound to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// On-disk layout the reader detected when the file was opened.
    pub fn layout(&self) -> SchemaLayout {
        self.layout
    }

    /// Read the single stored session header. When the database holds
    /// multiple sessions (both layouts allow it), returns the first one
    /// in `id` order. Returns `None` when the database has no sessions.
    pub fn session_header(&self) -> Result<Option<SessionRow>> {
        let sql = format!(
            "SELECT {} FROM sessions ORDER BY id LIMIT 1",
            self.sessions_columns()
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(self.row_to_session(row)?));
        }
        Ok(None)
    }

    /// Single session row by id. Returns `None` when the database has no
    /// session with that id.
    pub fn session_row(&self, session_id: &str) -> Result<Option<SessionRow>> {
        let sql = format!(
            "SELECT {} FROM sessions WHERE id = ?1",
            self.sessions_columns()
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([session_id])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(self.row_to_session(row)?));
        }
        Ok(None)
    }

    /// All sessions stored in this database, ordered by `created_at`
    /// ascending.
    pub fn list_sessions(&self) -> Result<Vec<SessionRow>> {
        let sql = format!(
            "SELECT {} FROM sessions ORDER BY created_at ASC, id ASC",
            self.sessions_columns()
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(self.row_to_session(row)?);
        }
        Ok(out)
    }

    /// All entries for a given `session_id` in stable order: `seq`
    /// ascending, then `id` ascending to break upstream ties.
    pub fn iter_entries(&self, session_id: &str) -> Result<Vec<DecodedEntry>> {
        let sql = format!(
            "SELECT {} FROM entries WHERE session_id = ?1 ORDER BY {}",
            self.entries_columns(),
            self.entries_order()
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([session_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(self.decode_entry_row(row)?);
        }
        Ok(out)
    }

    /// Single entry by `(session_id, seq)`. Upstream keys rows by
    /// `(session_id, id)`, so the lookup is an ordered scan on `seq` and
    /// returns the lowest-`id` row among `seq` ties.
    pub fn get_entry(&self, session_id: &str, seq: i64) -> Result<Option<DecodedEntry>> {
        let sql = match self.layout {
            SchemaLayout::RustLegacy => format!(
                "SELECT {} FROM entries WHERE session_id = ?1 AND seq = ?2",
                self.entries_columns()
            ),
            SchemaLayout::UpstreamV4 => format!(
                "SELECT {} FROM entries WHERE session_id = ?1 AND seq = ?2 ORDER BY seq ASC, id ASC LIMIT 1",
                self.entries_columns()
            ),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params![session_id, seq])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(self.decode_entry_row(row)?));
        }
        Ok(None)
    }

    /// Single entry by `(session_id, entry_id)` (the logical id stamped
    /// on the entry by the agent; useful when replaying a tree). In the
    /// upstream layout `entry_id` is matched against the `entries.id`
    /// primary-key column.
    pub fn get_message(&self, session_id: &str, entry_id: &str) -> Result<Option<DecodedEntry>> {
        let sql = match self.layout {
            SchemaLayout::RustLegacy => format!(
                "SELECT {} FROM entries WHERE session_id = ?1 AND entry_id = ?2 LIMIT 1",
                self.entries_columns()
            ),
            SchemaLayout::UpstreamV4 => format!(
                "SELECT {} FROM entries WHERE session_id = ?1 AND id = ?2 LIMIT 1",
                self.entries_columns()
            ),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params![session_id, entry_id])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(self.decode_entry_row(row)?));
        }
        Ok(None)
    }

    /// Identify the "latest" session in the file (highest `created_at`,
    /// then highest `id` for ties).
    pub fn latest_session(&self) -> Result<Option<SessionRow>> {
        let sql = format!(
            "SELECT {} FROM sessions ORDER BY created_at DESC, id DESC LIMIT 1",
            self.sessions_columns()
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(self.row_to_session(row)?));
        }
        Ok(None)
    }

    /// Total number of entries for a session — handy for pagination /
    /// progress reporting in `pi session show`.
    pub fn count_entries(&self, session_id: &str) -> Result<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT count(*) FROM entries WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    fn sessions_columns(&self) -> &'static str {
        match self.layout {
            SchemaLayout::RustLegacy => LEGACY_SESSIONS_SQL,
            SchemaLayout::UpstreamV4 => UPSTREAM_SESSIONS_SQL,
        }
    }

    fn entries_columns(&self) -> &'static str {
        match self.layout {
            SchemaLayout::RustLegacy => {
                "session_id, seq, parent_seq, entry_id, parent_entry_id, type, timestamp, payload"
            }
            SchemaLayout::UpstreamV4 => {
                "session_id, id, parent_id, seq, type, custom_type, timestamp, payload"
            }
        }
    }

    fn entries_order(&self) -> &'static str {
        match self.layout {
            SchemaLayout::RustLegacy => "seq ASC",
            SchemaLayout::UpstreamV4 => "seq ASC, id ASC",
        }
    }

    fn row_to_session(&self, row: &Row<'_>) -> Result<SessionRow> {
        match self.layout {
            SchemaLayout::RustLegacy => Ok(SessionRow {
                id: row.get(0)?,
                created_at: row.get(1)?,
                parent_session: row.get(2)?,
                cwd: row.get(3)?,
                version: row.get(4)?,
                metadata: row.get(5)?,
            }),
            SchemaLayout::UpstreamV4 => {
                let metadata: Option<String> = row.get(4)?;
                Ok(SessionRow {
                    id: row.get(0)?,
                    created_at: row.get(1)?,
                    parent_session: row.get(2)?,
                    // Upstream has no `cwd` (and no per-session `version`)
                    // column; see the module docs for the mapping.
                    cwd: None,
                    version: version_from_metadata(metadata.as_deref()),
                    metadata,
                })
            }
        }
    }

    fn decode_entry_row(&self, row: &Row<'_>) -> Result<DecodedEntry> {
        match self.layout {
            SchemaLayout::RustLegacy => decode_legacy_entry_row(row),
            SchemaLayout::UpstreamV4 => decode_upstream_entry_row(row),
        }
    }
}

/// Pull `version` out of the upstream `sessions.metadata` JSON blob.
///
/// Upstream sets `metadata` to `NULL` at session creation, so this is
/// normally `None` and [`SessionRow::to_header`] yields an empty version.
/// Treats anything that is not a JSON object with a string `version`
/// field as absent rather than erroring.
fn version_from_metadata(metadata: Option<&str>) -> Option<String> {
    let metadata = metadata?;
    let parsed: Value = serde_json::from_str(metadata).ok()?;
    parsed
        .get("version")?
        .as_str()
        .map(std::string::ToString::to_string)
}

fn decode_legacy_entry_row(row: &Row<'_>) -> Result<DecodedEntry> {
    let entry_row = EntryRow {
        session_id: row.get(0)?,
        seq: row.get(1)?,
        parent_seq: row.get(2)?,
        entry_id: row.get(3)?,
        parent_entry_id: row.get(4)?,
        type_: row.get(5)?,
        timestamp: row.get(6)?,
        payload: row.get(7)?,
    };
    let bytes = entry_row.payload;
    let decompressed = if bytes.len() >= 4 && bytes[..4] == [0x28, 0xB5, 0x2F, 0xFD] {
        zstd::decode_all(bytes.as_slice()).map_err(|e| SessionError::Zstd(e.to_string()))?
    } else {
        // Fallback: payload was stored uncompressed (older databases or
        // hand-written fixtures). Try to decode as UTF-8 JSON.
        bytes.clone()
    };
    let entry: SessionEntry = serde_json::from_slice(&decompressed)?;
    Ok(DecodedEntry {
        seq: entry_row.seq,
        parent_seq: entry_row.parent_seq,
        entry_id: entry_row.entry_id,
        parent_entry_id: entry_row.parent_entry_id,
        type_: entry_row.type_,
        timestamp: entry_row.timestamp,
        entry,
    })
}

/// Decode one upstream (`AgentHarness` storage format 4) entry row.
fn decode_upstream_entry_row(row: &Row<'_>) -> Result<DecodedEntry> {
    let entry_id: String = row.get(1)?;
    let parent_entry_id: Option<String> = row.get(2)?;
    let seq: i64 = row.get(3)?;
    let type_: String = row.get(4)?;
    let custom_type: Option<String> = row.get(5)?;
    let timestamp: i64 = row.get(6)?;
    let payload: String = row.get(7)?;

    let entry = decode_upstream_entry(&type_, custom_type.as_deref(), &payload)?;
    Ok(DecodedEntry {
        seq,
        parent_seq: None,
        entry_id: Some(entry_id),
        parent_entry_id,
        type_,
        timestamp,
        entry,
    })
}

/// Map an upstream entry (`type` + optional `custom_type` + plain-JSON
/// `payload`) onto a [`SessionEntry`].
///
/// Entry types this crate does not model are passed through as
/// [`SessionEntry::Extension`] so nothing is dropped, and a warning is
/// logged so the gap is visible in logs.
pub fn decode_upstream_entry(
    type_: &str,
    custom_type: Option<&str>,
    payload: &str,
) -> Result<SessionEntry> {
    let payload: Value = serde_json::from_str(payload)?;
    Ok(match type_ {
        "message" => upstream_message_entry(&payload),
        "compaction" => upstream_compaction_entry(&payload),
        // `pi_protocol::SessionEntry` is matched exhaustively in
        // `pi-coding-agent`; a new `BranchSummary` variant would break
        // that crate's build, so the upstream payload travels verbatim.
        "branch_summary" => SessionEntry::Extension {
            extension: "branch_summary".into(),
            kind: "branch_summary".into(),
            payload,
        },
        "custom" => SessionEntry::Extension {
            extension: "custom".into(),
            kind: custom_type.unwrap_or("custom").to_string(),
            payload: payload.get("data").cloned().unwrap_or(payload),
        },
        other => {
            tracing::warn!(
                entry_type = other,
                "pi-session: upstream entry type has no Rust mapping; passing the payload through as an extension"
            );
            SessionEntry::Extension {
                extension: "upstream".into(),
                kind: other.to_string(),
                payload,
            }
        }
    })
}

fn upstream_message_entry(payload: &Value) -> SessionEntry {
    let message = payload.get("message").unwrap_or(&Value::Null);
    match message.get("role").and_then(Value::as_str) {
        Some("user") => SessionEntry::UserMessage(Message {
            role: Role::User,
            content: upstream_content(message.get("content")),
            model: None,
        }),
        Some("system") => SessionEntry::UserMessage(Message {
            role: Role::System,
            content: upstream_content(message.get("content")),
            model: None,
        }),
        Some("assistant") => SessionEntry::AssistantMessage(AssistantMessage {
            model: message
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            content: upstream_content(message.get("content")),
            stop_reason: upstream_stop_reason(message.get("stopReason").and_then(Value::as_str)),
            usage: upstream_usage(message.get("usage")),
            error_message: message
                .get("errorMessage")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(std::string::ToString::to_string),
        }),
        Some("toolResult") => SessionEntry::ToolResult(ToolResult {
            tool_call_id: message
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            content: Box::new(upstream_tool_result_content(message.get("content"))),
            is_error: message
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            details: non_null(message.get("details")),
            added_tool_names: message.get("addedToolNames").and_then(Value::as_array).map(
                |names| {
                    names
                        .iter()
                        .filter_map(|name| name.as_str().map(std::string::ToString::to_string))
                        .collect()
                },
            ),
        }),
        other => {
            tracing::warn!(
                role = other.unwrap_or("<missing>"),
                "pi-session: upstream message role has no Rust mapping; passing the message through as an extension"
            );
            SessionEntry::Extension {
                extension: "message".into(),
                kind: other.unwrap_or("unknown").to_string(),
                payload: message.clone(),
            }
        }
    }
}

fn upstream_compaction_entry(payload: &Value) -> SessionEntry {
    SessionEntry::Compaction {
        summary: payload
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        retained_tail: payload
            .get("retainedTail")
            .and_then(Value::as_array)
            .map(|tail| tail.iter().filter_map(upstream_context_message).collect())
            .unwrap_or_default(),
        tokens_before: payload
            .get("tokensBefore")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .try_into()
            .unwrap_or(u32::MAX),
        usage: non_null(payload.get("usage"))
            .as_ref()
            .map(|usage| upstream_usage(Some(usage))),
        details: non_null(payload.get("details")),
    }
}

/// Convert an upstream `AgentMessage` into the Rust context `Message`
/// shape used by [`SessionEntry::Compaction::retained_tail`].
fn upstream_context_message(message: &Value) -> Option<Message> {
    match message.get("role").and_then(Value::as_str)? {
        "user" => Some(Message {
            role: Role::User,
            content: upstream_content(message.get("content")),
            model: None,
        }),
        "system" => Some(Message {
            role: Role::System,
            content: upstream_content(message.get("content")),
            model: None,
        }),
        "assistant" => Some(Message {
            role: Role::Assistant,
            content: upstream_content(message.get("content")),
            model: message
                .get("model")
                .and_then(Value::as_str)
                .map(std::string::ToString::to_string),
        }),
        "toolResult" => Some(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                tool_call_id: message
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                content: Box::new(upstream_tool_result_content(message.get("content"))),
                is_error: message
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                details: non_null(message.get("details")),
                added_tool_names: None,
            })],
            model: None,
        }),
        other => {
            tracing::warn!(
                role = other,
                "pi-session: dropping a retained-tail message whose role has no Rust mapping"
            );
            None
        }
    }
}

fn non_null(value: Option<&Value>) -> Option<Value> {
    value.filter(|value| !value.is_null()).cloned()
}

/// Upstream message content: a bare string or an array of content blocks.
fn upstream_content(value: Option<&Value>) -> Vec<Content> {
    match value {
        Some(Value::String(text)) => vec![Content::Text(TextContent { text: text.clone() })],
        Some(Value::Array(blocks)) => blocks.iter().filter_map(upstream_block).collect(),
        Some(block @ Value::Object(_)) => upstream_block(block).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn upstream_block(block: &Value) -> Option<Content> {
    let object = block.as_object()?;
    match object.get("type").and_then(Value::as_str) {
        Some("text") => Some(Content::Text(TextContent {
            text: object
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })),
        Some("image") => Some(Content::Image(ImageContent {
            mime_type: object
                .get("mimeType")
                .or_else(|| object.get("mime_type"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            data: object
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })),
        Some("toolCall") => Some(Content::ToolCall(ToolCall {
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
            arguments: object.get("arguments").cloned().unwrap_or(Value::Null),
        })),
        other => {
            // `thinking` is the common case: `pi_protocol::Content` has no
            // thinking variant yet, so it cannot be represented.
            tracing::warn!(
                block_type = other.unwrap_or("<missing>"),
                "pi-session: upstream content block has no Rust mapping; skipping it"
            );
            None
        }
    }
}

/// Upstream tool-result content (an array of text/image blocks) folded
/// into the single block [`pi_protocol::ToolResult::content`] can hold.
fn upstream_tool_result_content(value: Option<&Value>) -> Content {
    let mut blocks = upstream_content(value);
    match blocks.len() {
        0 => Content::Text(TextContent::default()),
        1 => blocks.pop().expect("len checked"),
        n => {
            if blocks.iter().all(|block| matches!(block, Content::Text(_))) {
                let text = blocks
                    .iter()
                    .filter_map(|block| match block {
                        Content::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Content::Text(TextContent { text })
            } else {
                tracing::warn!(
                    blocks = n,
                    "pi-session: upstream tool result has several non-text blocks; only the first is representable"
                );
                blocks.into_iter().next().expect("len checked")
            }
        }
    }
}

fn upstream_usage(value: Option<&Value>) -> Usage {
    let Some(value) = value else {
        return Usage::default();
    };
    let field = |name: &str| -> u32 {
        value
            .get(name)
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .try_into()
            .unwrap_or(u32::MAX)
    };
    Usage {
        input: field("input"),
        output: field("output"),
        cache_read: field("cacheRead"),
        cache_write: field("cacheWrite"),
        total: field("totalTokens"),
    }
}

fn upstream_stop_reason(value: Option<&str>) -> StopReason {
    match value {
        Some("toolUse") => StopReason::ToolUse,
        Some("maxTokens") => StopReason::MaxTokens,
        Some("aborted") => StopReason::Aborted,
        Some("error") => StopReason::Error,
        Some("empty") => StopReason::Empty,
        _ => StopReason::Stop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_missing_file_errors_with_not_found() {
        let path = std::env::temp_dir().join("pi-session-reader-missing.sqlite");
        let _ = std::fs::remove_file(&path);
        let err = SessionReader::open(&path).unwrap_err();
        assert!(matches!(err, SessionError::NotFound(_)));
    }

    #[test]
    fn version_from_metadata_reads_only_string_version() {
        assert_eq!(
            version_from_metadata(Some(r#"{"version":"1.2.3"}"#)).as_deref(),
            Some("1.2.3")
        );
        assert_eq!(version_from_metadata(Some("null")), None);
        assert_eq!(version_from_metadata(Some(r#"{"version":42}"#)), None);
        assert_eq!(version_from_metadata(None), None);
    }

    #[test]
    fn upstream_stop_reasons_round_trip_to_rust_variants() {
        assert_eq!(upstream_stop_reason(Some("toolUse")), StopReason::ToolUse);
        assert_eq!(
            upstream_stop_reason(Some("maxTokens")),
            StopReason::MaxTokens
        );
        assert_eq!(upstream_stop_reason(Some("aborted")), StopReason::Aborted);
        assert_eq!(upstream_stop_reason(Some("error")), StopReason::Error);
        assert_eq!(upstream_stop_reason(Some("empty")), StopReason::Empty);
        assert_eq!(upstream_stop_reason(Some("stop")), StopReason::Stop);
        assert_eq!(upstream_stop_reason(None), StopReason::Stop);
    }

    #[test]
    fn unknown_upstream_entry_types_are_passed_through_not_dropped() {
        let entry = decode_upstream_entry("future_thing", None, r#"{"a":1}"#).unwrap();
        match entry {
            SessionEntry::Extension {
                extension,
                kind,
                payload,
            } => {
                assert_eq!(extension, "upstream");
                assert_eq!(kind, "future_thing");
                assert_eq!(payload["a"], 1);
            }
            other => panic!("expected passthrough extension, got {other:?}"),
        }
    }

    #[test]
    fn branch_summary_is_preserved_as_an_extension() {
        let payload = r#"{"fromId":"m1","summary":"forked","fromHook":true}"#;
        match decode_upstream_entry("branch_summary", None, payload).unwrap() {
            SessionEntry::Extension {
                extension, payload, ..
            } => {
                assert_eq!(extension, "branch_summary");
                assert_eq!(payload["fromId"], "m1");
                assert_eq!(payload["fromHook"], true);
            }
            other => panic!("expected branch summary passthrough, got {other:?}"),
        }
    }

    #[test]
    fn custom_entries_map_custom_type_to_kind() {
        match decode_upstream_entry("custom", Some("demo:marker"), r#"{"data":{"x":1}}"#).unwrap() {
            SessionEntry::Extension {
                extension,
                kind,
                payload,
            } => {
                assert_eq!(extension, "custom");
                assert_eq!(kind, "demo:marker");
                assert_eq!(payload["x"], 1);
            }
            other => panic!("expected extension, got {other:?}"),
        }
    }

    #[test]
    fn multi_text_tool_result_blocks_are_joined() {
        let content = upstream_tool_result_content(Some(&serde_json::json!([
            {"type": "text", "text": "a"},
            {"type": "text", "text": "b"},
        ])));
        match content {
            Content::Text(text) => assert_eq!(text.text, "a\nb"),
            other => panic!("expected joined text, got {other:?}"),
        }
    }

    #[test]
    fn message_entries_dispatch_on_role() {
        let user = decode_upstream_entry(
            "message",
            None,
            r#"{"message":{"role":"user","content":"hi","timestamp":1}}"#,
        )
        .unwrap();
        assert!(
            matches!(
                user,
                SessionEntry::UserMessage(Message {
                    role: Role::User,
                    ..
                })
            ),
            "got {user:?}"
        );

        let call = decode_upstream_entry(
            "message",
            None,
            r#"{"message":{"role":"assistant","model":"faux/m","content":[{"type":"toolCall","id":"c1","name":"bash","arguments":{"cmd":"ls"}}],"stopReason":"toolUse"}}"#,
        )
        .unwrap();
        match call {
            SessionEntry::AssistantMessage(message) => {
                assert_eq!(message.model, "faux/m");
                assert_eq!(message.stop_reason, StopReason::ToolUse);
                match &message.content[0] {
                    Content::ToolCall(call) => {
                        assert_eq!(call.id, "c1");
                        assert_eq!(call.arguments["cmd"], "ls");
                    }
                    other => panic!("expected tool call, got {other:?}"),
                }
            }
            other => panic!("expected assistant message, got {other:?}"),
        }
    }
}
