//! Wide-character (CJK / emoji) layout in the composer (LUM-1336).
//!
//! A frame buffer cell is one terminal **column**, and `ratatui` measures a
//! cell's symbol with `unicode-width` when it diffs the frame. The port laid
//! the draft out one cell per *character* instead, which broke the composer in
//! three visible ways on a real PTY:
//!
//! 1. Two wide glyphs were written into two adjacent cells (2 columns each on
//!    screen), so the buffer's cells and the terminal's columns drifted apart
//!    by one per wide glyph. `Buffer::diff` then skipped the cell after each
//!    wide symbol (`to_skip = symbol_width - 1`) and left the *placeholder*
//!    from the previous frame on screen: typing `世界` rendered as `世p`.
//! 2. The wrap measured characters, so a CJK draft wrapped at twice the
//!    composer's width and the tail of a long line was painted past the area
//!    edge — invisible and uneditable.
//! 3. The `▍` marker was placed at the character index of the caret, which is
//!    not the column it is drawn at once wide glyphs are on the row.
//!
//! Every assertion reads the **rendered frame** (cell grid), not internal
//! state, so the invariants below are the ones the terminal actually sees.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::input::{
    InputEvent, Key, KeyCode, KeyModifiers, MouseButton, MouseGesture, MouseGestureKind,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const WIDTH: u16 = 40;
const HEIGHT: u16 = 14;
/// The composer's label gutter (`Prompt::new("> ")`).
const GUTTER: u16 = 2;
/// The caret glyph `Prompt` draws.
const CARET: char = '▍';

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: Some("Faux".into()),
        context_window: 1024,
        max_output_tokens: 256,
    }
}

fn app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    App::new(&agent, AppConfig::default())
}

fn frame(app: &mut App) -> Buffer {
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    buf
}

/// `x` of the last cell the symbol at `(x, y)` covers, when that symbol is
/// wider than one column.
fn covered_cells(buf: &Buffer, x: u16, y: u16) -> u16 {
    let width = buf
        .cell((x, y))
        .map(|cell| unicode_width::UnicodeWidthStr::width(cell.symbol()))
        .unwrap_or(1);
    width.saturating_sub(1) as u16
}

/// The row's symbol at `x`, or a space when the cell is out of the area.
fn symbol(buf: &Buffer, x: u16, y: u16) -> String {
    buf.cell((x, y))
        .map(|cell| cell.symbol().to_string())
        .unwrap_or_else(|| " ".into())
}

/// The row's **visible** text: cells covered by the left half of a wide
/// glyph are skipped, exactly like a terminal renders them.
fn row_text(buf: &Buffer, y: u16) -> String {
    let mut out = String::new();
    let mut x = 0u16;
    while x < buf.area.width {
        out.push_str(&symbol(buf, x, y));
        x += 1 + covered_cells(buf, x, y);
    }
    out
}

/// Columns the row occupies on screen.
fn row_width(buf: &Buffer, y: u16) -> usize {
    let mut used = 0usize;
    let mut x = 0u16;
    while x < buf.area.width {
        used += 1 + covered_cells(buf, x, y) as usize;
        x += 1 + covered_cells(buf, x, y);
    }
    used
}

/// Every composer row (a row that starts with the label or with one of the
/// continuation markers), top to bottom.
fn composer_rows(buf: &Buffer) -> Vec<(u16, String)> {
    (0..buf.area.height)
        .filter_map(|y| {
            let text = row_text(buf, y);
            let starts_here = text.starts_with("> ")
                || text.starts_with("  ")
                || text.starts_with('↑')
                || text.starts_with('↓')
                || text.starts_with('↕');
            (starts_here && text.trim_end().len() > 2).then_some((y, text))
        })
        .collect()
}

/// Cell column of the caret marker in a frame, if it is on screen.
fn caret_cell(buf: &Buffer) -> Option<(u16, u16)> {
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            if symbol(buf, x, y).starts_with(CARET) {
                return Some((x, y));
            }
        }
    }
    None
}

/// Type `text` one character per key event — a burst, the way a terminal
/// delivers a fast typist's keystrokes.
fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        app.step_key(Key::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
}

/// The invariant the terminal depends on: a cell holding a wide symbol must be
/// followed by cells that hold nothing (`ratatui` clears them in
/// `Buffer::set_stringn`; the composer used to leave the previous frame's
/// characters there, which is what produced the ghost).
fn assert_no_cell_under_a_wide_glyph(buf: &Buffer) {
    for y in 0..buf.area.height {
        let mut x = 0u16;
        while x < buf.area.width {
            let covered = covered_cells(buf, x, y);
            for extra in 1..=covered {
                assert_eq!(
                    symbol(buf, x + extra, y),
                    " ",
                    "row {y} column {} is covered by the wide glyph at column {x} but holds {:?}",
                    x + extra,
                    symbol(buf, x + extra, y)
                );
            }
            x += 1 + covered;
        }
    }
}

// ---------------------------------------------------------------------------
// 1. A CJK draft wraps on columns, and every row fits the composer
// ---------------------------------------------------------------------------

#[test]
fn a_cjk_draft_wraps_at_the_composer_width_in_columns() {
    let mut app = app();
    // 38 body columns (40 minus the 2-cell label) hold 19 wide glyphs.
    type_text(&mut app, &"你".repeat(30));
    let buf = frame(&mut app);
    let rows: Vec<String> = composer_rows(&buf).into_iter().map(|(_, t)| t).collect();
    assert_eq!(
        rows.len(),
        2,
        "30 wide glyphs are 60 columns and cannot fit one 38-column row: {rows:#?}"
    );
    assert_eq!(
        rows[0].trim_end(),
        format!("> {}", "你".repeat(19)),
        "the first row takes 19 glyphs = 38 columns"
    );
    assert_eq!(
        rows[1].trim_end(),
        format!("  {}▍", "你".repeat(11)),
        "the remaining 11 glyphs continue on an indented row, caret after them"
    );
    for (y, _) in composer_rows(&buf) {
        assert!(
            row_width(&buf, y) <= WIDTH as usize,
            "row {y} is wider than the composer"
        );
    }
    assert_no_cell_under_a_wide_glyph(&buf);
}

