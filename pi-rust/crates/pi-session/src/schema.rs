//! Schema management for the SQLite session backend.
//!
//! # Two on-disk layouts
//!
//! A session file is written in one of two shapes, and this module is
//! responsible for telling them apart (`SchemaLayout`) before any column
//! is read. They are **not** compatible: the same file name means
//! different tables and different payload encodings.
//!
//! | | Upstream `session-backends/sqlite-node` (AgentHarness storage
//! format 4 / `storageVersion 1`) | `pi-session` Rust legacy |
//! | --- | --- | --- |
//! | session row | `sessions(id, created_at, parent_session_id, storage_version, metadata, message_count, usage_payload, next_seq)` | `sessions(id, created_at, parent_session, cwd, version, metadata)` |
//! | entry row | `entries(session_id, id, parent_id, seq, type, custom_type, timestamp, payload TEXT)`, PK `(session_id, id)` | `entries(session_id, seq, parent_seq, entry_id, parent_entry_id, type, timestamp, payload BLOB)`, PK `(session_id, seq)` |
//! | payload | plain JSON text (`entries.ts` `JSON.parse(row.payload)`) | zstd (level 3) compressed JSON BLOB |
//! | other tables | `scalar_values`, `list_values`, `usage_ledger`, `branch_entries`, `branch_meta` + 2 triggers | `meta(key, value)` |
//! | version marker | `sessions.storage_version = 1` column (does **not** write `PRAGMA user_version`) | `PRAGMA user_version = 1` |
//!
//! Stage 55 moved the write path onto the upstream layout: [`open_and_init`]
//! now creates the upstream v4 tables from [`UPSTREAM_INITIAL_SQL`] (a
//! verbatim copy of
//! `packages/session-backends/sqlite-node/src/sqlite/migrations/001_initial.sql`),
//! and [`detect_layout`] dispatches on the *structure* of the
//! `sessions` / `entries` tables — never `PRAGMA user_version`, which the
//! upstream writer does not set. Relying on `user_version` alone would
//! classify every upstream file as version 0.
//!
//! # Rust legacy layout
//!
//! The narrow Rust layout ("Rust legacy" here, because it predates
//! upstream-format support) is **read-only** as of Stage 55: existing
//! files still open through [`SessionReader`](crate::SessionReader), but
//! [`SessionWriter`](crate::SessionWriter) refuses to append to one and
//! returns [`SessionError::LegacyLayout`](crate::SessionError::LegacyLayout).
//! Convert them with `pi session migrate <path>` or
//! [`crate::migrate::migrate_file`]. The legacy DDL stays in
//! [`INITIAL_SQL`] so the migration fixtures can reproduce the old shape;
//! it is still versioned via `PRAGMA user_version` exactly as it was in
//! Stage 5.

use rusqlite::Connection;

use crate::error::Result;

/// Current schema version of the **Rust** layout. Bump alongside any
/// backwards-incompatible change to the SQL below.
pub const SCHEMA_VERSION: i64 = 1;

/// DDL applied to a fresh database. Idempotent — safe to run on every
/// `open`.
///
/// This is the **Rust legacy** shape. As of Stage 55 the writer defaults
/// to [`UPSTREAM_INITIAL_SQL`]; this constant is kept so the reader
/// fixtures and the `pi session migrate` tests can reproduce a legacy
/// file, and so [`detect_layout`] has a second shape to recognise.
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

/// DDL of the upstream TS `packages/session-backends/sqlite-node`
/// session backend — AgentHarness storage format 4 / `storageVersion 1`.
///
/// Copied **verbatim** (byte-for-byte, tab indentation and comments
/// included) from
/// `packages/session-backends/sqlite-node/src/sqlite/migrations/001_initial.sql`.
/// `pi-session`'s `tests/write_path.rs` re-reads that file and fails if
/// the two drift apart.
pub const UPSTREAM_INITIAL_SQL: &str = r#"-- AgentHarness storage format 4 / storageVersion 1.
-- A SQLite database file is a session container. The current default repo
-- placement still creates one file per session, but the schema supports any
-- number of sessions per file by scoping every durable row with session_id.
-- Authoritative durable state is entries + scalar_values + list_values +
-- usage_ledger; branch_* and stats columns on sessions are maintained
-- projections/caches.

