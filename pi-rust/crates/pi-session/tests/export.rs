//! JSONL export tests.
//!
//! The contract under test is the round trip: a session written through
//! the SQLite backend, exported to JSONL, and migrated back into a fresh
//! database must yield the same entries in the same order — including a
//! database whose `entries` table also carries a `header` row (the shape
//! the TS writer produces).

use std::path::{Path, PathBuf};

use pi_protocol::{Content, Message, Role, SessionEntry, TextContent, Usage};
use pi_session::{
    export_jsonl, export_session, migrate_jsonl, render_jsonl, SessionReader, SessionWriter,
};

mod common;

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pi-session-export-{tag}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn user_message(text: &str) -> SessionEntry {
    SessionEntry::UserMessage(Message {
        role: Role::User,
        content: vec![Content::Text(TextContent { text: text.into() })],
        model: None,
    })
}

fn header(id: &str) -> SessionEntry {
    SessionEntry::Header {
        id: id.into(),
        created_at: chrono::Utc::now(),
        version: "0.1.0".into(),
    }
}

/// Write a session with `entries` appended after the header.
fn write_session(path: &Path, id: &str, entries: &[SessionEntry]) {
    let writer = SessionWriter::open(path).expect("open writer");
    writer.write_header(header(id)).expect("header");
    for entry in entries {
        writer.append(entry.clone()).expect("append");
    }
    writer.checkpoint().expect("checkpoint");
}

#[test]
fn render_jsonl_emits_header_first_then_entries() {
    let dir = tempdir("render");
    let db = dir.join("session.sqlite");
    write_session(
        &db,
        "render-id",
        &[
            user_message("hello"),
            SessionEntry::Extension {
                extension: "demo".into(),
                kind: "marker".into(),
                payload: serde_json::json!({"n": 1}),
            },
            SessionEntry::Compaction {
                summary: "tightened".into(),
                retained_tail: vec![],
                tokens_before: 1234,
                usage: Some(Usage::default()),
                details: None,
            },
        ],
    );

    let reader = SessionReader::open(&db).expect("open reader");
    let jsonl = render_jsonl(&reader, "render-id").expect("render");

    assert!(jsonl.ends_with('\n'), "file must end with a newline");
    let lines: Vec<&str> = jsonl.lines().collect();
    assert_eq!(lines.len(), 4, "header + 3 entries");

    let first: serde_json::Value = serde_json::from_str(lines[0]).expect("header json");
    assert_eq!(first["type"], "header");
    assert_eq!(first["id"], "render-id");
    assert_eq!(first["version"], "0.1.0");

    let kinds: Vec<String> = lines[1..]
        .iter()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).expect("entry json");
            value["type"].as_str().expect("tag").to_string()
        })
        .collect();
    assert_eq!(kinds, vec!["user_message", "extension", "compaction"]);
}

#[test]
fn export_then_migrate_round_trips_every_entry() {
    let dir = tempdir("round-trip");
    let db = dir.join("session.sqlite");
    let entries = vec![
        user_message("first"),
        SessionEntry::Extension {
            extension: "demo".into(),
            kind: "marker".into(),
            payload: serde_json::json!({"nested": [1, 2, 3]}),
        },
        user_message("second"),
    ];
    write_session(&db, "round-trip-id", &entries);

    let reader = SessionReader::open(&db).expect("open reader");
    let jsonl_path = dir.join("nested").join("exported.jsonl");
    let report = export_jsonl(&reader, "round-trip-id", &jsonl_path).expect("export");
    assert_eq!(report.session_id, "round-trip-id");
    assert_eq!(report.entries_written, 3);
    assert_eq!(report.destination, jsonl_path);
    assert_eq!(
        report.bytes_written,
        std::fs::metadata(&jsonl_path).expect("metadata").len() as usize
    );

    // Re-import the exported file into a fresh database.
    let reimported = dir.join("reimported.sqlite");
    let migration = migrate_jsonl(&jsonl_path, &reimported).expect("migrate");
    assert_eq!(migration.entries_migrated, 3);
    assert_eq!(migration.header_id.as_deref(), Some("round-trip-id"));

    let before = SessionReader::open(&db).expect("open original");
    let after = SessionReader::open(&reimported).expect("open reimported");
    let original = before.iter_entries("round-trip-id").expect("entries");
    let round_tripped = after.iter_entries("round-trip-id").expect("entries");
    assert_eq!(original.len(), round_tripped.len());
    for (left, right) in original.iter().zip(round_tripped.iter()) {
        assert_eq!(left.type_, right.type_, "entry type at seq {}", left.seq);
        assert_eq!(left.seq, right.seq);
        assert_eq!(
            serde_json::to_value(&left.entry).unwrap(),
            serde_json::to_value(&right.entry).unwrap(),
            "entry payload at seq {}",
            left.seq
        );
    }
}

