//! Round-trip integration tests for the `pi-session` crate.
//!
//! These tests construct a `SessionEntry` sequence in-memory, persist it
//! via [`SessionWriter`], reopen the file via [`SessionReader`], and
//! assert the entries decode to the same shape.

use std::path::PathBuf;

use pi_protocol::{
    AssistantMessage, Content, Message, Role, SessionEntry, StopReason, TextContent, ToolCall,
    ToolResult, Usage,
};
use pi_session::{SessionReader, SessionWriter};

fn fresh_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pi-session-int-{label}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0),
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn assistant_message(text: &str) -> AssistantMessage {
    AssistantMessage {
        model: "faux/faux-model".into(),
        content: vec![Content::text(text)],
        stop_reason: StopReason::Stop,
        usage: Usage::default(),
    }
}

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![Content::Text(TextContent { text: text.into() })],
        model: None,
    }
}

#[test]
fn round_trip_user_assistant_extension_toolcall_toolresult() {
    let dir = fresh_dir("round_trip");
    let path = dir.join("session.sqlite");

    let writer = SessionWriter::open(&path).expect("open writer");
    writer
        .write_header(SessionEntry::Header {
            id: "abc".into(),
            created_at: chrono::Utc::now(),
            version: "0.1.0".into(),
        })
        .expect("header");
    writer
        .append(SessionEntry::UserMessage(user_message("hello pi")))
        .expect("user message");
    writer
        .append(SessionEntry::AssistantMessage(assistant_message("hi, human")))
        .expect("assistant message");
    writer
        .append(SessionEntry::ToolCall(ToolCall {
            id: "tool-1".into(),
            name: "bash".into(),
            arguments: serde_json::json!({"cmd": "echo hi"}),
        }))
        .expect("tool call");
    writer
        .append(SessionEntry::ToolResult(ToolResult {
            tool_call_id: "tool-1".into(),
            content: Box::new(Content::text("hi")),
            is_error: false,
            details: None,
        }))
        .expect("tool result");
    writer
        .append(SessionEntry::Extension {
            extension: "demo".into(),
            kind: "marker".into(),
            payload: serde_json::json!({"k": 1, "v": [1, 2, 3]}),
        })
        .expect("extension");
    writer.checkpoint().expect("checkpoint");

    let reader = SessionReader::open(&path).expect("open reader");
    let header = reader.session_header().expect("header").expect("has header");
    assert_eq!(header.id, "abc");
    assert_eq!(header.version.as_deref(), Some("0.1.0"));

    let entries = reader.iter_entries("abc").expect("entries");
    assert_eq!(entries.len(), 5);

    match &entries[0].entry {
        SessionEntry::UserMessage(msg) => {
            assert_eq!(msg.role, Role::User);
            if let Content::Text(t) = &msg.content[0] {
                assert_eq!(t.text, "hello pi");
            } else {
                panic!("expected text");
            }
        }
        other => panic!("expected user message, got {other:?}"),
    }
    match &entries[1].entry {
        SessionEntry::AssistantMessage(msg) => {
            assert_eq!(msg.model, "faux/faux-model");
            assert_eq!(msg.stop_reason, StopReason::Stop);
        }
        other => panic!("expected assistant message, got {other:?}"),
    }
    match &entries[2].entry {
        SessionEntry::ToolCall(call) => {
            assert_eq!(call.id, "tool-1");
            assert_eq!(call.name, "bash");
            assert_eq!(call.arguments["cmd"], "echo hi");
        }
        other => panic!("expected tool call, got {other:?}"),
    }
    match &entries[3].entry {
        SessionEntry::ToolResult(result) => {
            assert_eq!(result.tool_call_id, "tool-1");
            assert!(!result.is_error);
            if let Content::Text(t) = result.content.as_ref() {
                assert_eq!(t.text, "hi");
            } else {
                panic!("expected text");
            }
        }
        other => panic!("expected tool result, got {other:?}"),
    }
    match &entries[4].entry {
        SessionEntry::Extension { extension, kind, payload } => {
            assert_eq!(extension, "demo");
            assert_eq!(kind, "marker");
            assert_eq!(payload["k"], 1);
            assert_eq!(payload["v"][2], 3);
        }
        other => panic!("expected extension entry, got {other:?}"),
    }
}

#[test]
fn writer_is_idempotent_for_header() {
    let dir = fresh_dir("idem");
    let path = dir.join("idem.sqlite");
    let writer = SessionWriter::open(&path).expect("open");
    let header = SessionEntry::Header {
        id: "abc".into(),
        created_at: chrono::Utc::now(),
        version: "0.1.0".into(),
    };
    writer.write_header(header.clone()).expect("write 1");
    writer.write_header(header).expect("write 2 (no-op)");
    writer.checkpoint().expect("checkpoint");

    let reader = SessionReader::open(&path).expect("open reader");
    let sessions = reader.list_sessions().expect("list");
    assert_eq!(sessions.len(), 1, "header re-write must not duplicate");
    assert_eq!(sessions[0].id, "abc");
}

#[test]
fn read_finds_latest_session() {
    let dir = fresh_dir("latest");
    let older = dir.join("older.sqlite");
    let newer = dir.join("newer.sqlite");

    let writer = SessionWriter::open(&older).unwrap();
    writer
        .write_header(SessionEntry::Header {
            id: "older".into(),
            created_at: chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
            version: "0.1.0".into(),
        })
        .unwrap();
    writer.checkpoint().unwrap();

    let writer = SessionWriter::open(&newer).unwrap();
    writer
        .write_header(SessionEntry::Header {
            id: "newer".into(),
            created_at: chrono::DateTime::<chrono::Utc>::from_timestamp(1_800_000_000, 0).unwrap(),
            version: "0.1.0".into(),
        })
        .unwrap();
    writer.checkpoint().unwrap();

    let reader = SessionReader::open(&older).unwrap();
    assert_eq!(reader.latest_session().unwrap().unwrap().id, "older");
    let reader = SessionReader::open(&newer).unwrap();
    assert_eq!(reader.latest_session().unwrap().unwrap().id, "newer");
}

#[test]
fn corrupt_db_returns_session_error_corrupt() {
    let dir = fresh_dir("corrupt");
    let path = dir.join("corrupt.sqlite");
    std::fs::write(&path, b"this is not a sqlite database").unwrap();

    let err = SessionReader::open(&path).unwrap_err();
    assert!(
        matches!(err, pi_session::SessionError::Corrupt(_) | pi_session::SessionError::Sqlite(_)),
        "expected Corrupt or Sqlite error, got {err:?}"
    );
}

#[test]
fn empty_db_round_trips() {
    let dir = fresh_dir("empty");
    let path = dir.join("empty.sqlite");
    let writer = SessionWriter::open(&path).unwrap();
    writer.checkpoint().unwrap();
    let reader = SessionReader::open(&path).unwrap();
    let sessions = reader.list_sessions().unwrap();
    assert!(sessions.is_empty());
    let header = reader.session_header().unwrap();
    assert!(header.is_none());
}