CREATE TABLE IF NOT EXISTS sessions (
	id TEXT PRIMARY KEY,
	created_at INTEGER NOT NULL,
	parent_session_id TEXT,
	storage_version INTEGER NOT NULL,
	metadata TEXT,
	message_count INTEGER NOT NULL,
	usage_payload TEXT NOT NULL,
	next_seq INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS entries (
	session_id TEXT NOT NULL,
	id TEXT NOT NULL,
	parent_id TEXT,
	seq INTEGER NOT NULL,
	type TEXT NOT NULL,
	custom_type TEXT,
	timestamp INTEGER NOT NULL,
	payload TEXT NOT NULL,
	PRIMARY KEY (session_id, id)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS ix_entry_parent ON entries(session_id, parent_id);
CREATE INDEX IF NOT EXISTS ix_entry_seq ON entries(session_id, seq, type);

CREATE TABLE IF NOT EXISTS scalar_values (
	session_id TEXT NOT NULL,
	namespace TEXT NOT NULL,
	key TEXT NOT NULL,
	seq INTEGER NOT NULL,
	value TEXT NOT NULL,
	PRIMARY KEY (session_id, namespace, key)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS list_values (
	session_id TEXT NOT NULL,
	namespace TEXT NOT NULL,
	key TEXT NOT NULL,
	seq INTEGER NOT NULL,
	value TEXT NOT NULL,
	PRIMARY KEY (session_id, namespace, key, seq)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS usage_ledger (
	session_id TEXT NOT NULL,
	id TEXT NOT NULL,
	seq INTEGER NOT NULL,
	entry_id TEXT,
	adjustment INTEGER NOT NULL,
	usage TEXT NOT NULL,
	details TEXT,
	PRIMARY KEY (session_id, id)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS ix_usage_seq ON usage_ledger(session_id, seq);

-- Storage-level integrity that spans rows/tables. Primary keys enforce
-- same-table duplicate ids; these triggers enforce the shared entry/usage id
-- namespace and ordered parent insertion without a TypeScript preflight pass.
CREATE TRIGGER IF NOT EXISTS trg_entries_validate
BEFORE INSERT ON entries
BEGIN
	SELECT RAISE(ABORT, 'missing parent entry')
	WHERE NEW.parent_id IS NOT NULL
		AND NOT EXISTS (
			SELECT 1 FROM entries WHERE session_id = NEW.session_id AND id = NEW.parent_id
		);

	SELECT RAISE(ABORT, 'duplicate entry or usage id')
	WHERE EXISTS (
		SELECT 1 FROM usage_ledger WHERE session_id = NEW.session_id AND id = NEW.id
	);
END;

CREATE TRIGGER IF NOT EXISTS trg_usage_ledger_validate
BEFORE INSERT ON usage_ledger
BEGIN
	SELECT RAISE(ABORT, 'duplicate entry or usage id')
	WHERE EXISTS (
		SELECT 1 FROM entries WHERE session_id = NEW.session_id AND id = NEW.id
	);
END;

-- Private branch index. Not values/lists; no equivalent in the other backends.
CREATE TABLE IF NOT EXISTS branch_entries (
	session_id TEXT NOT NULL,
	branch_id TEXT NOT NULL,
	entry_id TEXT NOT NULL,
	entry_seq INTEGER NOT NULL,
	entry_type TEXT NOT NULL,
	PRIMARY KEY (session_id, branch_id, entry_id)
) WITHOUT ROWID;

-- Ordered scans. entry_seq must follow session_id, branch_id directly or ORDER
-- BY needs a temp b-tree; entry_id and entry_type trail so the index covers
-- id-only reads.
CREATE INDEX IF NOT EXISTS ix_be_seq ON branch_entries(session_id, branch_id, entry_seq, entry_id, entry_type);
-- Type-filtered scans.
CREATE INDEX IF NOT EXISTS ix_be_type ON branch_entries(session_id, branch_id, entry_type, entry_seq, entry_id);
CREATE INDEX IF NOT EXISTS ix_be_entry ON branch_entries(session_id, entry_id);

CREATE TABLE IF NOT EXISTS branch_meta (
	session_id TEXT NOT NULL,
	branch_id TEXT NOT NULL,
	tip_entry_id TEXT NOT NULL,
	tip_seq INTEGER NOT NULL,
	base_branch_id TEXT,
	base_seq INTEGER,
	PRIMARY KEY (session_id, branch_id)
) WITHOUT ROWID;

CREATE UNIQUE INDEX IF NOT EXISTS ix_bm_tip ON branch_meta(session_id, tip_entry_id);
"#;

/// On-disk layout of a session database.
///
/// Detected by [`detect_layout`] from the table structure, never from
/// `PRAGMA user_version` (the upstream writer does not set it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaLayout {
    /// Upstream TS `packages/session-backends/sqlite-node` — AgentHarness
    /// storage format 4 / `storageVersion 1`: `entries.payload` is plain
    /// JSON text, `entries` is keyed by `(session_id, id)` and every
    /// durable row is scoped by `session_id`.
    UpstreamV4,
    /// The narrow layout written by this crate's
    /// [`SessionWriter`](crate::SessionWriter) (Stage 5): one row per
    /// [`SessionEntry`](pi_protocol::SessionEntry), `entries.payload` is
    /// a zstd BLOB, `entries` is keyed by `(session_id, seq)` and
    /// `PRAGMA user_version` carries the version.
    RustLegacy,
}

/// Columns that only the upstream `sessions` table declares.
const UPSTREAM_SESSION_COLUMNS: &[&str] = &[
    "id",
    "created_at",
    "parent_session_id",
    "storage_version",
    "metadata",
    "message_count",
    "usage_payload",
    "next_seq",
];
/// Columns that only the upstream `entries` table declares. Note
/// `custom_type` (upstream) versus `entry_id` / `parent_seq` (Rust).
const UPSTREAM_ENTRY_COLUMNS: &[&str] = &[
    "session_id",
    "id",
    "parent_id",
    "seq",
    "type",
    "custom_type",
    "timestamp",
    "payload",
];
/// Columns that only the Rust `sessions` table declares.
const LEGACY_SESSION_COLUMNS: &[&str] = &[
    "id",
    "created_at",
    "parent_session",
    "cwd",
    "version",
    "metadata",
];
/// Columns that only the Rust `entries` table declares.
const LEGACY_ENTRY_COLUMNS: &[&str] = &[
    "session_id",
    "seq",
    "parent_seq",
    "entry_id",
    "parent_entry_id",
    "type",
    "timestamp",
    "payload",
];

/// Column names of `table`, in declaration order. Returns an empty vec
/// when the table does not exist.
fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row.get::<_, String>(1)?);
    }
    Ok(out)
}

fn has_all(columns: &[String], required: &[&str]) -> bool {
    required
        .iter()
        .all(|want| columns.iter().any(|got| got == want))
}

/// Detect the layout of an open connection by probing `PRAGMA
/// table_info` for the two `sessions` / `entries` shapes.
///
/// Both tables must match the *same* layout; a half-migrated file (e.g.
/// upstream `sessions` next to Rust `entries`) is reported as
/// [`SessionError::Corrupt`](crate::SessionError::Corrupt) rather than
/// guessed at, so a renamed or hand-edited file can never be silently
/// misread.
///
/// [`SessionError::Corrupt`]: crate::SessionError::Corrupt
pub fn detect_layout(conn: &Connection) -> Result<SchemaLayout> {
    let session_columns = table_columns(conn, "sessions")?;
    if session_columns.is_empty() {
        return Err(crate::error::SessionError::Corrupt(
            "missing `sessions` table".to_string(),
        ));
    }
    let entry_columns = table_columns(conn, "entries")?;

    let upstream_sessions = has_all(&session_columns, UPSTREAM_SESSION_COLUMNS);
    let upstream_entries = has_all(&entry_columns, UPSTREAM_ENTRY_COLUMNS);
    let legacy_sessions = has_all(&session_columns, LEGACY_SESSION_COLUMNS);
    let legacy_entries = has_all(&entry_columns, LEGACY_ENTRY_COLUMNS);

    match (
        upstream_sessions,
        upstream_entries,
        legacy_sessions,
        legacy_entries,
    ) {
        (true, true, false, false) => Ok(SchemaLayout::UpstreamV4),
        (false, false, true, true) => Ok(SchemaLayout::RustLegacy),
        _ => Err(crate::error::SessionError::Corrupt(format!(
            "unrecognised session layout: sessions({}) entries({})",
            session_columns.join(", "),
            entry_columns.join(", "),
        ))),
    }
}

/// Open or create a database at `path`, returning the connection and the
/// detected [`SchemaLayout`].
///
/// * A fresh (or empty) file gets [`UPSTREAM_INITIAL_SQL`] applied and is
///   reported as [`SchemaLayout::UpstreamV4`]. No `PRAGMA user_version`
///   is written — upstream identifies the format by
///   `sessions.storage_version` instead.
/// * An existing upstream file is left as-is (the DDL is
///   `CREATE TABLE IF NOT EXISTS`).
/// * An existing Rust legacy file is **not** modified — the layout is
///   probed before any pragma is applied — and yields
///   [`SessionError::LegacyLayout`](crate::SessionError::LegacyLayout) so
///   the caller can point at `pi session migrate`.
pub fn open_and_init(path: impl AsRef<std::path::Path>) -> Result<(Connection, SchemaLayout)> {
    let path = path.as_ref();
    let conn = Connection::open(path)?;
    let session_columns = table_columns(&conn, "sessions")?;
    if session_columns.is_empty() {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(UPSTREAM_INITIAL_SQL)?;
        return Ok((conn, SchemaLayout::UpstreamV4));
    }
    let layout = detect_layout(&conn)?;
    if layout == SchemaLayout::RustLegacy {
        return Err(crate::error::SessionError::LegacyLayout(path.to_path_buf()));
    }
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok((conn, layout))
}

/// Open an existing database read-only and detect its layout.
///
/// Returns [`SessionError::Corrupt`](crate::SessionError::Corrupt) when
/// the file has no `sessions` table or no layout this reader knows.
///
/// [`SessionError::Corrupt`]: crate::SessionError::Corrupt
pub fn open_read_only_with_layout(
    path: impl AsRef<std::path::Path>,
) -> Result<(Connection, SchemaLayout)> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let layout = detect_layout(&conn)?;
    if layout == SchemaLayout::RustLegacy {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(crate::error::SessionError::Corrupt(format!(
                "schema version {version} is newer than the reader ({SCHEMA_VERSION})"
            )));
        }
    }
    Ok((conn, layout))
}

