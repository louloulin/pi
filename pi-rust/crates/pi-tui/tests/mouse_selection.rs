//! Chat-log text selection and copy-on-select in the fullscreen [`App`].
//!
//! The alternate screen hides the terminal's own scrollback *and* mouse
//! capture takes native text selection away, so upstream implements
//! selection itself and copies on release (`copyOnSelect ?? true`,
//! `packages/tui/src/tui-alt-screen.ts:272,1343-1379,1443-1462`). These
//! tests cover the component half: gesture handling, the selected text,
//! the reversed-video highlight and the clipboard request the driver
//! turns into an OSC 52 write.

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::backend::event::{Event as CtEvent, KeyModifiers, MouseEvent, MouseEventKind};
use pi_tui::input::{InputEvent, MouseButton, MouseGesture, MouseGestureKind};
use pi_tui::selector::{Selector, SelectorItem};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

const WIDTH: u16 = 40;
/// Snapshot height; the message viewport is this minus the status bar and
/// the prompt row, i.e. 8 rows.
const HEIGHT: u16 = 10;
const VIEWPORT: usize = (HEIGHT - 2) as usize;

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

fn app_with(config: AppConfig) -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        std::sync::Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    App::new(&agent, config)
}

fn selection_app() -> App {
    selection_app_with(AppConfig {
        session_id: "selection".into(),
        ..AppConfig::default()
    })
}

