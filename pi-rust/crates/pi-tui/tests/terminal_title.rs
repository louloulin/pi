//! LUM-1485: the terminal window/tab title (upstream `Terminal.setTitle`,
//! OSC 0 + BEL).
//!
//! Two things have to stay true at once, and they are the two halves of this
//! file:
//!
//! * the **automatic** title — `"pi - <session name> - <cwd basename>"`,
//!   upstream's `updateTerminalTitle` — follows every session fact the driver
//!   hands the App (`set_session_name`, `set_status_cwd`), and
//! * the **queued** title is *edge* triggered: the driver drains exactly one
//!   title per change, and an unchanged tick writes nothing. That second
//!   property is what keeps a full-screen repaint from emitting an OSC sequence
//!   on every frame.
//!
//! The App never writes to the terminal (see `App::take_terminal_title`), so
//! what these tests assert is the queue, not the bytes; the wire format is
//! pinned in `pi_tui::terminal_title`'s own unit tests and the sequence is
//! observed on a real ConPTY stream by
//! `scripts/pty_scenarios/lum1485-terminal-title.json`.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use tokio::sync::Mutex as AsyncMutex;

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

async fn new_app() -> App {
    let agent = Arc::new(AsyncMutex::new(Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ))));
    let config = AppConfig {
        session_id: "title".into(),
        ..AppConfig::default()
    };
    App::new(&*agent.lock().await, config)
}

#[tokio::test]
async fn a_fresh_app_queues_nothing() {
    let mut app = new_app().await;
    assert_eq!(app.terminal_title(), None);
    assert_eq!(app.take_terminal_title(), None);
}

/// The driver's startup order is `set_session_name` then `set_status_cwd`
/// (interactive.rs), and it must produce **one** write, not two: the pending
/// slot is a single value, so the cwd's arrival replaces the name-only title.
#[tokio::test]
async fn startup_hands_the_driver_one_title() {
    let mut app = new_app().await;

    app.set_session_name(None);
    app.set_status_cwd(Some(r"C:\work\repo".to_string()));

    assert_eq!(app.terminal_title(), Some("pi - repo"));
    assert_eq!(app.take_terminal_title().as_deref(), Some("pi - repo"));
    // Drained: the next tick has nothing to write.
    assert_eq!(app.take_terminal_title(), None);
}

#[tokio::test]
async fn the_session_name_leads_the_title() {
    let mut app = new_app().await;
    app.set_status_cwd(Some("/srv/repo".to_string()));
    let _ = app.take_terminal_title();

    app.set_session_name(Some("renamed".to_string()));

    assert_eq!(app.terminal_title(), Some("pi - renamed - repo"));
    assert_eq!(
        app.take_terminal_title().as_deref(),
        Some("pi - renamed - repo")
    );
}

#[tokio::test]
async fn clearing_the_session_name_drops_its_component() {
    let mut app = new_app().await;
    app.set_status_cwd(Some("/srv/repo".to_string()));
    app.set_session_name(Some("demo".to_string()));
    let _ = app.take_terminal_title();

    // `/new` clears the name (interactive.rs `reset_session_name`).
    app.set_session_name(None);

    assert_eq!(app.terminal_title(), Some("pi - repo"));
}

#[tokio::test]
async fn an_unchanged_title_is_not_re_queued() {
    let mut app = new_app().await;
    app.set_status_cwd(Some("/srv/repo".to_string()));
    assert!(app.take_terminal_title().is_some());

    // The driver refreshes the same facts again (a session switch that kept
    // the cwd and the name); nothing changed, so nothing is queued.
    app.set_status_cwd(Some("/srv/repo".to_string()));
    app.set_session_name(None);

    assert_eq!(app.take_terminal_title(), None);
}

#[tokio::test]
async fn an_extension_title_wins_until_the_session_moves() {
    let mut app = new_app().await;
    app.set_status_cwd(Some("/srv/repo".to_string()));
    let _ = app.take_terminal_title();

    app.set_terminal_title("agent is compiling");

    assert_eq!(app.terminal_title(), Some("agent is compiling"));
    assert_eq!(
        app.take_terminal_title().as_deref(),
        Some("agent is compiling")
    );

    // A rename is a session change, so the automatic title replaces it —
    // upstream's `updateTerminalTitle` does not know about `setTitle`.
    app.set_session_name(Some("demo".to_string()));
    assert_eq!(app.terminal_title(), Some("pi - demo - repo"));
}

#[tokio::test]
async fn an_extension_title_cannot_escape_the_sequence() {
    let mut app = new_app().await;

    // A BEL would terminate the OSC 0 sequence early and a following ESC
    // would start a new control sequence; both are neutralised. Each control
    // character becomes its own space.
    app.set_terminal_title("evil\u{7}\u{1b}]52;c;payload");

    assert_eq!(app.terminal_title(), Some("evil  ]52;c;payload"));
    assert!(!app.take_terminal_title().unwrap().contains('\u{7}'));
}

#[tokio::test]
async fn reassertion_forces_a_write_after_a_child_owned_the_tty() {
    let mut app = new_app().await;
    app.set_status_cwd(Some("/srv/repo".to_string()));
    app.set_session_name(Some("demo".to_string()));
    let _ = app.take_terminal_title();

    // `app.suspend` / `app.editor.external` gave the title away and got it
    // back; the dedup must not swallow the restore.
    app.reassert_terminal_title();

    assert_eq!(
        app.take_terminal_title().as_deref(),
        Some("pi - demo - repo")
    );
}
