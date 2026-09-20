//! Integration tests for the built-in tool bundle.
//!
//! Each test creates a unique temp directory so parallel test runs do not
//! collide. The `ReadTool` / `WriteTool` round-trip lives in this file so
//! the write/read contract is covered by a single observable behaviour.

use std::path::{Path, PathBuf};

use pi_coding_agent::tools::{
    standard_tools, AbortLike, AgentTool, BashTool, EditTool, EditToolDetails, ReadTool, WriteTool,
};
use pi_protocol::Content;
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
    assert!(
        msg.contains("oops"),
        "stderr should surface in error: {msg}"
    );
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

/// The 24-byte PNG header the `pi-tui` fixtures use (320x240).
const PNG_HEADER: [u8; 24] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x01, 0x40, 0x00, 0x00, 0x00, 0xf0,
];

#[tokio::test]
async fn read_image_file_returns_a_text_note_and_an_image_block() {
    let dir = fresh_tmp("read-image");
    let path = dir.join("shot.png");
    std::fs::write(&path, PNG_HEADER).unwrap();

    let output = ReadTool
        .execute(
            json!({ "path": path.display().to_string() }),
            AbortLike::none(),
        )
        .await
        .expect("read image");

    // Upstream order: the note first, the image second.
    assert_eq!(output.content.len(), 2, "{:?}", output.content);
    assert_eq!(first_text(&output), "Read image file [image/png]");
    match &output.content[1] {
        Content::Image(image) => {
            assert_eq!(image.mime_type, "image/png");
            assert_eq!(image.data, "iVBORw0KGgoAAAANSUhEUgAAAUAAAADw");
        }
        other => panic!("expected an image block, got {other:?}"),
    }
}

#[tokio::test]
async fn read_bmp_without_a_converter_is_reported_as_omitted() {
    let dir = fresh_tmp("read-bmp");
    let path = dir.join("shot.bmp");
    // A minimal 1x1 single-plane 24-bit BMP header (upstream `isBmp`).
    let mut bmp = vec![0u8; 58];
    bmp[0] = b'B';
    bmp[1] = b'M';
    bmp[2..6].copy_from_slice(&58u32.to_le_bytes()); // declared file size
    bmp[10..14].copy_from_slice(&54u32.to_le_bytes()); // pixel data offset
    bmp[14..18].copy_from_slice(&40u32.to_le_bytes()); // DIB header size
    bmp[26..28].copy_from_slice(&1u16.to_le_bytes()); // colour planes
    bmp[28..30].copy_from_slice(&24u16.to_le_bytes()); // bits per pixel
    std::fs::write(&path, bmp).unwrap();

    let output = ReadTool
        .execute(
            json!({ "path": path.display().to_string() }),
            AbortLike::none(),
        )
        .await
        .expect("read bmp");

    assert_eq!(output.content.len(), 1, "no image block without a converter");
    assert_eq!(
        first_text(&output),
        "Read image file [image/bmp]\n[Image omitted: could not be converted to a supported inline image format.]"
    );
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
async fn edit_replaces_single_occurrence_with_edits() {
    let dir = fresh_tmp("edit-single");
    let path = dir.join("note.txt");
    std::fs::write(&path, "hello world\nfoo bar\n").unwrap();

    let out = EditTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "edits": [{ "oldText": "foo bar", "newText": "foo baz" }],
            }),
            AbortLike::none(),
        )
        .await
        .expect("single edit");

    assert!(first_text(&out).contains("Successfully replaced 1 block(s)"));

    let details: EditToolDetails =
        serde_json::from_value(out.details.expect("details")).expect("decode");
    assert!(details.diff.contains("-2 foo bar"), "{}", details.diff);
    assert!(details.diff.contains("+2 foo baz"), "{}", details.diff);
    assert_eq!(details.first_changed_line, Some(2));
    assert!(details.patch.contains("--- "), "{}", details.patch);
    assert!(details.patch.contains("+++ "), "{}", details.patch);
    assert!(details.patch.contains("@@"), "{}", details.patch);
    assert!(details.patch.contains("-foo bar"), "{}", details.patch);
    assert!(details.patch.contains("+foo baz"), "{}", details.patch);
    assert_eq!(apply_patch_body(&details.patch), "hello world\nfoo baz\n");

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "hello world\nfoo baz\n"
    );
}

