//! SQLite session writer for the **upstream v4** layout.
//!
//! [`SessionWriter::open`] creates the upstream
//! `packages/session-backends/sqlite-node` schema (AgentHarness storage
//! format 4 / `storageVersion 1` — see
//! [`UPSTREAM_INITIAL_SQL`](crate::schema::UPSTREAM_INITIAL_SQL)) and
//! buffers entries in memory until [`SessionWriter::commit`] flushes them
//! inside a single transaction. The staged rows and the
//! `sessions.message_count` / `sessions.next_seq` bookkeeping are updated
//! in the same transaction, so a half-written batch can never be observed
//! by a reader and a rejected row (the `trg_entries_validate` trigger
//! aborts on a missing parent or a duplicate entry/usage id) rolls the
//! whole batch back.
//!
//! # Column mapping
//!
//! | Rust [`SessionEntry`] | upstream `entries.type` | `entries.custom_type` | `payload` |
//! | --- | --- | --- | --- |
//! | [`Header`](SessionEntry::Header) | — | — | written to `sessions` (metadata carries `version`) |
//! | [`UserMessage`](SessionEntry::UserMessage) | `message` | — | `{"message": <AgentMessage>}` |
//! | [`AssistantMessage`](SessionEntry::AssistantMessage) | `message` | — | `{"message": <AssistantMessage>}` |
//! | [`ToolResult`](SessionEntry::ToolResult) | `message` | — | `{"message": <ToolResultMessage>}` |
//! | [`ToolCall`](SessionEntry::ToolCall) | `custom` | `tool_call` | `{"data": {id, name, arguments}}` |
//! | [`Extension`](SessionEntry::Extension) (`extension = "branch_summary"`) | `branch_summary` | — | the extension payload verbatim |
//! | [`Extension`](SessionEntry::Extension) (anything else) | `custom` | `kind` | `{"data": <payload>}` |
//! | [`Compaction`](SessionEntry::Compaction) | `compaction` | — | `{summary, retainedTail, tokensBefore, fromHook, …}` |
//!
//! Deliberate, documented degradations:
//!
//! * A standalone [`SessionEntry::ToolCall`] has no upstream entry type
//!   (upstream keeps tool calls inside the assistant `message` content
//!   array), so it is stored as a `custom` entry with
//!   `custom_type = "tool_call"`. On the way back it is an
//!   [`SessionEntry::Extension`] with `extension = "custom"` — see
//!   [`SessionReader`](crate::SessionReader) for the inverse mapping.
//! * The `extension` name of an [`SessionEntry::Extension`] is not stored:
//!   upstream models that namespace with a single `custom_type` string, so
//!   the value lands in `kind` and reads back as `extension = "custom"`.
//! * [`pi_protocol::Usage`] has no cost fields. Assistant/compaction usage
//!   is written with a zeroed `cost` object so the JSON still parses into
//!   the upstream `Usage` type.
//! * `sessions.usage_payload` is initialised to the upstream-shaped zero
//!   usage object (`zeroUsage()`), **not** `{}`: the upstream read path
//!   (`addUsageToSessionStats`) indexes the individual counters and would
//!   produce `NaN` on an empty object. This writer emits no
//!   `usage_ledger` rows yet, so the column stays at zero — the usage that
//!   travels with each assistant message stays inside that entry payload.
//!
//! # Rust legacy files
//!
//! [`SessionWriter::open`] on a pre-Stage-55 Rust file returns
//! [`SessionError::LegacyLayout`]; convert it first with
//! [`crate::migrate::migrate_file`] (CLI: `pi session migrate <path>`).

