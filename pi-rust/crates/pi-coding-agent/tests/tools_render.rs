//! End-to-end tests for the `read` / `write` / `bash` / `find` / `grep` / `ls`
//! renderers.
//!
//! These drive the *real* tools (not fixtures) through [`ToolRenderSession`],
//! the same entry point `text_fallback.rs` uses, and assert on the ANSI the
//! terminal would see. That keeps the "language detection → highlighter →
//! display" chain covered by one test instead of three unit-level assertions.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use pi_coding_agent::tools::{
    render_lines_ansi, render_lines_plain, AbortLike, AgentTool, BashTool, FindTool, GrepTool,
    LsTool, ReadTool, ToolOutput, ToolRenderSession, WriteTool,
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
        added_tool_names: None,
    }
}

fn has_ansi(text: &str, slot: ThemeColor) -> bool {
    text.contains(theme().get_fg_ansi(slot))
}

/// Process-wide mutex that serialises the search/listing renderer tests.
///
/// `find` / `grep` / `ls` resolve relative paths against the *process* cwd and
/// reject absolute paths, so those tests `chdir` into their temp directory.
/// `set_current_dir` is process-global, hence the lock (same approach as
/// `tests/tools_navigation.rs`).
static CWD_LOCK: Mutex<()> = Mutex::new(());

/// A temp directory that is also the process cwd for the duration of a test.
struct NavTmp {
    _guard: std::sync::MutexGuard<'static, ()>,
    dir: PathBuf,
    prev_cwd: PathBuf,
}

