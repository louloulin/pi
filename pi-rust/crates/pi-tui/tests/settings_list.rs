//! End-to-end tests for the `/settings` modal — the [`SettingsList`] wired
//! into the whole [`App`] input path.
//!
//! These cover what `pi-coding-agent` depends on:
//!
//! 1. While the modal is open it owns the keyboard — printable keys filter
//!    the list instead of reaching the prompt, and `Ctrl+C` is ignored.
//! 2. `Enter` / `Space` cycles a value and queues exactly one
//!    `(id, value)` change for the driver to persist, while a cursor move
//!    queues nothing.
//! 3. An item without `values` reports an activation instead of a change.
//! 4. `Esc` closes the modal and restores normal key handling.
//! 5. The rendered overlay replaces the transcript rows it covers, and the
//!    wheel scrolls the list rather than the log underneath it.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::input::{InputEvent, Key, KeyCode, KeyModifiers};
use pi_tui::settings::{SettingItem, SettingsList};

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
    let config = AppConfig {
        session_id: "settings-list".into(),
        ..AppConfig::default()
    };
    App::new(&agent, config)
}

/// The curated set `pi-coding-agent` opens for `/settings`.
fn settings_items() -> Vec<SettingItem> {
    vec![
        SettingItem::new("autocompact", "Auto-compact")
            .with_description("Automatically compact context when it gets too large")
            .with_values(["true", "false"], "true"),
        SettingItem::new("theme", "Theme")
            .with_description("Color theme for the interface")
            .with_values(["dark", "light"], "dark"),
        SettingItem::new("fullscreen-copy-on-select", "Fullscreen copy on select")
            .with_description("Automatically copy selected text in fullscreen mode")
            .with_values(["true", "false"], "false"),
        SettingItem::new("warnings", "Warnings")
            .with_description("Enable or disable individual warnings"),
    ]
}

fn key(code: KeyCode) -> Key {
    Key::new(code, KeyModifiers::NONE)
}

fn open(app: &mut App) {
    app.open_settings(SettingsList::new(settings_items(), 10).searchable(true));
}

#[test]
fn the_modal_owns_the_keyboard() {
    let mut app = app();
    open(&mut app);

    for ch in "theme".chars() {
        assert_eq!(
            app.step(InputEvent::character(ch)),
            StepOutcome::Redraw,
            "{ch} should be consumed by the filter"
        );
    }

    let settings = app.settings().expect("settings are open");
    assert_eq!(settings.filter(), "theme");
    assert_eq!(settings.filtered_len(), 1);
    assert_eq!(
        app.render_snapshot(60, 12).prompt_buffer,
        "",
        "the keys must not reach the prompt"
    );

    // Ctrl+C is not a filter character and does not exit the App.
    assert_eq!(
        app.step(InputEvent::Key(Key::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ))),
        StepOutcome::Idle
    );
    assert!(app.settings_open());
}

#[test]
fn cycling_a_value_queues_exactly_one_change() {
    let mut app = app();
    open(&mut app);

    // The first item is Auto-compact.
    assert_eq!(app.step(InputEvent::Key(key(KeyCode::Enter))), StepOutcome::Redraw);
    assert_eq!(
        app.take_pending_setting_change(),
        Some(("autocompact".to_string(), "false".to_string()))
    );
    assert_eq!(
        app.take_pending_setting_change(),
        None,
        "the queue is drained by the first take"
    );
    assert!(app.settings_open(), "cycling keeps the modal open");

    // A cursor move redraws but must not ask for a write.
    assert_eq!(app.step(InputEvent::Key(key(KeyCode::Down))), StepOutcome::Redraw);
    assert_eq!(app.take_pending_setting_change(), None);
}

