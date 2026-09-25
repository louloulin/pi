//! Selection granularity (word / line) and drag edge auto-scroll.
//!
//! Upstream's fullscreen TUI lets a double click select the word under the
//! pointer and a triple click the whole line (`SelectionGranularity`,
//! `packages/tui/src/tui-alt-screen.ts:104`), and keeps scrolling while a
//! drag rests on the viewport's top or bottom row
//! (`updateSelectionAutoScroll`, `tui-alt-screen.ts:1247-1297`). The App
//! reconstructs the click count from press timing because crossterm exposes
//! none, and advances the auto-scroll on the draw beat because this crate
//! has no timer thread — see the `app` module docs for both deviations.

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::input::{InputEvent, MouseButton, MouseGesture, MouseGestureKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

const WIDTH: u16 = 40;
/// Snapshot height; the message viewport is this minus the status bar,
/// the prompt row, and the editor border row (Phase 2 / G3), i.e. 7 rows.
const HEIGHT: u16 = 10;
const VIEWPORT: usize = (HEIGHT - 3) as usize;

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

fn app_with_lines(lines: &[&str]) -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        std::sync::Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "granularity".into(),
            ..AppConfig::default()
        },
    );
    for line in lines {
        app.info(line.to_string());
    }
    // Render once so `viewport` / `viewport_origin` describe a real screen.
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    app
}

/// A log with exactly one viewport of lines, so row `i` is line `i`.
fn single_viewport_app() -> App {
    app_with_lines(&[
        "filler a",
        "filler b",
        "filler c",
        "alpha beta-gamma delta",
        "filler d",
        "filler e",
        "filler f",
        "filler g",
    ])
}

