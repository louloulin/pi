//! The keybinding *consumer*: the TUI components must resolve their chords
//! through the process-wide registry, so an installed override both moves a
//! chord and releases the default one.
//!
//! Kept in its own integration binary: `set_keybindings` mutates process
//! state, and cargo gives every `tests/*.rs` file its own process, so this
//! test can replace the global table without racing the other suites.

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::editor::{Editor, EditorAction};
use pi_tui::input::{Key, KeyCode, KeyModifiers};
use pi_tui::keybindings::{
    reset_keybindings, set_keybindings, tui_default_keybindings, KeybindingDefinition,
    KeybindingsConfig, KeybindingsManager,
};

const WIDTH: u16 = 40;
const HEIGHT: u16 = 10;

fn key(code: KeyCode, modifiers: KeyModifiers) -> Key {
    Key::new(code, modifiers)
}

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

#[test]
fn installed_overrides_win_over_the_builtin_chords() {
    // One override per consumer: the editor's cursor-up moves to `Ctrl+P`,
    // the App's page-up to `Ctrl+Y`, and the tool-block toggle to `Ctrl+G`,
    // releasing `Up` / `PageUp` / `Ctrl+O`.
    let mut config = KeybindingsConfig::new();
    config.set("tui.editor.cursorUp", ["ctrl+p"]);
    config.set("tui.altScreen.pageUp", ["ctrl+y"]);
    config.set("app.tools.expand", ["ctrl+g"]);
    config.set("app.header", ["alt+j"]);
    // `pi-tui`'s own table has no `app.*` entries (the coding-agent driver
    // installs those), so add the ones the App consumes before overriding
    // them.
    let mut definitions = tui_default_keybindings();
    definitions.push((
        "app.tools.expand".to_string(),
        KeybindingDefinition::new(["ctrl+o"]),
    ));
    definitions.push((
        "app.header".to_string(),
        KeybindingDefinition::new(["alt+h"]),
    ));
    let mut manager = KeybindingsManager::new(definitions, KeybindingsConfig::new());
    manager.set_user_bindings(config);
    set_keybindings(manager);

    // --- Editor ---------------------------------------------------------
    let mut editor = Editor::new();
    editor.push_history("previous");
    // The default chord is released.
    assert_eq!(
        editor.handle_key(key(KeyCode::Up, KeyModifiers::NONE)),
        EditorAction::None
    );
    // The override drives the same action.
    assert_eq!(
        editor.handle_key(key(KeyCode::Char('p'), KeyModifiers::CONTROL)),
        EditorAction::Changed
    );
    assert_eq!(editor.text(), "previous");

    // --- App ------------------------------------------------------------
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        std::sync::Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "keybinding-consumer".into(),
            ..AppConfig::default()
        },
    );
    for i in 0..40 {
        app.info(format!("line {i}"));
    }
    // Render once so the App knows its viewport (as `app_scroll` does).
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    assert_eq!(app.messages().scroll_offset(), 0);

    // The default chord no longer scrolls.
    assert_eq!(
        app.step_key(key(KeyCode::PageUp, KeyModifiers::NONE)),
        StepOutcome::Idle
    );
    assert_eq!(app.messages().scroll_offset(), 0);
    // The override scrolls a page.
    assert_eq!(
        app.step_key(key(KeyCode::Char('y'), KeyModifiers::CONTROL)),
        StepOutcome::Redraw
    );
    assert!(app.messages().scroll_offset() > 0);

    // --- Tool-block expansion ------------------------------------------
    // `app.tools.expand` was moved off its default `Ctrl+O` by the override
    // above, so the freed chord no longer toggles and the new one does.
    assert!(!app.tools_expanded());
    assert_eq!(
        app.step_key(key(KeyCode::Char('o'), KeyModifiers::CONTROL)),
        StepOutcome::Idle
    );
    assert!(!app.tools_expanded());
    assert_eq!(
        app.step_key(key(KeyCode::Char('g'), KeyModifiers::CONTROL)),
        StepOutcome::Redraw
    );
    assert!(app.tools_expanded());

    // --- Startup header -------------------------------------------------
    // `app.header` is a Rust-only id whose built-in fallback is `Alt+H`
    // (upstream folds the header together with the tool blocks). Once the id
    // is in the table the registry wins, so `Alt+H` is released and `Alt+J`
    // drives it instead. `busy_feedback.rs` covers the no-registry branch,
    // where `matches_with_fallback` falls back to the built-in chord.
    assert!(app.header_expanded());
    assert_eq!(
        app.step_key(key(KeyCode::Char('h'), KeyModifiers::ALT)),
        StepOutcome::Idle
    );
    assert!(app.header_expanded());
    assert_eq!(
        app.step_key(key(KeyCode::Char('j'), KeyModifiers::ALT)),
        StepOutcome::Redraw
    );
    assert!(!app.header_expanded());

    reset_keybindings();
}
