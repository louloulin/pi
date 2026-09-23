//! `migrate_file`: convert a pre-Stage-55 **Rust legacy** SQLite database
//! into the upstream v4 layout without touching the source.

use std::path::PathBuf;

use pi_protocol::{
    AssistantMessage, Content, Message, Role, SessionEntry, StopReason, TextContent, ToolCall,
    ToolResult, Usage,
};
use pi_session::{
    default_layout_destination, migrate_file, SchemaLayout, SessionError, SessionReader,
};
use rusqlite::Connection;

mod common;

use common::{fresh_dir, write_rust_legacy_fixture};

fn sample_entries() -> Vec<SessionEntry> {
    vec![
        SessionEntry::UserMessage(Message {
            role: Role::User,
            content: vec![Content::text("before the migration")],
            model: None,
        }),
        SessionEntry::AssistantMessage(AssistantMessage {
            model: "faux/faux-model".into(),
            content: vec![
                Content::Text(TextContent {
                    text: "using a tool".into(),
                }),
                Content::ToolCall(ToolCall {
                    id: "legacy-call".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"cmd": "pwd"}),
                }),
            ],
            stop_reason: StopReason::ToolUse,
            usage: Usage {
                input: 7,
                output: 3,
                cache_read: 0,
                cache_write: 0,
                total: 10,
            },
            error_message: None,
        }),
        SessionEntry::ToolResult(ToolResult {
            tool_call_id: "legacy-call".into(),
            content: Box::new(Content::text("/tmp")),
            is_error: false,
            details: None,
            added_tool_names: None,
        }),
        SessionEntry::Extension {
            extension: "demo".into(),
            kind: "marker".into(),
            payload: serde_json::json!({"legacy": true}),
        },
        SessionEntry::Compaction {
            summary: "old summary".into(),
            retained_tail: vec![Message {
                role: Role::Assistant,
                content: vec![Content::text("kept")],
                model: Some("faux/faux-model".into()),
            }],
            tokens_before: 1024,
            usage: None,
            details: None,
        },
    ]
}

#[test]
fn migrates_a_legacy_file_to_upstream_v4_and_preserves_the_source() {
    let dir = fresh_dir("migrate-legacy");
    let source = dir.join("legacy.sqlite");
    write_rust_legacy_fixture(&source, "legacy-migrate", "0.9.9", &sample_entries());

    let bytes_before = std::fs::read(&source).expect("read source");
    let expected_destination = default_layout_destination(&source);
    assert_eq!(expected_destination, dir.join("legacy.upstream.sqlite"));

    let report = migrate_file(&source, None).expect("migrate");
    assert_eq!(report.layout_before, SchemaLayout::RustLegacy);
    assert_eq!(report.layout_after, SchemaLayout::UpstreamV4);
    assert!(!report.already_upstream);
    assert!(report.source_preserved);
    assert_eq!(report.sessions_migrated, 1);
    assert_eq!(report.entries_migrated, 5);
    assert_eq!(report.source, source);
    assert_eq!(report.destination, expected_destination);

    // The source is byte-for-byte untouched.
    assert_eq!(
        std::fs::read(&source).expect("re-read source"),
        bytes_before
    );

    // The destination is a real upstream v4 database.
    let conn = Connection::open(&expected_destination).expect("open destination");
    assert_eq!(
        conn.query_row("SELECT storage_version FROM sessions", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='meta'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    drop(conn);

    let reader = SessionReader::open(&expected_destination).expect("open reader");
    assert_eq!(reader.layout(), SchemaLayout::UpstreamV4);
    let sessions = reader.list_sessions().expect("list");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "legacy-migrate");
    assert_eq!(sessions[0].version.as_deref(), Some("0.9.9"));

    let decoded: Vec<SessionEntry> = reader
        .iter_entries("legacy-migrate")
        .expect("entries")
        .into_iter()
        .map(|row| row.entry)
        .collect();
    assert_eq!(decoded.len(), 5);

    let original = sample_entries();
    assert_eq!(decoded[0], original[0]);
    assert_eq!(decoded[1], original[1]);
    assert_eq!(decoded[2], original[2]);
    assert_eq!(decoded[4], original[4]);
    // The extension name normalises to `custom` (documented degradation);
    // everything else about the entry survives.
    match &decoded[3] {
        SessionEntry::Extension {
            extension,
            kind,
            payload,
        } => {
            assert_eq!(extension, "custom");
            assert_eq!(kind, "marker");
            assert_eq!(payload["legacy"], true);
        }
        other => panic!("expected extension, got {other:?}"),
    }
}

#[test]
fn honours_an_explicit_destination_and_refuses_to_overwrite() {
    let dir = fresh_dir("migrate-destination");
    let source = dir.join("legacy.sqlite");
    let destination = dir.join("converted.sqlite");
    write_rust_legacy_fixture(&source, "explicit", "0.1.0", &sample_entries()[..1]);

    let report = migrate_file(&source, Some(&destination)).expect("migrate");
    assert_eq!(report.destination, destination);
    assert_eq!(report.entries_migrated, 1);
    assert!(destination.exists());

    // A second run against the same destination must not clobber it.
    let err = migrate_file(&source, Some(&destination)).expect_err("destination exists");
    assert!(
        matches!(err, SessionError::AlreadyExists(ref path) if path == &destination),
        "expected AlreadyExists, got {err:?}"
    );
}

#[test]
fn an_upstream_source_is_a_no_op() {
    let dir = fresh_dir("migrate-upstream");
    let source = dir.join("upstream.sqlite");
    write_rust_legacy_fixture(&source, "legacy", "0.1.0", &[]);

    // Convert once, then migrate the *result*: nothing to do.
    let converted = migrate_file(&source, None).expect("first migrate");
    let bytes = std::fs::read(&converted.destination).expect("read converted");

    let report = migrate_file(&converted.destination, None).expect("second migrate");
    assert!(report.already_upstream);
    assert_eq!(report.layout_before, SchemaLayout::UpstreamV4);
    assert_eq!(report.layout_after, SchemaLayout::UpstreamV4);
    assert_eq!(report.destination, converted.destination);
    assert_eq!(report.entries_migrated, 0);
    assert_eq!(
        std::fs::read(&converted.destination).expect("re-read"),
        bytes,
        "a no-op migration must not rewrite the file"
    );
}

#[test]
fn refuses_a_destination_equal_to_the_source_and_missing_sources() {
    let dir = fresh_dir("migrate-refusals");
    let source = dir.join("legacy.sqlite");
    write_rust_legacy_fixture(&source, "same", "0.1.0", &[]);

    let err = migrate_file(&source, Some(&source)).expect_err("same path");
    assert!(
        err.to_string().contains("never overwritten"),
        "unexpected error: {err}"
    );

    let missing = dir.join("nope.sqlite");
    let err = migrate_file(&missing, None).expect_err("missing source");
    assert!(matches!(err, SessionError::NotFound(_)), "got {err:?}");
}

#[test]
fn default_layout_destination_is_a_sibling_upstream_file() {
    assert_eq!(
        default_layout_destination(PathBuf::from("/tmp/abc.sqlite")),
        PathBuf::from("/tmp/abc.upstream.sqlite")
    );
    assert_eq!(
        default_layout_destination(PathBuf::from("/tmp/no-extension")),
        PathBuf::from("/tmp/no-extension.upstream.sqlite")
    );
}
