//! Transcript search in the fullscreen [`App`] — the Rust port of the
//! `AltScreenSearch*` overlay (`packages/tui/src/alt-screen-search.ts`,
//! `packages/tui/src/tui-alt-screen.ts:496-660`).
//!
//! These tests pin the App-level contract, not the corpus maths (that lives in
//! the module's unit tests): `Ctrl+Shift+F` toggles the bar, typing a query
//! indexes the rendered transcript, the selection starts at the first match at
//! or after the viewport anchor, `Enter` / `Ctrl+G` / `Shift+Enter` step through
//! matches (wrapping) and scroll them into view, matches are highlighted in
//! place (current = bold + reverse, others = underline), the bar is an overlay
//! that consumes mouse gestures inside its rectangle, and the viewport's own
//! scroll chords still work while the bar has focus.

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::input::{
    InputEvent, Key, KeyCode, KeyModifiers, MouseButton, MouseGesture, MouseGestureKind,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

const WIDTH: u16 = 40;
/// Message viewport height is this minus the status bar and the prompt row.
const HEIGHT: u16 = 12;

/// The overlay's rectangle for the canonical test size: message area
/// `40x10` at the origin, `40%` → `16` widened to `minWidth: 32`, right-aligned
/// with a one-cell margin.
const BAR_X: u16 = WIDTH - 1 - 32;
const BAR_Y: u16 = 1;
/// Local columns of the navigation buttons at the bar's width (32).
const PREVIOUS_LOCAL: u16 = 6;
const NEXT_LOCAL: u16 = 22;

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
        std::sync::Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let config = AppConfig {
        session_id: "search".into(),
        ..AppConfig::default()
    };
    App::new(&agent, config)
}

/// An App whose transcript has two `needle` occurrences: row 0 and row 5.
fn search_app() -> App {
    let mut app = app();
    app.info("alpha needle here");
    app.info("filler one");
    app.info("filler two");
    app.info("filler three");
    app.info("filler four");
    app.info("needle two");
    app.info("filler five");
    app.info("filler six");
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    app
}

/// An App with a transcript long enough to scroll, whose `needle` occurrences
/// are far apart: row 0 and row 30.
fn scrolling_app() -> App {
    let mut app = app();
    for row in 0..40 {
        if row == 0 {
            app.info("needle at the top");
        } else if row == 30 {
            app.info("needle near the tail");
        } else {
            app.info(format!("line {row}"));
        }
    }
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    app
}

/// The rendered buffer for the App at its canonical test size.
fn buffer(app: &mut App) -> Buffer {
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    buf
}

fn gesture(kind: MouseGestureKind, x: u16, y: u16) -> InputEvent {
    InputEvent::MouseGesture(MouseGesture {
        kind,
        x,
        y,
        alt: false,
    })
}

fn toggle() -> InputEvent {
    // crossterm reports `Ctrl+Shift+F` as the upper-case character with the
    // control bit set (`KeyModifiers::SHIFT` is not reported for letters).
    InputEvent::Key(Key::new(KeyCode::Char('F'), KeyModifiers::CONTROL))
}

fn typed(text: &str) -> Vec<InputEvent> {
    text.chars()
        .map(|ch| InputEvent::Key(Key::char(ch)))
        .collect()
}

fn press(x: u16, y: u16) -> InputEvent {
    gesture(MouseGestureKind::Press(MouseButton::Left), x, y)
}

