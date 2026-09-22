//! Mouse-wheel scrolling in the fullscreen [`App`].
//!
//! The alternate screen hides the terminal's own scrollback, so the App owns
//! scrolling. Upstream enables mouse capture for every alt-screen session
//! (`packages/tui/src/tui-alt-screen.ts:168,265,353-362`) and routes the wheel
//! to the scroll view under the pointer (`:973-984`), with one line per notch
//! and an Alt multiplier of 5 (`:75,264,968-971`). These tests cover the
//! component half of that path: the crossterm → [`InputEvent`] translation
//! and the viewport mutation `App::step` performs.

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::backend::event::{Event as CtEvent, KeyModifiers, MouseEvent, MouseEventKind};
use pi_tui::input::{InputEvent, MouseButton, MouseGesture, MouseGestureKind};
use pi_tui::message::ToolBlock;
use pi_tui::selector::{Selector, SelectorItem};
use pi_tui::styled::{SpanStyle, StyledLine, StyledSpan};
use pi_tui::theme::ThemeColor;

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

fn app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        std::sync::Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    App::new(
        &agent,
        AppConfig {
            session_id: "mouse".into(),
            ..AppConfig::default()
        },
    )
}

/// Fill the log with `count` one-line items, then render once so the App
/// knows its viewport.
fn app_with_lines(count: usize) -> App {
    let mut app = app();
    for i in 0..count {
        app.info(format!("line {i}"));
    }
    // Render once so `max_scroll` sees the viewport height.
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    app
}

fn top_line(app: &App) -> String {
    app.render_snapshot(WIDTH, HEIGHT).lines[0]
        .trim()
        .to_string()
}

fn mouse(kind: MouseEventKind, modifiers: KeyModifiers) -> CtEvent {
    CtEvent::Mouse(MouseEvent {
        kind,
        column: 1,
        row: 1,
        modifiers,
    })
}

#[test]
fn scroll_up_and_down_translate_to_mouse_events() {
    assert_eq!(
        App::translate_event(mouse(MouseEventKind::ScrollUp, KeyModifiers::NONE)),
        InputEvent::Mouse {
            up: true,
            alt: false,
            x: 1,
            y: 1,
        }
    );
    assert_eq!(
        App::translate_event(mouse(MouseEventKind::ScrollDown, KeyModifiers::NONE)),
        InputEvent::Mouse {
            up: false,
            alt: false,
            x: 1,
            y: 1,
        }
    );
    assert_eq!(
        App::translate_event(mouse(MouseEventKind::ScrollUp, KeyModifiers::ALT)),
        InputEvent::Mouse {
            up: true,
            alt: true,
            x: 1,
            y: 1,
        }
    );
}

#[test]
fn non_wheel_mouse_events_translate_to_gestures() {
    // Clicks / moves / drags reach the App as gestures; the App turns the
    // left-button ones into a chat-log text selection (see
    // `tests/mouse_selection.rs`).
    let expect = |kind: MouseEventKind, gesture: MouseGestureKind| {
        assert_eq!(
            App::translate_event(mouse(kind, KeyModifiers::NONE)),
            InputEvent::MouseGesture(MouseGesture::new(gesture, 1, 1, false)),
            "{kind:?}"
        );
    };
    expect(
        MouseEventKind::Down(pi_tui::backend::event::MouseButton::Left),
        MouseGestureKind::Press(MouseButton::Left),
    );
    expect(
        MouseEventKind::Up(pi_tui::backend::event::MouseButton::Left),
        MouseGestureKind::Release(MouseButton::Left),
    );
    expect(MouseEventKind::Moved, MouseGestureKind::Move);
    expect(
        MouseEventKind::Drag(pi_tui::backend::event::MouseButton::Left),
        MouseGestureKind::Drag(MouseButton::Left),
    );
    expect(
        MouseEventKind::Down(pi_tui::backend::event::MouseButton::Right),
        MouseGestureKind::Press(MouseButton::Right),
    );

    // The Alt modifier rides along on gestures too.
    assert_eq!(
        App::translate_event(mouse(MouseEventKind::Moved, KeyModifiers::ALT)),
        InputEvent::MouseGesture(MouseGesture::new(MouseGestureKind::Move, 1, 1, true))
    );
}

#[test]
fn wheel_moves_one_line_per_notch() {
    let mut app = app_with_lines(40);
    assert!(app.messages().is_following());
    assert_eq!(top_line(&app), "> line 32");

    assert_eq!(
        app.step(InputEvent::wheel(true, false)),
        StepOutcome::Redraw
    );
    assert_eq!(app.messages().scroll_offset(), 1);
    assert_eq!(top_line(&app), "> line 31");
    assert!(!app.messages().is_following());

    assert_eq!(
        app.step(InputEvent::wheel(false, false)),
        StepOutcome::Redraw
    );
    // Back at the tail the viewport re-attaches to new output.
    assert_eq!(app.messages().scroll_offset(), 0);
    assert!(app.messages().is_following());
    assert_eq!(top_line(&app), "> line 32");
}

