//! SQLite session reader.
//!
//! [`SessionReader::open`] opens a database produced by either the Rust
//! [`SessionWriter`](crate::SessionWriter) or the TS
//! `packages/session-backends/sqlite-node` port. [`SessionReader::iter_entries`]
//! yields each row as a [`SessionEntry`](pi_protocol::SessionEntry) in
//! stable (seq) order; zstd-compressed payloads are decompressed and
//! JSON-decoded transparently.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, Row};
use serde::Deserialize;

use crate::error::{Result, SessionError};
use crate::schema::{self, EntryRow, SessionRow};

/// Stored row whose `payload` column has already been decoded.
#[derive(Debug, Clone)]
pub struct DecodedEntry {
    /// 1-based sequence number within the session.
    pub seq: i64,
    /// Sequence number of the parent entry (for tree-shaped sessions).
    pub parent_seq: Option<i64>,
    /// Logical entry id (e.g. a uuid stamped by the agent).
    pub entry_id: Option<String>,
    /// Logical id of the parent entry.
    pub parent_entry_id: Option<String>,
    /// Entry type discriminator (header, user_message, ...).
    pub type_: String,
    /// Wall-clock timestamp in milliseconds since the unix epoch.
    pub timestamp: i64,
    /// Decoded [`SessionEntry`](pi_protocol::SessionEntry) value.
    pub entry: pi_protocol::SessionEntry,
}

impl DecodedEntry {
    /// Borrow the inner [`SessionEntry`](pi_protocol::SessionEntry).
    pub fn entry(&self) -> &pi_protocol::SessionEntry {
        &self.entry
    }
}

/// Read-only handle over a session database file.
#[derive(Debug)]
pub struct SessionReader {
    conn: Connection,
    path: PathBuf,
}

impl SessionReader {
    /// Open an existing session database. Returns
    /// [`SessionError::NotFound`](crate::SessionError::NotFound) when the
    /// file does not exist, and
    /// [`SessionError::Corrupt`](crate::SessionError::Corrupt) when the
    /// file is not a valid session database.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(SessionError::NotFound(path.to_path_buf()));
        }
        let conn = schema::open_read_only(path)?;
        Ok(Self {
            conn,
            path: path.to_path_buf(),
        })
    }

    /// Path the reader is bound to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the single stored session header. When the database holds
    /// multiple sessions (the schema allows it), returns the first one
    /// in `id` order. Returns `None` when the database has no sessions.
    pub fn session_header(&self) -> Result<Option<SessionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, parent_session, cwd, version, metadata FROM sessions ORDER BY id LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(row_to_session(row)?));
        }
        Ok(None)
    }

    /// All sessions stored in this database, ordered by `created_at`
    /// ascending.
    pub fn list_sessions(&self) -> Result<Vec<SessionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, parent_session, cwd, version, metadata FROM sessions ORDER BY created_at ASC, id ASC",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row_to_session(row)?);
        }
        Ok(out)
    }

    /// All entries for a given `session_id` in stable `seq` order.
    pub fn iter_entries(&self, session_id: &str) -> Result<Vec<DecodedEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, seq, parent_seq, entry_id, parent_entry_id, type, timestamp, payload \
             FROM entries WHERE session_id = ?1 ORDER BY seq ASC",
        )?;
        let mut rows = stmt.query([session_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(decode_entry_row(row)?);
        }
        Ok(out)
    }

    /// Single entry by `(session_id, seq)`.
    pub fn get_entry(&self, session_id: &str, seq: i64) -> Result<Option<DecodedEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, seq, parent_seq, entry_id, parent_entry_id, type, timestamp, payload \
             FROM entries WHERE session_id = ?1 AND seq = ?2",
        )?;
        let mut rows = stmt.query(rusqlite::params![session_id, seq])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(decode_entry_row(row)?));
        }
        Ok(None)
    }

    /// Single entry by `(session_id, entry_id)` (the logical id stamped
    /// on the entry by the agent; useful when replaying a tree).
    pub fn get_message(&self, session_id: &str, entry_id: &str) -> Result<Option<DecodedEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, seq, parent_seq, entry_id, parent_entry_id, type, timestamp, payload \
             FROM entries WHERE session_id = ?1 AND entry_id = ?2 LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![session_id, entry_id])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(decode_entry_row(row)?));
        }
        Ok(None)
    }

    /// Identify the "latest" session in the file (highest `created_at`,
    /// then highest `id` for ties).
    pub fn latest_session(&self) -> Result<Option<SessionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, parent_session, cwd, version, metadata \
             FROM sessions ORDER BY created_at DESC, id DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(row_to_session(row)?));
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
}

fn row_to_session(row: &Row<'_>) -> Result<SessionRow> {
    Ok(SessionRow {
        id: row.get(0)?,
        created_at: row.get(1)?,
        parent_session: row.get(2)?,
        cwd: row.get(3)?,
        version: row.get(4)?,
        metadata: row.get(5)?,
    })
}

fn decode_entry_row(row: &Row<'_>) -> Result<DecodedEntry> {
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
    let entry: pi_protocol::SessionEntry = serde_json::from_slice(&decompressed)?;
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

/// Lightweight shape used to JSON-decode TS-recorded fixtures without
/// depending on the exact Rust `SessionEntry` enum tagging. We only use
/// this on the `tests/` integration side; readers on production code
/// paths go through [`serde_json::from_slice`] directly.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum RawEntryShape {
    /// Session header (mirrors [`SessionEntry::Header`]).
    Header {
        /// Session identifier.
        id: String,
        /// Wall-clock time the session started.
        created_at: chrono::DateTime<chrono::Utc>,
        /// Pi version that produced the session.
        version: String,
    },
    /// User message — stored as opaque JSON for fixture comparison.
    UserMessage(serde_json::Value),
    /// Assistant message — stored as opaque JSON for fixture comparison.
    AssistantMessage(serde_json::Value),
    /// Tool call — stored as opaque JSON for fixture comparison.
    ToolCall(serde_json::Value),
    /// Tool result — stored as opaque JSON for fixture comparison.
    ToolResult(serde_json::Value),
    /// Free-form extension entry.
    Extension {
        /// Extension that wrote the entry.
        extension: String,
        /// User-supplied kind label.
        kind: String,
        /// Arbitrary JSON payload.
        payload: serde_json::Value,
    },
    /// Compaction checkpoint (`/compact`).
    Compaction {
        /// Structured summary text.
        summary: String,
        /// Recent messages kept verbatim.
        #[serde(default)]
        retained_tail: Vec<serde_json::Value>,
        /// Estimated context tokens before compaction.
        #[serde(default)]
        tokens_before: u32,
    },
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
}