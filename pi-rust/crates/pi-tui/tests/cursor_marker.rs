//! P23 (C2.9) — IME cursor marker.
//!
//! Upstream emits a zero-width cursor marker at the caret position so
//! the OS can place the IME candidate window without the renderer
//! having to know about platform IMEs. The Rust port mirrors that in
//! three layers, each pinned by a test below:
//!
//! 1. [`pi_tui::ts_compat::CURSOR_MARKER`] — the public, TS-compatible
//!    sentinel (a zero-width space, `'\u{200B}'`). The constant must
//!    stay at the `ts_compat` surface because plugin authors read it
//!    from `pi_tui::CURSOR_MARKER` against upstream's TypeScript name.
//! 2. [`pi_tui::Prompt::cursor_position`] — the cursor's cell coordinates
//!    inside the composer's rectangle. A roundtrip through
//!    `cursor_position` + `cursor_marker_line` must put the marker
//!    exactly where `cursor_position` says it is.
//! 3. [`pi_tui::layout::cursor_marker_line`] — the styled-line factory
//!    that emits the public marker at the requested cell. The
//!    `paint_prompt` path applies the **reversed-video** modifier to
//!    the `▍` caret glyph, which is what the OS reads to anchor the
//!    IME candidate window.

use pi_tui::input::Key;
use pi_tui::layout::cursor_marker_line;
use pi_tui::prompt::Prompt;
use pi_tui::styled::plain_text;
use pi_tui::theme::{builtin_theme, ColorMode};
use pi_tui::ts_compat::CURSOR_MARKER as PUBLIC_CURSOR_MARKER;
use pi_tui::SpanStyle;
use ratatui::style::Modifier;

fn type_text(prompt: &mut Prompt, text: &str) {
    for ch in text.chars() {
        prompt.handle_key(Key::char(ch));
    }
}

/// 1 — The public, TS-compatible cursor marker is a zero-width space.
/// Plugin authors import `pi_tui::CURSOR_MARKER` and compare it against
/// the upstream TypeScript value: any drift here silently breaks
/// IME-aware plugins.
#[test]
fn public_cursor_marker_is_a_zero_width_space() {
    assert_eq!(PUBLIC_CURSOR_MARKER, '\u{200B}');
    assert_eq!(
        PUBLIC_CURSOR_MARKER as u32, 0x200B,
        "CURSOR_MARKER must be U+200B — the upstream sentinel"
    );
}

/// 2 — `cursor_marker_line(row, col, width)` returns a `StyledLine`
/// whose only non-blank character is the public `CURSOR_MARKER`, placed
/// at exactly `col`. Pad-left is empty when `col == 0`; pad-right is
/// empty when the marker is at the last cell of the rectangle.
#[test]
fn cursor_marker_line_emits_marker_at_the_requested_cell() {
    let (row, line) = cursor_marker_line(0, 3, 10);
    assert_eq!(row, 0);
    let text = plain_text(line.as_slice());
    assert_eq!(
        text.chars().filter(|c| *c == PUBLIC_CURSOR_MARKER).count(),
        1,
        "the line must contain exactly one IME marker; got {text:?}"
    );
    let pos = text.find(PUBLIC_CURSOR_MARKER).expect("marker present");
    assert_eq!(pos, 3, "the marker must land at column 3");
}

/// 3 — `cursor_marker_line` clamps the column to the rectangle's last
/// cell when `col >= width`, so an overflowing caret never panics and
/// never produces a marker past the right edge.
#[test]
fn cursor_marker_line_clamps_oversized_column_to_last_cell() {
    let (_, line) = cursor_marker_line(0, 99, 10);
    let text = plain_text(line.as_slice());
    assert_eq!(text.chars().count(), 10, "line width stays at 10");
    assert_eq!(
        text.chars().last(),
        Some(PUBLIC_CURSOR_MARKER),
        "the marker must land at the last cell when col overflows"
    );
}

/// 4 — `cursor_marker_line` always emits at least one cell (even when
/// `width == 0`) so callers can rely on a non-empty line and the
/// layout engine never has to special-case "no marker".
#[test]
fn cursor_marker_line_handles_zero_width() {
    let (_, line) = cursor_marker_line(0, 0, 0);
    let text = plain_text(line.as_slice());
    assert!(
        text.contains(PUBLIC_CURSOR_MARKER),
        "even with width=0 the marker must appear"
    );
}

