//! Integration tests for the built-in tool bundle.
//!
//! Each test creates a unique temp directory so parallel test runs do not
//! collide. The `ReadTool` / `WriteTool` round-trip lives in this file so
//! the write/read contract is covered by a single observable behaviour.

use std::path::PathBuf;

use pi_coding_agent::tools::{
    standard_tools, AbortLike, AgentTool, BashTool, EditTool, EditToolDetails, ReadTool, WriteTool,
};
use serde_json::json;

fn fresh_tmp(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pi-coding-agent-test-{}-{}",
        label,
        std::process::id(),
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn first_text(out: &pi_coding_agent::tools::ToolOutput) -> String {
    use pi_protocol::Content;
    match out.content.first().expect("content block") {
        Content::Text(t) => t.text.clone(),
        other => panic!("expected text content, got {:?}", other),
    }
}

#[tokio::test]
async fn bash_runs_ls() {
    let dir = fresh_tmp("bash-runs-ls");
    std::fs::write(dir.join("alpha.txt"), b"alpha").unwrap();
    std::fs::write(dir.join("beta.txt"), b"beta").unwrap();

    let tool = BashTool;
    let out = tool
        .execute(
            json!({ "command": format!("ls {}", dir.display()) }),
            AbortLike::none(),
        )
        .await
        .expect("ls should succeed");

    let text = first_text(&out);
    assert!(text.contains("alpha.txt"), "missing alpha.txt in: {text}");
    assert!(text.contains("beta.txt"), "missing beta.txt in: {text}");

    let details = out.details.expect("details present");
    assert_eq!(details["timed_out"], json!(false));
    assert_eq!(details["exit_code"], json!(0));
}

#[tokio::test]
async fn bash_reports_non_zero_exit() {
    let tool = BashTool;
    let err = tool
        .execute(
            json!({ "command": "sh -c 'echo oops 1>&2; exit 7'" }),
            AbortLike::none(),
        )
        .await
        .expect_err("non-zero exit is an error");

    let msg = err.to_string();
    assert!(msg.contains("oops"), "stderr should surface in error: {msg}");
    assert!(
        msg.contains("[exit code 7]"),
        "exit code should surface in error: {msg}"
    );
}

#[tokio::test]
async fn bash_respects_cwd() {
    let dir = fresh_tmp("bash-cwd");
    std::fs::write(dir.join("marker.txt"), b"present").unwrap();

    let tool = BashTool;
    let out = tool
        .execute(
            json!({
                "command": "ls",
                "cwd": dir.display().to_string(),
            }),
            AbortLike::none(),
        )
        .await
        .expect("ls in cwd should succeed");

    let text = first_text(&out);
    assert!(
        text.contains("marker.txt"),
        "marker.txt missing from ls: {text}"
    );
}

#[tokio::test]
async fn write_and_read_roundtrip() {
    let dir = fresh_tmp("write-read");
    let path = dir.join("hello.txt");
    let body = "line one\nline two\nline three\n";

    WriteTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "content": body,
            }),
            AbortLike::none(),
        )
        .await
        .expect("write");

    let read_back = ReadTool
        .execute(
            json!({ "path": path.display().to_string() }),
            AbortLike::none(),
        )
        .await
        .expect("read");

    assert_eq!(first_text(&read_back), body);
}

#[tokio::test]
async fn write_creates_missing_parent_dirs() {
    let dir = fresh_tmp("write-mkdir");
    let path = dir.join("nested/dir/file.txt");

    WriteTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "content": "ok",
            }),
            AbortLike::none(),
        )
        .await
        .expect("write should auto-create parent dirs");

    let read_back = ReadTool
        .execute(
            json!({ "path": path.display().to_string() }),
            AbortLike::none(),
        )
        .await
        .expect("read");

    assert_eq!(first_text(&read_back), "ok");
}

#[tokio::test]
async fn edit_replaces_single_occurrence() {
    let dir = fresh_tmp("edit-single");
    let path = dir.join("note.txt");
    std::fs::write(&path, "hello world\nfoo bar\n").unwrap();

    let out = EditTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "old_text": "foo bar",
                "new_text": "foo baz",
            }),
            AbortLike::none(),
        )
        .await
        .expect("single edit");

    let details: EditToolDetails =
        serde_json::from_value(out.details.expect("details")).expect("decode");
    assert_eq!(details.replaced, 1);
    assert_eq!(details.diff_summary, "1 occurrence replaced");

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "hello world\nfoo baz\n"
    );
}