/// A log long enough to scroll, showing its tail: row 0 is line 32.
fn scrollable_app() -> App {
    let lines: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    app_with_lines(&refs)
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

/// One full click: press + release on the same cell. Returns the outcome of
/// the press (the release is always `Idle` for an empty click).
fn click(app: &mut App, x: u16, y: u16) -> StepOutcome {
    let outcome = app.step(press(x, y));
    app.step(release(x, y));
    outcome
}

#[test]
fn double_click_selects_the_word_under_the_pointer() {
    let mut app = single_viewport_app();
    // Row 2 = "> alpha beta-gamma delta"; col 4 is inside "alpha".
    let _ = click(&mut app, 4, 2);
    assert_eq!(
        app.selection_text().as_deref(),
        None,
        "one click selects nothing"
    );
    let _ = click(&mut app, 4, 2);
    assert_eq!(app.selection_text().as_deref(), Some("alpha"));
}

#[test]
fn double_click_glues_tokens_across_a_joiner() {
    let mut app = single_viewport_app();
    // Col 10 is inside "beta"; "-" joins it to "gamma".
    let _ = click(&mut app, 10, 2);
    let _ = click(&mut app, 10, 2);
    assert_eq!(app.selection_text().as_deref(), Some("beta-gamma"));
}

#[test]
fn triple_click_selects_the_whole_line() {
    let mut app = single_viewport_app();
    let _ = click(&mut app, 10, 2);
    let _ = click(&mut app, 10, 2);
    let _ = click(&mut app, 10, 2);
    assert_eq!(
        app.selection_text().as_deref(),
        Some("> alpha beta-gamma delta")
    );
}

#[test]
fn the_click_count_cycles_back_to_character_after_three() {
    let mut app = single_viewport_app();
    let _ = click(&mut app, 10, 2);
    let _ = click(&mut app, 10, 2);
    let _ = click(&mut app, 10, 2);
    assert_eq!(
        app.selection_text().as_deref(),
        Some("> alpha beta-gamma delta"),
        "third click is a line selection"
    );
    // The fourth click restarts the cycle at a single character.
    let _ = click(&mut app, 10, 2);
    assert_eq!(app.selection_text().as_deref(), None);
}

#[test]
fn a_second_click_on_another_word_is_a_fresh_character_selection() {
    let mut app = single_viewport_app();
    let _ = click(&mut app, 4, 2);
    // A different word column restarts the count, even within the interval.
    let _ = click(&mut app, 10, 2);
    assert_eq!(app.selection_text().as_deref(), None);
}

#[test]
fn a_slow_second_click_is_a_character_selection() {
    let mut app = single_viewport_app();
    let _ = click(&mut app, 10, 2);
    std::thread::sleep(std::time::Duration::from_millis(550));
    let _ = click(&mut app, 10, 2);
    assert!(
        !app.has_selection(),
        "a pause longer than 500 ms restarts the click count"
    );
}

#[test]
fn dragging_after_a_double_click_extends_by_whole_words() {
    let mut app = single_viewport_app();
    // First click, then the second press of the double click (still held).
    let _ = click(&mut app, 4, 2);
    assert_eq!(app.step(press(4, 2)), StepOutcome::Redraw);
    assert_eq!(app.selection_text().as_deref(), Some("alpha"));

    // Col 20 is inside "delta": the focus jumps by words, not characters.
    app.step(drag(20, 2));
    assert_eq!(
        app.selection_text().as_deref(),
        Some("alpha beta-gamma delta")
    );
}

#[test]
fn a_word_selection_is_rendered_in_reverse_video() {
    let mut app = single_viewport_app();
    let _ = click(&mut app, 10, 2);
    let _ = click(&mut app, 10, 2);

    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    // "beta-gamma" occupies columns 8..18 on row 2 (Phase 2 / G3 reserves
    // a row for the editor border, shrinking the viewport by one).
    for x in 8..18 {
        assert!(reversed(&buf, x, 2), "column {x} is selected");
    }
    assert!(!reversed(&buf, 7, 2), "the space before is not selected");
    assert!(!reversed(&buf, 18, 2), "the space after is not selected");
}

#[test]
fn dragging_to_the_top_edge_autoscrolls_the_viewport() {
    let mut app = scrollable_app();
    // Phase 2 / G3 reserves a row for the editor border, so the viewport
    // shrinks from 8 to 7 — the visible tail starts at line 33 (was 32),
    // and row 3 carries line 36 (was row 4).
    assert_eq!(
        app.step(press(2, 3)),
        StepOutcome::Redraw,
        "row 3 is line 36"
    );
    // Row 0 is the top edge (line 33), so the drag arms the up auto-scroll.
    assert_eq!(app.step(drag(2, 0)), StepOutcome::Redraw);
    assert!(
        app.selection_text()
            .is_some_and(|text| text.starts_with("line 33")),
        "the selection reaches the top visible line"
    );

    // Each beat moves the viewport one line and carries the focus with it.
    assert!(app.advance_selection_autoscroll(), "first beat scrolls");
    assert!(
        app.selection_text()
            .is_some_and(|text| text.starts_with("line 32")),
        "the focus followed the viewport up"
    );
    assert!(app.advance_selection_autoscroll(), "second beat scrolls");
    assert!(
        app.selection_text()
            .is_some_and(|text| text.starts_with("line 31")),
        "the focus followed the viewport up again"
    );
}

#[test]
fn a_drag_away_from_the_edge_stops_the_autoscroll() {
    let mut app = scrollable_app();
    app.step(press(2, 3));
    app.step(drag(2, 0));
    assert!(app.advance_selection_autoscroll());

    // Moving the pointer back into the middle disarms the auto-scroll.
    app.step(drag(2, 2));
    assert!(
        !app.advance_selection_autoscroll(),
        "nothing is pending once the pointer leaves the edge"
    );
}

#[test]
fn autoscroll_stops_when_the_viewport_cannot_move() {
    // A log shorter than the viewport has no scrollback at all.
    let mut app = app_with_lines(&["only line"]);
    app.step(press(2, VIEWPORT as u16 - 1));
    app.step(drag(2, 0));
    assert!(
        !app.advance_selection_autoscroll(),
        "the viewport is already at the top"
    );
    assert!(!app.advance_selection_autoscroll());
}

#[test]
fn releasing_a_word_selection_copies_the_word() {
    let mut app = single_viewport_app();
    let _ = click(&mut app, 10, 2);
    app.step(press(10, 2));
    assert_eq!(app.selection_text().as_deref(), Some("beta-gamma"));
    // Copy-on-select fires on the release, like a character selection.
    app.step(release(10, 2));
    assert_eq!(app.take_clipboard_request().as_deref(), Some("beta-gamma"));
}
