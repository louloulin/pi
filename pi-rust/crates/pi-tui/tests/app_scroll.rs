//! Chat-log scrolling in the fullscreen [`App`].
//!
//! The interactive TUI runs on the alternate screen, so the terminal's own
//! scrollback is unavailable — the App has to own scrolling. These tests
//! drive the real key path (`step_key`) and assert on the rendered snapshot,
//! covering the upstream `tui.altScreen.pageUp` / `pageDown` / `top` /
//! `bottom` bindings (`packages/tui/src/keybindings.ts:159-165,208-209`) and
//! the "detached viewport keeps its position while output streams" rule.

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::input::{Key, KeyCode, KeyModifiers};

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
            session_id: "scroll".into(),
            ..AppConfig::default()
        },
    )
}

/// Fill the log with `count` one-line items, then render once so the App
/// knows its viewport. Returns the top visible line of that render.
fn app_with_lines(count: usize) -> (App, String) {
    let mut app = app();
    for i in 0..count {
        app.info(format!("line {i}"));
    }
    let top = top_line(&app);
    (app, top)
}

fn top_line(app: &App) -> String {
    app.render_snapshot(WIDTH, HEIGHT).lines[0]
        .trim()
        .to_string()
}

fn key(code: KeyCode) -> Key {
    Key::new(code, KeyModifiers::NONE)
}

#[test]
fn page_up_and_down_move_by_one_viewport() {
    let (mut app, top) = app_with_lines(40);
    // Pinned to the tail: the last viewport-height lines are visible.
    assert_eq!(top, "> line 32");
    assert_eq!(app.messages().scroll_offset(), 0);
    assert!(app.messages().is_following());

    assert_eq!(app.step_key(key(KeyCode::PageUp)), StepOutcome::Redraw);
    assert_eq!(app.messages().scroll_offset(), VIEWPORT);
    assert_eq!(top_line(&app), "> line 24");
    assert!(!app.messages().is_following());

    assert_eq!(app.step_key(key(KeyCode::PageUp)), StepOutcome::Redraw);
    assert_eq!(app.messages().scroll_offset(), 2 * VIEWPORT);
    assert_eq!(top_line(&app), "> line 16");

    assert_eq!(app.step_key(key(KeyCode::PageDown)), StepOutcome::Redraw);
    assert_eq!(top_line(&app), "> line 24");

    assert_eq!(app.step_key(key(KeyCode::PageDown)), StepOutcome::Redraw);
    // Back at the tail the viewport re-attaches to new output.
    assert_eq!(app.messages().scroll_offset(), 0);
    assert!(app.messages().is_following());
    assert_eq!(top_line(&app), "> line 32");
}

#[test]
fn page_scroll_clamps_at_both_ends() {
    let (mut app, _) = app_with_lines(10);
    // 10 lines in an 8-row viewport leaves only 2 lines of scrollback.
    assert_eq!(app.step_key(key(KeyCode::PageUp)), StepOutcome::Redraw);
    assert_eq!(app.messages().scroll_offset(), 2);
    assert_eq!(top_line(&app), "> line 0");
    // Already at the top — nothing to redraw.
    assert_eq!(app.step_key(key(KeyCode::PageUp)), StepOutcome::Idle);
    assert_eq!(app.step_key(key(KeyCode::PageDown)), StepOutcome::Redraw);
    assert_eq!(app.step_key(key(KeyCode::PageDown)), StepOutcome::Idle);
    assert!(app.messages().is_following());
}

#[test]
fn home_and_end_jump_to_the_extremes() {
    let (mut app, _) = app_with_lines(40);

    assert_eq!(app.step_key(key(KeyCode::Home)), StepOutcome::Redraw);
    assert_eq!(top_line(&app), "> line 0");
    assert!(!app.messages().is_following());
    // Idempotent: a second Home is not a redraw.
    assert_eq!(app.step_key(key(KeyCode::Home)), StepOutcome::Idle);

    // PageDown from the `scroll_to_top` sentinel must move, not stay stuck.
    let offset_at_top = app.messages().scroll_offset();
    assert_eq!(app.step_key(key(KeyCode::PageDown)), StepOutcome::Redraw);
    assert!(app.messages().scroll_offset() < offset_at_top);
    assert_eq!(top_line(&app), "> line 8");

    assert_eq!(app.step_key(key(KeyCode::End)), StepOutcome::Redraw);
    assert_eq!(top_line(&app), "> line 32");
    assert!(app.messages().is_following());
    assert_eq!(app.step_key(key(KeyCode::End)), StepOutcome::Idle);
}