fn selection_app_with(config: AppConfig) -> App {
    let mut app = app_with(config);
    for i in 0..40 {
        app.info(format!("line {i}"));
    }
    // Render once so `viewport` / `viewport_origin` describe a real screen.
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

/// The rendered buffer for the App at its canonical test size.
fn buffer(app: &mut App) -> Buffer {
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    buf
}

fn reversed(buf: &Buffer, x: u16, y: u16) -> bool {
    buf.cell((x, y))
        .map(|cell| cell.modifier.contains(Modifier::REVERSED))
        .unwrap_or(false)
}

#[test]
fn drag_selects_characters_and_reports_the_text() {
    let mut app = selection_app();
    // Rows 0 and 7 are the viewport edges, where a drag arms the autoscroll;
    // row 1 selects plainly. Visible rows are the tail, so row 1 is line 33.
    assert_eq!(
        app.step(press(2, 1)),
        StepOutcome::Redraw,
        "a press starts a selection"
    );
    assert_eq!(app.step(drag(8, 1)), StepOutcome::Redraw);
    // Row 1 is "> line 33": columns 2..=8 are "line 33".
    assert_eq!(app.selection_text().as_deref(), Some("line 33"));
}

#[test]
fn drag_across_lines_joins_them_with_newlines() {
    let mut app = selection_app();
    app.step(press(2, 1));
    app.step(drag(8, 2));
    assert_eq!(
        app.selection_text().as_deref(),
        Some("line 33\n> line 34"),
        "the second entry keeps the rendered prefix"
    );
}

#[test]
fn selection_survives_scrolling_and_follows_the_viewport() {
    let mut app = selection_app();
    app.step(press(2, 1));
    app.step(drag(8, 1));
    assert_eq!(app.selection_text().as_deref(), Some("line 33"));

    // The selection is anchored to log lines, not screen rows: scrolling
    // keeps the text and carries the highlight along with the content.
    assert_eq!(
        app.step(InputEvent::wheel(true, false, 0, 0)),
        StepOutcome::Redraw
    );
    assert_eq!(app.selection_text().as_deref(), Some("line 33"));
    let buf = buffer(&mut app);
    assert!(
        !reversed(&buf, 2, 1),
        "one line further down means the second row is a different line"
    );
    assert!(
        reversed(&buf, 2, 2),
        "the selected line is now the third visible row"
    );

    // Scrolling the selection out of view drops the highlight without
    // touching the selection itself.
    assert_eq!(
        app.step(InputEvent::key(
            pi_tui::input::KeyCode::PageUp,
            pi_tui::input::KeyModifiers::NONE
        )),
        StepOutcome::Redraw
    );
    assert_eq!(app.selection_text().as_deref(), Some("line 33"));
    let buf = buffer(&mut app);
    assert!(
        !(0..VIEWPORT as u16).any(|y| reversed(&buf, 2, y)),
        "line 33 scrolled off the top"
    );
}

#[test]
fn selected_cells_are_rendered_in_reverse_video() {
    let mut app = selection_app();
    app.step(press(2, 1));
    app.step(drag(8, 1));

    let buf = buffer(&mut app);
    for x in 2..=8 {
        assert!(reversed(&buf, x, 1), "column {x} is selected");
    }
    assert!(!reversed(&buf, 1, 1), "the prompt prefix is not selected");
    assert!(!reversed(&buf, 9, 1), "one past the focus is not selected");
}

#[test]
fn copy_on_select_queues_the_text_on_release() {
    let mut app = selection_app();
    app.step(press(2, 0));
    app.step(drag(8, 0));
    assert_eq!(app.take_clipboard_request(), None, "not copied mid-drag");

    assert_eq!(app.step(release(8, 0)), StepOutcome::Idle);
    assert_eq!(app.take_clipboard_request().as_deref(), Some("line 32"));
    assert_eq!(
        app.take_clipboard_request(),
        None,
        "the request is consumed once"
    );
    // Upstream keeps the selection visible after copying.
    assert!(app.has_selection());
    assert_eq!(app.selection_text().as_deref(), Some("line 32"));
}

#[test]
fn copy_on_select_disabled_keeps_the_selection_without_copying() {
    let mut app = selection_app_with(AppConfig {
        session_id: "no-copy".into(),
        copy_on_select: false,
        ..AppConfig::default()
    });
    app.step(press(2, 0));
    app.step(drag(8, 0));
    app.step(release(8, 0));

    assert_eq!(app.take_clipboard_request(), None);
    assert_eq!(app.selection_text().as_deref(), Some("line 32"));
}

#[test]
fn a_click_clears_the_previous_selection() {
    let mut app = selection_app();
    app.step(press(2, 0));
    app.step(drag(8, 0));
    assert!(app.has_selection());

    // Press and release on the same cell leaves an empty selection, so a
    // plain click dismisses the old one.
    app.step(press(20, 3));
    assert_eq!(app.step(release(20, 3)), StepOutcome::Idle);
    assert!(!app.has_selection());
    assert_eq!(app.selection_text(), None);
    assert_eq!(app.take_clipboard_request(), None);
}

#[test]
fn gestures_outside_the_message_viewport_are_ignored() {
    let mut app = selection_app();
    // Row HEIGHT-1 is the status bar; row VIEWPORT is the prompt. Both sit
    // outside the message viewport but a press on the prompt would normally
    // arm a composer drag — and a drag with an armed prompt would still be
    // the composer's, not a selection. Verify the selection path stays Idle
    // even though the prompt owns the gesture.
    assert_eq!(app.step(press(3, HEIGHT - 1)), StepOutcome::Idle);
    assert_eq!(app.step(drag(3, VIEWPORT as u16)), StepOutcome::Idle);
    assert!(!app.has_selection());
}

#[test]
fn an_open_modal_swallows_gestures() {
    let mut app = selection_app();
    app.open_selector(Selector::new(
        "models",
        vec![SelectorItem::new("model:faux", "faux")],
    ));
    assert_eq!(app.step(press(2, 0)), StepOutcome::Idle);
    assert_eq!(app.step(drag(8, 0)), StepOutcome::Idle);
    assert!(!app.has_selection());
}

#[test]
fn non_left_buttons_do_not_start_a_selection() {
    let mut app = selection_app();
    for button in [MouseButton::Middle, MouseButton::Right] {
        assert_eq!(
            app.step(gesture(MouseGestureKind::Press(button), 2, 0)),
            StepOutcome::Idle,
            "{button:?}"
        );
    }
    assert!(!app.has_selection());
}

#[test]
fn a_drag_without_a_press_does_not_select() {
    let mut app = selection_app();
    assert_eq!(app.step(drag(8, 0)), StepOutcome::Idle);
    assert!(!app.has_selection());
}

#[test]
fn gestures_translate_from_crossterm_with_coordinates() {
    use pi_tui::backend::event::MouseButton as CtMouseButton;
    let translate = |kind: MouseEventKind| {
        App::translate_event(CtEvent::Mouse(MouseEvent {
            kind,
            column: 7,
            row: 3,
            modifiers: KeyModifiers::SHIFT,
        }))
        .unwrap()
    };
    assert_eq!(
        translate(MouseEventKind::Down(CtMouseButton::Left)),
        InputEvent::gesture(MouseGesture::new(
            MouseGestureKind::Press(MouseButton::Left),
            7,
            3,
            false
        ))
    );
    assert_eq!(
        translate(MouseEventKind::Up(CtMouseButton::Left)),
        InputEvent::gesture(MouseGesture::new(
            MouseGestureKind::Release(MouseButton::Left),
            7,
            3,
            false
        ))
    );
    assert_eq!(
        translate(MouseEventKind::Drag(CtMouseButton::Left)),
        InputEvent::gesture(MouseGesture::new(
            MouseGestureKind::Drag(MouseButton::Left),
            7,
            3,
            false
        ))
    );
    assert_eq!(
        translate(MouseEventKind::Moved),
        InputEvent::gesture(MouseGesture::new(MouseGestureKind::Move, 7, 3, false))
    );
}
