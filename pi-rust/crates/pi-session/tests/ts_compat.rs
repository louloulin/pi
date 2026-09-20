//! Upstream-format compatibility test.
//!
//! Reads `fixtures/ts_recorded.sqlite` and asserts the Rust reader
//! decodes it into the expected `SessionEntry` sequence.
//!
//! The fixture is **upstream-shaped**, not Rust-shaped: it is built by
//! `scripts/make-ts-fixture.mjs` from the verbatim DDL in
//! `packages/session-backends/sqlite-node/src/sqlite/migrations/001_initial.sql`
//! (AgentHarness storage format 4 / `storageVersion 1`), with plain-JSON
//! `entries.payload` values matching the upstream `Entry` union. The old
//! version of this test asserted `PRAGMA user_version == 1` against a
//! fixture the generator had written in the Rust schema — a tautology
//! that could never fail. The structural assertions below are the
//! replacement: they pin the *upstream* columns and prove the reader maps
//! them instead of round-tripping its own layout.

use std::path::{Path, PathBuf};

use pi_protocol::{Content, Message, Role, SessionEntry, StopReason, TextContent, Usage};
use pi_session::{SchemaLayout, SessionReader};
use rusqlite::Connection;

mod common;

use common::{fresh_dir, write_rust_legacy_fixture};

const FIXTURE_PATH: &str = "fixtures/ts_recorded.sqlite";
const SESSION_ID: &str = "ts-recorded-fixture";
/// Milliseconds since the unix epoch, fixed by the fixture generator.
const T0: i64 = 1_700_000_000_123;

fn fixture_path() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURE_PATH);
    assert!(
        path.exists(),
        "fixture missing at {} — regenerate with \
         `node pi-rust/crates/pi-session/scripts/make-ts-fixture.mjs <path>`",
        path.display()
    );
    path
}

fn open_fixture() -> SessionReader {
    SessionReader::open(fixture_path()).expect("open fixture")
}

fn columns(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    let mut rows = stmt.query([]).unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        out.push(row.get::<_, String>(1).unwrap());
    }
    out
}

#[test]
fn fixture_is_the_upstream_v4_layout_not_the_rust_one() {
    let conn = Connection::open(fixture_path()).expect("open raw fixture");

    // Column-by-column: this is the upstream `001_initial.sql` shape.
    assert_eq!(
        columns(&conn, "sessions"),
        vec![
            "id",
            "created_at",
            "parent_session_id",
            "storage_version",
            "metadata",
            "message_count",
            "usage_payload",
            "next_seq",
        ]
    );
    assert_eq!(
        columns(&conn, "entries"),
        vec![
            "session_id",
            "id",
            "parent_id",
            "seq",
            "type",
            "custom_type",
            "timestamp",
            "payload",
        ]
    );

    // The Rust-only columns must be absent, otherwise this fixture would
    // once again be testing the Rust layout against itself.
    for rust_only in ["parent_session", "cwd", "version"] {
        assert!(
            !columns(&conn, "sessions").iter().any(|c| c == rust_only),
            "fixture sessions table must not have the Rust-only column {rust_only}"
        );
    }
    for rust_only in ["parent_seq", "entry_id", "parent_entry_id"] {
        assert!(
            !columns(&conn, "entries").iter().any(|c| c == rust_only),
            "fixture entries table must not have the Rust-only column {rust_only}"
        );
    }

    // Tables that only the upstream migrations create.
    for table in [
        "scalar_values",
        "list_values",
        "usage_ledger",
        "branch_entries",
        "branch_meta",
    ] {
        let exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "upstream table {table} missing from fixture");
    }

    // `payload` is plain JSON text, not a zstd BLOB.
    let payload_type: String = conn
        .query_row(
            "SELECT type FROM pragma_table_info('entries') WHERE name = 'payload'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(payload_type, "TEXT");
    let payload: String = conn
        .query_row(
            "SELECT payload FROM entries WHERE session_id = ?1 ORDER BY seq LIMIT 1",
            [SESSION_ID],
            |row| row.get(0),
        )
        .unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&payload).expect("payload is plain JSON text");
    assert_eq!(parsed["message"]["role"], "user");

    // And the reader agrees about the layout.
    assert_eq!(open_fixture().layout(), SchemaLayout::UpstreamV4);
}