use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use pi_protocol::{
    AssistantMessage, Content, Message, Role, SessionEntry, StopReason, ToolResult, Usage,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::error::{Result, SessionError};
use crate::schema::{self, SchemaLayout};

/// zstd compression level used by the **Rust legacy** layout's `payload`
/// BLOB (zstd's own default). The upstream layout this writer emits
/// stores plain JSON text and never compresses, so the constant is kept
/// for the read path and the migration fixtures only.
pub const ZSTD_LEVEL: i32 = 3;

/// `sessions.storage_version` for the upstream layout this writer emits.
const STORAGE_VERSION: i64 = 1;

/// In-memory staging area for one upstream `entries` row.
#[derive(Debug, Clone)]
struct Pending {
    session_id: String,
    id: String,
    parent_id: Option<String>,
    seq: i64,
    type_: String,
    custom_type: Option<String>,
    timestamp: i64,
    payload: String,
}

/// Streamed entry after mapping onto the upstream shape.
struct MappedEntry {
    type_: String,
    custom_type: Option<String>,
    payload: Value,
    timestamp: i64,
}

/// Append-only session writer (upstream v4 layout).
///
/// Cloning shares the underlying connection (one writer per file).
#[derive(Debug)]
pub struct SessionWriter {
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    conn: Connection,
    path: PathBuf,
    layout: SchemaLayout,
    next_seq: i64,
    last_entry_id: Option<String>,
    current_session: Option<String>,
    pending: Vec<Pending>,
    buffer_limit: usize,
}

impl SessionWriter {
    /// Open or create an upstream v4 session database at `path`.
    ///
    /// Missing parent directories are created. An existing Rust legacy
    /// file is rejected with [`SessionError::LegacyLayout`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let (conn, layout) = schema::open_and_init(&path)?;
        Ok(Self {
            inner: Mutex::new(Inner {
                conn,
                path,
                layout,
                next_seq: 1,
                last_entry_id: None,
                current_session: None,
                pending: Vec::new(),
                buffer_limit: 64,
            }),
        })
    }

    /// Path the writer is bound to.
    pub fn path(&self) -> PathBuf {
        self.inner.lock().path.clone()
    }

    /// Detected on-disk layout. Always [`SchemaLayout::UpstreamV4`] for a
    /// writer that opened successfully.
    pub fn layout(&self) -> SchemaLayout {
        self.inner.lock().layout
    }

    /// Current next-seq value (1-based), mirroring
    /// `sessions.next_seq` once staged entries are committed. Before
    /// [`write_header`](Self::write_header) / [`resume`](Self::resume)
    /// this is the default `1`.
    pub fn next_seq(&self) -> i64 {
        self.inner.lock().next_seq
    }

    /// Stage a session header. The header is written when
    /// [`commit`](Self::commit) is called.
    ///
    /// The upstream schema keeps the header in `sessions` and has no
    /// header entry type, so this inserts one `sessions` row. Re-opening
    /// a session is idempotent: an existing row is left untouched and the
    /// in-memory sequence is resynchronised from it (equivalent to a
    /// follow-up [`resume`](Self::resume)).
    ///
    /// Rust's [`SessionEntry::Header::version`] has no upstream column, so
    /// it is stored in `sessions.metadata` as `{"version": …}` — the same
    /// place the reader looks for it.
    pub fn write_header(&self, header: SessionEntry) -> Result<()> {
        let SessionEntry::Header {
            id,
            created_at,
            version,
        } = header
        else {
            return Err(SessionError::Other(
                "write_header requires SessionEntry::Header".to_string(),
            ));
        };
        self.flush_before_switching_session(&id)?;

        let mut inner = self.inner.lock();
        let exists: i64 = inner.conn.query_row(
            "SELECT count(*) FROM sessions WHERE id = ?1",
            params![&id],
            |row| row.get(0),
        )?;
        if exists == 0 {
            let metadata = if version.is_empty() {
                None
            } else {
                Some(json!({ "version": version }).to_string())
            };
            inner.conn.execute(
                "INSERT INTO sessions \
                 (id, created_at, parent_session_id, storage_version, metadata, message_count, usage_payload, next_seq) \
                 VALUES (?1, ?2, NULL, ?3, ?4, 0, ?5, 1)",
                params![
                    &id,
                    created_at.timestamp_millis(),
                    STORAGE_VERSION,
                    metadata,
                    zero_usage_json().to_string(),
                ],
            )?;
            inner.next_seq = 1;
            inner.last_entry_id = None;
        } else {
            inner.next_seq = next_seq_of(&inner.conn, &id)?;
            inner.last_entry_id = last_entry_id_of(&inner.conn, &id)?;
        }
        inner.current_session = Some(id);
        Ok(())
    }

    /// Attach the writer to an existing session in this database.
    ///
    /// Sets the current session and continues the entry sequence from
    /// `sessions.next_seq` (upstream's authoritative counter), restoring
    /// the parent-entry chain from the highest `seq` row so a resumed
    /// append keeps the linear transcript connected (the
    /// `trg_entries_validate` trigger aborts on a missing parent).
    ///
    /// Call after [`write_header`](Self::write_header), which creates the
    /// `sessions` row.
    pub fn resume(&self, session_id: &str) -> Result<()> {
        self.flush_before_switching_session(session_id)?;
        let mut inner = self.inner.lock();
        inner.next_seq = next_seq_of(&inner.conn, session_id)?;
        inner.last_entry_id = last_entry_id_of(&inner.conn, session_id)?;
        inner.current_session = Some(session_id.to_string());
        Ok(())
    }

    /// Store the session's display name in the upstream
    /// `sessions.metadata` JSON blob, preserving any other keys already
    /// there (the `version` written by
    /// [`write_header`](Self::write_header)).
    ///
    /// Upstream records the name in a `session_info` entry; the Rust port
    /// has no such entry variant, so `/name` keeps it in the session row's
    /// metadata instead — the same place [`crate::schema::session_name_from_metadata`]
    /// reads it back from. Requires a current session
    /// ([`write_header`](Self::write_header) or [`resume`](Self::resume)).
    pub fn set_session_name(&self, name: &str) -> Result<()> {
        let inner = self.inner.lock();
        let session_id = inner.current_session.clone().ok_or_else(|| {
            SessionError::Other(
                "set_session_name called before write_header/resume: the upstream schema needs a sessions row"
                    .to_string(),
            )
        })?;
        let metadata: Option<String> = inner
            .conn
            .query_row(
                "SELECT metadata FROM sessions WHERE id = ?1",
                params![&session_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                SessionError::Other(format!(
                    "unknown session {session_id:?} in this database; call write_header first"
                ))
            })?;
        let mut object = metadata
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        object.insert("name".to_string(), Value::String(name.to_string()));
        inner.conn.execute(
            "UPDATE sessions SET metadata = ?1 WHERE id = ?2",
            params![Value::Object(object).to_string(), &session_id],
        )?;
        Ok(())
    }

    /// Append a [`SessionEntry`]. The entry is staged in memory until
    /// [`commit`](Self::commit) is called (or the buffer limit is
    /// reached, in which case a commit runs implicitly).
    ///
    /// [`SessionEntry::Header`] delegates to
    /// [`write_header`](Self::write_header). Any other entry requires a
    /// current session (set by `write_header` or `resume`).
    pub fn append(&self, entry: SessionEntry) -> Result<()> {
        if matches!(entry, SessionEntry::Header { .. }) {
            return self.write_header(entry);
        }

        let mut inner = self.inner.lock();
        let session_id = inner.current_session.clone().ok_or_else(|| {
            SessionError::Other(
                "append called before write_header/resume: the upstream schema needs a sessions row"
                    .to_string(),
            )
        })?;
        let seq = inner.next_seq;
        let id = entry_id_for_seq(seq);
        let mapped = map_entry(&entry)?;
        let parent_id = inner.last_entry_id.clone();
        inner.next_seq += 1;
        inner.pending.push(Pending {
            session_id,
            id: id.clone(),
            parent_id,
            seq,
            type_: mapped.type_,
            custom_type: mapped.custom_type,
            timestamp: mapped.timestamp,
            payload: mapped.payload.to_string(),
        });
        inner.last_entry_id = Some(id);
        if inner.pending.len() >= inner.buffer_limit {
            drop(inner);
            self.commit()?;
        }
        Ok(())
    }

    /// Flush the staged entries inside one transaction. Returns the number
    /// of rows committed.
    ///
    /// The `sessions.message_count` (message entries only, as upstream
    /// defines it) and `sessions.next_seq` columns are updated in the same
    /// transaction. On failure the staged rows are kept in memory and can
    /// be retried; nothing is written to the database.
    pub fn commit(&self) -> Result<usize> {
        let mut inner = self.inner.lock();
        if inner.pending.is_empty() {
            return Ok(0);
        }
        let count = inner.pending.len();
        let next_seq = inner.next_seq;
        let session_id = inner.current_session.clone();
        let pending = std::mem::take(&mut inner.pending);
        match flush(&mut inner.conn, &pending, session_id.as_deref(), next_seq) {
            Ok(()) => Ok(count),
            Err(err) => {
                // Keep the batch so the caller can inspect/retry; the
                // transaction already rolled back, so the database is
                // clean.
                inner.pending = pending;
                Err(err)
            }
        }
    }

    /// Force a commit + run `PRAGMA wal_checkpoint(TRUNCATE)` so the WAL
    /// file is folded back into the main database. Use this before
    /// handing the file to another process (`node:sqlite`, the TS
    /// `SqliteStorage`, a copy) so the on-disk shape is exactly the
    /// committed state.
    pub fn checkpoint(&self) -> Result<usize> {
        let committed = self.commit()?;
        let inner = self.inner.lock();
        inner
            .conn
            .pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
        Ok(committed)
    }

    /// Drop the staged rows and resynchronise the in-memory sequence and
    /// parent chain from the committed state. The database is untouched.
    pub fn rollback(&self) {
        let mut inner = self.inner.lock();
        inner.pending.clear();
        if let Some(session_id) = inner.current_session.clone() {
            if let Ok(next_seq) = next_seq_of(&inner.conn, &session_id) {
                inner.next_seq = next_seq;
            }
            inner.last_entry_id = last_entry_id_of(&inner.conn, &session_id).ok().flatten();
        }
    }

    /// Commit staged rows when the caller switches to a different session,
    /// so one transaction never mixes two session ids.
    fn flush_before_switching_session(&self, session_id: &str) -> Result<()> {
        let needs_flush = {
            let inner = self.inner.lock();
            !inner.pending.is_empty() && inner.current_session.as_deref() != Some(session_id)
        };
        if needs_flush {
            self.commit()?;
        }
        Ok(())
    }
}

