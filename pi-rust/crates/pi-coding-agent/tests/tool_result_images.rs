//! Tool-result image blocks: `Content::Image` → `pi-tui` `Image` /
//! `terminal_image::render_image`, with the `[Image: …]` text fallback for a
//! terminal that cannot render inline images (`--print` / text fallback).
//!
//! The capability cache is process-global (`pi_tui::set_capabilities`), so
//! every case that renders an image runs under one lock — a `#[test]` binary
//! runs its cases in threads and crosstalk between them would make the
//! capability branches racy.

use std::sync::{Mutex, MutexGuard};

use pi_agent_core::tools::ToolExecutor;
use pi_coding_agent::tool_executor::BuiltinToolExecutor;
use pi_coding_agent::tools::render::{image_fallback_text, image_lines, ToolRenderContext};
use pi_coding_agent::tools::{get_text_output, render_lines_plain, ToolOutput, ToolRenderSession};
use pi_protocol::{Content, ImageContent, ToolCall, ToolResult};
use pi_tui::{
    set_capabilities, set_cell_dimensions, CellDimensions, ImageProtocol, TerminalCapabilities,
    KITTY_PREFIX, ITERM2_PREFIX,
};

/// PNG header carrying 320x240 (same fixture as the `pi-tui` tests).
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAUAAAADw";

const CELLS: CellDimensions = CellDimensions {
    width_px: 9,
    height_px: 18,
};

static CAPABILITIES: Mutex<()> = Mutex::new(());

fn caps(images: Option<ImageProtocol>) -> TerminalCapabilities {
    TerminalCapabilities {
        images,
        true_color: true,
        hyperlinks: false,
    }
}

/// Serialize every case that installs a terminal capability.
fn capabilities() -> MutexGuard<'static, ()> {
    CAPABILITIES.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn image_content() -> ImageContent {
    ImageContent {
        mime_type: "image/png".to_string(),
        data: PNG.to_string(),
    }
}

fn image_block() -> Content {
    Content::Image(image_content())
}

/// The block the `read` tool contributes once folders keep the image over the
/// caption (`tool_executor::fold_content`).
fn image_output() -> ToolOutput {
    ToolOutput {
        content: vec![image_block()],
        details: None,
    }
}

fn session(dir: &std::path::Path, show_images: bool) -> ToolRenderSession {
    ToolRenderSession::new(dir)
        .with_show_images(show_images)
        .with_width(40)
}

fn call() -> ToolCall {
    ToolCall {
        id: "call_image".to_string(),
        name: "read".to_string(),
        arguments: serde_json::json!({ "path": "shot.png" }),
    }
}

fn result() -> ToolResult {
    ToolResult {
        tool_call_id: "call_image".to_string(),
        content: Box::new(image_block()),
        is_error: false,
        details: None,
        added_tool_names: None,
    }
}

/// A PNG header on disk for the real `read` tool to pick up.
const PNG_HEADER: [u8; 24] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x01, 0x40, 0x00, 0x00, 0x00, 0xf0,
];

#[test]
fn image_block_renders_kitty_escapes() {
    let _guard = capabilities();
    set_capabilities(caps(Some(ImageProtocol::Kitty)));
    set_cell_dimensions(CELLS);

    let ctx = ToolRenderContext::new("/work")
        .with_show_images(true)
        .with_width(40);
    let lines = image_lines(&image_block(), &ctx);
    let rendered = render_lines_plain(&lines);

    assert!(
        rendered.starts_with(KITTY_PREFIX),
        "kitty sequence must be the first row: {rendered:?}"
    );
    assert!(rendered.contains("a=T,f=100"), "kitty payload: {rendered:?}");
    assert!(rendered.contains(PNG), "payload must carry the base64 data");

    // A non-image block contributes no rows.
    assert!(image_lines(&Content::text("plain"), &ctx).is_empty());
}

#[test]
fn image_block_renders_iterm2_escapes() {
    let _guard = capabilities();
    set_capabilities(caps(Some(ImageProtocol::Iterm2)));
    set_cell_dimensions(CELLS);

    let ctx = ToolRenderContext::new("/work")
        .with_show_images(true)
        .with_width(40);
    let rendered = render_lines_plain(&image_lines(&image_block(), &ctx));

    assert!(
        rendered.contains(ITERM2_PREFIX),
        "iTerm2 sequence expected: {rendered:?}"
    );
    assert!(rendered.contains("inline=1"), "iTerm2 payload: {rendered:?}");
}

