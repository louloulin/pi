//! SQLite session backend for the Pi Rust port.
//!
//! The `pi-session` crate is the Rust analogue of
//! `packages/session-backends/sqlite-node` in the TS monorepo. It writes
//! [`SessionEntry`](pi_protocol::SessionEntry) rows to a SQLite
//! database (one row per entry) with the payload column zstd-compressed
//! (level 3) and JSON-encoded. The TS port reads/writes the same
//! payload format, so files written by either side round-trip cleanly.
//!
//! # Quick start
//!
//! ```no_run
//! use pi_session::{SessionWriter, SessionReader, SessionEntry};
//!
//! // Create a new session file and append a header + a few entries.
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
//! // Read it back.
//! let reader = SessionReader::open("/tmp/example.sqlite").unwrap();
//! let entries = reader.iter_entries("demo").unwrap();
//! assert_eq!(entries.len(), 1);
//! ```
//!
//! # Migrating from JSONL
//!
//! Stage 4 of the Rust port wrote sessions as JSONL. The
//! [`migrate::migrate_jsonl`] function replays those files into a fresh
//! SQLite database; the original JSONL is preserved on disk.
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
pub use migrate::{default_destination, migrate_jsonl, MigrationReport};
pub use reader::{DecodedEntry, SessionReader};
pub use schema::{EntryRow, SessionRow, SCHEMA_VERSION};
pub use writer::{SessionWriter, ZSTD_LEVEL};

// Re-export the protocol `SessionEntry` so downstream users don't have
// to add a second `pi-protocol` dependency.
pub use pi_protocol::SessionEntry;
