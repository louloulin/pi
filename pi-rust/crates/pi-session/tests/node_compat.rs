//! Cross-implementation read: a database written by the Rust
//! [`SessionWriter`] is opened by `node:sqlite` (the engine behind the
//! upstream TS `packages/session-backends/sqlite-node`) via
//! `scripts/verify-upstream-db.mjs`.
//!
//! The test is deliberately a real subprocess: it proves the *file*, not
//! another Rust reader, is upstream-compatible. `node` is required and
//! the test fails loudly when it is missing.

use std::path::{Path, PathBuf};
use std::process::Command;

use pi_protocol::{
    AssistantMessage, Content, Message, Role, SessionEntry, StopReason, TextContent, ToolCall,
    ToolResult, Usage,
};
use pi_session::SessionWriter;

mod common;

use common::fresh_dir;

fn verify_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/verify-upstream-db.mjs")
}

#[test]
fn node_reads_a_rust_written_database() {
    let dir = fresh_dir("node-compat");
    let db = dir.join("session.sqlite");

    let writer = SessionWriter::open(&db).expect("open writer");
    writer
        .write_header(SessionEntry::Header {
            id: "node-compat".into(),
            created_at: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(1_700_000_000_123)
                .unwrap(),
            version: "0.1.0".into(),
        })
        .expect("header");
    for entry in [
        SessionEntry::UserMessage(Message {
            role: Role::User,
            content: vec![Content::text("hello from rust")],
            model: None,
        }),
        SessionEntry::AssistantMessage(AssistantMessage {
            model: "faux/faux-model".into(),
            content: vec![
                Content::Text(TextContent {
                    text: "calling a tool".into(),
                }),
                Content::ToolCall(ToolCall {
                    id: "call-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"cmd": "echo hi"}),
                }),
            ],
            stop_reason: StopReason::ToolUse,
            usage: Usage {
                input: 5,
                output: 2,
                cache_read: 0,
                cache_write: 0,
                total: 7,
            },
            error_message: None,
        }),
        SessionEntry::ToolResult(ToolResult {
            tool_call_id: "call-1".into(),
            content: Box::new(Content::text("ok\n")),
            is_error: false,
            details: Some(serde_json::json!({"exitCode": 0})),
            added_tool_names: None,
            images: Vec::new(),
        }),
        SessionEntry::Compaction {
            summary: "## Goal\ndone".into(),
            retained_tail: vec![Message {
                role: Role::User,
                content: vec![Content::text("kept")],
                model: None,
            }],
            tokens_before: 2048,
            usage: None,
            details: None,
        },
        SessionEntry::Extension {
            extension: "demo".into(),
            kind: "marker".into(),
            payload: serde_json::json!({"k": 1}),
        },
        SessionEntry::Extension {
            extension: "branch_summary".into(),
            kind: "branch_summary".into(),
            payload: serde_json::json!({"fromId": "e4", "summary": "explored", "fromHook": true}),
        },
    ] {
        writer.append(entry).expect("append");
    }
    writer.checkpoint().expect("checkpoint");
    drop(writer);

    // Fold the WAL back into the main file so the reader sees exactly the
    // committed state (mirrors what the CLI does before handing off).
    assert!(db.exists());

    let output = Command::new("node").arg(verify_script()).arg(&db).output();
    let output = match output {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => panic!(
            "`node` is required for the cross-implementation test; \
             install Node 22+ (node:sqlite) and re-run"
        ),
        Err(error) => panic!("failed to run node: {error}"),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "node verify-upstream-db.mjs failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // Surface the cross-implementation report when run with `-- --nocapture`.
    println!("{stdout}");

    let report: serde_json::Value =
        serde_json::from_str(&stdout).expect("script prints a JSON report");

    assert_eq!(report["userVersion"], 0);
    assert_eq!(report["sessions"].as_array().unwrap().len(), 1);
    let session = &report["sessions"][0];
    assert_eq!(session["id"], "node-compat");
    assert_eq!(session["messageCount"], 3);
    assert_eq!(session["nextSeq"], 7);
    assert_eq!(session["storageVersion"], 1);
    assert_eq!(session["metadata"]["version"], "0.1.0");
    assert_eq!(session["usage"]["input"], 0);
    assert_eq!(session["usage"]["cost"]["total"], 0);

    let entries = report["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 6);
    let types: Vec<&str> = entries
        .iter()
        .map(|entry| entry["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        vec![
            "message",
            "message",
            "message",
            "compaction",
            "custom",
            "branch_summary"
        ]
    );

    // The assistant message the TS side will read: camelCase stop reason
    // and a tool call in the content array.
    let assistant = &entries[1]["payload"]["message"];
    assert_eq!(assistant["role"], "assistant");
    assert_eq!(assistant["stopReason"], "toolUse");
    assert_eq!(assistant["usage"]["input"], 5);
    assert_eq!(assistant["content"][1]["type"], "toolCall");
    assert_eq!(assistant["content"][1]["name"], "bash");

    // Tool result message.
    let tool_result = &entries[2]["payload"]["message"];
    assert_eq!(tool_result["role"], "toolResult");
    assert_eq!(tool_result["toolCallId"], "call-1");
    assert_eq!(tool_result["content"][0]["text"], "ok\n");

    // Custom entry: `data` wrapper + custom_type.
    assert_eq!(entries[4]["customType"], "marker");
    assert_eq!(entries[4]["payload"]["data"]["k"], 1);

    // Branch summary travels verbatim.
    assert_eq!(entries[5]["payload"]["summary"], "explored");
    assert_eq!(entries[5]["payload"]["fromHook"], true);

    // The triggers the script checked are the upstream ones.
    let triggers: Vec<&str> = report["triggers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert!(triggers.contains(&"trg_entries_validate"));
    assert!(triggers.contains(&"trg_usage_ledger_validate"));
}
