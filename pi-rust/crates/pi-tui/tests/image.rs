//! Tests for the `Image` component and its ANSI/OSC-8 aware truncation
//! (`packages/tui/src/components/image.ts` + `utils.ts::truncateToWidth`).
//!
//! Offline and fixture-free: every payload is an inline base64 sample. The
//! capability cache and the cell dimensions are process-global, so every test
//! that renders lives in the single `component_branches_are_sequenced` case —
//! a `#[test]` binary runs its cases in threads, and crosstalk between them
//! would make the capability branches racy.

use pi_tui::hyperlink::strip_ansi;
use pi_tui::styled::{plain_text, themed_text};
use pi_tui::{
    builtin_theme, calculate_image_cell_size, close_hyperlink, hyperlink, open_hyperlink,
    set_capabilities, set_cell_dimensions, truncate_to_width, visible_width, CellDimensions,
    ColorMode, Component, Image, ImageDimensions, ImageOptions, ImageProtocol, ImageTheme,
    SpanStyle, TerminalCapabilities, ThemeColor,
};

/// PNG header carrying 320x240 (see `terminal_image_render.rs`).
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAUAAAADw";

/// The cell size every rendering case installs.
const CELLS: CellDimensions = CellDimensions {
    width_px: 9,
    height_px: 18,
};

const SAMPLE: ImageDimensions = ImageDimensions {
    width_px: 320,
    height_px: 240,
};

fn caps(images: Option<ImageProtocol>, hyperlinks: bool) -> TerminalCapabilities {
    TerminalCapabilities {
        images,
        true_color: true,
        hyperlinks,
    }
}

fn theme() -> ImageTheme {
    ImageTheme::fallback(SpanStyle::fg(ThemeColor::ToolOutput))
}

// ---------------------------------------------------------------------------
// truncate_to_width — pure, no capability reads
// ---------------------------------------------------------------------------

#[test]
fn truncation_returns_fitting_text_verbatim() {
    assert_eq!(truncate_to_width("hello", 5), "hello");
    assert_eq!(truncate_to_width("hello", 40), "hello");
    assert_eq!(truncate_to_width("", 10), "");
    // Escapes count as zero width, so an SGR-wrapped label that fits is not
    // rewritten in any way.
    let colored = "\u{1b}[31mred\u{1b}[39m";
    assert_eq!(truncate_to_width(colored, 3), colored);
}

#[test]
fn truncation_appends_the_ellipsis_only_when_it_has_to() {
    assert_eq!(truncate_to_width("hello world", 8), "hello...");
    assert_eq!(visible_width(&truncate_to_width("hello world", 8)), 8);
    // Upstream clips the ellipsis itself when it does not fit.
    assert_eq!(truncate_to_width("hello", 3), "...");
    assert_eq!(truncate_to_width("hello", 1), ".");
    assert_eq!(truncate_to_width("hello", 0), "");
}

#[test]
fn truncation_never_splits_an_escape_and_closes_an_open_link() {
    let linked = hyperlink("read the docs", "https://example.com");
    let out = truncate_to_width(&linked, 10);
    assert!(out.starts_with(&open_hyperlink("https://example.com")));
    assert!(out.ends_with(&close_hyperlink()));
    assert_eq!(strip_ansi(&out), "read th...");
    assert_eq!(visible_width(&out), 10);

    // A link that ends inside the kept prefix needs no extra close sequence.
    let closed = format!(
        "{} rest of the text",
        hyperlink("ab", "https://example.com")
    );
    let out = truncate_to_width(&closed, 12);
    assert_eq!(strip_ansi(&out), "ab rest o...");
    assert!(!out.ends_with(&close_hyperlink()));
    assert_eq!(visible_width(&out), 12);

    // A cut in the middle of a long CSI sequence's visible run keeps the
    // sequence intact and the width exact.
    let colored = format!("\u{1b}[38;2;1;2;3m{}\u{1b}[39m", "abcdefghij");
    let out = truncate_to_width(&colored, 6);
    assert_eq!(out, "\u{1b}[38;2;1;2;3mabc...");
    assert_eq!(visible_width(&out), 6);
}

// ---------------------------------------------------------------------------
// Image — every capability branch, sequenced in one case
// ---------------------------------------------------------------------------