/// Rebuild the new file from a unified patch body (context + added lines).
/// Mirrors what jsdiff's `applyPatch` validates in the upstream tests.
fn apply_patch_body(patch: &str) -> String {
    let mut rebuilt = String::new();
    for line in patch.lines() {
        if line.starts_with("@@")
            || line.starts_with("---")
            || line.starts_with("+++")
            || line.starts_with("===")
            || line == "\\ No newline at end of file"
        {
            continue;
        }
        let (prefix, text) = line.split_at(1);
        match prefix {
            " " | "+" => {
                rebuilt.push_str(text);
                rebuilt.push('\n');
            }
            _ => {}
        }
    }
    rebuilt
}

#[tokio::test]
async fn edit_accepts_legacy_single_edit_form() {
    let dir = fresh_tmp("edit-legacy");
    let path = dir.join("note.txt");
    std::fs::write(&path, "hello world\nfoo bar\n").unwrap();

    EditTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "old_text": "foo bar",
                "new_text": "foo baz",
            }),
            AbortLike::none(),
        )
        .await
        .expect("legacy single edit");

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "hello world\nfoo baz\n"
    );
}

#[tokio::test]
async fn edit_accepts_stringified_edits() {
    let dir = fresh_tmp("edit-stringified");
    let path = dir.join("note.txt");
    std::fs::write(&path, "alpha\n").unwrap();

    EditTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "edits": "[{\"oldText\":\"alpha\",\"newText\":\"beta\"}]",
            }),
            AbortLike::none(),
        )
        .await
        .expect("stringified edits");

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "beta\n");
}

#[tokio::test]
async fn edit_replace_all_legacy() {
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
    assert!(
        details.diff.contains("+1 ZZZ bbb ZZZ ccc ZZZ"),
        "{}",
        details.diff
    );

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "ZZZ bbb ZZZ ccc ZZZ\n"
    );
}

#[tokio::test]
async fn edit_replaces_multiple_disjoint_regions_in_one_call() {
    let dir = fresh_tmp("edit-multi");
    let path = dir.join("multi.txt");
    std::fs::write(&path, "alpha\nbeta\ngamma\ndelta\n").unwrap();

    let out = EditTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "edits": [
                    { "oldText": "alpha\n", "newText": "ALPHA\n" },
                    { "oldText": "gamma\n", "newText": "GAMMA\n" },
                ],
            }),
            AbortLike::none(),
        )
        .await
        .expect("multi edit");

    assert!(first_text(&out).contains("Successfully replaced 2 block(s)"));
    let details: EditToolDetails =
        serde_json::from_value(out.details.expect("details")).expect("decode");
    assert!(details.diff.contains("ALPHA"), "{}", details.diff);
    assert!(details.diff.contains("GAMMA"), "{}", details.diff);

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "ALPHA\nbeta\nGAMMA\ndelta\n"
    );
}

#[tokio::test]
async fn edit_matches_edits_against_the_original_file() {
    let dir = fresh_tmp("edit-multi-original");
    let path = dir.join("multi.txt");
    std::fs::write(&path, "foo\nbar\nbaz\n").unwrap();

    EditTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "edits": [
                    { "oldText": "foo\n", "newText": "foo bar\n" },
                    { "oldText": "bar\n", "newText": "BAR\n" },
                ],
            }),
            AbortLike::none(),
        )
        .await
        .expect("edits are matched against the original content");

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "foo bar\nBAR\nbaz\n"
    );
}

