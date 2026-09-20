//! SQLite session backend for the Pi Rust port.
//!
//! The `pi-session` crate is the Rust analogue of
//! `packages/session-backends/sqlite-node` in the TS monorepo. Two
//! on-disk layouts exist and are **not** interchangeable:
//!
//! * **Upstream v4** — `AgentHarness storage format 4 / storageVersion 1`,
//!   the layout the TS `packages/session-backends/sqlite-node` writer
//!   produces: `entries.payload` holds plain JSON, the row key is
//!   `(session_id, id)` and the session version lives in the
//!   `sessions.storage_version` column. Since Stage 55 this is what
//!   [`SessionWriter`] **writes**, so a Rust-written file opens directly
//!   in the upstream TS `SqliteStorage`.
//! * **Rust legacy** — the narrow Stage 5 layout: one row per
//!   [`SessionEntry`](pi_protocol::SessionEntry) in a narrow schema with
//!   the payload column zstd-compressed (level 3) and JSON-encoded.
//!   Read-only as of Stage 55; convert with
//!   [`migrate_file`] / `pi session migrate`.
//!
//! [`SessionReader::open`] tells them apart from the table structure
//! ([`SchemaLayout`]) and reads either one. Writing a Rust legacy file is
//! refused with [`SessionError::LegacyLayout`] rather than silently
//! producing a file the TS side cannot open.
//!
//! # Quick start
//!
//! ```no_run
//! use pi_session::{SessionWriter, SessionReader, SessionEntry};
//!
//! // Create a new session file (upstream v4) and append a header + an entry.
//! let writer = SessionWriter::open("/tmp/example.sqlite").unwrap();
//! writer.write_header(SessionEntry::Header {
//!     id: "demo".into(),
//!     created_at: chrono::Utc::now(),
//!     version: "0.1.0".into(),
//! }).unwrap();
//! writer.append(SessionEntry::Extension {
//!     extension: "demo".into(),
//!     kind: "ping".into(),
//!     payload: serde_json::json!({"hello": "world"}),
//! }).unwrap();
//! writer.checkpoint().unwrap();
//!
//! // Read it back (and, in the TS monorepo, `SqliteStorage.open` reads it too).
//! let reader = SessionReader::open("/tmp/example.sqlite").unwrap();
//! let entries = reader.iter_entries("demo").unwrap();
//! assert_eq!(entries.len(), 1);
//! ```
//!
//! # Migrating
//!
//! * [`migrate::migrate_jsonl`] replays a Stage 4 JSONL file into a fresh
//!   upstream-v4 SQLite database; the original JSONL is preserved.
//! * [`migrate::migrate_file`] converts a Rust legacy SQLite database into
//!   upstream v4. The source is never modified — the conversion lands in a
//!   sibling `<stem>.upstream.sqlite` unless a destination is given.
//!
//! # Exporting back to JSONL
//!
//! [`export::export_jsonl`] is the inverse: it writes the session header
//! plus every stored entry back to JSONL. `export` followed by `migrate`
//! is the identity on the `entries` table, so sessions can move between
//! the SQLite backend and portable JSONL files losslessly.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod export;
pub mod migrate;
pub mod reader;
pub mod schema;
pub mod writer;

pub use error::{Result, SessionError};
pub use export::{default_export_path, export_jsonl, export_session, render_jsonl, ExportReport};
pub use migrate::{
    default_destination, default_layout_destination, migrate_file, migrate_jsonl,
    FileMigrationReport, MigrationReport,
};
pub use reader::{decode_upstream_entry, DecodedEntry, SessionReader};
pub use schema::{EntryRow, SchemaLayout, SessionRow, SCHEMA_VERSION, UPSTREAM_INITIAL_SQL};
pub use writer::{SessionWriter, ZSTD_LEVEL};

// Re-export the protocol `SessionEntry` so downstream users don't have
// to add a second `pi-protocol` dependency.
pub use pi_protocol::SessionEntry;
