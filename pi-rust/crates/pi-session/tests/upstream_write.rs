//! Write-path tests: a database written by [`SessionWriter`] must be the
//! **upstream v4** layout, field for field, so the TS `SqliteStorage` in
//! `packages/session-backends/sqlite-node` can open it directly.
//!
//! The DDL identity test below reads the TypeScript migration from the
//! monorepo and asserts it is byte-identical to the SQL this crate embeds,
//! which is what keeps the two schemas from drifting.

use std::path::{Path, PathBuf};

use pi_protocol::{
    AssistantMessage, Content, ImageContent, Message, Role, SessionEntry, StopReason, TextContent,
    ToolCall, ToolResult, Usage,
};
use pi_session::{SchemaLayout, SessionReader, SessionWriter, UPSTREAM_INITIAL_SQL};
use rusqlite::Connection;

mod common;

use common::fresh_dir;

/// `packages/session-backends/sqlite-node/src/sqlite/migrations/001_initial.sql`
/// relative to `pi-rust/crates/pi-session`.
fn upstream_migration_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "../../../packages/session-backends/sqlite-node/src/sqlite/migrations/001_initial.sql",
    )
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

fn scalar<T: rusqlite::types::FromSql>(conn: &Connection, sql: &str) -> T {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn written_database_has_the_upstream_v4_shape() {
    let dir = fresh_dir("upstream-shape");
    let path = dir.join("session.sqlite");

    let writer = SessionWriter::open(&path).expect("open writer");
    assert_eq!(writer.layout(), SchemaLayout::UpstreamV4);
    writer
        .write_header(SessionEntry::Header {
            id: "shape".into(),
            created_at: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(1_700_000_000_123)
                .unwrap(),
            version: "0.1.0".into(),
        })
        .expect("header");
    writer
        .append(SessionEntry::UserMessage(Message {
            role: Role::User,
            content: vec![Content::text("hi")],
            model: None,
        }))
        .expect("append");
    writer.checkpoint().expect("checkpoint");
    drop(writer);

    let conn = Connection::open(&path).expect("open written db");

    // Column order is the upstream `001_initial.sql` order.
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

    // Every upstream table exists, and the Rust-only `meta` table does not.
    for table in [
        "sessions",
        "entries",
        "scalar_values",
        "list_values",
        "usage_ledger",
        "branch_entries",
        "branch_meta",
    ] {
        let exists: i64 = scalar(
            &conn,
            &format!("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='{table}'"),
        );
        assert_eq!(exists, 1, "missing upstream table {table}");
    }
    let meta: i64 = scalar(
        &conn,
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='meta'",
    );
    assert_eq!(meta, 0, "Rust-only `meta` table must not exist");

    // Both upstream triggers guard the entry/usage id namespace.
    for trigger in ["trg_entries_validate", "trg_usage_ledger_validate"] {
        let exists: i64 = scalar(
            &conn,
            &format!(
                "SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name='{trigger}'"
            ),
        );
        assert_eq!(exists, 1, "missing upstream trigger {trigger}");
    }

    // `payload` is plain JSON text, not a zstd BLOB.
    let payload_type: String = scalar(
        &conn,
        "SELECT type FROM pragma_table_info('entries') WHERE name='payload'",
    );
    assert_eq!(payload_type, "TEXT");
    let payload: String = scalar(&conn, "SELECT payload FROM entries WHERE seq = 1");
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("plain JSON payload");
    assert_eq!(parsed["message"]["role"], "user");

    // Format identity lives in `sessions`; `PRAGMA user_version` stays 0.
    assert_eq!(
        scalar::<i64>(&conn, "PRAGMA user_version"),
        0,
        "upstream files do not use `user_version`"
    );
    assert_eq!(
        scalar::<i64>(&conn, "SELECT storage_version FROM sessions"),
        1
    );
    assert_eq!(
        scalar::<i64>(&conn, "SELECT message_count FROM sessions"),
        1
    );
    assert_eq!(scalar::<i64>(&conn, "SELECT next_seq FROM sessions"), 2);
    let metadata: String = scalar(&conn, "SELECT metadata FROM sessions");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&metadata).unwrap()["version"],
        "0.1.0"
    );
    // `usage_payload` is the full zero-usage object, never `{}`.
    let usage: String = scalar(&conn, "SELECT usage_payload FROM sessions");
    let usage: serde_json::Value = serde_json::from_str(&usage).unwrap();
    assert_eq!(usage["input"], 0);
    assert_eq!(usage["totalTokens"], 0);
    assert_eq!(usage["cost"]["total"], 0);
}

