//! Phase 5 / G5 — `─` separator above modal footer hints.
//!
//! Upstream brackets the key-hint block of a modal between two
//! `DynamicBorder` lines (`extension-selector.ts:44,75`, `model-selector.ts:100,151`).
//! The Rust port already painted the title-list separator; this file pins
//! the second half — the matching `─` row above the hints — so a modal with
//! `with_footer(...)` lines reads as a closed block the same way TS reads.
//!
//! The selector's plain-text and themed render paths both have to carry the
//! new row: the App's overlay uses the styled buffer (which the themed
//! render path also produces) and the snapshot tests use the plain path.

use pi_tui::selector::{Selector, SelectorItem};
use pi_tui::styles::SelectListStyles;
use pi_tui::theme::{builtin_theme, ColorMode, Theme};

// dark.json: borderMuted = "darkGray" → 80,80,80.
const BORDER_MUTED: &str = "\x1b[38;2;80;80;80m";
const RESET: &str = "\x1b[39m";

fn dark() -> Theme {
    builtin_theme("dark", ColorMode::TrueColor).expect("built-in dark theme")
}

/// Strip ANSI escapes from a themed-render line so we can match on the
/// underlying characters.
fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn selector_with_footer(footer: Vec<String>) -> Selector {
    Selector::new(
        "Pick a model",
        vec![
            SelectorItem::new("model:alpha", "Alpha"),
            SelectorItem::new("model:beta", "Beta"),
        ],
    )
    .with_footer(footer)
}

#[test]
fn selector_plain_render_separates_list_from_footer_hints() {
    // The plain text render must carry the same line the styled render
    // does: the snapshot tests assert against `render_lines`, and a missing
    // separator there would split the layout in half.
    let selector = selector_with_footer(vec!["enter to choose".into()]);
    let lines = selector.render_lines(40);

    // Structure: title / ─ / items / ─ / footer
    assert!(
        lines[0].contains("Pick a model"),
        "title at the head: {lines:?}"
    );
    assert_eq!(lines[1], "─".repeat(40), "title-list separator");

    // Walk backwards to find the footer separator (last `─` row before the
    // hint), then the hint itself.
    let footer_separator_index = lines
        .iter()
        .rposition(|line| line.chars().all(|ch| ch == '─'))
        .expect("a `─` row exists somewhere in the rendered lines");
    assert!(
        footer_separator_index < lines.len() - 1,
        "the `─` row is the footer separator, not the tail: {lines:?}"
    );
    assert!(
        lines[footer_separator_index + 1].contains("enter to choose"),
        "the hint follows the footer separator: {lines:?}"
    );
}

#[test]
fn selector_themed_render_paints_the_footer_separator_with_border_muted() {
    // The themed render wraps both separators in the same `borderMuted` SGR
    // the title-list separator already used, so the modal reads as a single
    // visually-bracketed block.
    let theme = dark();
    let styles = SelectListStyles::new(&theme);
    let selector = selector_with_footer(vec!["enter to choose".into()]);
    let lines = selector.render_lines_themed(40, &styles);

    // Strip ANSI so we can find the `─` row before colour-matching.
    let plain: Vec<String> = lines.iter().map(|line| strip_ansi(line)).collect();
    let footer_separator_index = plain
        .iter()
        .rposition(|line| !line.is_empty() && line.chars().all(|ch| ch == '─'))
        .expect("a `─` row exists in the themed render");
    assert!(
        footer_separator_index < plain.len() - 1,
        "the `─` row is the footer separator, not the tail: {plain:?}"
    );
    assert!(
        lines[footer_separator_index].contains(BORDER_MUTED),
        "the footer separator carries the borderMuted SGR: {:?}",
        lines[footer_separator_index]
    );
    assert!(
        lines[footer_separator_index].contains(RESET),
        "the footer separator closes the slot before the next row: {:?}",
        lines[footer_separator_index]
    );
}

#[test]
fn selector_without_a_footer_does_not_paint_the_extra_separator() {
    // No footer → no footer separator. The trailing row stays the last
    // item, not a stray `─`, so the modal closes flush instead of leaving
    // an empty line.
    let selector = Selector::new(
        "Pick a model",
        vec![SelectorItem::new("model:alpha", "Alpha")],
    );
    let lines = selector.render_lines(40);

    // Exactly one `─` row — the title-list separator.
    let separator_count = lines
        .iter()
        .filter(|line| !line.is_empty() && line.chars().all(|ch| ch == '─'))
        .count();
    assert_eq!(separator_count, 1, "only the title-list separator: {lines:?}");
    assert!(
        !lines.last().unwrap().is_empty() || lines.last().unwrap().chars().all(|ch| ch == '─'),
        "no trailing `─` row: {lines:?}"
    );
}

#[test]
fn dialog_brackets_each_case_block_with_separators() {
    use pi_protocol::UiRequest;
    use pi_tui::dialog::Dialog;
    use tokio::sync::oneshot;

    // The dialog has its own `render_lines` (plain text), which the App
    // overlay reads. Each case — Confirm / Input / Select — has to bracket
    // its body and its key-hint block, matching what the selector does.
    let cases: Vec<(UiRequest, &str)> = vec![
        (
            UiRequest::Confirm {
                title: "Deploy".into(),
                body: "Ship the release?".into(),
            },
            "Ship the release?",
        ),
        (
            UiRequest::Input {
                title: "Branch".into(),
                placeholder: Some("main".into()),
            },
            "main",
        ),
    ];

    for (request, marker) in cases {
        let (tx, _rx) = oneshot::channel();
        let dialog = Dialog::new(request, tx);
        let lines = dialog.render_lines(40);

        // Two `─` rows bracket the body+footer block: one right after the
        // title (`title─`), one just above the key hints.
        let separator_count = lines
            .iter()
            .filter(|line| !line.is_empty() && line.chars().all(|ch| ch == '─'))
            .count();
        assert!(
            separator_count >= 2,
            "{marker}: expected both a title separator and a footer separator: {lines:?}"
        );
        // The footer separator sits directly above the key-hint row.
        let footer_separator_index = lines
            .iter()
            .rposition(|line| !line.is_empty() && line.chars().all(|ch| ch == '─'))
            .expect("a footer `─` row exists");
        assert!(
            lines[footer_separator_index + 1]
                .chars()
                .any(|ch| ch == '['),
            "{marker}: the row right after the footer separator carries key hints: {lines:?}"
        );
    }
}