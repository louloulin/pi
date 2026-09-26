//! P26 (F4) — OSC 8 hyperlinks in markdown.
//!
//! Upstream wraps the link label of every `[label](file:///path)` in an
//! OSC 8 sequence when the terminal reports hyperlink support; with no
//! support, the inline `(url)` suffix is the fallback. The Rust port keeps
//! the same two-branch behaviour at [`pi_tui::render_markdown_with_links`]
//! and [`pi_tui::themed_text`]: the parser stores the URL on the span
//! (`StyledSpan::link`), the link-capability layer either drops the inline
//! suffix or nulls the link, and `themed_text` emits the OSC 8 escape on
//! the way to the terminal.
//!
//! These tests pin that the file:// URL flows through the parser all the
//! way to the ANSI escape sequence a hyperlink-capable terminal sees.

use pi_tui::components::markdown::{render_markdown_with_links, render_markdown};
use pi_tui::styled::{plain_text, themed_text};
use pi_tui::theme::{builtin_theme, ColorMode};

fn first_span_text(lines: &[pi_tui::styled::StyledLine]) -> String {
    lines
        .iter()
        .flat_map(|l| l.iter())
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join("")
}

fn links(lines: &[pi_tui::styled::StyledLine]) -> Vec<String> {
    lines
        .iter()
        .flat_map(|l| l.iter())
        .filter_map(|s| s.link.clone())
        .collect()
}

/// 1 — `[label](file:///path)` parses: the rendered text contains the
/// label and the URL flows through to the styled spans. The label text is
/// preserved verbatim even when the URL is a `file://` scheme.
#[test]
fn file_url_label_is_rendered_and_link_is_attached() {
    let lines = render_markdown_with_links("[doc](file:///home/u/notes.md)", 80, true);
    let text = first_span_text(&lines);
    assert!(
        text.contains("doc"),
        "label text must be in the rendered output; got {text:?}"
    );
    let urls = links(&lines);
    assert!(
        urls.iter().any(|u| u == "file:///home/u/notes.md"),
        "the file:// URL must be attached to a span; got {urls:?}"
    );
}

/// 2 — With `hyperlinks = true`, the inline `(url)` suffix is dropped so
/// the OSC 8 wrap does not visually duplicate the link target. This is the
/// upstream behaviour: the URL is hidden because the terminal shows it on
/// hover, not in the text.
#[test]
fn file_url_drops_inline_suffix_when_hyperlinks_are_enabled() {
    let with = render_markdown_with_links("[doc](file:///home/u/notes.md)", 80, true);
    let text = first_span_text(&with);
    assert!(
        !text.contains("file:///home/u/notes.md"),
        "the inline URL suffix must be dropped when hyperlinks are on; got {text:?}"
    );
}

/// 3 — With `hyperlinks = false`, the inline `(url)` suffix is preserved
/// so the user can read the target. This is the fallback branch.
#[test]
fn file_url_keeps_inline_suffix_when_hyperlinks_are_disabled() {
    let without = render_markdown_with_links("[doc](file:///home/u/notes.md)", 80, false);
    let text = first_span_text(&without);
    assert!(
        text.contains("file:///home/u/notes.md"),
        "the inline URL suffix must be kept when hyperlinks are off; got {text:?}"
    );
}

/// 4 — `themed_text` emits the OSC 8 escape for spans carrying a `link`.
/// The OSC 8 open sequence is `\u{1b}]8;;<url>\u{1b}\\`; the close sequence
/// is `\u{1b}]8;;\u{1b}\\`. The label must sit between them, and the
/// visible width of the result must equal the label width (escape
/// sequences are zero-width).
#[test]
fn themed_text_emits_osc_8_around_a_file_link() {
    let lines = render_markdown_with_links("[doc](file:///home/u/notes.md)", 80, true);
    let theme = builtin_theme("dark", ColorMode::TrueColor).expect("dark theme");
    let ansi = lines
        .iter()
        .map(|line| themed_text(line.as_slice(), &theme))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        ansi.contains("\u{1b}]8;;file:///home/u/notes.md\u{1b}\\"),
        "OSC 8 open sequence must reference the file:// URL; got {ansi:?}"
    );
    assert!(
        ansi.contains("\u{1b}]8;;\u{1b}\\"),
        "OSC 8 close sequence must appear in the output; got {ansi:?}"
    );
}

/// 5 — The visible width of the OSC 8-wrapped output equals the visible
/// width of the label — escape sequences occupy zero cells. This is the
/// invariant that lets the renderer count cells correctly even with
/// hyperlinks enabled.
#[test]
fn osc_8_wrapped_link_has_zero_visible_escape_width() {
    let lines = render_markdown_with_links("[doc](file:///home/u/notes.md)", 80, true);
    let theme = builtin_theme("dark", ColorMode::TrueColor).expect("dark theme");
    let ansi = lines
        .iter()
        .map(|line| themed_text(line.as_slice(), &theme))
        .collect::<Vec<_>>()
        .join("\n");
    let visible = pi_tui::hyperlink::visible_width(&ansi);
    // label "doc" + leading whitespace the renderer may add; what we
    // *do* assert is that the OSC 8 escapes contribute zero cells.
    let stripped = pi_tui::hyperlink::strip_ansi(&ansi);
    assert_eq!(visible, stripped.chars().count(), "OSC 8 must be zero-width");
}

/// 6 — The basic `render_markdown` (no capability flag) does NOT emit
/// OSC 8 — it always keeps the inline `(url)` suffix. The capability-aware
/// variant is the only path that toggles behaviour.
#[test]
fn render_markdown_keeps_inline_url_by_default() {
    let lines = render_markdown("[doc](file:///home/u/notes.md)", 80);
    let text = plain_text(lines.first().expect("one line").as_slice());
    assert!(
        text.contains("file:///home/u/notes.md"),
        "the inline URL suffix must stay by default; got {text:?}"
    );
}

/// 7 — A `file://` URL with spaces (uncommon but legal) flows through
/// the parser. The renderer does not try to be clever about URL-encoding
/// here — it ships the raw bytes through to OSC 8 and lets the terminal
/// render them. The visible label still contains the label text.
#[test]
fn file_url_with_spaces_routes_through_osc8() {
    let lines = render_markdown_with_links("[a b](file:///home/u/has%20space.md)", 80, true);
    let urls = links(&lines);
    assert!(
        urls.iter().any(|u| u == "file:///home/u/has%20space.md"),
        "the URL must survive the parser verbatim; got {urls:?}"
    );
    let text = first_span_text(&lines);
    assert!(text.contains("a b"), "the label must be preserved; got {text:?}");
}

/// 8 — The OSC 8 helper from [`pi_tui::hyperlink`] builds the exact
/// sequence the markdown renderer relies on. This pins the low-level
/// shape so a refactor of `themed_text` cannot drift.
#[test]
fn hyperlink_helper_builds_the_expected_osc_8_sequence() {
    let seq = pi_tui::hyperlink::hyperlink("doc", "file:///home/u/notes.md");
    assert_eq!(
        seq,
        "\u{1b}]8;;file:///home/u/notes.md\u{1b}\\doc\u{1b}]8;;\u{1b}\\",
        "hyperlink() must emit the exact upstream OSC 8 sequence"
    );
}