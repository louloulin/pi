//! `BuiltinToolExecutor` coerces model-issued argument types before dispatch.
//!
//! Models frequently emit a string where the schema asks for an integer or a
//! boolean (`"limit": "2"`, `"replace_all": "true"`). Upstream's
//! `validateToolArguments` coerces those; these tests pin the same behaviour
//! at the executor boundary, where the agent loop's tool calls actually land,
//! and pin that genuinely invalid arguments are still rejected.

use pi_agent_core::tools::ToolExecutor;
use pi_coding_agent::tool_executor::BuiltinToolExecutor;
use pi_protocol::{Content, ToolCall, ToolResult};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: format!("call-{name}"),
        name: name.to_string(),
        arguments,
    }
}

async fn execute(executor: &BuiltinToolExecutor, call: ToolCall) -> ToolResult {
    executor
        .execute(&call, CancellationToken::new())
        .await
        .expect("executor returns a tool result, not a fatal loop error")
}

fn text(result: &ToolResult) -> String {
    match result.content.as_ref() {
        Content::Text(block) => block.text.clone(),
        other => panic!("expected text content, got {other:?}"),
    }
}

fn fresh_tmp(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pi-coding-agent-coercion-{}-{}",
        label,
        std::process::id(),
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[tokio::test]
async fn string_integer_is_coerced_before_the_tool_parses_it() {
    let dir = fresh_tmp("read-limit");
    let file = dir.join("lines.txt");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();

    let executor = BuiltinToolExecutor::with_default_tools();
    let result = execute(
        &executor,
        call(
            "read",
            json!({ "path": file.display().to_string(), "limit": "1" }),
        ),
    )
    .await;

    assert!(!result.is_error, "read failed: {}", text(&result));
    let body = text(&result);
    assert!(body.contains("one"), "first line missing: {body}");
    assert!(
        !body.contains("two"),
        "limit=1 should not read line two: {body}"
    );
}

#[tokio::test]
async fn string_boolean_is_coerced_before_the_tool_parses_it() {
    let dir = fresh_tmp("edit-replace-all");
    let file = dir.join("dup.txt");
    std::fs::write(&file, "alpha alpha alpha").unwrap();

    let executor = BuiltinToolExecutor::with_default_tools();
    let result = execute(
        &executor,
        call(
            "edit",
            json!({
                "path": file.display().to_string(),
                "old_text": "alpha",
                "new_text": "beta",
                "replace_all": "true"
            }),
        ),
    )
    .await;

    assert!(!result.is_error, "edit failed: {}", text(&result));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "beta beta beta");
}

#[tokio::test]
async fn genuinely_invalid_arguments_are_still_rejected() {
    let dir = fresh_tmp("read-bad-limit");
    let file = dir.join("lines.txt");
    std::fs::write(&file, "one\n").unwrap();

    let executor = BuiltinToolExecutor::with_default_tools();
    let result = execute(
        &executor,
        call(
            "read",
            json!({ "path": file.display().to_string(), "limit": "not-a-number" }),
        ),
    )
    .await;

    assert!(
        result.is_error,
        "a non-numeric limit must stay an error, got: {}",
        text(&result)
    );
    let body = text(&result).to_lowercase();
    assert!(
        body.contains("invalid") || body.contains("limit") || body.contains("integer"),
        "error should explain the argument problem: {body}"
    );
}

#[tokio::test]
async fn optional_null_is_dropped_so_the_tool_default_applies() {
    let dir = fresh_tmp("read-null-limit");
    let file = dir.join("lines.txt");
    std::fs::write(&file, "one\ntwo\n").unwrap();

    let executor = BuiltinToolExecutor::with_default_tools();
    let result = execute(
        &executor,
        call(
            "read",
            json!({ "path": file.display().to_string(), "limit": null }),
        ),
    )
    .await;

    assert!(!result.is_error, "read failed: {}", text(&result));
    assert!(
        text(&result).contains("two"),
        "null limit should read all lines"
    );
}