#[test]
fn alt_wheel_multiplies_the_step() {
    let mut app = app_with_lines(40);

    assert_eq!(app.step(InputEvent::wheel(true, true)), StepOutcome::Redraw);
    assert_eq!(app.messages().scroll_offset(), 5);
    assert_eq!(top_line(&app), "> line 27");

    // A shorter-than-five-line overscroll clamps at the top instead of
    // wrapping.
    assert_eq!(app.step(InputEvent::wheel(true, true)), StepOutcome::Redraw);
    assert_eq!(app.messages().scroll_offset(), 10);
    assert_eq!(app.step(InputEvent::wheel(true, true)), StepOutcome::Redraw);
    assert_eq!(app.messages().scroll_offset(), 15);
    assert_eq!(app.step(InputEvent::wheel(true, true)), StepOutcome::Redraw);
    assert_eq!(app.messages().scroll_offset(), 20);
    assert_eq!(app.step(InputEvent::wheel(true, true)), StepOutcome::Redraw);
    assert_eq!(app.messages().scroll_offset(), 25);
    assert_eq!(top_line(&app), "> line 7");
    assert_eq!(app.step(InputEvent::wheel(true, true)), StepOutcome::Redraw);
    assert_eq!(app.messages().scroll_offset(), 30);
    assert_eq!(top_line(&app), "> line 2");
    // The final notch clamps at the top instead of overshooting.
    assert_eq!(app.step(InputEvent::wheel(true, true)), StepOutcome::Redraw);
    assert_eq!(app.messages().scroll_offset(), 32);
    assert_eq!(top_line(&app), "> line 0");

    // Already at the top — no redraw.
    assert_eq!(app.step(InputEvent::wheel(true, true)), StepOutcome::Idle);
}

#[test]
fn wheel_clamps_at_both_ends() {
    // 10 lines in an 8-row viewport leaves only 2 lines of scrollback.
    let mut app = app_with_lines(10);

    assert_eq!(
        app.step(InputEvent::wheel(true, false)),
        StepOutcome::Redraw
    );
    assert_eq!(app.messages().scroll_offset(), 1);
    assert_eq!(
        app.step(InputEvent::wheel(true, false)),
        StepOutcome::Redraw
    );
    assert_eq!(app.messages().scroll_offset(), 2);
    assert_eq!(app.step(InputEvent::wheel(true, false)), StepOutcome::Idle);
    assert_eq!(top_line(&app), "> line 0");

    assert_eq!(
        app.step(InputEvent::wheel(false, false)),
        StepOutcome::Redraw
    );
    assert_eq!(
        app.step(InputEvent::wheel(false, false)),
        StepOutcome::Redraw
    );
    assert!(app.messages().is_following());
    assert_eq!(app.step(InputEvent::wheel(false, false)), StepOutcome::Idle);
}

#[test]
fn an_open_modal_does_not_scroll_the_log_behind_it() {
    // Selectors and dialogs own the pointer as well as the keyboard until
    // they close, matching the App's "an open modal owns the input" rule.
    let mut app = app_with_lines(40);
    app.open_selector(Selector::new(
        "models",
        vec![SelectorItem::new("model:faux", "faux")],
    ));

    assert_eq!(app.step(InputEvent::wheel(true, false)), StepOutcome::Idle);
    assert_eq!(app.messages().scroll_offset(), 0);
    assert!(app.messages().is_following());
}

#[test]
fn wheel_moves_multiple_lines_like_page_keys_and_leaves_the_editor_untouched() {
    // The editor must not consume the wheel: the prompt text and cursor are
    // unchanged after scrolling.
    let mut app = app_with_lines(40);
    app.prompt_mut().editor_mut().insert_str("hello");
    app.step(InputEvent::wheel(true, false));

    assert_eq!(app.prompt().text(), "hello");
    assert_eq!(app.prompt_mut().editor_mut().cursor(), 5);
    assert!(!app.messages().is_following());
    assert_eq!(app.messages().scroll_offset(), 1);

    // Scrolling one wheel notch at a time reaches the top like `page_up`
    // would, just without the viewport-sized jump.
    for _ in 0..(40 - VIEWPORT) {
        app.step(InputEvent::wheel(true, false));
    }
    assert_eq!(top_line(&app), "> line 0");
}

#[test]
fn wheel_over_a_tool_block_scrolls_instead_of_toggling_it() {
    // Tool blocks are click-targets for expansion, but the wheel is still a
    // scroll: only the left button toggles.
    let mut app = app_with_lines(40);
    app.messages_mut()
        .start_tool_execution("call-1", "bash", "{\"command\":\"echo hi\"}");
    let body: Vec<StyledLine> = (0..10)
        .map(|i| {
            vec![StyledSpan::new(
                format!("bash-line-{i}"),
                SpanStyle::fg(ThemeColor::ToolOutput),
            )]
        })
        .collect();
    app.messages_mut().finish_tool_execution_with_lines(
        "call-1",
        1,
        "hi",
        false,
        Some(ToolBlock::new(Vec::new(), body)),
    );
    let _ = app.render_snapshot(WIDTH, HEIGHT);

    assert_eq!(app.messages().scroll_offset(), 0);
    assert!(!app.tools_expanded());
    assert_eq!(
        app.step(InputEvent::wheel(true, false)),
        StepOutcome::Redraw
    );
    assert_eq!(app.messages().scroll_offset(), 1);
    assert!(!app.tools_expanded(), "the wheel must not toggle expansion");
}
