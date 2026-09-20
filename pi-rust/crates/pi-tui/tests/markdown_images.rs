//! Markdown images: a standalone `![alt](data:image/…;base64,…)` line renders
//! through the `pi-tui` image kernel (`markdown.rs` → `Image` →
//! `terminal_image::render_image`), and an assistant message body keeps those
//! escape rows verbatim (`message.rs`).
//!
//! The capability cache is process-global, so every rendering case here runs
//! under one lock — a `#[test]` binary runs its cases in threads and crosstalk
//! would make the capability branches racy.

use std::sync::{Mutex, MutexGuard};

use pi_tui::markdown::render_markdown;
use pi_tui::message::{MessageItem, MessageView};
use pi_tui::styled::{plain_text, StyledLine};
use pi_tui::{
    is_image_line, set_capabilities, set_cell_dimensions, CellDimensions, ImageProtocol,
    TerminalCapabilities, ThemeColor, ITERM2_PREFIX, KITTY_PREFIX,
};

/// PNG header carrying 320x240 (same fixture as the other image tests).
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAUAAAADw";

const CELLS: CellDimensions = CellDimensions {
    width_px: 9,
    height_px: 18,
};

static CAPABILITIES: Mutex<()> = Mutex::new(());

fn capabilities(images: Option<ImageProtocol>) -> MutexGuard<'static, ()> {
    let guard = CAPABILITIES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    set_capabilities(TerminalCapabilities {
        images,
        true_color: true,
        hyperlinks: false,
    });
    set_cell_dimensions(CELLS);
    guard
}

fn image_markdown(alt: &str, payload: &str) -> String {
    format!("![{alt}](data:image/png;base64,{payload})")
}

fn texts(lines: &[StyledLine]) -> Vec<String> {
    lines.iter().map(|line| plain_text(line)).collect()
}

#[test]
fn a_data_uri_image_renders_kitty_rows() {
    let _guard = capabilities(Some(ImageProtocol::Kitty));

    let lines = render_markdown(&image_markdown("shot", PNG), 40);
    let rendered = texts(&lines);

    assert!(
        rendered
            .iter()
            .any(|line| is_image_line(line) && line.starts_with(KITTY_PREFIX)),
        "kitty rows expected: {rendered:?}"
    );
    assert!(
        rendered.iter().any(|line| line.contains(PNG)),
        "the payload must travel inside the escape: {rendered:?}"
    );

    // Surrounding prose keeps its layout: the image sits between the two
    // paragraphs and no prose line is swallowed.
    let lines = render_markdown(&format!("before\n\n{}\n\nafter", image_markdown("shot", PNG)), 40);
    let rendered = texts(&lines);
    assert_eq!(rendered.first().map(String::as_str), Some("before"));
    assert_eq!(rendered.last().map(String::as_str), Some("after"));
}

#[test]
fn a_data_uri_image_renders_iterm2_rows() {
    let _guard = capabilities(Some(ImageProtocol::Iterm2));

    let rendered = texts(&render_markdown(&image_markdown("shot", PNG), 40));
    assert!(
        rendered
            .iter()
            .any(|line| line.contains(ITERM2_PREFIX) && line.contains("inline=1")),
        "iTerm2 rows expected: {rendered:?}"
    );
}

#[test]
fn an_image_without_terminal_support_becomes_the_fallback_label() {
    let _guard = capabilities(None);

    let lines = render_markdown(&image_markdown("shot", PNG), 40);
    assert_eq!(texts(&lines), vec!["[Image: [image/png] 320x240]"]);
    let fallback = &lines[0][0];
    assert_eq!(fallback.style.fg, Some(ThemeColor::MdLink));
    assert!(
        !plain_text(&lines[0]).contains('\u{1b}'),
        "the fallback must be escape-free"
    );
}

#[test]
fn a_payload_that_cannot_be_sized_falls_back_to_the_alt_text() {
    let _guard = capabilities(Some(ImageProtocol::Kitty));

    // Not a base64 PNG header: the kernel cannot read its dimensions, so the
    // image is a broken one and marked-style alt text is the right answer.
    let rendered = texts(&render_markdown(
        &image_markdown("a landscape", "bm90LWEtcG5n"),
        40,
    ));
    assert_eq!(rendered, vec!["a landscape"]);
    assert!(!rendered[0].contains('\u{1b}'));
}

#[test]
fn a_non_data_image_renders_as_its_alt_text_link() {
    let _guard = capabilities(Some(ImageProtocol::Kitty));

    // No payload to draw, so the alt text is the visible content — with the
    // `!` removed, and the target still discoverable.
    let lines = render_markdown("see ![the chart](https://example.com/c.png)", 60);
    let rendered = texts(&lines);
    assert_eq!(rendered, vec!["see the chart (https://example.com/c.png)"]);

    // An image sharing a line with prose is inline even when the payload is a
    // drawable data URI.
    let inline = texts(&render_markdown(
        &format!("shot: {}", image_markdown("shot", PNG)),
        200,
    ));
    assert_eq!(inline.len(), 1, "inline images do not become rows: {inline:?}");
    assert!(!inline[0].contains(KITTY_PREFIX));
    assert!(inline[0].starts_with("shot: shot"));
}

#[test]
fn an_assistant_body_keeps_image_rows_verbatim() {
    let _guard = capabilities(Some(ImageProtocol::Kitty));

    let body = format!("Here is the render:\n\n{}\n\nDone.", image_markdown("shot", PNG));
    let mut view = MessageView::new().with_markdown(true);
    view.push(MessageItem::assistant(body));

    let lines = view.render_lines(80);
    let image_rows: Vec<&String> = lines
        .iter()
        .filter(|line| is_image_line(line))
        .collect();
    assert_eq!(image_rows.len(), 1, "one escape row expected: {lines:?}");
    let escape = image_rows[0];
    assert!(
        escape.starts_with(KITTY_PREFIX),
        "the role prefix must not precede the escape: {escape:?}"
    );
    assert!(escape.contains(PNG), "{escape:?}");

    // The prose keeps its role prefix.
    assert!(
        lines.iter().any(|line| line.contains("Here is the render:")),
        "{lines:?}"
    );
}