/// Open an existing database read-only. Returns
/// [`SessionError::Corrupt`](crate::SessionError::Corrupt) when the file
/// does not have a `sessions` table or its layout is unknown. Use
/// [`open_read_only_with_layout`] when the caller needs the detected
/// layout as well.
pub fn open_read_only(path: impl AsRef<std::path::Path>) -> Result<Connection> {
    Ok(open_read_only_with_layout(path)?.0)
}

/// Stored row matching `SELECT * FROM entries` in the **Rust legacy**
/// layout. Upstream rows are decoded into
/// [`DecodedEntry`](crate::DecodedEntry) directly and never surface as
/// an `EntryRow`.
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
    /// Only ever set for the Rust legacy layout.
    pub payload: Vec<u8>,
}

/// Stored row matching `SELECT * FROM sessions` in the **Rust legacy**
/// layout. The upstream `sessions` row has different columns and is
/// decoded by the reader instead of through this struct.
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

/// The session display name stored under `metadata.name`, when present.
///
/// Upstream keeps the name in a `session_info` entry; the Rust port has no
/// dedicated entry variant, so `/name` stores it in the session row's
/// metadata JSON blob (`{"version": …, "name": …}`) instead. Trailing
/// whitespace is trimmed and an empty name reads as `None`, matching
/// upstream `getSessionName()`.
pub fn session_name_from_metadata(metadata: Option<&str>) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(metadata?).ok()?;
    parsed
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

