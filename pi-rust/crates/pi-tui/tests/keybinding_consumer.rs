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
    reset_keybindings, set_keybindings, KeybindingsConfig, KeybindingsManager,
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
    // One override per consumer: the editor's cursor-up moves to `Ctrl+P`
    // and the App's page-up to `Ctrl+O`, releasing `Up` / `PageUp`.
    let mut config = KeybindingsConfig::new();
    config.set("tui.editor.cursorUp", ["ctrl+p"]);
    config.set("tui.altScreen.pageUp", ["ctrl+o"]);
    let mut manager = KeybindingsManager::tui_defaults();
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
        app.step_key(key(KeyCode::Char('o'), KeyModifiers::CONTROL)),
        StepOutcome::Redraw
    );
    assert!(app.messages().scroll_offset() > 0);

    reset_keybindings();
}