#[tokio::test]
async fn edit_collapses_large_unchanged_gaps() {
    let dir = fresh_tmp("edit-large-gap");
    let path = dir.join("long.txt");
    let lines: Vec<String> = (1..=600).map(|i| format!("line {i:03}")).collect();
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

    let out = EditTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "edits": [
                    { "oldText": "line 100\n", "newText": "LINE 100\n" },
                    { "oldText": "line 300\n", "newText": "LINE 300\n" },
                    { "oldText": "line 500\n", "newText": "LINE 500\n" },
                ],
            }),
            AbortLike::none(),
        )
        .await
        .expect("large-gap edit");

    let details: EditToolDetails =
        serde_json::from_value(out.details.expect("details")).expect("decode");
    assert!(details.diff.contains("LINE 100"));
    assert!(details.diff.contains("LINE 300"));
    assert!(details.diff.contains("LINE 500"));
    assert!(details.diff.contains("..."));
    assert!(!details.diff.contains("line 250"));
    assert!(details.diff.split('\n').count() < 50, "{}", details.diff);
}

#[tokio::test]
async fn edit_errors_on_missing_or_ambiguous_matches() {
    let dir = fresh_tmp("edit-strict");
    let path = dir.join("dup.txt");
    std::fs::write(&path, "x x x\n").unwrap();

    let err = EditTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "edits": [{ "oldText": "nope", "newText": "yes" }],
            }),
            AbortLike::none(),
        )
        .await
        .expect_err("zero matches should error");
    assert!(
        err.to_string().contains("Could not find the exact text"),
        "got: {err}"
    );

    let err = EditTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "edits": [{ "oldText": "x", "newText": "y" }],
            }),
            AbortLike::none(),
        )
        .await
        .expect_err("multiple matches should error");
    assert!(
        err.to_string().contains("Found 3 occurrences"),
        "got: {err}"
    );

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "x x x\n");
}

#[tokio::test]
async fn edit_rejects_empty_edits_and_overlapping_regions() {
    let dir = fresh_tmp("edit-invalid");
    let path = dir.join("file.txt");
    std::fs::write(&path, "one\ntwo\nthree\n").unwrap();

    let err = EditTool
        .execute(
            json!({ "path": path.display().to_string(), "edits": [] }),
            AbortLike::none(),
        )
        .await
        .expect_err("empty edits should error");
    assert!(
        err.to_string()
            .contains("edits must contain at least one replacement"),
        "got: {err}"
    );

    let err = EditTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "edits": [
                    { "oldText": "one\ntwo\n", "newText": "ONE\nTWO\n" },
                    { "oldText": "two\nthree\n", "newText": "TWO\nTHREE\n" },
                ],
            }),
            AbortLike::none(),
        )
        .await
        .expect_err("overlapping edits should error");
    assert!(err.to_string().contains("overlap"), "got: {err}");

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\nthree\n");
}

#[tokio::test]
async fn edit_does_not_partially_apply_when_one_edit_fails() {
    let dir = fresh_tmp("edit-no-partial");
    let path = dir.join("file.txt");
    std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();

    let err = EditTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "edits": [
                    { "oldText": "alpha\n", "newText": "ALPHA\n" },
                    { "oldText": "missing\n", "newText": "MISSING\n" },
                ],
            }),
            AbortLike::none(),
        )
        .await
        .expect_err("a failing edit aborts the whole call");
    assert!(err.to_string().contains("Could not find"), "got: {err}");

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "alpha\nbeta\ngamma\n"
    );
}

#[tokio::test]
async fn edit_reports_missing_files() {
    let dir = fresh_tmp("edit-missing");
    let path = dir.join("missing.txt");

    let err = EditTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "edits": [{ "oldText": "hello", "newText": "world" }],
            }),
            AbortLike::none(),
        )
        .await
        .expect_err("missing file should error");
    assert!(
        err.to_string().starts_with("Could not edit file:"),
        "got: {err}"
    );
}

#[tokio::test]
async fn edit_preserves_bom_and_crlf_line_endings() {
    let dir = fresh_tmp("edit-endings");
    let path = dir.join("win.txt");
    std::fs::write(&path, "\u{feff}alpha\r\nbeta\r\n").unwrap();

    EditTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "edits": [{ "oldText": "beta", "newText": "BETA" }],
            }),
            AbortLike::none(),
        )
        .await
        .expect("crlf edit");

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "\u{feff}alpha\r\nBETA\r\n"
    );
}

