//! LUM-1426 — the pointer, the selection and the search marks are measured in
//! **terminal cells**, while the selection text is cut in **characters**.
//!
//! LUM-1418 fixed the layout half of the CJK column bug: every rendered row is
//! measured in terminal columns, so a Chinese paragraph wraps once instead of
//! twice. The pointer half was still open, and it is the half a user feels
//! directly:
//!
//! * a click at screen column `x` was read as *character offset* `x`, but a CJK
//!   line holds half as many characters as it fills cells. In `> 你好世界` the
//!   glyph `好` sits on cells 4–5 while the character at offset 4 is `世` and
//!   the one at offset 5 is `界`; a click aimed at `好` therefore resolved to a
//!   **neighbouring** ideograph, and a double click selected (and copied) the
//!   wrong character;
//! * the reversed-video selection highlight painted one cell per character, so
//!   it covered only the left half of each wide glyph and drifted further left
//!   the further right the selection went;
//! * the transcript-search span table was built in graphemes, so a highlight on
//!   a CJK line was painted at half its real column and under the wrong glyph.
//!
//! The three defects share one cause: the screen is addressed in cells and the
//! text is sliced in characters, and nothing converted between them. These
//! tests pin both directions:
//!
//! * pointer cell → character offset, snapped to the **whole** glyph drawn on
//!   that cell ([`pi_tui::width::char_index_at_column`], upstream's
//!   `getGraphemeCellRange`);
//! * character range → cell span ([`pi_tui::width::columns_before`]).
//!
//! `frame_dump_for_the_screenshot` prints a real 100×30 frame with a CJK
//! selection and a CJK search mark; `scripts/frame_to_png.py` renders it into
//! `docs/screenshots/lum1426-pointer-columns.png`.
//!
//! # The composer
//!
//! The same mapping drives the composer: a click on a draft row places the
//! caret on the glyph the pointer hit (upstream's `Editor.handleMouse` click
//! branch, `packages/tui/src/components/editor.ts:620-666`). Before this the
//! composer had no pointer target at all, so clicking a draft did nothing —
//! while codex, Martty and the TypeScript pi all place the caret.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::input::{
    InputEvent, Key, KeyCode, KeyModifiers, MouseButton, MouseGesture, MouseGestureKind,
};
use pi_tui::message::MessageItem;
use pi_tui::width::{columns, columns_before};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

const WIDTH: u16 = 100;
const ROWS: u16 = 30;

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

fn cjk_app(lines: &[&str]) -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1426-pointer-columns".into(),
            ..AppConfig::default()
        },
    );
    for line in lines {
        app.messages_mut().push(MessageItem::user(*line));
    }
    let _ = app.render_snapshot(WIDTH, ROWS);
    app
}

fn buffer(app: &mut App) -> Buffer {
    let area = Rect::new(0, 0, WIDTH, ROWS);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    buf
}

/// The visible transcript rows as plain text, paired with the screen row they
/// are drawn on (the message area starts at the top of the frame).
fn visible(app: &App) -> Vec<(usize, String)> {
    let (_, height) = app.viewport();
    let (_, lines) = app.messages().visible_lines(WIDTH, height);
    lines
        .iter()
        .enumerate()
        .map(|(row, line)| (row, pi_tui::styled::plain_text(line)))
        .collect()
}

/// Screen row and text of the visible line containing `needle`.
fn line_at(app: &App, needle: &str) -> (usize, String) {
    visible(app)
        .into_iter()
        .find(|(_, text)| text.contains(needle))
        .unwrap_or_else(|| panic!("no visible line contains {needle:?}"))
}

/// Character offset of `needle` inside `text`.
fn char_index_of(text: &str, needle: &str) -> usize {
    let byte = text
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {text:?}"));
    text[..byte].chars().count()
}

/// Screen cell of `needle`'s first glyph inside `text`.
fn cell_of(text: &str, needle: &str) -> usize {
    columns_before(text, char_index_of(text, needle))
}

/// One past the last screen cell of `needle` inside `text`.
fn cell_end_of(text: &str, needle: &str) -> usize {
    columns_before(text, char_index_of(text, needle) + needle.chars().count())
}

fn gesture(kind: MouseGestureKind, x: u16, y: u16) -> InputEvent {
    InputEvent::gesture(MouseGesture::new(kind, x, y, false))
}

fn press(x: u16, y: u16) -> InputEvent {
    gesture(MouseGestureKind::Press(MouseButton::Left), x, y)
}

fn drag(x: u16, y: u16) -> InputEvent {
    gesture(MouseGestureKind::Drag(MouseButton::Left), x, y)
}

fn release(x: u16, y: u16) -> InputEvent {
    gesture(MouseGestureKind::Release(MouseButton::Left), x, y)
}