#[test]
fn fixture_sessions_row_maps_to_a_session_header() {
    let reader = open_fixture();
    let row = reader
        .session_header()
        .expect("session_header")
        .expect("has a session row");

    assert_eq!(row.id, SESSION_ID);
    assert_eq!(row.created_at, T0, "created_at stays in milliseconds");
    assert_eq!(row.parent_session, None);
    // Upstream has no `cwd` column.
    assert_eq!(row.cwd, None);
    // Upstream has no `version` column either: it comes out of `metadata`.
    assert_eq!(row.version.as_deref(), Some("0.85.1-fixture"));
    assert!(row.metadata.as_deref().unwrap().contains("sqlite-node"));

    match row.to_header() {
        SessionEntry::Header {
            id,
            created_at,
            version,
        } => {
            assert_eq!(id, SESSION_ID);
            assert_eq!(created_at.timestamp_millis(), T0);
            assert_eq!(version, "0.85.1-fixture");
        }
        other => panic!("expected header, got {other:?}"),
    }

    assert_eq!(reader.list_sessions().expect("list").len(), 1);
    assert_eq!(
        reader
            .latest_session()
            .expect("latest")
            .expect("has session")
            .id,
        SESSION_ID
    );
}

#[test]
fn fixture_entries_decode_to_the_expected_session_entries() {
    let reader = open_fixture();
    let entries = reader.iter_entries(SESSION_ID).expect("iter_entries");
    assert_eq!(entries.len(), 6, "fixture holds six upstream entries");

    // Upstream rows keep their id / parent id / timestamp verbatim.
    assert_eq!(entries[0].seq, 1);
    assert_eq!(entries[0].entry_id.as_deref(), Some("e1-user"));
    assert_eq!(entries[0].parent_entry_id, None);
    assert_eq!(entries[0].timestamp, T0);
    assert_eq!(entries[0].type_, "message");
    // Upstream parents are ids, not seqs.
    assert_eq!(entries[1].parent_seq, None);
    assert_eq!(entries[1].parent_entry_id.as_deref(), Some("e1-user"));
    assert_eq!(entries[2].timestamp, T0 + 200);
    assert_eq!(entries[3].type_, "compaction");
    assert_eq!(entries[4].type_, "custom");
    assert_eq!(entries[5].type_, "branch_summary");

    // 1. User message.
    match &entries[0].entry {
        SessionEntry::UserMessage(Message { role, content, .. }) => {
            assert_eq!(*role, Role::User);
            match &content[0] {
                Content::Text(text) => {
                    assert_eq!(text.text, "what is the capital of france?");
                }
                other => panic!("expected text content, got {other:?}"),
            }
        }
        other => panic!("expected user message, got {other:?}"),
    }

    // 2. Assistant message: the upstream `thinking` block is not
    //    representable in `pi_protocol::Content` and is skipped; text and
    //    tool call survive.
    match &entries[1].entry {
        SessionEntry::AssistantMessage(message) => {
            assert_eq!(message.model, "faux/faux-model");
            assert_eq!(message.stop_reason, StopReason::ToolUse);
            assert_eq!(message.error_message, None);
            assert_eq!(
                message.usage,
                Usage {
                    input: 120,
                    output: 8,
                    cache_read: 4,
                    cache_write: 0,
                    total: 128,
                }
            );
            assert_eq!(message.content.len(), 2);
            match &message.content[0] {
                Content::Text(text) => assert_eq!(text.text, "Paris."),
                other => panic!("expected text content, got {other:?}"),
            }
            match &message.content[1] {
                Content::ToolCall(call) => {
                    assert_eq!(call.id, "ts-call-1");
                    assert_eq!(call.name, "bash");
                    assert_eq!(call.arguments["cmd"], "echo hello");
                }
                other => panic!("expected tool call, got {other:?}"),
            }
        }
        other => panic!("expected assistant message, got {other:?}"),
    }

    // 3. Tool result.
    match &entries[2].entry {
        SessionEntry::ToolResult(result) => {
            assert_eq!(result.tool_call_id, "ts-call-1");
            assert!(!result.is_error);
            match result.content.as_ref() {
                Content::Text(text) => assert_eq!(text.text, "hello\n"),
                other => panic!("expected text content, got {other:?}"),
            }
            assert_eq!(result.details.as_ref().expect("details")["exitCode"], 0);
        }
        other => panic!("expected tool result, got {other:?}"),
    }

    // 4. Compaction.
    match &entries[3].entry {
        SessionEntry::Compaction {
            summary,
            retained_tail,
            tokens_before,
            usage,
            details,
        } => {
            assert_eq!(summary, "## Goal\nkeep the fixture small");
            assert_eq!(*tokens_before, 12_345);
            assert_eq!(retained_tail.len(), 2);
            assert_eq!(retained_tail[0].role, Role::User);
            assert_eq!(
                retained_tail[0].content,
                vec![Content::Text(TextContent {
                    text: "summarised earlier".into()
                })]
            );
            assert_eq!(retained_tail[1].role, Role::Assistant);
            assert_eq!(retained_tail[1].model.as_deref(), Some("faux/faux-model"));
            assert_eq!(
                usage.expect("usage"),
                Usage {
                    input: 100,
                    output: 20,
                    cache_read: 0,
                    cache_write: 0,
                    total: 120,
                }
            );
            assert_eq!(
                details.as_ref().expect("details")["readFiles"][0],
                "src/lib.rs"
            );
        }
        other => panic!("expected compaction, got {other:?}"),
    }

    // 5. Custom entry: `custom_type` becomes the extension kind.
    match &entries[4].entry {
        SessionEntry::Extension {
            extension,
            kind,
            payload,
        } => {
            assert_eq!(extension, "custom");
            assert_eq!(kind, "ts-fixture:marker");
            assert_eq!(payload["fixture"], true);
            assert_eq!(payload["version"], 1);
        }
        other => panic!("expected extension, got {other:?}"),
    }

    // 6. Branch summary: no Rust variant exists, so the payload is passed
    //    through as an extension instead of being dropped.
    match &entries[5].entry {
        SessionEntry::Extension {
            extension, payload, ..
        } => {
            assert_eq!(extension, "branch_summary");
            assert_eq!(payload["fromId"], "e4-compaction");
            assert_eq!(payload["summary"], "explored branch A");
            assert_eq!(payload["fromHook"], true);
        }
        other => panic!("expected branch summary passthrough, got {other:?}"),
    }
}