/// 5 — `Prompt::cursor_position(width, max_rows, scroll)` returns the
/// caret's cell coordinates inside the composer's rectangle. An empty
/// buffer pins the caret to `(0, label_width)` so the IME candidate
/// window still anchors at the label's right edge.
#[test]
fn empty_buffer_cursor_position_anchors_at_the_label() {
    let prompt = Prompt::new("> ");
    let (row, col) = prompt.cursor_position(40, 1, 0);
    assert_eq!(row, 0);
    assert_eq!(col, 2, "empty buffer — caret sits just past the label");
}

/// 6 — A non-empty buffer with the caret at the start places the
/// cursor at `(0, label_width)`. Typing more text shifts `col` to the
/// right.
#[test]
fn cursor_position_walks_the_caret_to_the_end() {
    let mut prompt = Prompt::new("> ");
    type_text(&mut prompt, "hello");
    prompt.place_cursor(0);
    let (row_start, col_start) = prompt.cursor_position(40, 1, 0);
    assert_eq!(row_start, 0);
    assert_eq!(col_start, 2, "caret at start — col = label_width");
    // Type more text — caret must advance.
    type_text(&mut prompt, " world");
    let (row_end, col_end) = prompt.cursor_position(40, 1, 0);
    assert_eq!(row_end, 0);
    assert!(
        col_end > col_start,
        "typing more text must advance the column; got {col_end}"
    );
}

/// 7 — `cursor_position` clamps to the rectangle's last cell when the
/// caret is past the right edge (long drafts truncated by the
/// composer's wrap), so the IME anchor never falls off-screen.
#[test]
fn cursor_position_clamps_to_last_cell() {
    let mut prompt = Prompt::new("> ");
    type_text(&mut prompt, "a".repeat(100).as_str());
    let (row, col) = prompt.cursor_position(10, 1, 0);
    assert_eq!(row, 0);
    assert!(
        col <= 9,
        "col must clamp to the last cell of a 10-column rectangle; got {col}"
    );
}

/// 8 — Roundtrip: `Prompt::cursor_position` + `cursor_marker_line`
/// produce a line whose marker sits at the cell `cursor_position`
/// returned. This is the IME-facing contract — the marker must land
/// where the renderer paints the caret.
#[test]
fn prompt_cursor_position_and_cursor_marker_line_roundtrip() {
    let mut prompt = Prompt::new("> ");
    type_text(&mut prompt, "hello world");
    let (row, col) = prompt.cursor_position(40, 1, 0);
    let (_, line) = cursor_marker_line(row as usize, col as usize, 40);
    let text = plain_text(line.as_slice());
    let pos = text.find(PUBLIC_CURSOR_MARKER).expect("marker present");
    assert_eq!(pos, col as usize, "marker must land at the cursor's column");
}

/// 9 — `Prompt::cursor_position` returns the **window-relative** row,
/// not the row of the underlying draft. A long draft scrolled past
/// row 0 places the caret's window row at the appropriate visible
/// offset, so the IME still anchors to the visible cell.
#[test]
fn cursor_position_returns_window_relative_row() {
    let mut prompt = Prompt::new("> ");
    // A 20-line draft in a 3-row window — the caret's window row is
    // bounded by `max_rows`.
    let body: String = (0..20)
        .map(|i| format!("line {i}\n"))
        .collect::<String>()
        .trim_end_matches('\n')
        .to_string();
    type_text(&mut prompt, body.as_str());
    let (row, _col) = prompt.cursor_position(40, 3, 0);
    assert!(
        (row as usize) < 3,
        "window-relative row must be within the visible window; got {row}"
    );
}

/// 10 — The paint path applies the **reversed-video** modifier to the
/// `▍` caret glyph so the IME candidate window anchors to the same
/// cell the user sees. Build the composer's rendered lines via
/// `Prompt::render_lines` and verify the line that owns the caret
/// contains exactly one `▍` glyph.
#[test]
fn rendered_prompt_carries_the_cursor_glyph_in_its_window_row() {
    let mut prompt = Prompt::new("> ");
    type_text(&mut prompt, "hello");
    let (lines, _scroll) = prompt.render_lines(40, 1, 0);
    assert_eq!(lines.len(), 1, "single-row composer renders one line");
    let caret_count = lines[0].chars().filter(|c| *c == '▍').count();
    assert_eq!(
        caret_count, 1,
        "the rendered composer must contain exactly one caret glyph; got {lines:?}"
    );
}

