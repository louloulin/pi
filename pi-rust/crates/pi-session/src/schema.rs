//! Schema management for the SQLite session backend.
//!
//! The schema is intentionally narrow: it stores one row per
//! [`SessionEntry`](pi_protocol::SessionEntry) with a JSON payload column
//! compressed via zstd. The TS port
//! (`packages/session-backends/sqlite-node`) writes the same JSON
//! serialization in its payload column, so files written by either side
//! can be read by the other.
//!
//! The schema is **versioned** via `PRAGMA user_version`. Stage 5 ships
//! version 1; future stages can add `v2_initial.sql` migrations without
//! breaking older readers (an older reader on a newer file just emits
//! [`SessionError::Corrupt`](crate::SessionError::Corrupt) with the
//! observed `user_version`).

use rusqlite::Connection;

use crate::error::Result;

/// Current schema version. Bump alongside any backwards-incompatible
/// change to the SQL below.
pub const SCHEMA_VERSION: i64 = 1;

/// DDL applied to a fresh database. Idempotent — safe to run on every
/// `open`.
pub const INITIAL_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id              TEXT PRIMARY KEY,
    created_at      INTEGER NOT NULL,
    parent_session  TEXT,
    cwd             TEXT,
    version         TEXT,
    metadata        TEXT
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS entries (
    session_id      TEXT NOT NULL,
    seq             INTEGER NOT NULL,
    parent_seq      INTEGER,
    entry_id        TEXT,
    parent_entry_id TEXT,
    type            TEXT NOT NULL,
    timestamp       INTEGER NOT NULL,
    payload         BLOB NOT NULL,
    PRIMARY KEY (session_id, seq)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS ix_entries_session_type
    ON entries(session_id, type, seq);

CREATE INDEX IF NOT EXISTS ix_entries_session_parent
    ON entries(session_id, parent_seq);

CREATE INDEX IF NOT EXISTS ix_entries_entry_id
    ON entries(session_id, entry_id);

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;
"#;

/// Open or create a database at `path` and run `INITIAL_SQL`.
///
/// The returned connection has `PRAGMA user_version = SCHEMA_VERSION`
/// applied when the database was previously empty.
pub fn open_and_init(path: impl AsRef<std::path::Path>) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(INITIAL_SQL)?;

    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == 0 {
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    } else if version > SCHEMA_VERSION {
        return Err(crate::error::SessionError::Corrupt(format!(
            "schema version {version} is newer than the reader ({SCHEMA_VERSION})"
        )));
    }
    Ok(conn)
}

/// Open an existing database read-only. Returns
/// [`SessionError::Corrupt`](crate::SessionError::Corrupt) when the file
/// does not have the `sessions` table.
pub fn open_read_only(path: impl AsRef<std::path::Path>) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let table_count: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='sessions'",
        [],
        |row| row.get(0),
    )?;
    if table_count == 0 {
        return Err(crate::error::SessionError::Corrupt(
            "missing `sessions` table".to_string(),
        ));
    }
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        return Err(crate::error::SessionError::Corrupt(format!(
            "schema version {version} does not match the reader ({SCHEMA_VERSION})"
        )));
    }
    Ok(conn)
}

/// Stored row matching `SELECT * FROM entries`.
#[derive(Debug, Clone)]
pub struct EntryRow {
    /// Session identifier (foreign key into `sessions.id`).
    pub session_id: String,
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
    /// zstd-compressed JSON payload (see [`crate::writer::ZSTD_LEVEL`]).
    pub payload: Vec<u8>,
}

/// Stored row matching `SELECT * FROM sessions`.
#[derive(Debug, Clone)]
pub struct SessionRow {
    /// Session identifier (primary key).
    pub id: String,
    /// Wall-clock timestamp in milliseconds since the unix epoch.
    pub created_at: i64,
    /// Parent session id (for forks).
    pub parent_session: Option<String>,
    /// Working directory the session was started in.
    pub cwd: Option<String>,
    /// Pi version that produced the session.
    pub version: Option<String>,
    /// Free-form metadata (JSON-encoded).
    pub metadata: Option<String>,
}

impl SessionRow {
    /// Map the stored row back to the protocol-level
    /// [`SessionEntry::Header`](pi_protocol::SessionEntry::Header) payload.
    ///
    /// `created_at` is stored as milliseconds since the unix epoch (see
    /// [`now_millis`] and [`SessionWriter::write_header`]), so it is
    /// converted with `from_timestamp_millis`.
    ///
    /// [`SessionWriter::write_header`]: crate::SessionWriter::write_header
    pub fn to_header(&self) -> pi_protocol::SessionEntry {
        pi_protocol::SessionEntry::Header {
            id: self.id.clone(),
            created_at: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(self.created_at)
                .unwrap_or_else(chrono::Utc::now),
            version: self.version.clone().unwrap_or_default(),
        }
    }
}

/// Bind the current time on a row insert.
pub fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_and_init_creates_schema() {
        let dir = tempdir();
        let path = dir.join("session.sqlite");
        let conn = open_and_init(&path).expect("open");
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name IN ('sessions','entries','meta')",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(count, 3);
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn to_header_reads_created_at_as_milliseconds() {
        let row = SessionRow {
            id: "ms-id".into(),
            created_at: 1_700_000_000_123,
            parent_session: None,
            cwd: None,
            version: Some("0.1.0".into()),
            metadata: None,
        };
        match row.to_header() {
            pi_protocol::SessionEntry::Header { created_at, .. } => {
                // Regression: the column is milliseconds, so a second-based
                // conversion lands ~1000x in the future (+58299-…).
                assert_eq!(created_at.timestamp_millis(), 1_700_000_000_123);
            }
            other => panic!("expected header, got {other:?}"),
        }
    }

    fn tempdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pi-session-schema-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }
}