#[tokio::test]
async fn edit_replace_all() {
    let dir = fresh_tmp("edit-replace-all");
    let path = dir.join("multi.txt");
    std::fs::write(&path, "aaa bbb aaa ccc aaa\n").unwrap();

    let out = EditTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "old_text": "aaa",
                "new_text": "ZZZ",
                "replace_all": true,
            }),
            AbortLike::none(),
        )
        .await
        .expect("replace_all edit");

    let details: EditToolDetails =
        serde_json::from_value(out.details.expect("details")).expect("decode");
    assert_eq!(details.replaced, 3);
    assert_eq!(details.diff_summary, "3 occurrences replaced");

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "ZZZ bbb ZZZ ccc ZZZ\n"
    );
}

#[tokio::test]
async fn edit_errors_on_zero_or_multiple_occurrences_without_replace_all() {
    let dir = fresh_tmp("edit-strict");
    let path = dir.join("dup.txt");
    std::fs::write(&path, "x x x\n").unwrap();

    // Zero occurrences → error.
    let err = EditTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "old_text": "nope",
                "new_text": "yes",
            }),
            AbortLike::none(),
        )
        .await
        .expect_err("zero matches should error");
    assert!(err.to_string().contains("not found"), "got: {err}");

    // Multiple occurrences → error unless replace_all.
    let err = EditTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "old_text": "x",
                "new_text": "y",
            }),
            AbortLike::none(),
        )
        .await
        .expect_err("multiple matches should error");
    let msg = err.to_string();
    assert!(
        msg.contains("matches 3 places"),
        "expected count of matches in error, got: {msg}"
    );

    // File should be untouched.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "x x x\n");
}

#[tokio::test]
async fn abort_like_short_circuits_tools() {
    let tool = BashTool;
    let err = tool
        .execute(
            json!({ "command": "echo hello" }),
            AbortLike::cancelled(),
        )
        .await
        .expect_err("pre-cancelled handle should reject the call");
    assert!(matches!(
        err,
        pi_coding_agent::tools::ToolError::Aborted
    ));
}

#[tokio::test]
async fn invalid_arguments_surface_as_invalid_arguments_error() {
    let tool = WriteTool;
    let err = tool
        .execute(
            json!({ "path": "/tmp/x" /* missing content */ }),
            AbortLike::none(),
        )
        .await
        .expect_err("missing required field is invalid arguments");
    assert!(matches!(
        err,
        pi_coding_agent::tools::ToolError::InvalidArguments(_)
    ));
}

#[tokio::test]
async fn standard_tools_returns_seven() {
    let tools = standard_tools();

    assert_eq!(
        tools.len(),
        7,
        "standard_tools() must expose read, write, edit, bash, find, grep, ls (got {} tools)",
        tools.len()
    );

    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert_eq!(
        names,
        vec!["read", "write", "edit", "bash", "find", "grep", "ls"],
        "tools must be returned in declared order"
    );

    // Spot-check definitions: each tool has a non-empty description and a
    // JSON Schema with `type: object`.
    for tool in &tools {
        assert!(!tool.description().is_empty(), "{} empty desc", tool.name());
        let params = tool.parameters();
        assert_eq!(
            params["type"],
            json!("object"),
            "{} schema must declare object",
            tool.name()
        );
    }

    // The shell / navigation tools pin Sequential; the FS-mutation tools
    // (read/write/edit) fall through to the agent loop default
    // (Parallel) and that's fine — they don't share state.
    for tool in &tools {
        match tool.name() {
            "bash" | "find" | "grep" | "ls" => assert_eq!(
                tool.execution_mode(),
                Some(pi_protocol::ToolExecutionMode::Sequential),
                "{} should declare Sequential execution",
                tool.name()
            ),
            _ => assert!(
                tool.execution_mode().is_none(),
                "{} should default to Parallel (None)",
                tool.name()
            ),
        }
    }
}