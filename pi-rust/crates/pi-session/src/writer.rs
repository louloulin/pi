//! SQLite session writer.
//!
//! [`SessionWriter::open`] opens (or creates) a session database and
//! buffers entries in memory until [`SessionWriter::commit`] flushes
//! them inside a single transaction. zstd compression runs inside the
//! same transaction so a half-written entry can never be observed by a
//! reader.
//!
//! The writer is intentionally **single-writer-per-file**: a second
//! [`SessionWriter::open`] on the same path will not block — it simply
//! sees the rows committed by the first writer. Higher-level locking is
//! the caller's responsibility (the `pi-coding-agent` binary uses a
//! process-local mutex; the CLI migrate command takes an exclusive lock
//! via SQLite's own WAL).

use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use pi_protocol::SessionEntry;
use rusqlite::{params, Connection};

use crate::error::{Result, SessionError};
use crate::schema;

/// Default zstd compression level — matches the level the TS port uses
/// for its `messages` blob (level 3 in `zstd::DEFAULT_LEVEL`).
pub const ZSTD_LEVEL: i32 = 3;

/// In-memory staging area for one [`SessionEntry`].
#[derive(Debug, Clone)]
struct Pending {
    session_id: String,
    seq: i64,
    parent_seq: Option<i64>,
    entry_id: Option<String>,
    parent_entry_id: Option<String>,
    type_: String,
    timestamp: i64,
    payload: Vec<u8>,
}

/// Append-only session writer.
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
    next_seq: i64,
    current_session: Option<String>,
    pending: Vec<Pending>,
    buffer_limit: usize,
}

/// Tuple form used by [`SessionWriter::append`] staging.
#[allow(clippy::type_complexity)]
type Classified = (
    String,
    Option<i64>,
    Option<String>,
    Option<String>,
    String,
    i64,
);