/// The active-leaf entry id stored under `metadata.leaf`, when present.
///
/// Upstream keeps the tree cursor (`leafId`) in memory only, losing it on
/// restart. The Rust port records `/tree`'s cursor move in the same
/// metadata JSON blob `/name` uses (`{"version": …, "leaf": "e7"}`) so
/// the branch survives `pi --resume`; the TS reader ignores the unknown
/// key, and a session without it falls back to the tip.
pub fn session_leaf_from_metadata(metadata: Option<&str>) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(metadata?).ok()?;
    parsed
        .get("leaf")
        .and_then(serde_json::Value::as_str)
        .filter(|leaf| !leaf.is_empty())
        .map(str::to_string)
}

/// Bind the current time on a row insert.
pub fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_and_init_creates_the_upstream_schema() {
        let dir = tempdir();
        let path = dir.join("session.sqlite");
        let (conn, layout) = open_and_init(&path).expect("open");
        assert_eq!(layout, SchemaLayout::UpstreamV4);

        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .expect("prepare");
        let tables: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query")
            .collect::<std::result::Result<_, _>>()
            .expect("collect");
        for expected in [
            "sessions",
            "entries",
            "scalar_values",
            "list_values",
            "usage_ledger",
            "branch_entries",
            "branch_meta",
        ] {
            assert!(tables.iter().any(|t| t == expected), "missing {expected}");
        }
        assert!(
            !tables.iter().any(|t| t == "meta"),
            "a fresh database must not create the Rust legacy `meta` table"
        );

        // Upstream identifies the format via `sessions.storage_version`;
        // the file must not carry a `PRAGMA user_version` marker.
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, 0);
    }

    #[test]
    fn open_and_init_refuses_legacy_files_without_touching_them() {
        let dir = tempdir();
        let path = dir.join("legacy.sqlite");
        // Build a legacy database the same way Stage 5 did.
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(INITIAL_SQL).expect("ddl");
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)
                .expect("stamp");
        }
        let before = std::fs::read(&path).expect("read");
        let err = open_and_init(&path).expect_err("legacy must be rejected");
        assert!(
            matches!(err, crate::error::SessionError::LegacyLayout(_)),
            "expected LegacyLayout, got {err:?}"
        );
        assert_eq!(
            std::fs::read(&path).expect("read"),
            before,
            "rejecting a legacy file must not modify it"
        );
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

    /// Minimal upstream-format DDL: a subset of
    /// `packages/session-backends/sqlite-node/src/sqlite/migrations/001_initial.sql`
    /// (the real file also ships `scalar_values` / `list_values` /
    /// `usage_ledger` / `branch_*` and two triggers).
    const UPSTREAM_TEST_SESSION_DDL: &str = r#"
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    parent_session_id TEXT,
    storage_version INTEGER NOT NULL,
    metadata TEXT,
    message_count INTEGER NOT NULL,
    usage_payload TEXT NOT NULL,
    next_seq INTEGER NOT NULL
) WITHOUT ROWID;
"#;

    const UPSTREAM_TEST_ENTRY_DDL: &str = r#"
