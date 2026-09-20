//! Tool-result image blocks: `Content::Image` → `pi-tui` `Image` /
//! `terminal_image::render_image`, with the `[Image: …]` text fallback for a
//! terminal that cannot render inline images (`--print` / text fallback).
//!
//! The capability cache is process-global (`pi_tui::set_capabilities`), so
//! every case that renders an image runs under one lock — a `#[test]` binary
//! runs its cases in threads and crosstalk between them would make the
//! capability branches racy.

use std::sync::{Mutex, MutexGuard};

use pi_coding_agent::tools::render::{image_lines, image_fallback_text, ToolRenderContext};
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