#[test]
fn ctrl_a_and_ctrl_e_still_reach_the_editor() {
    // Upstream shadows the *bare* Home/End bindings in fullscreen mode but
    // keeps the Ctrl+A / Ctrl+E aliases for the editor
    // (`packages/tui/src/keybindings.ts:99-103`).
    let (mut app, _) = app_with_lines(40);
    app.prompt_mut().editor_mut().insert_str("hello");
    assert_eq!(app.prompt().text(), "hello");

    app.step_key(Key::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
    assert_eq!(app.prompt_mut().editor_mut().cursor(), 0);
    assert_eq!(app.messages().scroll_offset(), 0);

    app.step_key(Key::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
    // The editor still owns the buffer text and cursor after both chords.
    assert_eq!(app.prompt().text(), "hello");
    assert_eq!(app.prompt_mut().editor_mut().cursor(), 5);
    // …and neither chord scrolled the log.
    assert_eq!(app.messages().scroll_offset(), 0);
    assert!(app.messages().is_following());
}

#[test]
fn detached_viewport_ignores_streaming_output() {
    let (mut app, _) = app_with_lines(40);
    app.step_key(key(KeyCode::PageUp));
    let detached_top = top_line(&app);
    let detached_offset = app.messages().scroll_offset();
    assert_eq!(detached_top, "> line 24");

    // A streaming delta on the trailing item (new assistant block) and a
    // finalized message both arrive while the reader is scrolled back.
    app.messages_mut().begin_assistant_stream("faux-model");
    app.messages_mut().append_assistant_delta("thinking…");
    app.info("a new tool ran");

    // The offset from the tail grows by exactly the two added lines, which
    // is what keeps the reader's text still. The viewport must stay
    // detached — a re-pin would jump back to the tail.
    assert_eq!(app.messages().scroll_offset(), detached_offset + 2);
    assert!(!app.messages().is_following());
    assert_eq!(
        top_line(&app),
        detached_top,
        "the viewport must not jump to the tail while detached"
    );
}

#[test]
fn following_viewport_pins_to_new_output() {
    let (mut app, _) = app_with_lines(40);
    assert!(app.messages().is_following());

    app.info("last line");
    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    assert_eq!(snapshot.lines[VIEWPORT - 1].trim(), "> last line");
    assert_eq!(app.messages().scroll_offset(), 0);
}

#[test]
fn scrolled_log_survives_a_resize() {
    let (mut app, _) = app_with_lines(40);
    app.step_key(key(KeyCode::PageUp));
    let narrow_top = {
        let snapshot = app.render_snapshot(WIDTH, HEIGHT);
        snapshot.lines[0].trim().to_string()
    };

    // A resize narrows the log, so more lines wrap — the offset is still a
    // valid line count from the tail and must not panic or re-pin.
    let wide = app.render_snapshot(80, HEIGHT);
    assert_eq!(wide.lines[0].trim(), narrow_top);
    assert!(!app.messages().is_following());
}

#[test]
fn scroll_keys_before_first_render_are_noops() {
    let mut app = app();
    app.info("only line");
    // No render yet → the viewport is unknown, so every scroll key is idle.
    assert_eq!(app.step_key(key(KeyCode::PageUp)), StepOutcome::Idle);
    assert_eq!(app.step_key(key(KeyCode::PageDown)), StepOutcome::Idle);
    assert_eq!(app.step_key(key(KeyCode::Home)), StepOutcome::Idle);
    assert_eq!(app.step_key(key(KeyCode::End)), StepOutcome::Idle);
    assert_eq!(app.messages().scroll_offset(), 0);
}

#[test]
fn ctrl_l_clears_the_log_and_repins() {
    let (mut app, _) = app_with_lines(40);
    app.step_key(key(KeyCode::PageUp));
    assert!(!app.messages().is_following());

    let outcome = app.step_key(Key::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
    assert_eq!(outcome, StepOutcome::Redraw);
    assert!(app.messages().is_empty());
    assert!(app.messages().is_following());
    assert_eq!(app.messages().scroll_offset(), 0);
    // Empty log → nothing to scroll.
    assert_eq!(app.step_key(key(KeyCode::PageUp)), StepOutcome::Idle);
}