#[tokio::test]
async fn edit_fuzzy_matches_smart_quotes() {
    let dir = fresh_tmp("edit-fuzzy");
    let path = dir.join("fuzzy.txt");
    std::fs::write(&path, "const x = \u{201c}hi\u{201d};\nkeep  \n").unwrap();

    EditTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "edits": [{ "oldText": "const x = \"hi\";", "newText": "const x = \"bye\";" }],
            }),
            AbortLike::none(),
        )
        .await
        .expect("fuzzy edit");

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "const x = \"bye\";\nkeep  \n"
    );
}

#[tokio::test]
async fn abort_like_short_circuits_tools() {
    let tool = BashTool;
    let err = tool
        .execute(json!({ "command": "echo hello" }), AbortLike::cancelled())
        .await
        .expect_err("pre-cancelled handle should reject the call");
    assert!(matches!(err, pi_coding_agent::tools::ToolError::Aborted));
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

// ---------------------------------------------------------------------------
// `read` tool: offset / limit / truncation
//
// Ported from `packages/coding-agent/test/tools.test.ts`
// ("read tool" describe block). The notices are asserted verbatim because the
// model relies on the `Use offset=N to continue.` phrasing to page through a
// file.
// ---------------------------------------------------------------------------

/// Write `n` lines `Line 1` … `Line n` (no trailing newline) to `name`.
fn write_numbered_lines(dir: &Path, name: &str, n: usize) -> PathBuf {
    let path = dir.join(name);
    let body = (1..=n)
        .map(|i| format!("Line {}", i))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, body).expect("write fixture");
    path
}

#[tokio::test]
async fn read_leaves_files_within_limits_untouched() {
    let dir = fresh_tmp("read-small");
    let path = dir.join("test.txt");
    let content = "Hello, world!\nLine 2\nLine 3";
    std::fs::write(&path, content).unwrap();

    let out = ReadTool
        .execute(
            json!({ "path": path.display().to_string() }),
            AbortLike::none(),
        )
        .await
        .expect("read");

    assert_eq!(first_text(&out), content);
    assert!(
        !first_text(&out).contains("Use offset="),
        "a file that fits must not carry a continuation notice"
    );
    assert!(
        out.details.is_none(),
        "details are only set when truncation happened, got {:?}",
        out.details
    );
}

#[tokio::test]
async fn read_truncates_files_exceeding_the_line_limit() {
    let dir = fresh_tmp("read-lines");
    let path = write_numbered_lines(&dir, "large.txt", 2500);

    let out = ReadTool
        .execute(
            json!({ "path": path.display().to_string() }),
            AbortLike::none(),
        )
        .await
        .expect("read");
    let text = first_text(&out);

    assert!(text.contains("Line 1"));
    assert!(text.contains("Line 2000"));
    assert!(!text.contains("Line 2001"));
    assert!(text.contains("[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]"));

    let details = out.details.expect("details");
    assert_eq!(details["truncation"]["truncated"], true);
    assert_eq!(details["truncation"]["truncated_by"], "lines");
    assert_eq!(details["truncation"]["total_lines"], 2500);
    assert_eq!(details["truncation"]["output_lines"], 2000);
}

#[tokio::test]
async fn read_truncates_when_the_byte_limit_is_hit() {
    let dir = fresh_tmp("read-bytes");
    let path = dir.join("large-bytes.txt");
    // Fewer than 2000 lines, but well over 50KB.
    let body = (1..=500)
        .map(|i| format!("Line {}: {}", i, "x".repeat(200)))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, body).unwrap();

    let out = ReadTool
        .execute(
            json!({ "path": path.display().to_string() }),
            AbortLike::none(),
        )
        .await
        .expect("read");
    let text = first_text(&out);

    assert!(text.contains("Line 1:"));
    assert!(
        text.contains("(50.0KB limit). Use offset="),
        "expected the byte-limit notice, got:\n{text}"
    );
    assert!(text.contains(" to continue.]"));
    let details = out.details.expect("details");
    assert_eq!(details["truncation"]["truncated_by"], "bytes");
    assert_eq!(details["truncation"]["total_lines"], 500);
    assert_eq!(
        details["truncation"]["output_bytes"],
        details["truncation"]["content"].as_str().unwrap().len()
    );
}

