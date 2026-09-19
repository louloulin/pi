//! End-to-end tests for the `read` / `write` renderers.
//!
//! These drive the *real* tools (not fixtures) through [`ToolRenderSession`],
//! the same entry point `text_fallback.rs` uses, and assert on the ANSI the
//! terminal would see. That keeps the "language detection → highlighter →
//! display" chain covered by one test instead of three unit-level assertions.

use std::path::PathBuf;

use pi_coding_agent::tools::{
    render_lines_ansi, render_lines_plain, AbortLike, AgentTool, ReadTool, ToolOutput,
    ToolRenderSession, WriteTool,
};
use pi_protocol::{Content, ToolCall, ToolResult};
use pi_tui::{builtin_theme, ColorMode, Theme, ThemeColor};
use serde_json::json;

fn fresh_tmp(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pi-coding-agent-render-{}-{}",
        label,
        std::process::id(),
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn theme() -> Theme {
    builtin_theme("dark", ColorMode::TrueColor).expect("built-in dark theme")
}

fn call(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments,
    }
}

fn as_result(id: &str, output: ToolOutput, is_error: bool) -> ToolResult {
    let content = output
        .content
        .into_iter()
        .next()
        .unwrap_or_else(|| Content::text(""));
    ToolResult {
        tool_call_id: id.to_string(),
        content: Box::new(content),
        is_error,
        details: output.details,
    }
}

fn has_ansi(text: &str, slot: ThemeColor) -> bool {
    text.contains(theme().get_fg_ansi(slot))
}

#[tokio::test]
async fn real_read_of_rust_file_renders_highlighted_ansi() {
    let dir = fresh_tmp("read-rust");
    let path = dir.join("main.rs");
    std::fs::write(
        &path,
        "fn main() {\n    let value = 1;\n    println!(\"{value}\");\n}\n",
    )
    .unwrap();
    let args = json!({ "path": path.display().to_string() });
    let output = ReadTool
        .execute(args.clone(), AbortLike::none())
        .await
        .expect("read succeeds");

    let mut session = ToolRenderSession::new(&dir);
    let call_lines = session.call(&call("c1", "read", args), false);
    let lines = session.result(&as_result("c1", output, false));
    let ansi = render_lines_ansi(&lines, &theme());
    let plain = render_lines_plain(&lines);

    assert_eq!(render_lines_plain(&call_lines), "read main.rs");
    assert!(plain.contains("fn main()"), "{plain:?}");
    assert!(
        has_ansi(&ansi, ThemeColor::SyntaxKeyword),
        "rust keywords must be highlighted: {ansi:?}"
    );
}

#[tokio::test]
async fn real_read_of_text_file_renders_plain_output() {
    let dir = fresh_tmp("read-text");
    let path = dir.join("notes.txt");
    std::fs::write(&path, "fn main() {}\n").unwrap();
    let args = json!({ "path": path.display().to_string() });
    let output = ReadTool
        .execute(args.clone(), AbortLike::none())
        .await
        .expect("read succeeds");

    let mut session = ToolRenderSession::new(&dir);
    session.call(&call("c1", "read", args), false);
    let lines = session.result(&as_result("c1", output, false));
    let ansi = render_lines_ansi(&lines, &theme());

    assert!(!has_ansi(&ansi, ThemeColor::SyntaxKeyword), "{ansi:?}");
    assert!(has_ansi(&ansi, ThemeColor::ToolOutput), "{ansi:?}");
}

#[tokio::test]
async fn real_read_folds_to_ten_lines_with_a_more_lines_hint() {
    let dir = fresh_tmp("read-fold");
    let path = dir.join("long.py");
    let content: String = (1..=14)
        .map(|n| format!("value_{n} = {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, &content).unwrap();
    let args = json!({ "path": path.display().to_string() });
    let output = ReadTool
        .execute(args.clone(), AbortLike::none())
        .await
        .expect("read succeeds");

    let mut session = ToolRenderSession::new(&dir);
    session.call(&call("c1", "read", args), false);
    let lines = session.result(&as_result("c1", output, false));
    let plain = pi_coding_agent::tools::render::render_lines_plain(&lines);

    assert!(plain.ends_with("... (4 more lines)"), "{plain:?}");
    assert_eq!(plain.lines().count(), 11);
    assert!(
        has_ansi(
            &render_lines_ansi(&lines, &theme()),
            ThemeColor::SyntaxNumber
        ),
        "python numbers must be highlighted"
    );
}

#[tokio::test]
async fn real_write_preview_is_highlighted() {
    let dir = fresh_tmp("write-rust");
    let path = dir.join("lib.rs");
    let content = "pub fn answer() -> u32 {\n    42\n}\n";
    let args = json!({ "path": path.display().to_string(), "content": content });

    let mut session = ToolRenderSession::new(&dir);
    let lines = session.call(&call("c1", "write", args), false);
    let ansi = render_lines_ansi(&lines, &theme());
    let plain = render_lines_plain(&lines);

    assert!(plain.starts_with("write lib.rs"), "{plain:?}");
    assert!(plain.contains("(3 lines,"), "{plain:?}");
    assert!(
        has_ansi(&ansi, ThemeColor::SyntaxKeyword),
        "write preview must be highlighted: {ansi:?}"
    );

    // A successful write result renders nothing (upstream parity).
    let output = WriteTool
        .execute(
            json!({ "path": path.display().to_string(), "content": content }),
            AbortLike::none(),
        )
        .await
        .expect("write succeeds");
    let rendered = session.result(&as_result("c1", output, false));
    assert!(rendered.is_empty());
}

#[tokio::test]
async fn real_read_error_is_not_highlighted() {
    let dir = fresh_tmp("read-error");
    let missing = dir.join("missing.rs");
    let args = json!({ "path": missing.display().to_string() });
    let error = ReadTool
        .execute(args.clone(), AbortLike::none())
        .await
        .expect_err("missing file fails");

    let mut session = ToolRenderSession::new(&dir);
    session.call(&call("c1", "read", args), true);
    let lines = session.result(&as_result("c1", ToolOutput::text(error.to_string()), true));
    let ansi = render_lines_ansi(&lines, &theme());
    assert!(!has_ansi(&ansi, ThemeColor::SyntaxKeyword), "{ansi:?}");
    assert!(has_ansi(&ansi, ThemeColor::ToolOutput), "{ansi:?}");
}