#[test]
fn fixture_lookups_use_upstream_keys() {
    let reader = open_fixture();
    assert_eq!(reader.count_entries(SESSION_ID).unwrap(), 6);

    // `get_entry` still works by seq.
    let entry = reader
        .get_entry(SESSION_ID, 4)
        .unwrap()
        .expect("seq 4 exists");
    assert_eq!(entry.entry_id.as_deref(), Some("e4-compaction"));
    assert!(matches!(entry.entry, SessionEntry::Compaction { .. }));

    // `get_message` matches the upstream primary key (`entries.id`).
    let custom = reader
        .get_message(SESSION_ID, "e5-custom")
        .unwrap()
        .expect("entry id exists");
    assert_eq!(custom.seq, 5);
    assert!(matches!(custom.entry, SessionEntry::Extension { .. }));
    assert!(reader.get_message(SESSION_ID, "nope").unwrap().is_none());
}

/// Regression: files written by the Stage 5 crate before Stage 55
/// (`~/.pi/sessions/*.sqlite`) must keep opening and decoding. The writer
/// no longer emits this layout, so the fixture is built directly.
#[test]
fn rust_legacy_files_still_read() {
    let dir = fresh_dir("rust-legacy");
    let path = dir.join("legacy.sqlite");

    write_rust_legacy_fixture(
        &path,
        "legacy",
        "0.1.0",
        &[
            SessionEntry::UserMessage(Message {
                role: Role::User,
                content: vec![Content::text("hello")],
                model: None,
            }),
            SessionEntry::Extension {
                extension: "demo".into(),
                kind: "marker".into(),
                payload: serde_json::json!({"legacy": true}),
            },
        ],
    );

    let reader = SessionReader::open(&path).expect("open reader");
    assert_eq!(reader.layout(), SchemaLayout::RustLegacy);
    let entries = reader.iter_entries("legacy").expect("entries");
    assert_eq!(entries.len(), 2);
    assert!(matches!(entries[0].entry, SessionEntry::UserMessage(_)));
    assert_eq!(
        reader.session_header().unwrap().unwrap().version.as_deref(),
        Some("0.1.0")
    );
}

/// A Rust legacy file renamed to look like an upstream file must not be
/// misdetected: detection is structural, per column.
#[test]
fn renamed_rust_file_is_not_misdetected_as_upstream() {
    let dir = fresh_dir("renamed");
    let path = dir.join("ts_recorded.sqlite");
    write_rust_legacy_fixture(&path, "renamed", "0.1.0", &[]);

    let reader = SessionReader::open(&path).expect("open reader");
    assert_eq!(reader.layout(), SchemaLayout::RustLegacy);
    assert_eq!(reader.list_sessions().unwrap()[0].id, "renamed");
}

/// The fixture is committed, so a missing/regressed generator shows up as
/// a byte-identity failure here rather than as a silently stale file.
#[test]
fn fixture_rows_are_self_consistent() {
    let reader = open_fixture();
    let sessions = reader.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1);

    let entries = reader.iter_entries(SESSION_ID).unwrap();
    let seqs: Vec<i64> = entries.iter().map(|entry| entry.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4, 5, 6]);
    for entry in &entries {
        assert!(
            entry.entry_id.is_some(),
            "upstream rows always carry an id: {entry:?}"
        );
    }
}

/// Keeps the fixture path helper honest even if the fixture moves.
#[test]
fn fixture_path_is_inside_the_crate() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(fixture_path().starts_with(manifest));
}