fn reversed(buf: &Buffer, x: u16, y: u16) -> bool {
    buf.cell((x, y))
        .map(|cell| cell.modifier.contains(Modifier::REVERSED))
        .unwrap_or(false)
}

fn underlined(buf: &Buffer, x: u16, y: u16) -> bool {
    buf.cell((x, y))
        .map(|cell| cell.modifier.contains(Modifier::UNDERLINED))
        .unwrap_or(false)
}

#[test]
fn a_click_on_the_right_half_of_a_wide_glyph_selects_that_glyph() {
    let mut app = cjk_app(&["你好世界"]);
    let (row, text) = line_at(&app, "你好世界");
    // The second cell of 好 — the one a CJK reader's pointer lands on when
    // aiming at the glyph.
    let half_of_hao = cell_of(&text, "好") + 1;

    app.step(press(half_of_hao as u16, row as u16));
    app.step(drag((cell_end_of(&text, "界") - 1) as u16, row as u16));
    let (start, _) = app.selection_bounds().expect("a live selection");
    assert_eq!(
        start.1,
        char_index_of(&text, "好"),
        "the press must resolve to 好, not to the character whose offset equals its cell"
    );
    assert_eq!(app.selection_text().as_deref(), Some("好世界"));
}

#[test]
fn a_drag_across_cjk_copies_whole_glyphs() {
    let mut app = cjk_app(&["你好世界和平"]);
    let (row, text) = line_at(&app, "你好世界和平");
    app.step(press(cell_of(&text, "你") as u16, row as u16));
    app.step(drag(cell_end_of(&text, "好") as u16 - 1, row as u16));
    assert_eq!(app.selection_text().as_deref(), Some("你好"));
}

#[test]
fn a_double_click_selects_the_wide_glyph_under_the_pointer() {
    let mut app = cjk_app(&["你好世界"]);
    let (row, text) = line_at(&app, "你好世界");
    let half_of_hao = cell_of(&text, "好") + 1;

    // Two presses on the same cell inside the double-click window.
    app.step(press(half_of_hao as u16, row as u16));
    app.step(release(half_of_hao as u16, row as u16));
    app.step(press(half_of_hao as u16, row as u16));

    assert_eq!(
        app.selection_text().as_deref(),
        Some("好"),
        "the word under the pointer is 好, not the character at offset 好's second cell"
    );
}

#[test]
fn the_selection_highlight_covers_every_cell_of_a_wide_glyph() {
    let mut app = cjk_app(&["你好世界"]);
    let (row, text) = line_at(&app, "你好世界");
    app.step(press(cell_of(&text, "好") as u16, row as u16));
    app.step(drag((cell_end_of(&text, "世") - 1) as u16, row as u16));
    assert_eq!(app.selection_text().as_deref(), Some("好世"));

    let buf = buffer(&mut app);
    let from = cell_of(&text, "好");
    let to = cell_end_of(&text, "世");
    for x in from..to {
        assert!(
            reversed(&buf, x as u16, row as u16),
            "cell {x} of 好世 must be highlighted"
        );
    }
    assert!(
        !reversed(&buf, (from - 1) as u16, row as u16),
        "the second cell of 你 must stay unhighlighted"
    );
    assert!(
        !reversed(&buf, to as u16, row as u16),
        "the first cell of 界 must stay unhighlighted"
    );
}

#[test]
fn a_mixed_line_keeps_the_press_on_the_glyph_it_hit() {
    // ASCII before the CJK run shifts the cell index away from the character
    // index even further: `ok ` is 3 cells and 3 characters, but 你 costs 2.
    let mut app = cjk_app(&["ok 你好 world"]);
    let (row, text) = line_at(&app, "ok 你好");
    let half_of_hao = cell_of(&text, "好") + 1;
    app.step(press(half_of_hao as u16, row as u16));
    app.step(drag((cell_end_of(&text, "你") - 1) as u16, row as u16));
    assert_eq!(app.selection_text().as_deref(), Some("你好"));
    assert_eq!(columns("你好"), 4, "sanity: 你好 is 4 terminal cells");
}

#[test]
fn search_segments_report_cell_columns_on_a_cjk_line() {
    let mut app = cjk_app(&["你好世界"]);
    let (row, text) = line_at(&app, "你好世界");
    app.step(InputEvent::Key(Key::new(
        KeyCode::Char('F'),
        KeyModifiers::CONTROL,
    )));
    for ch in "世界".chars() {
        app.step(InputEvent::Key(Key::char(ch)));
    }
    assert_eq!(app.search_match_index(), Some(0));

    let matches = app.search_matches();
    let segment = matches[0].segments[0];
    assert_eq!(segment.row, row, "the match is on the rendered row");
    assert_eq!(segment.start_col, cell_of(&text, "世界"));
    assert_eq!(segment.end_col, cell_end_of(&text, "世界"));
    assert!(
        segment.end_col - segment.start_col == 4,
        "two wide glyphs own four cells"
    );
}