#[test]
fn image_block_falls_back_to_text_without_image_support() {
    let _guard = capabilities();
    set_capabilities(caps(None));
    set_cell_dimensions(CELLS);

    let ctx = ToolRenderContext::new("/work")
        .with_show_images(true)
        .with_width(40);
    let rendered = render_lines_plain(&image_lines(&image_block(), &ctx));
    assert_eq!(rendered, "[Image: [image/png] 320x240]");
    assert!(!rendered.contains('\u{1b}'), "fallback must be escape-free");

    // The text path (collapsed / `--print`) shows the same indicator, and the
    // image is not duplicated when it is drawn.
    assert_eq!(
        get_text_output(&image_output(), false),
        "[Image: [image/png] 320x240]"
    );
    assert_eq!(get_text_output(&image_output(), true), "");
    assert_eq!(
        image_fallback_text(&image_content()),
        "[Image: [image/png] 320x240]"
    );
}

#[test]
fn render_session_draws_images_only_when_enabled() {
    let _guard = capabilities();
    set_capabilities(caps(Some(ImageProtocol::Kitty)));
    set_cell_dimensions(CELLS);

    let dir = std::env::temp_dir();
    let mut with_images = session(&dir, true);
    with_images.call(&call(), false);
    let rendered = render_lines_plain(&with_images.result(&result()));
    assert!(
        rendered.contains(KITTY_PREFIX),
        "an image-enabled session must emit the kitty escape: {rendered:?}"
    );

    // The text / `--print` shape: no image capability requested, so the block
    // degrades to one `[Image: …]` line and never leaks an escape sequence.
    let mut without_images = session(&dir, false);
    without_images.call(&call(), false);
    let rendered = render_lines_plain(&without_images.result(&result()));
    assert!(
        rendered.contains("[Image: [image/png] 320x240]"),
        "fallback indicator expected: {rendered:?}"
    );
    assert!(
        !rendered.contains('\u{1b}'),
        "the text path must stay escape-free: {rendered:?}"
    );
}

/// The `read` tool's image output, end to end: tool → executor fold → render.
#[tokio::test]
async fn read_tool_image_result_renders_through_the_executor() {
    let _guard = capabilities();
    set_capabilities(caps(Some(ImageProtocol::Kitty)));
    set_cell_dimensions(CELLS);

    let dir = std::env::temp_dir().join(format!("pi-image-read-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("shot.png");
    std::fs::write(&path, PNG_HEADER).expect("write png");

    let executor = BuiltinToolExecutor::with_default_tools();
    let tool_call = ToolCall {
        id: "call_read_image".to_string(),
        name: "read".to_string(),
        arguments: serde_json::json!({ "path": path.display().to_string() }),
    };
    let result = executor
        .execute(&tool_call, tokio_util::sync::CancellationToken::new())
        .await
        .expect("read image");

    // The fold keeps the image as `content` and parks the caption.
    assert!(
        matches!(result.content.as_ref(), Content::Image(_)),
        "image must win the single content slot: {:?}",
        result.content
    );
    assert_eq!(
        result.details.as_ref().and_then(|d| d["image_text"].as_str()),
        Some("Read image file [image/png]")
    );

    let mut with_images = session(&dir, true);
    with_images.call(&tool_call, false);
    let rendered = render_lines_plain(&with_images.result(&result));
    assert!(
        rendered.contains(KITTY_PREFIX),
        "read-image TUI path must emit kitty rows: {rendered:?}"
    );
    assert!(
        rendered.contains("Read image file [image/png]"),
        "the caption stays visible next to the image: {rendered:?}"
    );

    // Same result on a terminal without image support (and `--print`): text only.
    let mut plain = session(&dir, false);
    plain.call(&tool_call, false);
    let rendered = render_lines_plain(&plain.result(&result));
    assert!(rendered.contains("[Image: [image/png] 320x240]"), "{rendered:?}");
    assert!(!rendered.contains('\u{1b}'), "{rendered:?}");
}