#[tokio::test]
async fn read_honours_the_offset_parameter() {
    let dir = fresh_tmp("read-offset");
    let path = write_numbered_lines(&dir, "offset-test.txt", 100);

    let out = ReadTool
        .execute(
            json!({ "path": path.display().to_string(), "offset": 51 }),
            AbortLike::none(),
        )
        .await
        .expect("read");
    let text = first_text(&out);

    assert!(!text.contains("Line 50"));
    assert!(text.contains("Line 51"));
    assert!(text.contains("Line 100"));
    assert!(!text.contains("Use offset="));
    assert!(out.details.is_none());
}

#[tokio::test]
async fn read_honours_the_limit_parameter() {
    let dir = fresh_tmp("read-limit");
    let path = write_numbered_lines(&dir, "limit-test.txt", 100);

    let out = ReadTool
        .execute(
            json!({ "path": path.display().to_string(), "limit": 10 }),
            AbortLike::none(),
        )
        .await
        .expect("read");
    let text = first_text(&out);

    assert!(text.contains("Line 1"));
    assert!(text.contains("Line 10"));
    assert!(!text.contains("Line 11"));
    assert!(text.contains("[90 more lines in file. Use offset=11 to continue.]"));
}

#[tokio::test]
async fn read_combines_offset_and_limit() {
    let dir = fresh_tmp("read-offset-limit");
    let path = write_numbered_lines(&dir, "offset-limit-test.txt", 100);

    let out = ReadTool
        .execute(
            json!({
                "path": path.display().to_string(),
                "offset": 41,
                "limit": 20,
            }),
            AbortLike::none(),
        )
        .await
        .expect("read");
    let text = first_text(&out);

    assert!(!text.contains("Line 40"));
    assert!(text.contains("Line 41"));
    assert!(text.contains("Line 60"));
    assert!(!text.contains("Line 61"));
    assert!(text.contains("[40 more lines in file. Use offset=61 to continue.]"));
}

#[tokio::test]
async fn read_rejects_an_offset_beyond_the_end_of_the_file() {
    let dir = fresh_tmp("read-offset-oob");
    let path = write_numbered_lines(&dir, "short.txt", 3);

    let err = ReadTool
        .execute(
            json!({ "path": path.display().to_string(), "offset": 100 }),
            AbortLike::none(),
        )
        .await
        .expect_err("offset past EOF must fail");

    let msg = err.to_string();
    assert!(
        msg.contains("Offset 100 is beyond end of file (3 lines total)"),
        "unexpected error message: {msg}"
    );
}

#[tokio::test]
async fn read_points_at_a_bash_fallback_for_an_over_long_first_line() {
    let dir = fresh_tmp("read-first-line");
    let path = dir.join("one-huge-line.txt");
    std::fs::write(&path, "y".repeat(60 * 1024)).unwrap();

    let out = ReadTool
        .execute(
            json!({ "path": path.display().to_string() }),
            AbortLike::none(),
        )
        .await
        .expect("read");
    let text = first_text(&out);

    assert!(
        text.starts_with("[Line 1 is 60.0KB, exceeds 50.0KB limit. Use bash: sed -n '1p'"),
        "unexpected fallback text: {text}"
    );
    assert!(text.contains("| head -c 51200]"));
    let details = out.details.expect("details");
    assert_eq!(details["truncation"]["first_line_exceeds_limit"], true);
    assert_eq!(details["truncation"]["output_lines"], 0);
}