impl NavTmp {
    fn new(label: &str) -> Self {
        let guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = fresh_tmp(&format!("nav-{label}"));
        let prev_cwd = std::env::current_dir().expect("read cwd");
        std::env::set_current_dir(&dir).expect("chdir into temp dir");
        Self {
            _guard: guard,
            dir,
            prev_cwd,
        }
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for NavTmp {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.prev_cwd);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
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

// ---------------------------------------------------------------------------
// bash / find / grep / ls renderers
// ---------------------------------------------------------------------------

/// Every built-in tool that ships a presentation layer must be reachable
/// through the session's renderer registry.
#[test]
fn renderer_registry_covers_every_renderable_tool() {
    for name in ["read", "write", "edit", "bash", "find", "grep", "ls"] {
        let renderer = pi_coding_agent::tools::renderer_for(name);
        assert!(renderer.is_some(), "no renderer registered for {name}");
        assert_eq!(renderer.unwrap().name(), name);
    }
    // An extension tool has no presentation layer.
    assert!(pi_coding_agent::tools::renderer_for("extension_tool").is_none());
}

#[tokio::test]
async fn real_bash_shows_the_tail_and_an_earlier_lines_hint() {
    let dir = fresh_tmp("bash-tail");
    let args = json!({ "command": "for i in 1 2 3 4 5 6 7 8 9 10 11 12; do echo line-$i; done" });
    let output = BashTool
        .execute(args.clone(), AbortLike::none())
        .await
        .expect("bash succeeds");

    let mut session = ToolRenderSession::new(&dir);
    let call_lines = session.call(&call("c1", "bash", args), false);
    let plain_call = render_lines_plain(&call_lines);
    assert!(
        plain_call.starts_with("bash for i in 1 2 3"),
        "{plain_call:?}"
    );

    let lines = session.result(&as_result("c1", output, false));
    let plain = render_lines_plain(&lines);

    // A collapsed shell result keeps the *last* five lines …
    assert!(plain.contains("line-12"), "{plain:?}");
    assert!(plain.contains("line-8"), "{plain:?}");
    assert!(!plain.contains("line-7"), "{plain:?}");
    // … and reports how many earlier ones were skipped.
    assert!(
        plain.contains("... (7 earlier lines, to expand)"),
        "{plain:?}"
    );
    // The duration comes from the tool's own `details.elapsed_ms`.
    assert!(plain.contains("Took "), "{plain:?}");

    // Expanded shows the whole captured output.
    let expanded_args = json!({ "command": "echo first; echo last" });
    let expanded_output = BashTool
        .execute(expanded_args.clone(), AbortLike::none())
        .await
        .expect("bash succeeds");
    let mut expanded = ToolRenderSession::new(&dir).with_expanded(true);
    expanded.call(&call("c2", "bash", expanded_args), false);
    let expanded_plain =
        render_lines_plain(&expanded.result(&as_result("c2", expanded_output, false)));
    assert!(expanded_plain.contains("first"), "{expanded_plain:?}");
    assert!(expanded_plain.contains("last"), "{expanded_plain:?}");
}

#[tokio::test]
async fn real_bash_folds_its_own_truncation_footer_into_one_warning() {
    let dir = fresh_tmp("bash-truncated");
    // Well past the 2000-line / 50KB tail limits.
    let args = json!({ "command": "for i in $(seq 1 3000); do echo padding-$i; done" });
    let output = BashTool
        .execute(args.clone(), AbortLike::none())
        .await
        .expect("bash succeeds");

    let mut session = ToolRenderSession::new(&dir);
    session.call(&call("c1", "bash", args.clone()), false);
    let plain = render_lines_plain(&session.result(&as_result("c1", output.clone(), false)));

    // The tool's raw `[Showing lines …]` footer is replaced by the renderer's
    // warning line, so the path appears exactly once.
    assert!(!plain.contains("[Showing lines"), "{plain:?}");
    assert_eq!(plain.matches("[Full output: ").count(), 1, "{plain:?}");
    assert!(
        plain.contains("Truncated: showing 2000 of 3000 lines"),
        "{plain:?}"
    );

    let mut warning_session = ToolRenderSession::new(&dir);
    warning_session.call(&call("c2", "bash", args), false);
    let ansi = render_lines_ansi(
        &warning_session.result(&as_result("c2", output, false)),
        &theme(),
    );
    assert!(has_ansi(&ansi, ThemeColor::Warning), "{ansi:?}");
}

#[tokio::test]
async fn real_find_call_and_result_render_pattern_path_and_fold() {
    let nav = NavTmp::new("find-fold");
    for n in 1..=25 {
        std::fs::write(nav.path().join(format!("file-{n:02}.txt")), "x").unwrap();
    }
    let args = json!({ "pattern": "*.txt" });
    let output = FindTool
        .execute(args.clone(), AbortLike::none())
        .await
        .expect("find succeeds");

    let mut session = ToolRenderSession::new(nav.path());
    let plain_call = render_lines_plain(&session.call(&call("c1", "find", args), false));
    assert_eq!(plain_call, "find *.txt in .");

    let plain = render_lines_plain(&session.result(&as_result("c1", output, false)));
    assert!(plain.contains("file-01.txt"), "{plain:?}");
    assert!(plain.contains("file-20.txt"), "{plain:?}");
    assert!(!plain.contains("file-21.txt"), "{plain:?}");
    assert!(plain.contains("... (5 more lines, to expand)"), "{plain:?}");
    let ansi = render_lines_ansi(
        &{
            let mut s = ToolRenderSession::new(nav.path());
            s.call(&call("c2", "find", json!({ "pattern": "*.txt" })), false);
            let out = FindTool
                .execute(json!({ "pattern": "*.txt" }), AbortLike::none())
                .await
                .expect("find succeeds");
            s.result(&as_result("c2", out, false))
        },
        &theme(),
    );
    assert!(has_ansi(&ansi, ThemeColor::ToolOutput), "{ansi:?}");
}

#[tokio::test]
async fn real_grep_call_and_result_render_slashed_pattern_and_limit() {
    let nav = NavTmp::new("grep-fold");
    // More files than the limit, so the tool reports the hit limit and the
    // renderer has both a fold and a warning to draw.
    for n in 1..=30 {
        std::fs::write(
            nav.path().join(format!("mod-{n:02}.rs")),
            format!("let hit_{n} = {n};\n"),
        )
        .unwrap();
    }
    let args = json!({ "pattern": "hit_", "include": "*.rs", "limit": 20 });
    let output = GrepTool
        .execute(args.clone(), AbortLike::none())
        .await
        .expect("grep succeeds");

    let mut session = ToolRenderSession::new(nav.path());
    let plain_call = render_lines_plain(&session.call(&call("c1", "grep", args), false));
    assert_eq!(plain_call, "grep /hit_/ in . (*.rs) limit 20");

    let lines = session.result(&as_result("c1", output, false));
    let plain = render_lines_plain(&lines);
    assert!(plain.contains("mod-01.rs"), "{plain:?}");
    assert!(!plain.contains("mod-16.rs"), "{plain:?}");
    assert!(plain.contains("more lines, to expand)"), "{plain:?}");
    // The grep tool records its hit limit so the renderer can warn about it.
    assert!(plain.contains("[Truncated: 20 matches limit]"), "{plain:?}");
    assert!(has_ansi(
        &render_lines_ansi(&lines, &theme()),
        ThemeColor::Warning
    ));
}

#[tokio::test]
async fn real_ls_call_and_result_render_entries_and_fold() {
    let nav = NavTmp::new("ls-fold");
    for n in 1..=25 {
        std::fs::write(nav.path().join(format!("entry-{n:02}.txt")), "x").unwrap();
    }
    let args = json!({ "path": "." });
    let output = LsTool
        .execute(args.clone(), AbortLike::none())
        .await
        .expect("ls succeeds");

    let mut session = ToolRenderSession::new(nav.path());
    let plain_call = render_lines_plain(&session.call(&call("c1", "ls", args), false));
    assert_eq!(plain_call, "ls .");

    let plain = render_lines_plain(&session.result(&as_result("c1", output, false)));
    assert!(plain.contains("entry-01.txt"), "{plain:?}");
    assert!(plain.contains("entry-20.txt"), "{plain:?}");
    assert!(!plain.contains("entry-21.txt"), "{plain:?}");
    assert!(plain.contains("... (5 more lines, to expand)"), "{plain:?}");
}

#[test]
fn renderers_mark_invalid_arguments_and_default_ls_to_dot() {
    let dir = fresh_tmp("render-invalid");
    let mut session = ToolRenderSession::new(&dir);

    // Missing / `null` arguments read as the empty string, matching upstream's
    // `str()` helper: `bash` shows `...`, `find` / `ls` fall back to `.`.
    assert_eq!(
        render_lines_plain(&session.call(&call("c1", "bash", json!({})), false)),
        "bash ..."
    );
    assert_eq!(
        render_lines_plain(&session.call(&call("c2", "find", json!({ "pattern": "a" })), false)),
        "find a in ."
    );
    assert_eq!(
        render_lines_plain(&session.call(&call("c3", "ls", json!({ "path": "" })), false)),
        "ls ."
    );

    // A non-string value is an invalid argument.
    for (id, name, args) in [
        ("c4", "bash", json!({ "command": 7 })),
        ("c5", "find", json!({ "pattern": true })),
        ("c6", "grep", json!({ "pattern": ["a"] })),
        ("c7", "ls", json!({ "path": {"nested": 1} })),
    ] {
        let lines = session.call(&call(id, name, args), true);
        let plain = render_lines_plain(&lines);
        assert!(plain.contains("[invalid arg]"), "{name}: {plain:?}");
        assert!(
            has_ansi(&render_lines_ansi(&lines, &theme()), ThemeColor::Error),
            "{name} should mark the invalid argument in red"
        );
    }

    // An empty result renders nothing at all.
    session.call(&call("c8", "bash", json!({ "command": "true" })), false);
    let empty = session.result(&as_result("c8", ToolOutput::text(""), false));
    assert!(empty.is_empty(), "{empty:?}");
}
