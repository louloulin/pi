//! Integration tests for the navigation tools (`find`, `grep`, `ls`).
//!
//! Each test creates a fresh temp directory so parallel runs don't
//! collide, and runs the tool against that directory. `chdir` is used
//! to put the tool's "current working directory" inside the temp dir so
//! the relative-path sandbox model behaves the way it does for real
//! agent runs.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use pi_coding_agent::tools::{AbortLike, AgentTool, FindTool, FindType, GrepTool, LsTool};
use pi_protocol::Content;
use serde_json::json;

/// Process-wide mutex that serialises navigation tests. The navigation
/// tools all `chdir` into a temp directory for the duration of the test
/// to dodge the sandbox's "no absolute paths" rule, and `set_current_dir`
/// is process-global. Without this lock, parallel tests stomp on each
/// other's working directory and the whole suite falls over.
static CWD_LOCK: Mutex<()> = Mutex::new(());

/// Create a unique temp directory under `std::env::temp_dir()` and
/// return its path. Each call gets its own directory so tests can run in
/// parallel.
fn fresh_tmp(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pi-coding-agent-nav-{}-{}-{}",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Test fixture: holds a temp directory *and* makes it the current
/// working directory for the duration of the test, so the navigation
/// tools (which reject absolute paths via their sandbox check) see
/// everything as relative. On drop the original cwd is restored and the
/// temp directory is removed.
///
/// The fixture also holds the process-wide [`CWD_LOCK`] guard so
/// concurrent navigation tests can't stomp on each other's working
/// directory.
struct Tmp {
    _guard: std::sync::MutexGuard<'static, ()>,
    dir: PathBuf,
    prev_cwd: PathBuf,
}

impl Tmp {
    fn new(label: &str) -> Self {
        let guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = fresh_tmp(label);
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

impl Drop for Tmp {
    fn drop(&mut self) {
        // Best-effort: restore cwd, then remove the temp tree.
        let _ = std::env::set_current_dir(&self.prev_cwd);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn first_text(out: &pi_coding_agent::tools::ToolOutput) -> String {
    match out.content.first().expect("content block") {
        Content::Text(t) => t.text.clone(),
        other => panic!("expected text content, got {:?}", other),
    }
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------
// find
// ---------------------------------------------------------------------

#[tokio::test]
async fn find_filters_by_extension_and_ignores_dot_git() {
    let _keep = Tmp::new("find-glob");
    let dir = _keep.path().to_path_buf();
    // Layout mirrors a small Rust project with build artefacts that
    // must be skipped.
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn hi() {}").unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
    std::fs::write(dir.join("README.md"), "# project").unwrap();

    // A .git directory with a tracked file that would otherwise match
    // the glob — must be skipped.
    std::fs::create_dir_all(dir.join(".git/objects")).unwrap();
    std::fs::write(dir.join(".git/objects/abc.rs"), "binary").unwrap();

    // A node_modules tree that should also be skipped.
    std::fs::create_dir_all(dir.join("node_modules/foo")).unwrap();
    std::fs::write(dir.join("node_modules/foo/index.js"), "module").unwrap();

    // A target directory that should be skipped too.
    std::fs::create_dir_all(dir.join("target/debug")).unwrap();
    std::fs::write(dir.join("target/debug/foo.rs"), "binary").unwrap();

    let tool = FindTool;
    let out = tool
        .execute(
            json!({
                "pattern": "**/*.rs",
                "path": ".",
            }),
            AbortLike::none(),
        )
        .await
        .expect("find should succeed");

    let text = first_text(&out);
    let entries: Vec<String> = lines(&text);

    assert!(entries.iter().any(|e| e == "src/lib.rs"), "missing src/lib.rs in: {text}");
    assert!(entries.iter().any(|e| e == "src/main.rs"), "missing src/main.rs in: {text}");
    assert!(
        !entries.iter().any(|e| e.contains(".git")),
        ".git/objects/abc.rs must be ignored: {text}"
    );
    assert!(
        !entries.iter().any(|e| e.contains("node_modules")),
        "node_modules must be ignored: {text}"
    );
    assert!(
        !entries.iter().any(|e| e.contains("target")),
        "target must be ignored: {text}"
    );
    assert!(
        !entries.iter().any(|e| e.ends_with(".toml") || e.ends_with(".md")),
        "non-.rs files must not match **/*.rs: {text}"
    );

    // Results are sorted lexicographically.
    let mut sorted = entries.clone();
    sorted.sort();
    assert_eq!(entries, sorted, "results must be sorted");
}

#[tokio::test]
async fn find_rejects_absolute_paths() {
    let tool = FindTool;
    let err = tool
        .execute(
            json!({
                "pattern": "*",
                "path": "/etc",
            }),
            AbortLike::none(),
        )
        .await
        .expect_err("absolute path must be a sandbox violation");
    assert!(matches!(
        err,
        pi_coding_agent::tools::ToolError::SandboxViolation(_)
    ));
}

#[tokio::test]
async fn find_rejects_parent_traversal() {
    let tool = FindTool;
    let err = tool
        .execute(
            json!({
                "pattern": "*",
                "path": "../outside",
            }),
            AbortLike::none(),
        )
        .await
        .expect_err("parent traversal must be a sandbox violation");
    assert!(matches!(
        err,
        pi_coding_agent::tools::ToolError::SandboxViolation(_)
    ));
}

#[tokio::test]
async fn find_limit_and_offset() {
    let _keep = Tmp::new("find-limit");
    let dir = _keep.path().to_path_buf();
    // 100 files, named `f000.rs` ... `f099.rs` so lexicographic sort
    // matches numeric order.
    for i in 0..100 {
        let name = format!("f{:03}.rs", i);
        std::fs::write(dir.join(&name), "").unwrap();
    }
    // A sentinel we should *not* see (offset should skip past it).
    std::fs::write(dir.join("zzz_sentinel.rs"), "").unwrap();

    let tool = FindTool;
    let out = tool
        .execute(
            json!({
                "pattern": "*.rs",
                "path": ".",
                "limit": 10,
                "offset": 20,
            }),
            AbortLike::none(),
        )
        .await
        .expect("find should succeed");

    let text = first_text(&out);
    let entries = lines(&text);

    // Filter out the limit-reached footer so we only count file entries.
    let files: Vec<&String> = entries
        .iter()
        .filter(|e| !e.starts_with('[') && !e.is_empty())
        .collect();
    assert_eq!(files.len(), 10, "limit=10 should yield 10 entries: {text}");
    // We skipped the first 20 in lex order. f000..f019 are gone; f020..f029 remain.
    for (i, entry) in files.iter().enumerate() {
        let expected = format!("f{:03}.rs", 20 + i);
        assert_eq!(*entry, &expected, "wrong entry at position {i}: {text}");
    }
    assert!(
        !entries.iter().any(|e| e == "zzz_sentinel.rs"),
        "offset should have skipped the sentinel: {text}"
    );
}

#[tokio::test]
async fn find_type_directory() {
    let _keep = Tmp::new("find-type");
    let dir = _keep.path().to_path_buf();
    std::fs::create_dir_all(dir.join("a/b/c")).unwrap();
    std::fs::create_dir_all(dir.join("a/x")).unwrap();
    std::fs::write(dir.join("a/b/c/file.rs"), "").unwrap();

    let tool = FindTool;
    let out = tool
        .execute(
            json!({
                "pattern": "**",
                "path": ".",
                "type": "directory",
            }),
            AbortLike::none(),
        )
        .await
        .expect("find should succeed");

    let entries = lines(&first_text(&out));
    assert!(
        entries.iter().any(|e| e.ends_with("a/")),
        "missing a/: {entries:?}"
    );
    assert!(
        entries.iter().all(|e| e.ends_with('/')),
        "every entry must end with '/': {entries:?}"
    );
    assert!(
        !entries.iter().any(|e| e.ends_with(".rs")),
        "no files must match type=directory: {entries:?}"
    );
}

#[tokio::test]
async fn find_unknown_path_is_an_error() {
    let tool = FindTool;
    let err = tool
        .execute(
            json!({
                "pattern": "*",
                "path": "no/such/dir",
            }),
            AbortLike::none(),
        )
        .await
        .expect_err("unknown path must error");
    // The error path bubbles up via `ToolError::Execution`, not
    // SandboxViolation.
    assert!(matches!(
        err,
        pi_coding_agent::tools::ToolError::Execution(_)
    ));
}

#[tokio::test]
async fn find_type_enum_value_round_trip() {
    // Sanity check that the `type` parameter accepts the same wire
    // strings as the upstream TS tool.
    assert_eq!(
        serde_json::to_value(FindType::File).unwrap(),
        json!("file")
    );
    assert_eq!(
        serde_json::to_value(FindType::Directory).unwrap(),
        json!("directory")
    );
    assert_eq!(
        serde_json::to_value(FindType::Any).unwrap(),
        json!("any")
    );
}

// ---------------------------------------------------------------------
// grep
// ---------------------------------------------------------------------

#[tokio::test]
async fn grep_finds_anchor_in_cargo_toml() {
    let _keep = Tmp::new("grep-anchor");
    let dir = _keep.path().to_path_buf();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let tool = GrepTool;
    let out = tool
        .execute(
            json!({
                "pattern": "^name",
                "path": "Cargo.toml",
            }),
            AbortLike::none(),
        )
        .await
        .expect("grep should succeed");

    let text = first_text(&out);
    assert!(
        text.contains("Cargo.toml:2:name = \"demo\""),
        "missing match: {text}"
    );
}

#[tokio::test]
async fn grep_context_includes_neighbours() {
    let _keep = Tmp::new("grep-context");
    let dir = _keep.path().to_path_buf();
    let body = "line1\nline2\nline3\nline4\nline5\n";
    std::fs::write(dir.join("note.txt"), body).unwrap();

    let tool = GrepTool;
    let out = tool
        .execute(
            json!({
                "pattern": "line3",
                "path": "note.txt",
                "context": 1,
            }),
            AbortLike::none(),
        )
        .await
        .expect("grep should succeed");

    let text = first_text(&out);
    let entries = lines(&text);

    // Expect 3 lines: line2 (context), line3 (match), line4 (context).
    assert_eq!(entries.len(), 3, "expected 3 lines, got: {text}");
    assert!(
        entries[0].ends_with("-2-line2"),
        "missing line2 context: {text}"
    );
    assert!(
        entries[1].ends_with(":3:line3"),
        "missing match line: {text}"
    );
    assert!(
        entries[2].ends_with("-4-line4"),
        "missing line4 context: {text}"
    );
}

#[tokio::test]
async fn grep_skips_binary_files() {
    let _keep = Tmp::new("grep-binary");
    let dir = _keep.path().to_path_buf();
    // Plain text file with one match.
    std::fs::write(dir.join("ok.txt"), "alpha\nfoo bar\nbeta\n").unwrap();
    // Binary file with a NUL byte in the first 8 KiB.
    std::fs::write(dir.join("binary.bin"), b"\x00foo bar\x00").unwrap();

    let tool = GrepTool;
    let out = tool
        .execute(
            json!({
                "pattern": "foo",
                "path": ".",
            }),
            AbortLike::none(),
        )
        .await
        .expect("grep should succeed");

    let text = first_text(&out);
    assert!(
        text.contains("ok.txt:"),
        "expected ok.txt match: {text}"
    );
    assert!(
        !text.contains("binary.bin"),
        "binary.bin must be skipped: {text}"
    );
}

#[tokio::test]
async fn grep_ignore_case_toggle() {
    let _keep = Tmp::new("grep-case");
    let dir = _keep.path().to_path_buf();
    std::fs::write(dir.join("a.txt"), "Foo\nbar\nFOO\n").unwrap();

    let tool = GrepTool;
    let out_sensitive = tool
        .execute(
            json!({ "pattern": "foo", "path": "a.txt" }),
            AbortLike::none(),
        )
        .await
        .expect("grep should succeed");
    let sensitive = first_text(&out_sensitive);
    // Case-sensitive only matches "Foo" (line 1).
    assert_eq!(lines(&sensitive).len(), 1, "expected 1 match: {sensitive}");

    let out_insensitive = tool
        .execute(
            json!({
                "pattern": "foo",
                "path": "a.txt",
                "ignore_case": true,
            }),
            AbortLike::none(),
        )
        .await
        .expect("grep should succeed");
    let insensitive = first_text(&out_insensitive);
    // Case-insensitive matches both "Foo" and "FOO".
    assert_eq!(lines(&insensitive).len(), 2, "expected 2 matches: {insensitive}");
}

#[tokio::test]
async fn grep_include_glob() {
    let _keep = Tmp::new("grep-include");
    let dir = _keep.path().to_path_buf();
    std::fs::write(dir.join("a.rs"), "fn foo() {}\n").unwrap();
    std::fs::write(dir.join("a.txt"), "foo\n").unwrap();

    let tool = GrepTool;
    let out = tool
        .execute(
            json!({
                "pattern": "foo",
                "path": ".",
                "include": "*.rs",
            }),
            AbortLike::none(),
        )
        .await
        .expect("grep should succeed");

    let text = first_text(&out);
    assert!(text.contains("a.rs"), "a.rs must match: {text}");
    assert!(!text.contains("a.txt"), "a.txt must be excluded: {text}");
}

#[tokio::test]
async fn grep_limit_and_offset() {
    let _keep = Tmp::new("grep-limit");
    let dir = _keep.path().to_path_buf();
    let body: String = (1..=50)
        .fold(String::new(), |mut acc, i| {
            use std::fmt::Write;
            let _ = writeln!(acc, "match line {}", i);
            acc
        });
    std::fs::write(dir.join("multi.txt"), body).unwrap();

    let tool = GrepTool;
    let out = tool
        .execute(
            json!({
                "pattern": "match",
                "path": "multi.txt",
                "limit": 5,
                "offset": 10,
            }),
            AbortLike::none(),
        )
        .await
        .expect("grep should succeed");

    let entries = lines(&first_text(&out));
    // Filter out the limit-reached footer.
    let matches: Vec<&String> = entries
        .iter()
        .filter(|e| !e.starts_with('[') && !e.is_empty())
        .collect();
    assert_eq!(matches.len(), 5, "limit=5 should give 5 matches: {entries:?}");
    // Skip the first 10, so lines 11..15 should remain. The grep tool
    // renders matches as `path:line:content` (no space), matching
    // ripgrep's presentation.
    for (i, entry) in matches.iter().enumerate() {
        let expected = format!("multi.txt:{}:match line {}", 11 + i, 11 + i);
        assert_eq!(*entry, &expected, "wrong entry {i}");
    }
}

#[tokio::test]
async fn grep_invalid_regex_is_invalid_argument() {
    let tool = GrepTool;
    let err = tool
        .execute(
            json!({ "pattern": "(unclosed" }),
            AbortLike::none(),
        )
        .await
        .expect_err("invalid regex should fail");
    assert!(matches!(
        err,
        pi_coding_agent::tools::ToolError::InvalidArgument(_)
    ));
}

// ---------------------------------------------------------------------
// ls
// ---------------------------------------------------------------------

#[tokio::test]
async fn ls_hides_dotfiles_by_default() {
    let _keep = Tmp::new("ls-hide");
    let dir = _keep.path().to_path_buf();
    std::fs::write(dir.join("alpha.txt"), "a").unwrap();
    std::fs::write(dir.join("beta.txt"), "b").unwrap();
    std::fs::write(dir.join("gamma.txt"), "c").unwrap();
    std::fs::write(dir.join(".hidden"), "h").unwrap();

    let tool = LsTool;
    let out = tool
        .execute(
            json!({ "path": "." }),
            AbortLike::none(),
        )
        .await
        .expect("ls should succeed");

    let entries = lines(&first_text(&out));
    assert_eq!(entries.len(), 3, "hidden file should be excluded: {entries:?}");
    assert!(entries.iter().any(|e| e == "alpha.txt"));
    assert!(entries.iter().any(|e| e == "beta.txt"));
    assert!(entries.iter().any(|e| e == "gamma.txt"));
    assert!(!entries.iter().any(|e| e == ".hidden"));
}

#[tokio::test]
async fn ls_all_includes_dotfiles() {
    let _keep = Tmp::new("ls-all");
    let dir = _keep.path().to_path_buf();
    std::fs::write(dir.join("alpha.txt"), "a").unwrap();
    std::fs::write(dir.join(".hidden"), "h").unwrap();

    let tool = LsTool;
    let out = tool
        .execute(
            json!({
                "path": ".",
                "all": true,
            }),
            AbortLike::none(),
        )
        .await
        .expect("ls should succeed");

    let entries = lines(&first_text(&out));
    assert_eq!(entries.len(), 2, "all=true should expose dotfiles: {entries:?}");
    assert!(entries.iter().any(|e| e == ".hidden"));
    assert!(entries.iter().any(|e| e == "alpha.txt"));
}

#[tokio::test]
async fn ls_detail_prints_size_column() {
    let _keep = Tmp::new("ls-detail");
    let dir = _keep.path().to_path_buf();
    std::fs::write(dir.join("alpha.txt"), "hello").unwrap();
    std::fs::create_dir(dir.join("subdir")).unwrap();

    let tool = LsTool;
    let out = tool
        .execute(
            json!({
                "path": ".",
                "detail": true,
            }),
            AbortLike::none(),
        )
        .await
        .expect("ls should succeed");

    let text = first_text(&out);
    let entries = lines(&text);

    // Directory first.
    assert!(
        entries[0].starts_with('d') && entries[0].contains("subdir"),
        "directory should be first with `d` prefix: {}",
        entries[0]
    );
    // Then file with size column.
    let file_line = entries
        .iter()
        .find(|l| l.contains("alpha.txt"))
        .expect("alpha.txt line");
    assert!(
        file_line.starts_with('-') && file_line.contains("alpha.txt"),
        "file line should start with '-' and contain name: {file_line}"
    );
    // Sanity: size column should contain digits.
    let mut parts = file_line.split_whitespace();
    let type_char = parts.next().expect("type");
    let size = parts.next().expect("size");
    assert_eq!(type_char, "-");
    assert!(
        size.chars().all(|c| c.is_ascii_digit()),
        "size should be a digit string: {file_line}"
    );
}

#[tokio::test]
async fn ls_directories_sort_before_files() {
    let _keep = Tmp::new("ls-sort");
    let dir = _keep.path().to_path_buf();
    std::fs::write(dir.join("a.txt"), "").unwrap();
    std::fs::write(dir.join("b.txt"), "").unwrap();
    std::fs::create_dir(dir.join("zdir")).unwrap();
    std::fs::create_dir(dir.join("adir")).unwrap();

    let tool = LsTool;
    let out = tool
        .execute(
            json!({ "path": "." }),
            AbortLike::none(),
        )
        .await
        .expect("ls should succeed");

    let entries = lines(&first_text(&out));
    // The two directories must come first, then the files.
    assert!(
        entries[0].as_str() == "adir" && entries[1].as_str() == "zdir",
        "directories should sort first alphabetically: {entries:?}"
    );
    assert!(
        entries[2].as_str() == "a.txt" && entries[3].as_str() == "b.txt",
        "files should follow alphabetically: {entries:?}"
    );
}

#[tokio::test]
async fn ls_rejects_absolute_path() {
    let tool = LsTool;
    let err = tool
        .execute(json!({ "path": "/etc" }), AbortLike::none())
        .await
        .expect_err("absolute path must be rejected");
    assert!(matches!(
        err,
        pi_coding_agent::tools::ToolError::SandboxViolation(_)
    ));
}

#[tokio::test]
async fn ls_rejects_parent_traversal() {
    let tool = LsTool;
    let err = tool
        .execute(json!({ "path": "../outside" }), AbortLike::none())
        .await
        .expect_err("parent traversal must be rejected");
    assert!(matches!(
        err,
        pi_coding_agent::tools::ToolError::SandboxViolation(_)
    ));
}

#[tokio::test]
async fn ls_errors_on_file_not_directory() {
    let _keep = Tmp::new("ls-notdir");
    let dir = _keep.path().to_path_buf();
    let file = dir.join("file.txt");
    std::fs::write(&file, "").unwrap();

    let tool = LsTool;
    let err = tool
        .execute(json!({ "path": "file.txt" }), AbortLike::none())
        .await
        .expect_err("ls on a file must fail");
    assert!(matches!(
        err,
        pi_coding_agent::tools::ToolError::Execution(_)
    ));
}

#[tokio::test]
async fn ls_rejects_absolute_path_for_grep_and_find() {
    // Grep and find share the same sandbox rules as ls.
    let tool = GrepTool;
    let err = tool
        .execute(json!({ "pattern": "x", "path": "/etc" }), AbortLike::none())
        .await
        .expect_err("absolute path must be rejected");
    assert!(matches!(
        err,
        pi_coding_agent::tools::ToolError::SandboxViolation(_)
    ));
}

#[tokio::test]
async fn ls_path_root_normalises() {
    // Touching the helper too — `Path::new("")` should behave like `.`.
    let _ = Path::new("");
}

#[test]
fn default_tool_bundle_lists_seven_tools() {
    let bundle = pi_coding_agent::tools::default_tool_bundle();
    let names: Vec<&str> = bundle.iter().map(|t| t.name()).collect();
    assert_eq!(
        names,
        vec!["read", "write", "edit", "bash", "find", "grep", "ls"],
        "default_tool_bundle() must return the seven tools in fixed order"
    );

    // Each tool's `name` and `label` are 1:1 with the upstream TS port.
    for t in &bundle {
        assert_eq!(t.name(), t.label(), "{} label should equal name", t.name());
        assert!(!t.description().is_empty());
    }
}

// ---------------------------------------------------------------------------
// Output truncation (`core/tools/truncate.ts`)
//
// `find` / `grep` / `ls` all byte-limit their rendered block at 50KB and the
// grep tool additionally cuts long match lines at 500 chars. Both paths add an
// actionable notice so the model knows the output is incomplete.
// ---------------------------------------------------------------------------

/// A file name padded to ~50 bytes so a directory listing crosses the 50KB
/// byte limit without needing tens of thousands of entries.
fn padded_name(i: usize) -> String {
    let name = format!("entry-{:05}-{}", i, "p".repeat(40));
    assert!(name.len() <= 255, "name must fit a filesystem entry");
    name
}

#[tokio::test]
async fn grep_truncates_long_match_lines() {
    let keep = Tmp::new("grep-long-lines");
    let dir = keep.path().to_path_buf();
    std::fs::write(
        dir.join("long.txt"),
        format!("needle {}\n", "x".repeat(600)),
    )
    .unwrap();

    let out = GrepTool
        .execute(
            json!({ "pattern": "needle", "path": "long.txt" }),
            AbortLike::none(),
        )
        .await
        .expect("grep should succeed");
    let text = first_text(&out);

    assert!(text.contains("... [truncated]"), "missing marker: {text}");
    assert!(
        text.contains("[Some lines truncated to 500 chars. Use read tool to see full lines]"),
        "missing truncation notice: {text}"
    );
    // The rendered match line keeps exactly 500 characters before the marker.
    let first_line = text.lines().next().expect("a match line");
    assert!(first_line.ends_with("... [truncated]"));
    assert_eq!(
        first_line.len(),
        "long.txt:1:".len() + 500 + "... [truncated]".len()
    );
}

#[tokio::test]
async fn grep_truncates_the_result_block_at_the_byte_limit() {
    let keep = Tmp::new("grep-byte-limit");
    let dir = keep.path().to_path_buf();
    let body = (1..=300)
        .map(|i| format!("needle {} {}", i, "y".repeat(600)))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.join("many.txt"), body).unwrap();

    let out = GrepTool
        .execute(
            json!({ "pattern": "needle", "path": "many.txt", "limit": 300 }),
            AbortLike::none(),
        )
        .await
        .expect("grep should succeed");
    let text = first_text(&out);

    assert!(
        text.contains("50.0KB limit reached"),
        "missing notice: {text}"
    );
    let details = out.details.expect("details");
    assert_eq!(details["truncation"]["truncated"], true);
    assert_eq!(details["truncation"]["truncated_by"], "bytes");
    assert!(details["truncation"]["output_bytes"].as_u64().unwrap() <= 50 * 1024);
}

#[tokio::test]
async fn find_truncates_the_result_block_at_the_byte_limit() {
    let keep = Tmp::new("find-byte-limit");
    let dir = keep.path().to_path_buf();
    for i in 0..1300 {
        std::fs::write(dir.join(padded_name(i)), b"").unwrap();
    }

    let out = FindTool
        .execute(
            json!({ "pattern": "entry-*", "path": ".", "limit": 5000 }),
            AbortLike::none(),
        )
        .await
        .expect("find should succeed");
    let text = first_text(&out);

    assert!(
        text.contains("50.0KB limit reached"),
        "missing notice: {text}"
    );
    assert!(
        !text.contains("results limit reached"),
        "limit was not hit: {text}"
    );
    let details = out.details.expect("details");
    assert_eq!(details["truncation"]["truncated_by"], "bytes");
    assert!(details["truncation"]["output_bytes"].as_u64().unwrap() <= 50 * 1024);
}

#[tokio::test]
async fn ls_truncates_the_listing_at_the_byte_limit() {
    let keep = Tmp::new("ls-byte-limit");
    let dir = keep.path().to_path_buf();
    for i in 0..1300 {
        std::fs::write(dir.join(padded_name(i)), b"").unwrap();
    }

    let out = LsTool
        .execute(json!({ "path": ".", "all": true }), AbortLike::none())
        .await
        .expect("ls should succeed");
    let text = first_text(&out);

    assert!(
        text.contains("50.0KB limit reached"),
        "missing notice: {text}"
    );
    let details = out.details.expect("details");
    assert_eq!(details["truncation"]["truncated_by"], "bytes");
}

#[tokio::test]
async fn ls_does_not_load_details_for_a_short_listing() {
    let keep = Tmp::new("ls-short");
    let dir = keep.path().to_path_buf();
    std::fs::write(dir.join("only.txt"), b"").unwrap();

    let out = LsTool
        .execute(json!({ "path": "." }), AbortLike::none())
        .await
        .expect("ls should succeed");

    assert_eq!(first_text(&out), "only.txt");
    assert!(
        out.details.is_none(),
        "no details expected: {:?}",
        out.details
    );
}