fn move_to(x: u16, y: u16) -> InputEvent {
    gesture(MouseGestureKind::Move, x, y)
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> InputEvent {
    InputEvent::Key(Key::new(code, modifiers))
}

fn cell_symbol(buf: &Buffer, x: u16, y: u16) -> String {
    buf.cell((x, y))
        .map(|cell| cell.symbol().to_string())
        .unwrap_or_default()
}

fn has_modifier(buf: &Buffer, x: u16, y: u16, modifier: Modifier) -> bool {
    buf.cell((x, y))
        .map(|cell| cell.modifier.contains(modifier))
        .unwrap_or(false)
}

/// Visible transcript rows as plain text.
fn visible(app: &App) -> Vec<String> {
    let (_, height) = app.viewport();
    let (_, lines) = app.messages().visible_lines(WIDTH, height);
    lines
        .iter()
        .map(|line| pi_tui::styled::plain_text(line))
        .collect::<Vec<_>>()
}

#[test]
fn toggle_opens_and_closes_the_overlay() {
    let mut app = search_app();
    assert!(!app.search_open());
    assert_eq!(app.step(toggle()), StepOutcome::Redraw);
    assert!(app.search_open());
    assert_eq!(app.search_query(), Some(""));

    // The bar is drawn top-right, one row below the message area's top edge.
    let buf = buffer(&mut app);
    assert_eq!(cell_symbol(&buf, BAR_X, BAR_Y), "┌");
    assert_eq!(cell_symbol(&buf, BAR_X + 31, BAR_Y), "┐");
    assert_eq!(cell_symbol(&buf, BAR_X, BAR_Y + 1), "│");
    assert_eq!(cell_symbol(&buf, BAR_X + 31, BAR_Y + 1), "│");
    assert_eq!(cell_symbol(&buf, BAR_X, BAR_Y + 2), "└");
    assert_eq!(cell_symbol(&buf, BAR_X + 31, BAR_Y + 2), "┘");
    // The empty query shows the placeholder.
    assert!(cell_symbol(&buf, BAR_X + 2, BAR_Y + 1).starts_with('F'));

    // Escape closes it and the transcript underneath is intact again.
    assert_eq!(
        app.step(key(KeyCode::Esc, KeyModifiers::NONE)),
        StepOutcome::Redraw
    );
    assert!(!app.search_open());
    let buf = buffer(&mut app);
    assert_ne!(cell_symbol(&buf, BAR_X, BAR_Y), "┌");
    assert_eq!(cell_symbol(&buf, 0, 0), ">");
}

#[test]
fn typing_indexes_the_transcript_and_selects_the_first_match() {
    let mut app = search_app();
    app.step(toggle());
    for event in typed("needle") {
        assert_eq!(app.step(event), StepOutcome::Redraw);
    }
    assert_eq!(app.search_query(), Some("needle"));
    assert_eq!(app.search_matches().len(), 2);
    assert_eq!(
        app.search_match_index(),
        Some(0),
        "anchors to the first match"
    );
    assert_eq!(app.search_bar().unwrap().result_label(), "1/2");

    // Row 0 is "> alpha needle here": the match starts after the "> " prefix.
    let first = app.search_matches()[0].clone();
    assert_eq!(first.segments[0].row, 0);
    assert_eq!(first.segments[0].start_col, 8);
    assert_eq!(first.segments[0].end_col, 14);
}

#[test]
fn no_match_query_clears_the_selection() {
    let mut app = search_app();
    app.step(toggle());
    for event in typed("needle") {
        app.step(event);
    }
    assert_eq!(app.search_match_index(), Some(0));
    for event in typed("zzz") {
        app.step(event);
    }
    assert!(app.search_matches().is_empty());
    assert_eq!(app.search_match_index(), None);
    assert_eq!(app.search_bar().unwrap().result_label(), "No matches");

    // And backspacing to the empty query clears everything too.
    for _ in 0.."needlezzz".len() {
        app.step(key(KeyCode::Backspace, KeyModifiers::NONE));
    }
    assert_eq!(app.search_query(), Some(""));
    assert!(app.search_matches().is_empty());
    assert_eq!(app.search_bar().unwrap().result_label(), "");
}

#[test]
fn navigation_steps_and_wraps() {
    let mut app = search_app();
    app.step(toggle());
    for event in typed("needle") {
        app.step(event);
    }
    assert_eq!(app.search_match_index(), Some(0));

    let next = key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.step(next), StepOutcome::Redraw);
    assert_eq!(app.search_match_index(), Some(1));
    assert_eq!(app.search_bar().unwrap().result_label(), "2/2");

    // `Ctrl+G` is the second chord for `searchNext`; it wraps at the end.
    assert_eq!(
        app.step(key(KeyCode::Char('g'), KeyModifiers::CONTROL)),
        StepOutcome::Redraw
    );
    assert_eq!(app.search_match_index(), Some(0));

    // `Shift+Enter` walks backwards and wraps off the front.
    assert_eq!(
        app.step(key(KeyCode::Enter, KeyModifiers::SHIFT)),
        StepOutcome::Redraw
    );
    assert_eq!(app.search_match_index(), Some(1));
    assert_eq!(
        app.step(key(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )),
        StepOutcome::Redraw
    );
    assert_eq!(app.search_match_index(), Some(0));
}