#[test]
fn upstream_initial_sql_matches_the_typescript_migration_byte_for_byte() {
    let path = upstream_migration_path();
    assert!(
        path.exists(),
        "upstream migration missing at {} — this test must run inside the pi monorepo",
        path.display()
    );
    let on_disk = std::fs::read(&path).expect("read 001_initial.sql");
    let embedded = UPSTREAM_INITIAL_SQL.as_bytes();
    assert_eq!(
        embedded.len(),
        on_disk.len(),
        "embedded upstream DDL is {} bytes, on-disk migration is {} bytes",
        embedded.len(),
        on_disk.len()
    );
    assert!(
        embedded == on_disk.as_slice(),
        "embedded UPSTREAM_INITIAL_SQL drifted from {}",
        path.display()
    );
}

#[test]
fn six_entry_round_trip_is_field_for_field() {
    let dir = fresh_dir("six-entry");
    let path = dir.join("session.sqlite");

    let user = SessionEntry::UserMessage(Message {
        role: Role::User,
        content: vec![Content::text("hello pi")],
        model: None,
    });
    let assistant = SessionEntry::AssistantMessage(AssistantMessage {
        model: "faux/faux-model".into(),
        content: vec![
            Content::Text(TextContent {
                text: "calling a tool".into(),
            }),
            Content::ToolCall(ToolCall {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"cmd": "ls"}),
            }),
            Content::Image(ImageContent {
                mime_type: "image/png".into(),
                data: "AAAA".into(),
            }),
        ],
        stop_reason: StopReason::ToolUse,
        usage: Usage {
            input: 10,
            output: 4,
            cache_read: 2,
            cache_write: 1,
            total: 16,
        },
        error_message: Some("boom".into()),
    });
    let tool_result = SessionEntry::ToolResult(ToolResult {
        tool_call_id: "call-1".into(),
        content: Box::new(Content::Text(TextContent {
            text: "ok\n".into(),
        })),
        is_error: false,
        details: Some(serde_json::json!({"exitCode": 0})),
        added_tool_names: Some(vec!["grep".into()]),
        images: Vec::new(),
    });
    let compaction = SessionEntry::Compaction {
        summary: "## Goal\nkeep going".into(),
        retained_tail: vec![
            Message {
                role: Role::User,
                content: vec![Content::text("earlier")],
                model: None,
            },
            Message {
                role: Role::Assistant,
                content: vec![Content::text("answer")],
                model: Some("faux/faux-model".into()),
            },
        ],
        tokens_before: 4096,
        usage: Some(Usage {
            input: 100,
            output: 20,
            cache_read: 0,
            cache_write: 0,
            total: 120,
        }),
        details: Some(serde_json::json!({"readFiles": ["src/lib.rs"]})),
    };
    // A custom extension writes `data`; on the way back its *name* is
    // folded into `kind` (upstream has one `custom_type` slot) — the
    // documented degradation.
    let custom = SessionEntry::Extension {
        extension: "demo".into(),
        kind: "marker".into(),
        payload: serde_json::json!({"k": 1, "nested": {"deep": true}}),
    };
    let expected_custom = SessionEntry::Extension {
        extension: "custom".into(),
        kind: "marker".into(),
        payload: serde_json::json!({"k": 1, "nested": {"deep": true}}),
    };
    let branch = SessionEntry::Extension {
        extension: "branch_summary".into(),
        kind: "branch_summary".into(),
        payload: serde_json::json!({"fromId": "e4", "summary": "explored", "fromHook": true}),
    };

    let writer = SessionWriter::open(&path).expect("open writer");
    writer
        .write_header(SessionEntry::Header {
            id: "six".into(),
            created_at: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(1_700_000_000_123)
                .unwrap(),
            version: "0.1.0".into(),
        })
        .expect("header");
    for entry in [
        user.clone(),
        assistant.clone(),
        tool_result.clone(),
        compaction.clone(),
        custom.clone(),
        branch.clone(),
    ] {
        writer.append(entry).expect("append");
    }
    writer.checkpoint().expect("checkpoint");

    let reader = SessionReader::open(&path).expect("open reader");
    assert_eq!(reader.layout(), SchemaLayout::UpstreamV4);
    let header = reader
        .session_header()
        .expect("header")
        .expect("has header");
    assert_eq!(header.id, "six");
    assert_eq!(header.version.as_deref(), Some("0.1.0"));

    let decoded = reader.iter_entries("six").expect("entries");
    assert_eq!(decoded.len(), 6);

    // Ids and the parent chain are linear; timestamps are milliseconds.
    for (index, row) in decoded.iter().enumerate() {
        let expected_id = format!("e{}", index + 1);
        assert_eq!(row.entry_id.as_deref(), Some(expected_id.as_str()));
        assert_eq!(row.seq, (index + 1) as i64);
        let expected_parent = (index > 0).then(|| format!("e{index}"));
        assert_eq!(row.parent_entry_id.as_deref(), expected_parent.as_deref());
        assert!(row.timestamp > 0, "timestamps are milliseconds");
    }
    assert_eq!(
        decoded
            .iter()
            .map(|row| row.type_.as_str())
            .collect::<Vec<_>>(),
        vec![
            "message",
            "message",
            "message",
            "compaction",
            "custom",
            "branch_summary",
        ]
    );

    let entries: Vec<SessionEntry> = decoded.into_iter().map(|row| row.entry).collect();
    assert_eq!(entries[0], user);
    assert_eq!(entries[1], assistant);
    assert_eq!(entries[2], tool_result);
    assert_eq!(entries[3], compaction);
    assert_eq!(entries[4], expected_custom);
    assert_eq!(entries[5], branch);

    // `message_count` counts the three `type = "message"` rows.
    let conn = Connection::open(&path).expect("reopen");
    assert_eq!(
        scalar::<i64>(&conn, "SELECT message_count FROM sessions"),
        3
    );
    assert_eq!(scalar::<i64>(&conn, "SELECT next_seq FROM sessions"), 7);
}