#[test]
fn a_cjk_search_match_is_painted_under_the_glyphs_it_matched() {
    let mut app = cjk_app(&["你好世界"]);
    let (row, text) = line_at(&app, "你好世界");
    app.step(InputEvent::Key(Key::new(
        KeyCode::Char('F'),
        KeyModifiers::CONTROL,
    )));
    for ch in "世界".chars() {
        app.step(InputEvent::Key(Key::char(ch)));
    }

    let buf = buffer(&mut app);
    let row = row as u16;
    let from = cell_of(&text, "世界");
    let to = cell_end_of(&text, "世界");
    for x in from..to {
        assert!(
            reversed(&buf, x as u16, row) || underlined(&buf, x as u16, row),
            "cell {x} of the current 世界 match must carry the mark"
        );
    }
    assert!(
        !underlined(&buf, (from - 1) as u16, row) && !reversed(&buf, (from - 1) as u16, row),
        "the second cell of 好 must stay clean"
    );
    assert!(
        !underlined(&buf, to as u16, row) && !reversed(&buf, to as u16, row),
        "one past the match must stay clean"
    );
}

/// The frame row the composer's first draft row is drawn on, and the column
/// its label ends at.
fn composer_row(app: &App, cols: u16, rows: u16) -> (usize, usize) {
    let snapshot = app.render_snapshot(cols, rows);
    let row = snapshot
        .lines
        .iter()
        .position(|line| line.starts_with("> "))
        .expect("the composer row is on screen");
    (row, columns("> "))
}

/// An App with no transcript and `draft` in the composer, rendered once so
/// the pointer geometry is known.
fn composer_app(draft: &str, cols: u16, rows: u16) -> App {
    let mut app = cjk_app(&[]);
    app.prompt_mut().editor_mut().insert_str(draft);
    let _ = app.render_snapshot(cols, rows);
    app
}

fn click(app: &mut App, x: u16, y: u16) -> StepOutcome {
    let pressed = app.step(press(x, y));
    app.step(release(x, y));
    pressed
}

#[test]
fn a_click_in_the_composer_places_the_caret_on_the_clicked_glyph() {
    let (cols, rows) = (100u16, 30u16);
    let draft = "你好世界";
    let mut app = composer_app(draft, cols, rows);
    let (row, label) = composer_row(&app, cols, rows);
    // The second cell of 好 — a click a CJK reader aims at the glyph, and
    // the cell whose index would otherwise be read as character 3 (好).
    let x = label + cell_of(draft, "好") + 1;
    assert_eq!(click(&mut app, x as u16, row as u16), StepOutcome::Redraw);
    assert_eq!(app.prompt().cursor(), char_index_of(draft, "好"));
}

#[test]
fn a_click_on_the_composer_label_puts_the_caret_at_the_row_start() {
    let (cols, rows) = (100u16, 30u16);
    let mut app = composer_app("你好世界", cols, rows);
    let (row, _) = composer_row(&app, cols, rows);
    click(&mut app, 0, row as u16);
    assert_eq!(app.prompt().cursor(), 0);
}

#[test]
fn a_click_on_a_wrapped_draft_row_lands_on_that_row() {
    // 11 columns: the label takes 2, so the body is 9 — 你好世界 is 8 cells
    // and 你 would need 2 more, so the draft wraps into two rows.
    let (cols, rows) = (11u16, 14u16);
    let draft = "你好世界你";
    let mut app = composer_app(draft, cols, rows);
    let (row, label) = composer_row(&app, cols, rows);

    // The second row starts at the fifth character.
    click(&mut app, label as u16, row as u16 + 1);
    assert_eq!(app.prompt().cursor(), 4, "row 1 starts at 你 (index 4)");

    // A click past the end of the *first* row keeps the caret on that row
    // (upstream's `isLastSegment` correction) instead of jumping to row 1.
    click(&mut app, (label + 8) as u16, row as u16);
    assert_eq!(
        app.prompt().cursor(),
        3,
        "the caret stays on the row the pointer hit"
    );
}

#[test]
fn a_composer_click_does_not_start_a_transcript_selection() {
    let (cols, rows) = (100u16, 30u16);
    let mut app = composer_app("你好世界", cols, rows);
    let (row, label) = composer_row(&app, cols, rows);
    click(&mut app, (label + 2) as u16, row as u16);
    assert!(!app.has_selection(), "the composer owns the click");
    assert_eq!(app.take_clipboard_request(), None);
}