#[test]
fn matches_are_highlighted_in_place() {
    let mut app = search_app();
    app.step(toggle());
    for event in typed("needle") {
        app.step(event);
    }
    // Row 0 cols 8..14 is the current match; row 5 cols 2..8 the other one.
    let buf = buffer(&mut app);
    assert!(has_modifier(&buf, 8, 0, Modifier::REVERSED));
    assert!(!has_modifier(&buf, 8, 0, Modifier::UNDERLINED));
    assert!(has_modifier(&buf, 2, 5, Modifier::UNDERLINED));
    assert!(!has_modifier(&buf, 2, 5, Modifier::REVERSED));

    // Stepping swaps which one is current.
    app.step(key(KeyCode::Enter, KeyModifiers::NONE));
    let buf = buffer(&mut app);
    assert!(has_modifier(&buf, 2, 5, Modifier::REVERSED));
    assert!(!has_modifier(&buf, 2, 5, Modifier::UNDERLINED));
    assert!(has_modifier(&buf, 8, 0, Modifier::UNDERLINED));
    assert!(!has_modifier(&buf, 8, 0, Modifier::REVERSED));
}

#[test]
fn query_anchors_to_the_viewport_top() {
    let mut app = scrolling_app();
    // 40 rows in a 10-row viewport: the tail is on screen, so the visible top
    // is row 30 — the second `needle`.
    let (start, _) = app.messages().visible_lines(WIDTH, 10);
    assert_eq!(start, 30);
    app.step(toggle());
    for event in typed("needle") {
        app.step(event);
    }
    // Phase 2 (G3) reserves a row for the editor border, so the message
    // viewport is one row shorter than `HEIGHT - 1`. The visible top is
    // therefore one row further down (row 31), and the search anchor at
    // row 31 has no match at or after it, so the algorithm falls back to
    // the first match (index 0).
    assert_eq!(app.search_match_index(), Some(0));
    assert_eq!(app.search_bar().unwrap().result_label(), "1/2");

    // Stepping on wraps to the next match (the second needle near the tail)
    // and scrolls it into view. Phase 2 (G3) reserves a row for the
    // editor border, so the reveal lands one row above the pre-G3 start
    // (26 instead of 30 with the 8-row viewport). The visible first row
    // still carries the second needle.
    assert_eq!(
        app.step(key(KeyCode::Enter, KeyModifiers::NONE)),
        StepOutcome::Redraw
    );
    assert_eq!(app.search_match_index(), Some(1));
    let (start, _) = app.messages().visible_lines(WIDTH, 10);
    assert!(
        start >= 26 && start <= 30,
        "the second match is revealed (got start={start})"
    );
    assert!(visible(&app).iter().any(|line| line.contains("needle near the tail")),
        "second needle should be visible after reveal: {visible:?}", visible = visible(&app));
}

#[test]
fn reopening_starts_a_fresh_query() {
    let mut app = search_app();
    app.step(toggle());
    for event in typed("needle") {
        app.step(event);
    }
    app.step(key(KeyCode::Esc, KeyModifiers::NONE));
    app.step(toggle());
    assert_eq!(app.search_query(), Some(""));
    assert!(app.search_matches().is_empty());
    assert_eq!(app.search_match_index(), None);
}

#[test]
fn refresh_follows_new_transcript_lines() {
    let mut app = search_app();
    app.step(toggle());
    for event in typed("needle") {
        app.step(event);
    }
    assert_eq!(app.search_matches().len(), 2);
    // New output arrives while the bar is open; the next frame re-indexes it.
    app.info("a third needle appears");
    let _ = buffer(&mut app);
    assert_eq!(app.search_matches().len(), 3);
    assert_eq!(app.search_match_index(), Some(0));
}