CREATE TABLE entries (
    session_id TEXT NOT NULL,
    id TEXT NOT NULL,
    parent_id TEXT,
    seq INTEGER NOT NULL,
    type TEXT NOT NULL,
    custom_type TEXT,
    timestamp INTEGER NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY (session_id, id)
) WITHOUT ROWID;
"#;

    fn upstream_test_ddl() -> String {
        format!("{UPSTREAM_TEST_SESSION_DDL}{UPSTREAM_TEST_ENTRY_DDL}")
    }

    fn conn_with(ddl: &str) -> (Connection, std::path::PathBuf) {
        let dir = tempdir();
        let path = dir.join("probe.sqlite");
        let conn = Connection::open(&path).expect("open");
        conn.execute_batch(ddl).expect("ddl");
        (conn, path)
    }

    #[test]
    fn detect_layout_distinguishes_rust_legacy_from_upstream() {
        let (conn, _guard) = conn_with(INITIAL_SQL);
        assert_eq!(detect_layout(&conn).unwrap(), SchemaLayout::RustLegacy);

        let (conn, _guard) = conn_with(&upstream_test_ddl());
        assert_eq!(detect_layout(&conn).unwrap(), SchemaLayout::UpstreamV4);
    }

    #[test]
    fn detect_layout_ignores_user_version_for_upstream_files() {
        // The upstream writer never sets `PRAGMA user_version`; a Rust
        // reader that keyed off it would reject every real TS session.
        let (conn, _guard) = conn_with(&upstream_test_ddl());
        assert_eq!(
            conn.query_row::<i64, _, _>("PRAGMA user_version", [], |row| row.get(0))
                .unwrap(),
            0
        );
        assert_eq!(detect_layout(&conn).unwrap(), SchemaLayout::UpstreamV4);
    }

    #[test]
    fn detect_layout_rejects_rust_file_wearing_an_upstream_column() {
        // A Rust file that gained (or was renamed to look like it has) a
        // single upstream-only column must not flip to `UpstreamV4`.
        let (conn, _guard) = conn_with(INITIAL_SQL);
        conn.execute_batch(
            "ALTER TABLE sessions ADD COLUMN storage_version INTEGER;\
             ALTER TABLE entries ADD COLUMN custom_type TEXT;",
        )
        .unwrap();
        assert_eq!(detect_layout(&conn).unwrap(), SchemaLayout::RustLegacy);
    }

    /// Minimal Rust-legacy `entries` DDL (matches `INITIAL_SQL`).
    const RUST_ENTRY_DDL: &str = r#"
