//! LUM-1422 — mouse selection records **display columns**, not characters.
//!
//! Upstream's fullscreen selection measures with `visibleWidth`:
//! `getWordSelection` computes each segment's end as
//! `start + visibleWidth(segment.segment)` and `getLineSelection` ends at
//! `visibleWidth(line)` (`packages/tui/src/tui-alt-screen.ts:1160-1198`). The
//! port accumulated `chars().count()`, which is the same number for ASCII and
//! half the real width for CJK — so on a row containing wide glyphs the pointer
//! column was compared against character offsets, the highlight covered the
//! wrong cells, and the copied text came from the wrong slice.
//!
//! These tests measure the **rendered cells** (a `ratatui::Buffer` modifier) and
//! the copied string, so they cannot agree with the layout by sharing its
//! arithmetic.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::input::{InputEvent, MouseButton, MouseGesture, MouseGestureKind};
use pi_tui::width::columns;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

const WIDTH: u16 = 40;
/// Message viewport = height - status bar - prompt row.
const HEIGHT: u16 = 10;

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

/// A one-viewport log whose only row is `"> 中文标题 abcdef"`.
///
/// Columns: `"> "` = 2, `中文标题` = 8 (2 each), the space = 1, so `abcdef`
/// starts at display column 11 and ends at 17.
fn app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "selection-columns".into(),
            ..AppConfig::default()
        },
    );
    app.info("中文标题 abcdef");
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    app
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

/// One full click: press + release on the same cell.
fn click(app: &mut App, x: u16, y: u16) {
    let _ = app.step(press(x, y));
    let _ = app.step(release(x, y));
}

fn buffer(app: &mut App) -> Buffer {
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    buf
}

fn reversed_columns(buf: &Buffer, y: u16) -> Vec<u16> {
    (0..WIDTH)
        .filter(|x| {
            buf.cell((*x, y))
                .map(|cell| cell.modifier.contains(Modifier::REVERSED))
                .unwrap_or(false)
        })
        .collect()
}

#[test]
fn the_rendered_row_places_abcdef_after_eight_cjk_columns() {
    let app = app();
    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    let row = &snapshot.lines[0];
    assert!(row.contains("中文标题 abcdef"), "row: {row:?}");
    assert_eq!(
        columns(&row[..row.find("abcdef").expect("match")]),
        11,
        "`abcdef` starts at display column 11"
    );
}

#[test]
fn a_character_drag_copies_the_selected_columns() {
    let mut app = app();
    let _ = app.step(press(12, 0));
    let _ = app.step(drag(16, 0));
    // Columns 12..=16 are `bcdef`: column 11 is `a`.
    assert_eq!(app.selection_text().as_deref(), Some("bcdef"));
}

#[test]
fn a_drag_that_ends_inside_a_wide_glyph_takes_the_whole_glyph() {
    let mut app = app();
    let _ = app.step(press(2, 0));
    // Column 5 is the cell `文` hides; the selection must stop after `中文`.
    let _ = app.step(drag(5, 0));
    assert_eq!(app.selection_text().as_deref(), Some("中文"));
}

#[test]
fn a_double_click_after_cjk_text_selects_the_whole_word() {
    let mut app = app();
    click(&mut app, 13, 0);
    assert_eq!(
        app.selection_text().as_deref(),
        None,
        "one click selects nothing"
    );
    click(&mut app, 13, 0);
    assert_eq!(
        app.selection_text().as_deref(),
        Some("abcdef"),
        "the pointer column names the word `abcdef`, not a slice of it"
    );
}

#[test]
fn a_triple_click_selects_the_whole_cjk_line() {
    let mut app = app();
    click(&mut app, 13, 0);
    click(&mut app, 13, 0);
    click(&mut app, 13, 0);
    assert_eq!(app.selection_text().as_deref(), Some("> 中文标题 abcdef"));
}

#[test]
fn the_selection_highlight_covers_the_word_columns() {
    let mut app = app();
    click(&mut app, 13, 0);
    click(&mut app, 13, 0);
    let buf = buffer(&mut app);
    assert_eq!(
        reversed_columns(&buf, 0),
        (11..17).collect::<Vec<u16>>(),
        "the highlight is the word's own columns, 11..17"
    );
}