#[test]
fn component_branches_are_sequenced() {
    set_cell_dimensions(CELLS);
    let live = builtin_theme("dark", ColorMode::TrueColor).expect("dark theme");

    // --- no image capability: one styled, truncated fallback row ----------
    set_capabilities(caps(None, false));
    let plain = Image::new(PNG, "image/png", theme()).with_dimensions(SAMPLE);
    let lines = Component::render(&plain, 60);
    assert_eq!(lines.len(), 1);
    assert_eq!(plain_text(&lines[0]), "[Image: [image/png] 320x240]");
    assert_eq!(lines[0][0].style, SpanStyle::fg(ThemeColor::ToolOutput));
    // 着色: the slot resolves through the live theme into ANSI.
    assert_eq!(
        themed_text(&lines[0], &live),
        SpanStyle::fg(ThemeColor::ToolOutput).ansi(&live, "[Image: [image/png] 320x240]")
    );
    // The string view is the upstream `string[]` shape.
    assert_eq!(plain.render_lines(60), vec!["[Image: [image/png] 320x240]"]);

    // Over-wide fallback: truncated to `width`, ellipsis included.
    let narrow = Component::render(&plain, 20);
    assert_eq!(narrow.len(), 1);
    assert!(plain_text(&narrow[0]).ends_with("..."));
    assert_eq!(visible_width(&plain_text(&narrow[0])), 20);

    // --- fallback carrying an OSC 8 link (absolute filename) --------------
    set_capabilities(caps(None, true));
    let linked = Image::new(PNG, "image/png", theme())
        .with_dimensions(SAMPLE)
        .with_options(ImageOptions {
            filename: Some("/tmp/pic.png".to_string()),
            ..ImageOptions::default()
        });
    let lines = Component::render(&linked, 20);
    let text = plain_text(&lines[0]);
    assert!(text.contains(&open_hyperlink("file:///tmp/pic.png")));
    // The raw line ends with the close sequence the truncation appended; the
    // ellipsis sits inside the still-open link.
    assert!(text.ends_with(&close_hyperlink()));
    assert_eq!(visible_width(&text), 20);
    let plain_fallback = strip_ansi(&text);
    assert!(plain_fallback.ends_with("..."));
    assert!(plain_fallback.starts_with("[Image: /tmp/pic"));

    // --- kitty: sequence on row 1, `rows` lines total, id allocated -------
    set_capabilities(caps(Some(ImageProtocol::Kitty), false));
    let kitty = Image::new(PNG, "image/png", theme()).with_dimensions(SAMPLE);
    let lines = Component::render(&kitty, 40);
    let expected = calculate_image_cell_size(SAMPLE, 38, Some(19), CELLS);
    assert_eq!(
        lines.len(),
        usize::try_from(expected.rows).expect("rows fit")
    );
    assert!(plain_text(&lines[0]).starts_with("\u{1b}_G"));
    assert!(lines[1..].iter().all(|line| line.is_empty()));
    let id = kitty.get_image_id().expect("kitty allocates an id");
    assert!(plain_text(&lines[0]).contains(&format!("i={id}")));
    assert!(plain_text(&lines[0]).contains("C=1"));

    // Cache: same width returns the identical lines, even after the
    // capability changed — no recompute without `invalidate`.
    assert_eq!(Component::render(&kitty, 40), lines);
    set_capabilities(caps(None, false));
    assert_eq!(Component::render(&kitty, 40), lines);
    kitty.invalidate();
    let after = Component::render(&kitty, 40);
    assert_eq!(after.len(), 1);
    assert!(plain_text(&after[0]).starts_with("[Image: "));

    // A width change recomputes too (20 columns truncates the label).
    let narrow = Component::render(&kitty, 20);
    assert!(plain_text(&narrow[0]).ends_with("..."));
    assert_ne!(narrow, after);

    // A caller-supplied id is reused instead of allocating.
    let reused = Image::new(PNG, "image/png", theme())
        .with_dimensions(SAMPLE)
        .with_options(ImageOptions {
            image_id: Some(4242),
            ..ImageOptions::default()
        });
    set_capabilities(caps(Some(ImageProtocol::Kitty), false));
    let lines = Component::render(&reused, 40);
    assert_eq!(reused.get_image_id(), Some(4242));
    assert!(plain_text(&lines[0]).contains("i=4242"));

    // --- iTerm2: empty rows, then cursor-up + sequence --------------------
    set_capabilities(caps(Some(ImageProtocol::Iterm2), false));
    let iterm = Image::new(PNG, "image/png", theme()).with_dimensions(SAMPLE);
    let lines = Component::render(&iterm, 40);
    assert_eq!(
        lines.len(),
        usize::try_from(expected.rows).expect("rows fit")
    );
    assert!(lines[..lines.len() - 1].iter().all(|line| line.is_empty()));
    let last = plain_text(&lines[lines.len() - 1]);
    assert!(last.starts_with(&format!("\u{1b}[{}A", expected.rows - 1)));
    assert!(last.contains("\u{1b}]1337;File="));

    // A one-row image emits no cursor movement at all.
    let single = Image::new(PNG, "image/png", theme()).with_dimensions(ImageDimensions {
        width_px: 320,
        height_px: 1,
    });
    let rows = calculate_image_cell_size(
        ImageDimensions {
            width_px: 320,
            height_px: 1,
        },
        38,
        Some(19),
        CELLS,
    );
    assert_eq!(rows.rows, 1);
    let lines = Component::render(&single, 40);
    assert_eq!(lines.len(), 1);
    assert!(plain_text(&lines[0]).starts_with("\u{1b}]1337;File="));
    // No cursor movement at all on a single-row image: the sequence is the
    // first thing on the row.
    assert!(!plain_text(&lines[0]).starts_with("\u{1b}["));
}