#[test]
fn exported_header_keeps_the_original_timestamp() {
    // Regression: the `sessions.created_at` column holds milliseconds since
    // the epoch. Reading it as seconds moved the exported header ~1000x
    // into the future (`+58299-09-13`).
    let dir = tempdir("timestamp");
    let db = dir.join("session.sqlite");
    let created_at = chrono::DateTime::parse_from_rfc3339("2026-05-01T12:34:56.789Z")
        .expect("parse")
        .with_timezone(&chrono::Utc);

    let writer = SessionWriter::open(&db).expect("open writer");
    writer
        .write_header(SessionEntry::Header {
            id: "ts-id".into(),
            created_at,
            version: "0.1.0".into(),
        })
        .expect("header");
    writer.append(user_message("hi")).expect("append");
    writer.checkpoint().expect("checkpoint");

    let reader = SessionReader::open(&db).expect("open reader");
    let jsonl = render_jsonl(&reader, "ts-id").expect("render");
    let header: serde_json::Value =
        serde_json::from_str(jsonl.lines().next().expect("header line")).expect("json");
    assert_eq!(header["created_at"], "2026-05-01T12:34:56.789Z");
}

#[test]
fn header_entry_rows_are_folded_into_the_leading_header_line() {
    let dir = tempdir("folded-header");
    let db = dir.join("session.sqlite");

    // The Stage 5 writer could store a `header` row in `entries` in
    // addition to the `sessions` row. Export still folds that into the
    // single leading header line, so the legacy file exports cleanly.
    common::write_rust_legacy_fixture(&db, "folded-id", "0.1.0", &[user_message("body")]);
    {
        let conn = rusqlite::Connection::open(&db).expect("open legacy");
        let payload = zstd::encode_all(
            serde_json::to_vec(&header("folded-id"))
                .expect("json")
                .as_slice(),
            3,
        )
        .expect("zstd");
        conn.execute(
            "INSERT INTO entries \
             (session_id, seq, parent_seq, entry_id, parent_entry_id, type, timestamp, payload) \
             VALUES (?1, 0, NULL, 'legacy-0', NULL, 'header', 0, ?2)",
            rusqlite::params!["folded-id", payload],
        )
        .expect("header row");
    }

    let reader = SessionReader::open(&db).expect("open reader");
    assert_eq!(reader.layout(), pi_session::SchemaLayout::RustLegacy);
    // `iter_entries` sees both rows…
    assert_eq!(reader.iter_entries("folded-id").expect("entries").len(), 2);

    // …and the export emits exactly one header line.
    let jsonl = render_jsonl(&reader, "folded-id").expect("render");
    let lines: Vec<&str> = jsonl.lines().collect();
    assert_eq!(lines.len(), 2, "one header line + the user message");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(lines[0]).unwrap()["type"],
        "header"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(lines[1]).unwrap()["type"],
        "user_message"
    );
}

#[test]
fn export_session_uses_the_default_path_when_none_is_given() {
    let dir = tempdir("default-path");
    let db = dir.join("session.sqlite");
    write_session(&db, "default-id", &[user_message("hi")]);

    let reader = SessionReader::open(&db).expect("open reader");
    // Run from the temp dir so the default path lands inside it.
    let previous = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&dir).expect("chdir");
    let report = export_session(&reader, "default-id", None).expect("export");
    std::env::set_current_dir(&previous).expect("restore cwd");

    assert!(report.destination.ends_with("session-default-id.jsonl"));
    assert!(dir.join("session-default-id.jsonl").exists());
}

#[test]
fn export_unknown_session_is_an_error() {
    let dir = tempdir("unknown");
    let db = dir.join("session.sqlite");
    write_session(&db, "known-id", &[user_message("hi")]);

    let reader = SessionReader::open(&db).expect("open reader");
    let err = render_jsonl(&reader, "missing-id").expect_err("must fail");
    assert!(
        err.to_string().contains("missing-id"),
        "error should name the session: {err}"
    );
    let err = export_jsonl(&reader, "missing-id", dir.join("out.jsonl")).expect_err("must fail");
    assert!(err.to_string().contains("missing-id"));
    assert!(!dir.join("out.jsonl").exists(), "no file on failure");
}

#[test]
fn export_creates_missing_parent_directories() {
    let dir = tempdir("mkdir");
    let db = dir.join("session.sqlite");
    write_session(&db, "mkdir-id", &[user_message("hi")]);

    let reader = SessionReader::open(&db).expect("open reader");
    let target = dir.join("a").join("b").join("out.jsonl");
    export_jsonl(&reader, "mkdir-id", &target).expect("export");
    assert!(target.exists());
}