#[test]
fn a_rejected_batch_writes_nothing_and_can_be_retried() {
    let dir = fresh_dir("trigger-failure");
    let path = dir.join("session.sqlite");

    let writer = SessionWriter::open(&path).expect("open writer");
    writer
        .write_header(SessionEntry::Header {
            id: "guard".into(),
            created_at: chrono::Utc::now(),
            version: "0.1.0".into(),
        })
        .expect("header");
    writer
        .append(SessionEntry::UserMessage(Message {
            role: Role::User,
            content: vec![Content::text("first")],
            model: None,
        }))
        .expect("first message");
    writer.commit().expect("commit first");

    // Yank the parent row out from under the writer with a second
    // connection; its cached `last_entry_id` (`e1`) is now dangling.
    Connection::open(&path)
        .expect("second connection")
        .execute("DELETE FROM entries WHERE session_id = 'guard'", [])
        .expect("delete parent");

    writer
        .append(SessionEntry::AssistantMessage(AssistantMessage {
            model: "faux/faux-model".into(),
            content: vec![Content::text("second")],
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
            error_message: None,
        }))
        .expect("stage second");

    let err = writer.commit().expect_err("trigger must reject the batch");
    assert!(
        matches!(err, pi_session::SessionError::Sqlite(_)),
        "expected a Sqlite error, got {err:?}"
    );
    assert!(
        err.to_string().contains("missing parent entry"),
        "unexpected error: {err}"
    );

    // The transaction rolled back: no half-written rows survived.
    let conn = Connection::open(&path).expect("reopen");
    assert_eq!(scalar::<i64>(&conn, "SELECT count(*) FROM entries"), 0);

    // The staged batch is still buffered; `rollback` drops it and
    // resynchronises the parent chain with the committed state, after
    // which the writer is usable again.
    writer.rollback();
    writer
        .append(SessionEntry::Extension {
            extension: "demo".into(),
            kind: "recovered".into(),
            payload: serde_json::json!({"ok": true}),
        })
        .expect("stage after rollback");
    assert_eq!(writer.commit().expect("commit recovered"), 1);
    assert_eq!(scalar::<i64>(&conn, "SELECT count(*) FROM entries"), 1);
}