#[tokio::test]
async fn read_limit_that_covers_the_rest_of_the_file_has_no_notice() {
    let dir = fresh_tmp("read-limit-tail");
    let path = write_numbered_lines(&dir, "tail.txt", 10);

    let out = ReadTool
        .execute(
            json!({ "path": path.display().to_string(), "offset": 5, "limit": 100 }),
            AbortLike::none(),
        )
        .await
        .expect("read");
    let text = first_text(&out);

    assert_eq!(text, "Line 5\nLine 6\nLine 7\nLine 8\nLine 9\nLine 10");
    assert!(!text.contains("Use offset="));
}

#[tokio::test]
async fn bash_leaves_short_output_inline() {
    let out = BashTool
        .execute(json!({ "command": "seq 1 10" }), AbortLike::none())
        .await
        .expect("seq should succeed");

    let text = first_text(&out);
    assert!(text.starts_with("1\n2\n"));
    assert!(text.trim_end().ends_with("10"));
    assert!(!text.contains("Full output:"), "unexpected notice: {text}");
    let details = out.details.expect("details");
    assert!(
        details.get("truncation").is_none(),
        "short output must not report truncation: {details}"
    );
    assert!(details["full_output_path"].is_null());
}

#[tokio::test]
async fn bash_truncates_by_line_count_and_spills_the_full_output() {
    let out = BashTool
        .execute(json!({ "command": "seq 1 5000" }), AbortLike::none())
        .await
        .expect("seq should succeed");

    let text = first_text(&out);
    assert!(text.starts_with("3001\n"), "tail kept: {text}");
    assert!(text.contains("[Showing lines 3001-5000 of 5000. Full output: "));

    let details = out.details.expect("details");
    assert_eq!(details["truncation"]["truncated"], true);
    assert_eq!(details["truncation"]["truncated_by"], "lines");
    assert_eq!(details["truncation"]["total_lines"], 5000);
    assert_eq!(details["truncation"]["output_lines"], 2000);

    let spill = details["full_output_path"]
        .as_str()
        .expect("spill path")
        .to_string();
    let full = std::fs::read_to_string(&spill).expect("spilled output is readable");
    assert!(full.starts_with("1\n"));
    assert!(full.trim_end().ends_with("5000"));
    let _ = std::fs::remove_file(&spill);
}

#[tokio::test]
async fn bash_truncates_by_byte_limit_and_spills_the_full_output() {
    // A single 100KB line: the byte limit fires before the line limit.
    let out = BashTool
        .execute(
            json!({ "command": "head -c 100000 /dev/zero | tr '\\0' 'x'" }),
            AbortLike::none(),
        )
        .await
        .expect("command should succeed");

    let text = first_text(&out);
    assert!(
        text.contains("[Showing last 50.0KB of line 1 (line is 97.7KB). Full output: "),
        "unexpected notice: {text}"
    );

    let details = out.details.expect("details");
    assert_eq!(details["truncation"]["truncated_by"], "bytes");
    assert_eq!(details["truncation"]["last_line_partial"], true);
    assert_eq!(details["truncation"]["output_bytes"], 50 * 1024);

    let spill = details["full_output_path"]
        .as_str()
        .expect("spill path")
        .to_string();
    let spilled = std::fs::read_to_string(&spill).expect("spilled output");
    // 100000 bytes of `x`, plus the newline `combine_output` appends.
    assert_eq!(spilled.len(), 100001);
    assert!(spilled.starts_with("xxx"));
    let _ = std::fs::remove_file(&spill);
}

#[tokio::test]
async fn bash_reports_partial_output_when_the_timeout_fires() {
    let err = BashTool
        .execute(
            json!({ "command": "seq 1 3; sleep 30", "timeout": 1 }),
            AbortLike::none(),
        )
        .await
        .expect_err("timeout is an error");

    let msg = err.to_string();
    assert!(
        msg.contains("command exceeded timeout of 1s"),
        "missing timeout status: {msg}"
    );
    // The partial stdout that had already been flushed is preserved.
    assert!(msg.contains("1\n2\n3"), "partial output lost: {msg}");
}