/// 11 — The `inverse()` builder on `SpanStyle` produces the
/// reversed-video modifier that the paint path applies. Pin the
/// helper at the styled layer so the IME-facing wiring does not drift.
#[test]
fn inverse_style_sets_the_reversed_modifier() {
    let style = SpanStyle::PLAIN.inverse();
    assert!(style.inverse, "SpanStyle.inverse field must be true");
    // A non-plain theme propagates the modifier to ratatui's
    // `Modifier::REVERSED`. The paint path uses this exact code path.
    let theme = builtin_theme("dark", ColorMode::TrueColor).expect("dark theme");
    assert!(!theme.is_plain(), "test theme must be non-plain");
    let ratatui_style = style.to_style(&theme);
    assert_eq!(
        ratatui_style.add_modifier,
        Modifier::REVERSED,
        "inverse() must surface as the REVERSED ratatui modifier under a non-plain theme"
    );
}

/// 12 — The internal `layout::CURSOR_MARKER` (a non-printable `␁`)
/// stays distinct from the public `ts_compat::CURSOR_MARKER`
/// (`\u{200B}`). They serve different purposes: the internal sentinel
/// is for the layout engine's row scan, the public one is for
/// plugin authors. Confirmed at the constant level.
#[test]
fn internal_and_public_cursor_markers_are_distinct() {
    let internal = pi_tui::layout::CURSOR_MARKER;
    assert_eq!(
        internal, '\u{1}',
        "internal CURSOR_MARKER stays at U+0001 (non-printable)"
    );
    assert_ne!(
        internal, PUBLIC_CURSOR_MARKER,
        "internal and public markers must not collide"
    );
}

/// 13 — `cursor_marker_line` emits a line whose only span (apart from
/// padding) carries the public marker in `PLAIN` style. The
/// IME-facing line is intentionally unstyled so the consuming renderer
/// (the App or a plugin) decides what color/modifier to apply.
#[test]
fn cursor_marker_line_emits_plain_styled_spans() {
    let (_, line) = cursor_marker_line(0, 2, 6);
    assert!(!line.is_empty());
    // The marker span exists and is PLAIN — the IME bridge expects
    // this so the caller can decide whether to apply the `inverse`
    // modifier themselves (and so the marker is not double-styled).
    let marker_span = line
        .iter()
        .find(|s| s.text.contains(PUBLIC_CURSOR_MARKER))
        .expect("marker span");
    assert_eq!(marker_span.style, SpanStyle::PLAIN);
}

/// 14 — Multi-row composer: `cursor_position` reports a window row
/// inside `[0, max_rows)`. The marker line roundtrips with that row.
#[test]
fn cursor_position_handles_multi_row_composer() {
    let mut prompt = Prompt::new("> ");
    // 5 visible rows in a 12-column body — the draft wraps to ~3 rows
    // with these short lines, well within `max_rows`.
    let body: String = (0..5)
        .map(|i| format!("line {i}\n"))
        .collect::<String>()
        .trim_end_matches('\n')
        .to_string();
    type_text(&mut prompt, body.as_str());
    let (row, col) = prompt.cursor_position(20, 3, 0);
    assert!((row as usize) < 3, "row must be within the visible window");
    let (_, line) = cursor_marker_line(row as usize, col as usize, 20);
    let text = plain_text(line.as_slice());
    assert!(text.contains(PUBLIC_CURSOR_MARKER));
    let pos = text.find(PUBLIC_CURSOR_MARKER).expect("marker");
    assert_eq!(pos, col as usize);
}

/// 15 — The marker emitted by `cursor_marker_line` is exactly the
/// public, TS-compatible value. This is the strongest invariant for
/// plugin authors: they can search for `pi_tui::CURSOR_MARKER` in the
/// composer's output and find it.
#[test]
fn cursor_marker_line_emits_exactly_the_public_marker() {
    let (_, line) = cursor_marker_line(0, 1, 4);
    let combined: String = line
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join("");
    assert!(
        combined.contains(PUBLIC_CURSOR_MARKER),
        "the marker must be the public value; got {combined:?}"
    );
    // No other "marker-like" characters leak.
    assert_eq!(
        combined.chars().filter(|c| *c == PUBLIC_CURSOR_MARKER).count(),
        1,
        "exactly one marker per line; got {combined:?}"
    );
}