#[test]
fn a_mixed_width_draft_wraps_by_columns() {
    let mut app = app();
    // "ab" (2) + 18 wide glyphs (36) = 38 columns exactly, then "cd" wraps.
    let draft = format!("ab{}cd", "中".repeat(18));
    type_text(&mut app, &draft);
    let buf = frame(&mut app);
    let rows: Vec<String> = composer_rows(&buf).into_iter().map(|(_, t)| t).collect();
    assert_eq!(rows[0].trim_end(), format!("> ab{}", "中".repeat(18)));
    assert_eq!(rows[1].trim_end(), "  cd▍");
    assert_no_cell_under_a_wide_glyph(&buf);
}

// ---------------------------------------------------------------------------
// 2. No ghost: a burst of wide glyphs leaves nothing of the placeholder behind
// ---------------------------------------------------------------------------

#[test]
fn a_burst_of_wide_glyphs_leaves_no_placeholder_cells_behind() {
    let mut app = app();
    // The first frame paints the placeholder; the next one must clear it
    // completely, including the cells a wide glyph covers.
    let before = frame(&mut app);
    assert!(
        row_text(&before, HEIGHT - 2).contains("type a prompt"),
        "the empty composer shows the placeholder"
    );
    type_text(&mut app, "世界你好");
    let buf = frame(&mut app);
    let row = row_text(&buf, HEIGHT - 2);
    assert_eq!(row.trim_end(), "> 世界你好▍");
    for ghost in ["p", "e", "t", "y", "a", "r", "o", "m"] {
        assert!(
            !row.contains(ghost),
            "stale placeholder cell {ghost:?} survived on the composer row: {row:?}"
        );
    }
    assert_no_cell_under_a_wide_glyph(&buf);
}

// ---------------------------------------------------------------------------
// 3. The caret is drawn at the caret's *column*
// ---------------------------------------------------------------------------

#[test]
fn the_caret_sits_at_the_column_of_the_cursor_after_wide_glyphs() {
    let mut app = app();
    type_text(&mut app, "世界你好");
    let buf = frame(&mut app);
    let (x, y) = caret_cell(&buf).expect("the caret is on screen");
    // Cursor is after four wide glyphs: 2 columns of gutter + 8 columns.
    assert_eq!(x, GUTTER + 8, "caret cell on row {y}");
    assert_no_cell_under_a_wide_glyph(&buf);
}

#[test]
fn the_caret_sits_at_the_column_of_the_cursor_in_a_mixed_row() {
    let mut app = app();
    type_text(&mut app, "ab你好cd");
    let buf = frame(&mut app);
    let (x, _) = caret_cell(&buf).expect("the caret is on screen");
    // 2 (gutter) + 2 (ab) + 4 (你好) + 2 (cd) = 10.
    assert_eq!(x, GUTTER + 8);
}

#[test]
fn the_caret_stays_inside_the_composer_on_a_wrapped_cjk_draft() {
    let mut app = app();
    type_text(&mut app, &"你".repeat(30));
    let buf = frame(&mut app);
    let (x, y) = caret_cell(&buf).expect("the caret is on screen");
    assert!(x < WIDTH, "the caret is inside the composer: {x}");
    // 30 glyphs at 19 per row: the caret is at the end of the second row.
    let rows = composer_rows(&buf);
    assert_eq!(y, rows[1].0, "the caret is on the row being typed into");
    assert_eq!(x, GUTTER + 22, "11 glyphs = 22 columns into that row");
}

// ---------------------------------------------------------------------------
// 4. A paste of wide glyphs follows the same rules
// ---------------------------------------------------------------------------

#[test]
fn a_pasted_cjk_block_wraps_by_columns_too() {
    let mut app = app();
    app.step_paste(&"好".repeat(20));
    let buf = frame(&mut app);
    let rows: Vec<String> = composer_rows(&buf).into_iter().map(|(_, t)| t).collect();
    assert_eq!(rows[0].trim_end(), format!("> {}", "好".repeat(19)));
    assert_eq!(rows[1].trim_end(), "  好▍");
    assert_no_cell_under_a_wide_glyph(&buf);
}

// ---------------------------------------------------------------------------
// 5. The pointer addresses the same columns the renderer painted
// ---------------------------------------------------------------------------

#[test]
fn a_click_on_the_second_cell_of_a_wide_glyph_caret_before_that_glyph() {
    let mut app = app();
    type_text(&mut app, "你好世界");
    let _ = frame(&mut app);
    // 你 = columns 2-3, 好 = 4-5, 世 = 6-7, 界 = 8-9. Click the second cell
    // of 世 (column 7): the caret goes before 世, i.e. after 你好.
    // A click is a press and its release on the same cell.
    let click = |kind| InputEvent::gesture(MouseGesture::new(kind, GUTTER + 5, HEIGHT - 2, false));
    app.step(click(MouseGestureKind::Press(MouseButton::Left)));
    app.step(click(MouseGestureKind::Release(MouseButton::Left)));
    let buf = frame(&mut app);
    let (x, _) = caret_cell(&buf).expect("the caret is on screen");
    assert_eq!(x, GUTTER + 4, "caret after 你好 (4 columns)");
}