/// Insert the staged rows, the `message_count` delta and `next_seq` in a
/// single transaction.
fn flush(
    conn: &mut Connection,
    pending: &[Pending],
    session_id: Option<&str>,
    next_seq: i64,
) -> Result<()> {
    let tx = conn.transaction()?;
    for row in pending {
        tx.execute(
            "INSERT INTO entries \
             (session_id, id, parent_id, seq, type, custom_type, timestamp, payload) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                row.session_id,
                row.id,
                row.parent_id,
                row.seq,
                row.type_,
                row.custom_type,
                row.timestamp,
                row.payload,
            ],
        )?;
    }
    if let Some(session_id) = session_id {
        let messages = pending.iter().filter(|row| row.type_ == "message").count();
        if messages > 0 {
            tx.execute(
                "UPDATE sessions SET message_count = message_count + ?1 WHERE id = ?2",
                params![messages as i64, session_id],
            )?;
        }
        tx.execute(
            "UPDATE sessions SET next_seq = ?1 WHERE id = ?2",
            params![next_seq, session_id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn next_seq_of(conn: &Connection, session_id: &str) -> Result<i64> {
    conn.query_row(
        "SELECT next_seq FROM sessions WHERE id = ?1",
        params![session_id],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| {
        SessionError::Other(format!(
            "unknown session {session_id:?} in this database; call write_header first"
        ))
    })
}

fn last_entry_id_of(conn: &Connection, session_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT id FROM entries WHERE session_id = ?1 ORDER BY seq DESC, id DESC LIMIT 1",
            params![session_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Deterministic entry id: `e<seq>`. The upstream primary key is
/// `(session_id, id)` and `seq` continues past every stored row, so the
/// id is unique within the session. When resuming a TS-written file whose
/// ids are uuids, the new ids never collide with the existing ones.
fn entry_id_for_seq(seq: i64) -> String {
    format!("e{seq}")
}

/// Upstream zero-usage object (`zeroUsage()` in
/// `packages/session-backends/sqlite-node`): a full counter object with a
/// zeroed `cost` object, **not** `{}`.
fn zero_usage_json() -> Value {
    json!({
        "input": 0,
        "output": 0,
        "cacheRead": 0,
        "cacheWrite": 0,
        "totalTokens": 0,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0},
    })
}

fn usage_to_upstream(usage: &Usage) -> Value {
    json!({
        "input": usage.input,
        "output": usage.output,
        "cacheRead": usage.cache_read,
        "cacheWrite": usage.cache_write,
        "totalTokens": usage.total,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0},
    })
}

fn stop_reason_str(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Stop => "stop",
        StopReason::ToolUse => "toolUse",
        StopReason::MaxTokens => "maxTokens",
        StopReason::Aborted => "aborted",
        StopReason::Error => "error",
        StopReason::Empty => "empty",
    }
}

fn content_blocks(blocks: &[Content]) -> Value {
    Value::Array(blocks.iter().map(content_block).collect())
}

fn content_block(block: &Content) -> Value {
    match block {
        Content::Text(text) => json!({"type": "text", "text": text.text}),
        Content::Image(image) => {
            json!({"type": "image", "mimeType": image.mime_type, "data": image.data})
        }
        Content::ToolCall(call) => {
            json!({"type": "toolCall", "id": call.id, "name": call.name, "arguments": call.arguments})
        }
        Content::ToolResult(result) => {
            json!({"type": "text", "text": tool_result_inline_text(result)})
        }
    }
}

/// Render a nested tool result as a text block (a tool result is not a
/// content-block kind upstream).
fn tool_result_inline_text(result: &ToolResult) -> String {
    match result.content.as_ref() {
        Content::Text(text) => text.text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Upstream `ToolResultMessage` (role `toolResult`).
fn tool_result_message(result: &ToolResult, timestamp: i64) -> Value {
    let mut value = json!({
        "role": "toolResult",
        "toolCallId": result.tool_call_id,
        "content": [content_block(result.content.as_ref())],
        "isError": result.is_error,
        "timestamp": timestamp,
    });
    if let Some(details) = &result.details {
        value["details"] = details.clone();
    }
    if let Some(names) = &result.added_tool_names {
        value["addedToolNames"] = json!(names);
    }
    value
}

/// Map a context [`Message`] onto an upstream `AgentMessage` JSON value.
fn message_to_upstream(message: &Message, timestamp: i64) -> Value {
    match message.role {
        Role::System => json!({
            "role": "system",
            "content": content_blocks(&message.content),
            "timestamp": timestamp,
        }),
        Role::User => json!({
            "role": "user",
            "content": content_blocks(&message.content),
            "timestamp": timestamp,
        }),
        Role::Assistant => {
            let mut value = json!({
                "role": "assistant",
                "content": content_blocks(&message.content),
                "timestamp": timestamp,
            });
            if let Some(model) = &message.model {
                value["model"] = json!(model);
            }
            value
        }
        Role::Tool => {
            // A context tool message carries exactly one `ToolResult`
            // block; upstream spells that as a `toolResult` message.
            match message.content.iter().find_map(|block| match block {
                Content::ToolResult(result) => Some(result),
                _ => None,
            }) {
                Some(result) => tool_result_message(result, timestamp),
                None => json!({
                    "role": "toolResult",
                    "toolCallId": "",
                    "content": content_blocks(&message.content),
                    "isError": false,
                    "timestamp": timestamp,
                }),
            }
        }
    }
}

fn assistant_to_upstream(message: &AssistantMessage, timestamp: i64) -> Value {
    let mut value = json!({
        "role": "assistant",
        "model": message.model,
        "content": content_blocks(&message.content),
        "stopReason": stop_reason_str(message.stop_reason),
        "usage": usage_to_upstream(&message.usage),
        "timestamp": timestamp,
    });
    if let Some(error) = &message.error_message {
        value["errorMessage"] = json!(error);
    }
    value
}

fn map_entry(entry: &SessionEntry) -> Result<MappedEntry> {
    let timestamp = schema::now_millis();
    let mapped = match entry {
        // `append` routes headers to `write_header`.
        SessionEntry::Header { .. } => {
            return Err(SessionError::Other(
                "SessionEntry::Header must be written with write_header".to_string(),
            ))
        }
        SessionEntry::UserMessage(message) => MappedEntry {
            type_: "message".into(),
            custom_type: None,
            payload: json!({"message": message_to_upstream(message, timestamp)}),
            timestamp,
        },
        SessionEntry::AssistantMessage(message) => MappedEntry {
            type_: "message".into(),
            custom_type: None,
            payload: json!({"message": assistant_to_upstream(message, timestamp)}),
            timestamp,
        },
        SessionEntry::ToolResult(result) => MappedEntry {
            type_: "message".into(),
            custom_type: None,
            payload: json!({"message": tool_result_message(result, timestamp)}),
            timestamp,
        },
        SessionEntry::ToolCall(call) => MappedEntry {
            type_: "custom".into(),
            custom_type: Some("tool_call".into()),
            payload: json!({"data": serde_json::to_value(call)?}),
            timestamp,
        },
        SessionEntry::Extension {
            extension,
            kind,
            payload,
        } => {
            if extension == "branch_summary" {
                MappedEntry {
                    type_: "branch_summary".into(),
                    custom_type: None,
                    payload: payload.clone(),
                    timestamp,
                }
            } else {
                MappedEntry {
                    type_: "custom".into(),
                    custom_type: Some(kind.clone()),
                    payload: json!({"data": payload}),
                    timestamp,
                }
            }
        }
        SessionEntry::Compaction {
            summary,
            retained_tail,
            tokens_before,
            usage,
            details,
        } => {
            let mut payload = json!({
                "summary": summary,
                "retainedTail": retained_tail
                    .iter()
                    .map(|message| message_to_upstream(message, timestamp))
                    .collect::<Vec<_>>(),
                "tokensBefore": tokens_before,
                "fromHook": false,
            });
            if let Some(usage) = usage {
                payload["usage"] = usage_to_upstream(usage);
            }
            if let Some(details) = details {
                payload["details"] = details.clone();
            }
            MappedEntry {
                type_: "compaction".into(),
                custom_type: None,
                payload,
                timestamp,
            }
        }
    };
    Ok(mapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pi-session-writer-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn appends_header_then_entries() {
        let dir = tempdir();
        let path = dir.join("session.sqlite");
        let writer = SessionWriter::open(&path).expect("open");
        assert_eq!(writer.layout(), SchemaLayout::UpstreamV4);
        writer
            .write_header(SessionEntry::Header {
                id: "abc".into(),
                created_at: chrono::Utc::now(),
                version: "0.1.0".into(),
            })
            .expect("header");
        writer
            .append(SessionEntry::Extension {
                extension: "test".into(),
                kind: "marker".into(),
                payload: serde_json::json!({"hello": "world"}),
            })
            .expect("append");
        let n = writer.commit().expect("commit");
        assert_eq!(n, 1);
        assert_eq!(writer.next_seq(), 2);
        writer.checkpoint().expect("checkpoint");

        // The payload lands as plain JSON text, not a compressed blob.
        let conn = Connection::open(&path).expect("reopen");
        let (type_, custom_type, payload): (String, Option<String>, String) = conn
            .query_row(
                "SELECT type, custom_type, payload FROM entries",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("row");
        assert_eq!(type_, "custom");
        assert_eq!(custom_type.as_deref(), Some("marker"));
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload is JSON");
        assert_eq!(parsed["data"]["hello"], "world");
    }

    #[test]
    fn resume_continues_the_sequence() {
        let dir = tempdir();
        let path = dir.join("resume.sqlite");
        let writer = SessionWriter::open(&path).expect("open");
        writer
            .write_header(SessionEntry::Header {
                id: "resume-id".into(),
                created_at: chrono::Utc::now(),
                version: "0.1.0".into(),
            })
            .expect("header");
        for i in 0..3 {
            writer
                .append(SessionEntry::Extension {
                    extension: "test".into(),
                    kind: format!("marker-{i}"),
                    payload: serde_json::json!(i),
                })
                .expect("append");
        }
        writer.checkpoint().expect("checkpoint");
        drop(writer);

        // A second writer starts at seq 1 until it resumes the session.
        let writer = SessionWriter::open(&path).expect("reopen");
        assert_eq!(writer.next_seq(), 1);
        writer.resume("resume-id").expect("resume");
        assert_eq!(writer.next_seq(), 4);

        // write_header is also idempotent and resynchronises.
        writer
            .write_header(SessionEntry::Header {
                id: "resume-id".into(),
                created_at: chrono::Utc::now(),
                version: "0.1.0".into(),
            })
            .expect("re-header");
        assert_eq!(writer.next_seq(), 4);
    }

    #[test]
    fn session_name_round_trips_through_metadata() {
        let dir = tempdir();
        let path = dir.join("named.sqlite");
        let writer = SessionWriter::open(&path).expect("open");
        writer
            .write_header(SessionEntry::Header {
                id: "named".into(),
                created_at: chrono::Utc::now(),
                version: "0.1.0".into(),
            })
            .expect("header");
        writer.set_session_name("my session").expect("set name");
        writer.checkpoint().expect("checkpoint");
        drop(writer);

        // The name lives in `metadata.name` and does not clobber the
        // `version` `write_header` stored there.
        let reader = crate::SessionReader::open(&path).expect("reader");
        assert_eq!(
            reader.session_name("named").expect("read name").as_deref(),
            Some("my session")
        );
        let header = reader.session_row("named").expect("row").expect("session");
        assert_eq!(header.version.as_deref(), Some("0.1.0"));
        assert_eq!(
            crate::schema::session_name_from_metadata(header.metadata.as_deref()).as_deref(),
            Some("my session")
        );

        // Renaming overwrites the previous name.
        let writer = SessionWriter::open(&path).expect("reopen");
        writer.resume("named").expect("resume");
        writer.set_session_name("renamed").expect("rename");
        writer.checkpoint().expect("checkpoint");
        let reader = crate::SessionReader::open(&path).expect("reader");
        assert_eq!(
            reader.session_name("named").expect("read name").as_deref(),
            Some("renamed")
        );
    }

    #[test]
    fn append_before_header_is_an_error() {
        let dir = tempdir();
        let path = dir.join("no-header.sqlite");
        let writer = SessionWriter::open(&path).expect("open");
        let err = writer
            .append(SessionEntry::UserMessage(Message {
                role: Role::User,
                content: vec![Content::Text(pi_protocol::TextContent {
                    text: "hi".into(),
                })],
                model: None,
            }))
            .expect_err("must fail");
        assert!(err.to_string().contains("write_header"));
    }
}
