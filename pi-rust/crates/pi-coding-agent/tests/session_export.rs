//! End-to-end tests for the session export surface (LUM-1174).
//!
//! Unit tests for the individual pieces live next to their modules; this
//! file exercises the whole flow the CLI and the TUI actually use:
//! `pi --export` → HTML file, `/export` → HTML/JSONL, and the upstream
//! error messages for missing / malformed sessions.

use std::path::{Path, PathBuf};

use pi_coding_agent::cli::Cli;
use pi_coding_agent::commands::slash::{handle_command, SlashCommand};
use pi_coding_agent::export::{
    base64_encode, export_active_session_html, export_active_session_jsonl, export_from_file,
    read_session_file, session_data_from_messages,
};
use pi_protocol::{Content, Message, Role, ToolDefinition};

/// A session file in the upstream (`type: "session"` + `message`/`label`
/// entries) spelling. Every entry already carries an `id` and a `timestamp`,
/// so reading it is deterministic — which lets the tests compare the
/// base64 payload embedded in the HTML against a re-serialised
/// [`SessionData`](pi_coding_agent::export::SessionData).
const SESSION_JSONL: &str = concat!(
    r#"{"type":"session","version":3,"id":"session-2024","timestamp":"2024-12-03T14:00:00.000Z","cwd":"/tmp/project"}"#,
    "\n",
    r#"{"type":"message","id":"a1b2c3d4","parentId":null,"timestamp":"2024-12-03T14:00:01.000Z","message":{"role":"user","content":"Hello","timestamp":1733234401000}}"#,
    "\n",
    r#"{"type":"message","id":"b2c3d4e5","parentId":"a1b2c3d4","timestamp":"2024-12-03T14:00:02.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Hi!"},{"type":"toolCall","id":"call_123","name":"bash","arguments":{"command":"ls"}}],"model":"faux-model","usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0,"totalTokens":3},"stopReason":"toolUse","timestamp":1733234402000}}"#,
    "\n",
    r#"{"type":"message","id":"c3d4e5f6","parentId":"b2c3d4e5","timestamp":"2024-12-03T14:00:03.000Z","message":{"role":"toolResult","toolCallId":"call_123","toolName":"bash","content":[{"type":"text","text":"a.txt"}],"isError":false,"timestamp":1733234403000}}"#,
    "\n",
    r#"{"type":"label","id":"d4e5f6a7","parentId":"c3d4e5f6","timestamp":"2024-12-03T14:00:04.000Z","label":"checkpoint"}"#,
    "\n",
);

fn write_session(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).expect("write session fixture");
    path
}

#[test]
fn exports_a_session_file_to_a_self_contained_html_document() {
    let dir = tempfile::tempdir().expect("tempdir");
    let input = write_session(dir.path(), "session-2024.jsonl", SESSION_JSONL);
    let output = dir.path().join("out.html");

    let written = export_from_file(&input, Some(&output)).expect("export");
    assert_eq!(written, output);

    let html = std::fs::read_to_string(&written).expect("read html");
    assert!(html.starts_with("<!DOCTYPE html"), "missing doctype");
    assert!(html.contains("id=\"session-data\""), "missing payload node");
    // Theme CSS variables generated from the pi-tui theme.
    assert!(
        html.contains("--exportPageBg:"),
        "missing export theme vars"
    );
    assert!(html.contains("--userMessageBg:"), "missing theme vars");
    // Both vendored libraries are inlined, so the page needs no network.
    assert!(html.contains("hljs"), "highlight.js missing");
    assert!(html.contains("marked"), "marked missing");

    // The payload is exactly base64(JSON(SessionData)) for the file.
    let data = read_session_file(&input).expect("read back");
    assert_eq!(data.leaf_id.as_deref(), Some("d4e5f6a7"));
    assert_eq!(data.entries.len(), 4);
    assert_eq!(data.header.as_ref().unwrap()["id"], "session-2024");
    let payload = base64_encode(&serde_json::to_vec(&data).expect("serialise"));
    assert!(html.contains(&payload), "base64 payload mismatch");

    // Same input, same theme → byte-identical output.
    let second = dir.path().join("again.html");
    export_from_file(&input, Some(&second)).expect("export again");
    assert_eq!(html, std::fs::read_to_string(&second).expect("read again"));
}