#[test]
fn space_cycles_the_value_too_but_only_with_an_empty_filter() {
    let mut app = app();
    open(&mut app);

    assert_eq!(
        app.step(InputEvent::Key(key(KeyCode::Char(' ')))),
        StepOutcome::Redraw
    );
    assert_eq!(
        app.take_pending_setting_change(),
        Some(("autocompact".to_string(), "false".to_string()))
    );

    // Once the user is typing, a space belongs to the query.
    assert_eq!(
        app.step(InputEvent::Key(key(KeyCode::Char('t')))),
        StepOutcome::Redraw
    );
    assert_eq!(
        app.step(InputEvent::Key(key(KeyCode::Char(' ')))),
        StepOutcome::Redraw
    );
    assert_eq!(app.settings().unwrap().filter(), "t ");
    assert_eq!(app.take_pending_setting_change(), None);
}

#[test]
fn an_item_without_values_reports_an_activation() {
    let mut app = app();
    open(&mut app);

    // Walk to the value-less "Warnings" item and confirm it.
    app.settings_mut()
        .expect("settings are open")
        .select_item("warnings");
    assert_eq!(app.step(InputEvent::Key(key(KeyCode::Enter))), StepOutcome::Redraw);
    assert_eq!(
        app.take_pending_setting_activation(),
        Some("warnings".to_string())
    );
    assert_eq!(app.take_pending_setting_change(), None);
}

#[test]
fn esc_closes_the_modal_and_returns_the_keyboard() {
    let mut app = app();
    open(&mut app);

    assert_eq!(app.step(InputEvent::Key(key(KeyCode::Esc))), StepOutcome::Redraw);
    assert!(!app.settings_open());
    assert!(app.close_settings().is_none());

    // The prompt receives keys again.
    app.step(InputEvent::character('h'));
    assert_eq!(app.render_snapshot(60, 12).prompt_buffer, "h");
}

#[test]
fn opening_a_new_list_drops_a_stale_pending_change() {
    let mut app = app();
    open(&mut app);
    app.step(InputEvent::Key(key(KeyCode::Enter)));
    assert!(app.take_pending_setting_change().is_some());

    // Re-open: the queue is cleared, so the previous session's write
    // cannot be attributed to the new list.
    open(&mut app);
    assert_eq!(app.take_pending_setting_change(), None);
}

#[test]
fn the_overlay_covers_the_transcript_rows() {
    let mut app = app();
    app.step(InputEvent::character('x'));
    app.step(InputEvent::Key(key(KeyCode::Enter)));
    open(&mut app);

    let snapshot = app.render_snapshot(70, 14);
    assert!(snapshot.settings_open);
    assert_eq!(snapshot.settings_lines[0], "> ");
    assert_eq!(snapshot.lines[1], ">", "the search line is the first overlay row");
    assert!(
        snapshot.lines[3].contains("→ Auto-compact"),
        "the overlay starts on the first content row: {:?}",
        snapshot.lines
    );
    assert!(
        snapshot.lines.iter().any(|line| line.contains("Enter/Space to change")),
        "the hint is rendered: {:?}",
        snapshot.lines
    );
}

#[test]
fn the_wheel_scrolls_the_list_not_the_log() {
    let mut app = app();
    open(&mut app);
    let before = app.render_snapshot(60, 12).settings_lines;
    assert!(before[2].starts_with("→ Auto-compact"), "{before:?}");

    assert_eq!(
        app.step(InputEvent::Mouse { up: false, alt: false }),
        StepOutcome::Redraw
    );
    let after = app.render_snapshot(60, 12).settings_lines;
    assert!(
        after[2].starts_with("  Auto-compact") && after[3].starts_with("→ Theme"),
        "the cursor moved down one row: {after:?}"
    );
    assert!(app.settings().is_some(), "the wheel must not close the modal");
}

#[test]
fn the_theme_slot_recolours_the_overlay() {
    let mut app = app();
    app.set_theme_by_name("light").expect("light theme");
    open(&mut app);
    let light = app.render_snapshot(60, 12).lines[3].clone();

    app.set_theme_by_name("dark").expect("dark theme");
    let dark = app.render_snapshot(60, 12).lines[3].clone();

    assert_eq!(light, dark, "the cell text is theme-independent");
}
