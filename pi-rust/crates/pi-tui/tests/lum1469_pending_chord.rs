//! LUM-1469 — the queued-messages hint advertises the *effective*
//! `app.message.dequeue` chord.
//!
//! Upstream resolves the hint text through `getAppKeyDisplay("app.message.dequeue")`
//! (`interactive-mode.ts:4380-4382`), so a `keybindings.json` override moves
//! both the key that restores the queues and the row that tells the reader
//! which key that is. Before this round the port had no contextual hint at all
//! (the chord was only advertised on the startup header).
//!
//! `set_keybindings` mutates process state, so every phase lives in one test
//! and this file is its own integration binary — the rule
//! `tests/hint_bindings.rs` documents.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::keybindings::{
    reset_keybindings, set_keybindings, tui_default_keybindings, KeybindingDefinition,
    KeybindingsConfig, KeybindingsManager,
};
use pi_tui::message::{MessageItem, PendingMessageKind};

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: Some("Faux".into()),
        context_window: 8192,
        max_output_tokens: 512,
    }
}

/// An App with one queued prompt, rendered at a width that fits the hint.
fn app_with_queue() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1469-chord".into(),
            ..AppConfig::default()
        },
    );
    app.messages_mut()
        .push(MessageItem::user("queue something"));
    app.messages_mut()
        .push_pending(PendingMessageKind::Steer, "queued");
    app
}

/// A manager whose `app.message.dequeue` is bound to `keys` (empty = unbound).
fn manager(keys: Vec<&str>) -> KeybindingsManager {
    let mut definitions = tui_default_keybindings();
    definitions.push((
        "app.message.dequeue".to_string(),
        KeybindingDefinition::new(keys),
    ));
    KeybindingsManager::new(definitions, KeybindingsConfig::new())
}

/// The hint row of a rendered frame, without its trailing padding.
fn hint_row(app: &App) -> String {
    app.render_snapshot(90, 20)
        .lines
        .iter()
        .find(|line| line.trim_end().starts_with('↳'))
        .map(|line| line.trim_end().to_string())
        .expect("the queued-messages hint row is painted")
}

#[test]
fn the_dequeue_hint_follows_the_effective_keybindings() {
    // A bare `pi-tui` registry has no `app.message.dequeue` at all: the
    // shipped platform default stands in (`alt+up`, `alt+q` on Windows —
    // `packages/coding-agent/src/core/keybindings.ts:138`).
    reset_keybindings();
    let bare = hint_row(&app_with_queue());
    let expected_fallback = if cfg!(windows) { "Alt+Q" } else { "Alt+Up" };
    assert_eq!(
        bare,
        format!("↳ {expected_fallback} to edit all queued messages"),
        "an unknown id falls back to the shipped default"
    );

    // An override reaches the hint, exactly like every other hint surface
    // (LUM-1447/1450).
    set_keybindings(manager(vec!["ctrl+u"]));
    assert_eq!(
        hint_row(&app_with_queue()),
        "↳ Ctrl+U to edit all queued messages"
    );

    // A deliberately unbound id drops the chord instead of advertising a key
    // that does nothing.
    set_keybindings(manager(vec![]));
    assert_eq!(hint_row(&app_with_queue()), "↳ to edit all queued messages");

    reset_keybindings();
}