#[test]
fn exports_an_active_session_to_html_and_jsonl() {
    let dir = tempfile::tempdir().expect("tempdir");
    let messages = vec![
        Message {
            role: Role::User,
            content: vec![Content::text("hello")],
            model: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![
                Content::text("running"),
                Content::ToolCall(pi_protocol::ToolCall {
                    id: "call_1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "ls"}),
                }),
            ],
            model: Some("faux-model".into()),
        },
        Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(pi_protocol::ToolResult {
                tool_call_id: "call_1".into(),
                content: Box::new(Content::text("a.txt")),
                is_error: false,
                details: None,
                added_tool_names: None,
            })],
            model: None,
        },
    ];
    let tools = vec![ToolDefinition {
        name: "bash".into(),
        label: "Bash".into(),
        description: "Run a shell command".into(),
        parameters: serde_json::json!({"type": "object"}),
        metadata: None,
    }];
    let data = session_data_from_messages(
        "live-session",
        "/tmp/project",
        &messages,
        Some("be helpful"),
        &tools,
    );
    assert_eq!(data.entries.len(), 3);
    assert_eq!(data.tools.as_ref().unwrap()[0].name, "bash");

    // HTML path.
    let html_path = dir.path().join("live.html");
    let written = export_active_session_html(&data, Some(&html_path), Some("dark")).expect("html");
    let html = std::fs::read_to_string(&written).expect("read html");
    let payload = base64_encode(&serde_json::to_vec(&data).expect("serialise"));
    assert!(html.contains(&payload), "base64 payload mismatch");
    assert!(
        html.contains("--exportCardBg:"),
        "missing export theme vars"
    );

    // JSONL path: `session` header + a linear `parentId` chain.
    let jsonl_path = dir.path().join("live.jsonl");
    export_active_session_jsonl(&data, Some(&jsonl_path)).expect("jsonl");
    let jsonl = std::fs::read_to_string(&jsonl_path).expect("read jsonl");
    assert!(jsonl.ends_with('\n'), "missing trailing newline");

    let lines: Vec<serde_json::Value> = jsonl
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid json line"))
        .collect();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0]["type"], "session");
    assert_eq!(lines[0]["version"], 3);
    assert_eq!(lines[0]["id"], "live-session");
    assert_eq!(lines[0]["cwd"], "/tmp/project");
    assert_eq!(lines[0]["timestamp"].as_str().unwrap().len(), 24);
    assert_eq!(lines[1]["parentId"], serde_json::Value::Null);
    assert_eq!(lines[2]["parentId"], lines[1]["id"]);
    assert_eq!(lines[3]["parentId"], lines[2]["id"]);
    assert_eq!(lines[3]["message"]["role"], "toolResult");
    assert_eq!(lines[3]["message"]["toolName"], "bash");
}

#[test]
fn export_is_reachable_from_both_the_slash_and_cli_surfaces() {
    // `/export [path]` parsing, including the quoted-path form and the
    // `.jsonl` vs HTML distinction.
    assert_eq!(
        handle_command("/export").unwrap(),
        SlashCommand::Export { path: None }
    );
    assert_eq!(
        handle_command("/export out.html").unwrap(),
        SlashCommand::Export {
            path: Some("out.html".into())
        }
    );
    assert_eq!(
        handle_command("/export \"/tmp/my session.jsonl\"").unwrap(),
        SlashCommand::Export {
            path: Some("/tmp/my session.jsonl".into())
        }
    );

    // `pi --export <session.jsonl> [output.html]`.
    let cli = Cli::try_parse_from_args(["pi", "--export", "session.jsonl"]).expect("parses");
    assert_eq!(cli.export, Some(vec![PathBuf::from("session.jsonl")]));

    let cli =
        Cli::try_parse_from_args(["pi", "--export", "session.jsonl", "out.html"]).expect("parses");
    assert_eq!(
        cli.export,
        Some(vec![
            PathBuf::from("session.jsonl"),
            PathBuf::from("out.html")
        ])
    );

    // The optional second value must not swallow the next flag.
    let cli = Cli::try_parse_from_args(["pi", "--export", "session.jsonl", "--print", "hi"])
        .expect("parses");
    assert_eq!(cli.print.as_deref(), Some("hi"));
}

#[test]
fn a_relative_output_path_is_not_absolutised() {
    // Upstream only runs `normalizePath` on the output, so `Exported to:`
    // echoes the path the user gave, not an absolute one.
    let dir = tempfile::tempdir().expect("tempdir");
    let input = write_session(dir.path(), "session-2024.jsonl", SESSION_JSONL);
    let relative = PathBuf::from("target").join(format!("lum1174-rel-{}.html", std::process::id()));

    let written =
        pi_coding_agent::commands::export::run_cli_export(&input, Some(&relative)).expect("export");
    assert_eq!(written, relative, "relative output must stay relative");
    assert!(
        relative.exists(),
        "expected {} to exist",
        relative.display()
    );
    std::fs::remove_file(&relative).ok();
}

#[test]
fn missing_and_malformed_sessions_keep_the_upstream_error_text() {
    let dir = tempfile::tempdir().expect("tempdir");

    let missing = dir.path().join("nope.jsonl");
    let err = export_from_file(&missing, None).expect_err("missing file");
    assert_eq!(
        err.to_string(),
        format!("File not found: {}", missing.display())
    );

    let malformed = write_session(
        dir.path(),
        "malformed.jsonl",
        "{\"type\":\"message\",\"id\":\"a\"}\n",
    );
    let err = export_from_file(&malformed, None).expect_err("malformed file");
    assert_eq!(
        err.to_string(),
        format!(
            "Session file is not a valid pi session: {}",
            malformed.display()
        )
    );

    let empty = write_session(dir.path(), "empty.jsonl", "");
    let err = export_from_file(&empty, None).expect_err("empty file");
    assert_eq!(
        err.to_string(),
        format!(
            "Session file is not a valid pi session: {}",
            empty.display()
        )
    );
}
