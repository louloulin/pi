//! P18 — borrow nanopi's blockquote gutter (`▏ `) + non-italic body.
//!
//! nanopi's `src/render/markdown.rs:82-104` deliberately drops the italic
//! modifier and uses a half-width block (`▏ `, U+258F) instead of the
//! full-width box drawing (`│ `, U+2502) that the TS pi-tui uses. Their
//! comment explains why: italic + gray is also the model's *thinking*
//! stream, so an italic blockquote renders identically to the model
//! muttering to itself. Routing the "this is a quote" signal through the
//! gutter alone frees the body to use the same gray slot without the
//! italic collision.
//!
//! The tests below pin every leg of that borrowing:
//!   1. The gutter is `▏ `, not `│ `.
//!   2. The gutter is `MdQuoteBorder`.
//!   3. The body uses `MdQuote` and is non-italic.
//!   4. Inline bold inside the quote keeps its bold modifier and is still
//!      non-italic (no italic ever reaches the body).
//!   5. Multi-paragraph quotes render every line with the same gutter,
//!      so the borrow is applied uniformly (not just on the first line).
//!   6. Plain paragraphs that happen to contain a `>` mid-line are *not*
//!      turned into a quote — the borrow is scoped to block-level quotes.

use pi_tui::markdown::render_markdown;
use pi_tui::styled::{plain_text, StyledLine, StyledSpan};
use pi_tui::theme::ThemeColor;

const GUTTER: &str = "▏ ";
const GUTTER_WIDTH: usize = 2; // U+258F + space

fn texts(lines: &[StyledLine]) -> Vec<String> {
    lines.iter().map(|line| plain_text(line)).collect()
}

fn gutter(line: &StyledLine) -> &StyledSpan {
    line.first().expect("every quoted line begins with a gutter span")
}

/// 1 + 2 — the gutter is nanopi's half-block + `MdQuoteBorder` slot.
#[test]
fn gutter_is_nanopi_half_block_with_quote_border_slot() {
    let lines = render_markdown("> hello world", 40);
    assert_eq!(lines.len(), 1, "single-line quote renders as one line");
    let g = gutter(&lines[0]);
    assert_eq!(g.text, GUTTER, "gutter must be nanopi's `▏ `, not `│ `");
    assert_eq!(g.style.fg, Some(ThemeColor::MdQuoteBorder));
    // The rest of the line is the body, with the `hello world` text
    // immediately after the gutter and no extra padding.
    let body_text: String = lines[0]
        .iter()
        .skip(1)
        .map(|span| span.text.as_str())
        .collect();
    assert_eq!(body_text, "hello world");
    assert_eq!(
        texts(&lines),
        vec![format!("{GUTTER}hello world")],
        "rendered text matches the nanopi pattern",
    );
}

/// 3 — the body uses `MdQuote` and is non-italic. The italic guarantee is
/// the point of the borrow: italic + gray is what the thinking stream
/// already uses, so an italic blockquote would be visually indistinguishable.
#[test]
fn body_uses_md_quote_slot_and_is_not_italic() {
    let lines = render_markdown("> hello world", 40);
    let body = lines[0]
        .iter()
        .find(|span| span.text == "hello world")
        .expect("body span must exist");
    assert_eq!(body.style.fg, Some(ThemeColor::MdQuote));
    assert!(
        !body.style.italic,
        "blockquote body must be non-italic to avoid colliding with the thinking stream",
    );
}

/// 4 — inline bold inside a quote is bold, but stays non-italic.
#[test]
fn inline_bold_in_a_quote_is_bold_but_not_italic() {
    let lines = render_markdown("> hello **bold** world", 40);
    let bold = lines[0]
        .iter()
        .find(|span| span.text == "bold")
        .expect("inline bold span must exist");
    assert_eq!(bold.style.fg, Some(ThemeColor::MdQuote));
    assert!(bold.style.bold);
    assert!(!bold.style.italic);
}

/// 5 — every line in a multi-paragraph quote carries the nanopi gutter.
#[test]
fn multi_paragraph_quote_uniformly_uses_the_nanopi_gutter() {
    let lines = render_markdown("> first\n>\n> second", 40);
    // All three rendered lines start with `▏ ` — even the blank line in
    // the middle, because the markdown emits one rendered line per
    // source line and the quote handler prepends the gutter uniformly.
    assert!(
        lines.len() >= 3,
        "expected at least three rendered lines, got {lines:?}",
    );
    for line in &lines {
        assert_eq!(
            gutter(line).text,
            GUTTER,
            "every quoted line must use the nanopi gutter (line: {:?})",
            texts(&[line.clone()]),
        );
        assert_eq!(gutter(line).style.fg, Some(ThemeColor::MdQuoteBorder));
    }
}

/// 6 — a paragraph that mentions `>` inline (e.g. "use -> arrow") must not
/// be mis-classified as a blockquote. The borrow is scoped to block-level
/// quotes only; a stray `>` inside running text keeps the default slot.
#[test]
fn inline_arrow_is_not_promoted_to_a_blockquote() {
    let lines = render_markdown("a -> b", 40);
    assert_eq!(lines.len(), 1);
    let text = plain_text(&lines[0]);
    assert_eq!(text, "a -> b", "the `>` must not be turned into a gutter");
    // The gutter span must not be present — no `▏` may appear in the
    // rendered line at all.
    let rendered_text: String = lines[0].iter().map(|s| s.text.as_str()).collect();
    assert!(
        !rendered_text.contains('▏'),
        "non-quote lines must not contain the nanopi gutter",
    );
}

/// Width accounting — the inner width subtracts exactly the gutter width
/// so wrapping still kicks in at the right column.
#[test]
fn inner_width_subtracts_the_gutter_width() {
    // Build a quoted line whose body length exceeds `width - GUTTER_WIDTH`.
    // The wrapping must split at `width - GUTTER_WIDTH` columns; the gutter
    // is prepended to every wrapped line.
    let body = "x".repeat(40);
    let source = format!("> {body}");
    let lines = render_markdown(&source, 12);
    assert!(
        lines.len() > 1,
        "wrapping should produce more than one line, got {}",
        lines.len(),
    );
    for line in &lines {
        assert_eq!(gutter(line).text, GUTTER);
        // Every wrapped line body length must be <= width - GUTTER_WIDTH.
        let body_chars: String = line.iter().skip(1).map(|s| s.text.as_str()).collect();
        assert!(
            body_chars.chars().count() <= 12 - GUTTER_WIDTH,
            "wrapped body length {} must not exceed width - gutter ({} - {})",
            body_chars.chars().count(),
            12,
            GUTTER_WIDTH,
        );
    }
}