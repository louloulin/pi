//! `SessionWriter::delete_session` — the `/resume` picker's delete
//! (Stage 68 / LUM-1255).
//!
//! The deletion is layout-agnostic on purpose: it scans `sqlite_master`
//! for tables with a `session_id` column and prunes the row from each of
//! them plus the `sessions` row, so an upstream-v4 file *and* a Rust
//! legacy file are pruned without the caller knowing which one it holds.
//! These tests pin that, and pin that a *sibling* session in the same
//! file survives.

use std::path::Path;

use pi_protocol::SessionEntry;
use pi_session::{delete_session, SchemaLayout, SessionReader, SessionWriter};
use rusqlite::Connection;

mod common;

use common::{fresh_dir, write_rust_legacy_fixture};

fn header(session_id: &str) -> SessionEntry {
    SessionEntry::Header {
        id: session_id.to_string(),
        created_at: chrono::Utc::now(),
        version: "0.1.0".into(),
    }
}

fn write_session(path: &Path, session_id: &str, entries: usize) {
    let writer = SessionWriter::open(path).expect("open");
    writer.write_header(header(session_id)).expect("header");
    for index in 0..entries {
        writer
            .append(SessionEntry::Extension {
                extension: "test".into(),
                kind: "marker".into(),
                payload: serde_json::json!({ "index": index }),
            })
            .expect("append");
    }
    writer.checkpoint().expect("checkpoint");
}

fn count(path: &Path, table: &str, session_id: &str) -> i64 {
    let conn = Connection::open(path).expect("open");
    conn.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1"),
        rusqlite::params![session_id],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

#[test]
fn deletes_every_row_of_one_session_and_nothing_else() {
    let dir = fresh_dir("upstream");
    let path = dir.join("session.sqlite");
    write_session(&path, "keep-me", 2);
    write_session(&path, "delete-me", 3);

    let rows = delete_session(&path, "delete-me").expect("delete");
    assert!(rows >= 4, "3 entries + the sessions row, got {rows}");
    assert_eq!(count(&path, "entries", "delete-me"), 0);
    assert_eq!(count(&path, "entries", "keep-me"), 2, "sibling intact");
    assert_eq!(count(&path, "sessions", "delete-me"), 0);

    // The reader agrees: the session is gone and the sibling still reads.
    let reader = SessionReader::open(&path).expect("reader");
    assert_eq!(reader.count_entries("delete-me").expect("count"), 0);
    assert_eq!(reader.count_entries("keep-me").expect("count"), 2);
    assert!(reader.session_row("delete-me").expect("row").is_none());
    assert!(reader.session_row("keep-me").expect("row").is_some());
}

#[test]
fn deleting_an_unknown_session_is_a_no_op() {
    let dir = fresh_dir("unknown");
    let path = dir.join("session.sqlite");
    write_session(&path, "keep-me", 1);

    let rows = delete_session(&path, "never-existed").expect("delete");
    assert_eq!(rows, 0);
    let reader = SessionReader::open(&path).expect("reader");
    assert_eq!(reader.count_entries("keep-me").expect("count"), 1);
}

#[test]
fn the_file_itself_is_left_alone() {
    let dir = fresh_dir("file");
    let path = dir.join("session.sqlite");
    write_session(&path, "only", 1);

    delete_session(&path, "only").expect("delete");
    assert!(path.is_file(), "the caller decides when to unlink");
    // Still a readable session file — an empty one, not a broken one.
    let reader = SessionReader::open(&path).expect("reader");
    assert_eq!(reader.count_entries("only").expect("count"), 0);
}

#[test]
fn a_rust_legacy_file_is_pruned_too() {
    let dir = fresh_dir("legacy");
    let path = dir.join("legacy.sqlite");
    write_rust_legacy_fixture(
        &path,
        "legacy-session",
        "0.1.0",
        &[
            SessionEntry::Extension {
                extension: "test".into(),
                kind: "marker".into(),
                payload: serde_json::json!({ "index": 0 }),
            },
            SessionEntry::Extension {
                extension: "test".into(),
                kind: "marker".into(),
                payload: serde_json::json!({ "index": 1 }),
            },
        ],
    );

    assert_eq!(
        SessionReader::open(&path).expect("reader").layout(),
        SchemaLayout::RustLegacy
    );
    let rows = delete_session(&path, "legacy-session").expect("delete");
    assert!(rows >= 3, "2 entries + the sessions row, got {rows}");
    assert_eq!(count(&path, "entries", "legacy-session"), 0);
    assert_eq!(count(&path, "sessions", "legacy-session"), 0);
}
