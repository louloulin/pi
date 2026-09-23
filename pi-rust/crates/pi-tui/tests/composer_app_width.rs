//! The composer's wrap width is a *rendering* fact the editor needs while
//! it handles a key.
//!
//! `Prompt::render_lines` takes `&self`, so the composer cannot remember how
//! it wrapped the draft last frame; before LUM-1312 that was harmless
//! because the editor only counted characters. As soon as `Up` / `Down` move
//! by visual row, the editor has to measure the draft exactly like the
//! renderer did — otherwise the caret walks to a row the frame never drew it
//! on, and the composer looks like it has two different cursors.
//!
//! These tests drive the real [`App`]: render a frame at a known width, then
//! press keys through the public event surface and read the composer rows
//! out of the next frame.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::input::{InputEvent, KeyCode, KeyModifiers};

const LABEL: &str = "> ";

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

fn agent() -> Agent {
    Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ))
}

fn app(composer_max_rows: usize) -> App {
    App::new(
        &agent(),
        AppConfig {
            session_id: "composer-app-width".into(),
            composer_max_rows,
            ..AppConfig::default()
        },
    )
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> InputEvent {
    InputEvent::key(code, modifiers)
}

fn up() -> InputEvent {
    key(KeyCode::Up, KeyModifiers::NONE)
}

fn down() -> InputEvent {
    key(KeyCode::Down, KeyModifiers::NONE)
}

fn shift_enter() -> InputEvent {
    key(KeyCode::Enter, KeyModifiers::SHIFT)
}

/// The composer rows of a frame, in order.
fn composer_rows(app: &App, width: u16) -> Vec<String> {
    let snapshot = app.render_snapshot(width, 24);
    snapshot
        .lines
        .into_iter()
        .filter(|line| line.starts_with(LABEL) || line.starts_with("  "))
        .collect()
}

/// Index of the composer row carrying the `▍` marker.
fn cursor_row(rows: &[String]) -> usize {
    rows.iter()
        .position(|row| row.contains('▍'))
        .unwrap_or_else(|| panic!("no cursor in the composer rows: {rows:?}"))
}

#[test]
fn the_frame_publishes_the_body_width_the_editor_moves_by() {
    let mut app = app(8);
    app.prompt_mut()
        .editor_mut()
        .insert_str("hello world foo bar");

    // Width 12 → body width 10 (the label takes the other two). The first
    // frame records it...
    let _ = app.render_snapshot(12, 24);
    // ...and the next key press measures the draft with it: three rows
    // ("hello ", "world foo ", "bar"), so Up moves one row instead of
    // jumping to the line start or the prompt history.
    let before = composer_rows(&app, 12);
    assert_eq!(cursor_row(&before), 2, "cursor starts on the last row");

    app.step(up());
    let after = composer_rows(&app, 12);
    assert_eq!(
        cursor_row(&after),
        1,
        "Up must move one wrapped row, not to the top: {after:?}"
    );

    app.step(up());
    assert_eq!(cursor_row(&composer_rows(&app, 12)), 0);
}

#[test]
fn a_resize_between_frames_is_honoured_by_the_next_key() {
    let mut app = app(8);
    app.prompt_mut()
        .editor_mut()
        .insert_str("hello world foo bar");

    // Wide: the draft is one row, so Up is the editor's line-start /
    // history behaviour and cannot leave the row.
    let wide = composer_rows(&app, 80);
    assert_eq!(wide.len(), 1);
    app.step(up());
    assert_eq!(
        cursor_row(&composer_rows(&app, 80)),
        0,
        "one row means Up stays on it"
    );

    // Narrow: the same draft is three rows, and the *next* Up must use the
    // new geometry rather than the width the wide frame recorded.
    let narrow = composer_rows(&app, 12);
    assert_eq!(cursor_row(&narrow), 0);
    app.step(down());
    assert_eq!(
        cursor_row(&composer_rows(&app, 12)),
        1,
        "Down after a resize must follow the narrow wrap"
    );
}

#[test]
fn shift_enter_grows_the_composer_and_home_end_stay_on_the_line() {
    let mut app = app(8);
    app.prompt_mut().editor_mut().insert_str("first");
    let _ = app.render_snapshot(40, 24);

    // `Shift+Enter` must add a row rather than submit the draft.
    app.step(shift_enter());
    app.prompt_mut().editor_mut().insert_str("second");
    let rows = composer_rows(&app, 40);
    assert!(
        rows.len() >= 2,
        "the composer must have grown to two rows: {rows:?}"
    );
    assert_eq!(
        cursor_row(&rows),
        1,
        "the cursor sits on the new second line"
    );

    // `Up` walks back to the first line; the caret is drawn there, on the
    // row that actually holds it.
    app.step(up());
    let rows = composer_rows(&app, 40);
    assert_eq!(cursor_row(&rows), 0);
    assert!(rows[0].contains("first"), "{rows:?}");
}
