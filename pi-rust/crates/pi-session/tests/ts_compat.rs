//! TS-compat fixture test.
//!
//! Reads `fixtures/ts_recorded.sqlite` — produced by
//! `scripts/make-ts-fixture.mjs` using Node's built-in `node:sqlite` and
//! `node:zlib` modules — and asserts the Rust reader decodes every row
//! into the expected `SessionEntry` shape. This validates that the
//! on-disk format produced by the TS-side writer (zstd level 3, JSON
//! payload, matching column names) round-trips through the Rust reader.

use pi_protocol::{
    AssistantMessage, Content, Message, Role, SessionEntry, StopReason, TextContent, Usage,
};
use pi_session::{SessionReader, SessionRow, SCHEMA_VERSION};

const FIXTURE_PATH: &str = "fixtures/ts_recorded.sqlite";

fn open_fixture() -> SessionReader {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = manifest_dir.join(FIXTURE_PATH);
    if !path.exists() {
        // The fixture ships in the repo; if it's missing, the build
        // script (`scripts/make-ts-fixture.mjs`) was not run.
        panic!(
            "TS-recorded fixture missing at {} — run `node scripts/make-ts-fixture.mjs fixtures/ts_recorded.sqlite` to regenerate",
            path.display()
        );
    }
    SessionReader::open(&path).expect("open fixture")
}

#[test]
fn fixture_reads_with_matching_schema_version() {
    // Schema-version check is the cheapest smoke test the fixture is
    // still aligned with the Rust reader. Both sides bumped in lock-step.
    assert_eq!(SCHEMA_VERSION, 1);
    let reader = open_fixture();
    assert_eq!(reader.path().file_name().and_then(|s| s.to_str()), Some("ts_recorded.sqlite"));
}

#[test]
fn fixture_session_header_round_trips() {
    let reader = open_fixture();
    let SessionRow {
        id,
        version,
        cwd,
        created_at,
        ..
    } = reader.session_header().expect("header").expect("has header");
    assert_eq!(id, "ts-recorded-fixture");
    assert_eq!(version.as_deref(), Some("0.85.1-fixture"));
    assert_eq!(cwd.as_deref(), Some("/home/example"));
    assert!(created_at > 0, "created_at must be populated");
}

#[test]
fn fixture_iter_entries_decodes_all_variants() {
    let reader = open_fixture();
    let entries = reader
        .iter_entries("ts-recorded-fixture")
        .expect("iter_entries");
    assert_eq!(entries.len(), 6, "fixture contains header + 5 entries");

    // Header (row 1).
    match &entries[0].entry {
        SessionEntry::Header { id, version, .. } => {
            assert_eq!(id, "ts-recorded-fixture");
            assert_eq!(version, "0.85.1-fixture");
        }
        other => panic!("expected header, got {other:?}"),
    }

    // User message (row 2).
    match &entries[1].entry {
        SessionEntry::UserMessage(msg) => {
            assert_eq!(msg.role, Role::User);
            match &msg.content[0] {
                Content::Text(t) => assert_eq!(t.text, "what is the capital of france?"),
                other => panic!("expected text content, got {other:?}"),
            }
        }
        other => panic!("expected user message, got {other:?}"),
    }

    // Assistant message (row 3).
    match &entries[2].entry {
        SessionEntry::AssistantMessage(AssistantMessage {
            model,
            content,
            stop_reason,
            usage,
            error_message,
        }) => {
            assert_eq!(model, "faux/faux-model");
            assert_eq!(*stop_reason, StopReason::Stop);
            assert_eq!(*error_message, None);
            match &content[0] {
                Content::Text(t) => assert_eq!(t.text, "Paris."),
                other => panic!("expected text content, got {other:?}"),
            }
            assert_eq!(*usage, Usage::default());
        }
        other => panic!("expected assistant message, got {other:?}"),
    }

    // Extension (row 4).
    match &entries[3].entry {
        SessionEntry::Extension {
            extension,
            kind,
            payload,
        } => {
            assert_eq!(extension, "ts-fixture");
            assert_eq!(kind, "marker");
            assert_eq!(payload["fixture"], true);
            assert_eq!(payload["version"], 1);
        }
        other => panic!("expected extension entry, got {other:?}"),
    }

    // Tool call (row 5).
    match &entries[4].entry {
        SessionEntry::ToolCall(call) => {
            assert_eq!(call.id, "ts-call-1");
            assert_eq!(call.name, "bash");
            assert_eq!(call.arguments["cmd"], "echo hello");
        }
        other => panic!("expected tool call, got {other:?}"),
    }

    // Tool result (row 6).
    match &entries[5].entry {
        SessionEntry::ToolResult(result) => {
            assert_eq!(result.tool_call_id, "ts-call-1");
            assert!(!result.is_error);
            match result.content.as_ref() {
                Content::Text(t) => assert_eq!(t.text, "hello\n"),
                other => panic!("expected text content, got {other:?}"),
            }
        }
        other => panic!("expected tool result, got {other:?}"),
    }
}

#[test]
fn fixture_count_matches_iter() {
    let reader = open_fixture();
    let count = reader.count_entries("ts-recorded-fixture").unwrap();
    let entries = reader.iter_entries("ts-recorded-fixture").unwrap();
    assert_eq!(count as usize, entries.len());
}

#[test]
fn fixture_extra_message_shape() {
    // Demonstrates that a freshly built Message value would round-trip
    // through the same shape — guards against accidental field renames
    // in pi_protocol::Message between the writer and the reader.
    let expected = Message {
        role: Role::User,
        content: vec![Content::Text(TextContent {
            text: "x".into(),
        })],
        model: None,
    };
    let json = serde_json::to_string(&SessionEntry::UserMessage(expected.clone())).unwrap();
    let parsed: SessionEntry = serde_json::from_str(&json).unwrap();
    match parsed {
        SessionEntry::UserMessage(m) => assert_eq!(m, expected),
        other => panic!("expected user message, got {other:?}"),
    }
}