impl SessionWriter {
    /// Open or create a session database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = schema::open_and_init(&path)?;
        Ok(Self {
            inner: Mutex::new(Inner {
                conn,
                path,
                next_seq: 1,
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

    /// Current next-seq value (1-based). Mirrors the TS port's
    /// `sessions.next_seq`.
    pub fn next_seq(&self) -> i64 {
        self.inner.lock().next_seq
    }

    /// Stage a session header. The header is written when [`commit`] is
    /// called. If a header for the same `session_id` already exists in
    /// the database this is a no-op (idempotent on re-open).
    ///
    /// [`commit`]: Self::commit
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
        let mut inner = self.inner.lock();
        // Idempotent: only insert if missing.
        let existing: i64 = inner
            .conn
            .query_row(
                "SELECT count(*) FROM sessions WHERE id = ?1",
                params![&id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if existing == 0 {
            inner.conn.execute(
                "INSERT INTO sessions (id, created_at, parent_session, cwd, version, metadata) \
                 VALUES (?1, ?2, NULL, NULL, ?3, NULL)",
                params![&id, created_at.timestamp_millis(), &version],
            )?;
        }
        inner.current_session = Some(id);
        Ok(())
    }

    /// Attach the writer to an existing session in this database.
    ///
    /// Sets the current session id and continues the entry sequence
    /// *after* the highest `seq` already stored, so an append-only
    /// caller (e.g. `pi --print --continue`) never reuses a primary key.
    /// A freshly created session has no rows, so `next_seq` resets to 1.
    ///
    /// Call after [`write_header`](Self::write_header) (which creates the
    /// `sessions` row) — the two are complementary: `write_header` is
    /// idempotent on the header while `resume` derives the sequence from
    /// the `entries` table.
    pub fn resume(&self, session_id: &str) -> Result<()> {
        let mut inner = self.inner.lock();
        let max_seq: i64 = inner.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM entries WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        inner.next_seq = max_seq + 1;
        inner.current_session = Some(session_id.to_string());
        Ok(())
    }

    /// Append a [`SessionEntry`]. The entry is staged in memory until
    /// [`commit`] is called (or the buffer limit is reached, in which
    /// case a commit runs implicitly).
    ///
    /// [`commit`]: Self::commit
    pub fn append(&self, entry: SessionEntry) -> Result<()> {
        let mut inner = self.inner.lock();
        let (session_id, parent_seq, entry_id, parent_entry_id, type_, timestamp) =
            classify(&entry, inner.current_session.as_deref())?;
        let payload = encode_payload(&entry)?;
        let seq = inner.next_seq;
        inner.next_seq += 1;
        inner.pending.push(Pending {
            session_id,
            seq,
            parent_seq,
            entry_id,
            parent_entry_id,
            type_,
            timestamp,
            payload,
        });
        if inner.pending.len() >= inner.buffer_limit {
            drop(inner);
            self.commit()?;
        }
        Ok(())
    }

    /// Force a flush of the staged entries. Returns the number of rows
    /// committed.
    pub fn commit(&self) -> Result<usize> {
        let mut inner = self.inner.lock();
        if inner.pending.is_empty() {
            return Ok(0);
        }
        let count = inner.pending.len();
        let pending = std::mem::take(&mut inner.pending);
        let tx = inner.conn.transaction()?;
        for row in &pending {
            tx.execute(
                "INSERT INTO entries (session_id, seq, parent_seq, entry_id, parent_entry_id, type, timestamp, payload) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    row.session_id,
                    row.seq,
                    row.parent_seq,
                    row.entry_id,
                    row.parent_entry_id,
                    row.type_,
                    row.timestamp,
                    row.payload,
                ],
            )?;
        }
        tx.commit()?;
        Ok(count)
    }

    /// Force a commit + run `PRAGMA wal_checkpoint(TRUNCATE)` so the WAL
    /// file is folded back into the main database. Use this before
    /// shipping the file to another tool (e.g. `pi session export`) so
    /// the on-disk shape is exactly the committed state.
    pub fn checkpoint(&self) -> Result<usize> {
        let committed = self.commit()?;
        let inner = self.inner.lock();
        inner.conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
        Ok(committed)
    }

    /// Drop the connection without committing the staged rows. Staged
    /// rows are lost.
    pub fn rollback(&self) {
        self.inner.lock().pending.clear();
    }
}

fn classify(entry: &SessionEntry, current_session: Option<&str>) -> Result<Classified> {
    let (type_, ts_value): (String, i64) = match entry {
        SessionEntry::Header { .. } => ("header".to_string(), schema::now_millis()),
        SessionEntry::UserMessage(msg) => ("user_message".to_string(), msg_ts(msg)),
        SessionEntry::AssistantMessage(msg) => ("assistant_message".to_string(), msg_ts_assistant(msg)),
        SessionEntry::ToolCall(_call) => (
            "tool_call".to_string(),
            chrono::Utc::now().timestamp_millis(),
        ),
        SessionEntry::ToolResult(_) => (
            "tool_result".to_string(),
            chrono::Utc::now().timestamp_millis(),
        ),
        SessionEntry::Extension { .. } => ("extension".to_string(), chrono::Utc::now().timestamp_millis()),
    };
    let session_id = match entry {
        SessionEntry::Header { id, .. } => id.clone(),
        _ => current_session
            .map(str::to_string)
            .unwrap_or_else(|| "<default>".to_string()),
    };
    Ok((session_id, None, None, None, type_, ts_value))
}

fn msg_ts(_msg: &pi_protocol::Message) -> i64 {
    schema::now_millis()
}

fn msg_ts_assistant(_msg: &pi_protocol::AssistantMessage) -> i64 {
    schema::now_millis()
}

fn encode_payload(entry: &SessionEntry) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(entry)?;
    let compressed = zstd::encode_all(json.as_slice(), ZSTD_LEVEL).map_err(|e| SessionError::Zstd(e.to_string()))?;
    Ok(compressed)
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
        writer.checkpoint().expect("checkpoint");
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
    }
}