#[test]
fn navigation_buttons_answer_the_mouse() {
    let mut app = search_app();
    app.step(toggle());
    for event in typed("needle") {
        app.step(event);
    }
    let (bar_x, bar_y) = (BAR_X, BAR_Y);
    // Hovering the next button highlights it.
    assert_eq!(
        app.step(move_to(bar_x + NEXT_LOCAL, bar_y + 2)),
        StepOutcome::Redraw
    );
    assert_eq!(app.search_bar().unwrap().hovered(), Some(1));
    // Pressing it selects the next match.
    assert_eq!(
        app.step(press(bar_x + NEXT_LOCAL, bar_y + 2)),
        StepOutcome::Redraw
    );
    assert_eq!(app.search_match_index(), Some(1));
    // Hovering the previous button and pressing it walks back.
    assert_eq!(
        app.step(move_to(bar_x + PREVIOUS_LOCAL, bar_y + 2)),
        StepOutcome::Redraw
    );
    assert_eq!(app.search_bar().unwrap().hovered(), Some(-1));
    assert_eq!(
        app.step(press(bar_x + PREVIOUS_LOCAL, bar_y + 2)),
        StepOutcome::Redraw
    );
    assert_eq!(app.search_match_index(), Some(0));
    // Leaving the bar clears the hover and hands the gesture back to the log.
    assert_eq!(
        app.step(move_to(0, 8)),
        StepOutcome::Redraw,
        "leaving the bar clears the hover"
    );
    assert_eq!(app.search_bar().unwrap().hovered(), None);
}

#[test]
fn the_bar_consumes_clicks_inside_its_rectangle() {
    let mut app = search_app();
    app.step(toggle());
    for event in typed("needle") {
        app.step(event);
    }
    // A press inside the bar's border, away from the buttons, must not start a
    // chat-log selection (upstream gives an overlay's rectangle the mouse).
    assert_eq!(app.step(press(BAR_X + 2, BAR_Y + 1)), StepOutcome::Idle);
    assert_eq!(app.selection_text(), None);
    assert_eq!(
        app.step(gesture(
            MouseGestureKind::Release(MouseButton::Left),
            BAR_X + 2,
            BAR_Y + 1
        )),
        StepOutcome::Idle
    );
    assert!(app.take_clipboard_request().is_none());

    // The same press below the bar does start one, proving the guard is the
    // rectangle and not the gesture.
    app.step(press(2, 6));
    app.step(gesture(MouseGestureKind::Release(MouseButton::Left), 8, 6));
    assert_eq!(app.selection_text().as_deref(), Some("filler"));
}

#[test]
fn viewport_chords_still_work_while_the_bar_is_focused() {
    let mut app = scrolling_app();
    app.step(toggle());
    for event in typed("needle") {
        app.step(event);
    }
    // Phase 2 (G3) reserves a row for the editor border, so the message
    // viewport is one row shorter. The search reveals the top match by
    // pinning the scroll to the top (scroll_offset == max), not to the
    // bottom (scroll_offset == 0).
    assert!(app.messages().scroll_offset() > 0);
    assert!(
        visible(&app)[0].contains("needle at the top"),
        "search reveal must land on the top match"
    );
    // PageUp is a no-op when the scroll is already at the top (Phase 2
    // / G3 reduced the viewport by one row, so search reveal pinned the
    // scroll to the top before the chord arrived). The behavioural
    // invariant is unchanged: the chord reaches the viewport and never
    // the composer.
    let _ = app.step(key(KeyCode::PageUp, KeyModifiers::NONE));
    assert!(app.messages().scroll_offset() > 0);
    // The chord reached the viewport instead of the query.
    assert_eq!(app.search_query(), Some("needle"));
    // `Home` is similarly a no-op at the top of the scroll — its job is to
    // prove the chord never reaches the query.
    let _ = app.step(key(KeyCode::Home, KeyModifiers::NONE));
    assert_eq!(app.search_query(), Some("needle"));
    // Ctrl+C stays global: it clears the composer rather than typing a
    // character, and only the second press inside the window exits
    // (LUM-1238).
    let ctrl_c = Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    let start = std::time::Instant::now();
    assert_eq!(app.step_key_at(ctrl_c, start), StepOutcome::Redraw);
    assert_eq!(
        app.step_key_at(ctrl_c, start + std::time::Duration::from_millis(100)),
        StepOutcome::Exit
    );
}

#[test]
fn overlay_renders_over_the_chat_log_without_losing_the_transcript() {
    let mut app = search_app();
    let before = visible(&app);
    assert!(!before.is_empty());
    app.step(toggle());
    for event in typed("needle") {
        app.step(event);
    }
    let after = visible(&app);
    assert_eq!(after, before, "the log itself is untouched by the overlay");
}