CREATE TABLE entries (
    session_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    parent_seq INTEGER,
    entry_id TEXT,
    parent_entry_id TEXT,
    type TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    payload BLOB NOT NULL,
    PRIMARY KEY (session_id, seq)
) WITHOUT ROWID;
"#;

    #[test]
    fn detect_layout_rejects_a_mixed_layout() {
        // Upstream `sessions` + Rust `entries`: neither layout matches,
        // so it is `Corrupt` instead of being guessed at.
        let ddl = format!("{UPSTREAM_TEST_SESSION_DDL}{RUST_ENTRY_DDL}");
        let (conn, _guard) = conn_with(&ddl);
        let err = detect_layout(&conn).unwrap_err();
        assert!(matches!(err, crate::error::SessionError::Corrupt(_)));
    }

    #[test]
    fn detect_layout_rejects_an_unknown_sessions_table() {
        let (conn, _guard) = conn_with("CREATE TABLE sessions (id TEXT PRIMARY KEY);");
        let err = detect_layout(&conn).unwrap_err();
        assert!(matches!(err, crate::error::SessionError::Corrupt(_)));
    }

    #[test]
    fn detect_layout_rejects_a_missing_sessions_table() {
        let (conn, _guard) = conn_with("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);");
        let err = detect_layout(&conn).unwrap_err();
        assert!(matches!(err, crate::error::SessionError::Corrupt(_)));
    }
}