#[test]
fn a_drag_out_of_the_composer_does_not_start_a_selection() {
    let (cols, rows) = (100u16, 30u16);
    let mut app = cjk_app(&["你好世界"]);
    let (row, _) = line_at(&app, "你好世界");
    app.prompt_mut().editor_mut().insert_str("draft");
    let _ = app.render_snapshot(cols, rows);
    let (composer_y, label) = composer_row(&app, cols, rows);

    app.step(press((label + 1) as u16, composer_y as u16));
    app.step(drag((label + 1) as u16, row as u16));
    app.step(release((label + 1) as u16, row as u16));
    assert!(
        !app.has_selection(),
        "a drag that started on the composer must not select the transcript"
    );
}

/// The frame the screenshot is rendered from: a CJK transcript with a live
/// selection over `好世` and a search match on `世界`.
///
/// The dump carries the selection and the search mark as SGR codes (`7`
/// reverse, `1` bold, `4` underline) so `scripts/frame_to_png.py` can paint
/// the highlight — a plain text dump would show the layout but hide exactly
/// the thing this round fixes.
#[test]
fn frame_dump_for_the_screenshot() {
    let mut app = cjk_app(&[
        "你好世界，这是一段中文文本",
        "第二行：混合 mixed 中英文 content",
    ]);
    let (row, text) = line_at(&app, "你好世界");
    app.step(press(cell_of(&text, "好") as u16, row as u16));
    app.step(drag((cell_end_of(&text, "世") - 1) as u16, row as u16));
    app.step(InputEvent::Key(Key::new(
        KeyCode::Char('F'),
        KeyModifiers::CONTROL,
    )));
    for ch in "世界".chars() {
        app.step(InputEvent::Key(Key::char(ch)));
    }

    let snapshot = app.render_snapshot(WIDTH, ROWS);
    for (index, line) in snapshot.lines.iter().enumerate() {
        assert!(
            columns(line) <= WIDTH as usize,
            "frame row {index} overflows: {line:?}"
        );
    }

    let area = Rect::new(0, 0, WIDTH, ROWS);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    println!("FRAME DUMP cols={WIDTH} rows={ROWS}");
    for y in 0..ROWS {
        let line = dump_row(&buf, y, WIDTH);
        println!("|{line}|");
    }
    println!("END FRAME DUMP");
}

/// The composer frame for the screenshot: a CJK draft whose caret was placed
/// by clicking on a glyph two thirds of the way in.
#[test]
fn frame_dump_for_the_composer_screenshot() {
    let (cols, rows) = (72u16, 12u16);
    let draft = "你好世界，按列宽定位光标";
    let mut app = composer_app(draft, cols, rows);
    let (row, label) = composer_row(&app, cols, rows);
    let x = label + cell_of(draft, "按") + 1;
    assert_eq!(click(&mut app, x as u16, row as u16), StepOutcome::Redraw);
    assert_eq!(app.prompt().cursor(), char_index_of(draft, "按"));

    let area = Rect::new(0, 0, cols, rows);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    println!("FRAME DUMP cols={cols} rows={rows}");
    for y in 0..rows {
        println!("|{}|", dump_row(&buf, y, cols));
    }
    println!("END FRAME DUMP");
}

/// One buffer row as text plus SGR codes for the modifiers the App painted.
fn dump_row(buf: &Buffer, y: u16, cols: u16) -> String {
    let mut out = String::new();
    let mut active = String::new();
    let mut skip = 0usize;
    for x in 0..cols {
        let Some(cell) = buf.cell((x, y)) else {
            break;
        };
        if skip > 0 {
            // The second cell of a wide glyph: the symbol is already emitted,
            // and the dump must carry each glyph once (the renderer pads by
            // terminal columns).
            skip -= 1;
            continue;
        }
        skip = columns(cell.symbol()).saturating_sub(1);
        let want = sgr(cell.modifier);
        if want != active {
            out.push_str("\u{1b}[0m");
            out.push_str(&want);
            active = want;
        }
        out.push_str(cell.symbol());
    }
    out.push_str("\u{1b}[0m");
    out
}

/// The SGR sequence that reproduces the cell's mark — the subset the frame
/// renderer understands (reverse / bold / underline).
fn sgr(modifier: Modifier) -> String {
    let mut codes: Vec<&str> = Vec::new();
    if modifier.contains(Modifier::REVERSED) {
        codes.push("7");
    }
    if modifier.contains(Modifier::BOLD) {
        codes.push("1");
    }
    if modifier.contains(Modifier::UNDERLINED) {
        codes.push("4");
    }
    if codes.is_empty() {
        String::new()
    } else {
        format!("\u{1b}[{}m", codes.join(";"))
    }
}
