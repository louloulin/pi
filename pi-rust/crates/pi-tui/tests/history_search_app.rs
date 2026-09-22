//! The reverse history search at the **App** level.
//!
//! `tests/history_search.rs` pins the editor's state machine. This file pins
//! the two things only the App can get wrong:
//!
//! 1. the footer carries `reverse-i-search: <query>` while the search is open
//!    (codex renders the same text on its footer line, and the port has no
//!    spare row for it — it rides the status hint);
//! 2. the search keeps the whole keyboard, so an `app.*` chord cannot fire on
//!    a keystroke the user means as a query character (codex's
//!    `handle_history_search_key` calls this out explicitly).

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::input::{Key, KeyCode, KeyModifiers};

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
        session_id: "history-search".into(),
        ..AppConfig::default()
    };
    App::new(&agent, config)
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Key {
    Key::new(code, modifiers)
}

fn ctrl(c: char) -> Key {
    key(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn plain(c: char) -> Key {
    key(KeyCode::Char(c), KeyModifiers::NONE)
}

/// The footer row of a rendered frame, i.e. the row carrying the status hint.
fn hint_row(app: &App, width: u16, height: u16) -> String {
    app.render_snapshot(width, height)
        .lines
        .last()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn the_footer_carries_the_reverse_search_query_and_preview() {
    let mut app = app();
    app.prompt_mut().push_history("deploy the release");
    app.prompt_mut().push_history("write the changelog");

    // Nothing is shown before the search opens.
    assert!(!app.history_search_active());
    assert_eq!(app.history_search_hint(), None);
    assert!(!hint_row(&app, 80, 24).contains("reverse-i-search"));

    app.step_key(ctrl('r'));
    assert!(app.history_search_active());
    assert_eq!(
        app.history_search_hint().as_deref(),
        Some("reverse-i-search: "),
        "an empty query carries no accept/cancel affordance"
    );

    // Typing builds the query in the footer and previews the newest match.
    for ch in "deploy".chars() {
        app.step_key(plain(ch));
    }
    assert_eq!(
        app.history_search_hint().as_deref(),
        Some("reverse-i-search: deploy  Enter accept · Esc cancel")
    );
    assert_eq!(app.prompt().text(), "deploy the release");
    let frame = hint_row(&app, 80, 24);
    assert!(
        frame.contains("reverse-i-search: deploy"),
        "the footer row must carry the query:\n{frame}"
    );

    // A miss says so instead, and puts the draft back.
    app.step_key(plain('z'));
    assert_eq!(
        app.history_search_hint().as_deref(),
        Some("reverse-i-search: deployz  no match")
    );
    assert_eq!(app.prompt().text(), "");

    // Esc closes the search and clears the footer chrome.
    app.step_key(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.history_search_active());
    assert!(!hint_row(&app, 80, 24).contains("reverse-i-search"));
}

#[test]
fn enter_accepts_the_preview_into_the_composer() {
    let mut app = app();
    app.prompt_mut().push_history("deploy the release");

    for ch in "typed but not sent".chars() {
        app.step_key(plain(ch));
    }
    app.step_key(ctrl('r'));
    for ch in "deploy".chars() {
        app.step_key(plain(ch));
    }
    assert_eq!(app.prompt().text(), "deploy the release");

    app.step_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(!app.history_search_active());
    assert_eq!(app.prompt().text(), "deploy the release");
}

#[test]
fn the_search_keeps_the_keyboard_from_the_app_chords() {
    let mut app = app();
    app.prompt_mut().push_history("expand me");
    // Start from a known tools-fold state so the assertion is about this
    // keystroke and not about the default.
    let before = app.tools_expanded();

    app.step_key(ctrl('r'));
    for ch in "expand".chars() {
        app.step_key(plain(ch));
    }
    let query_before = app
        .prompt()
        .editor()
        .history_search_query()
        .map(str::to_string);

    // `app.tools.expand` (`Ctrl+O`) and `app.clipboard.pasteImage` (`Alt+V`)
    // would both fire here if the App kept claiming `app.*` chords during the
    // search.
    app.step_key(ctrl('o'));
    app.step_key(key(KeyCode::Char('v'), KeyModifiers::ALT));

    assert!(app.history_search_active(), "the search must stay open");
    assert_eq!(
        app.tools_expanded(),
        before,
        "Ctrl+O must not fold the tool blocks while the search owns the keyboard"
    );
    assert_eq!(
        app.prompt()
            .editor()
            .history_search_query()
            .map(str::to_string),
        query_before,
        "a control chord must not be appended to the query"
    );
    assert_eq!(app.prompt().text(), "expand me", "the preview is untouched");
}

#[test]
fn a_configured_history_file_is_loaded_at_construction() {
    // The App hands the editor its history path, which is what makes
    // `Ctrl+R` recall survive a restart (codex's
    // `~/.codex/history.jsonl`).
    let dir = std::env::temp_dir().join(format!(
        "pi-tui-app-history-{}-{}",
        std::process::id(),
        line!()
    ));
    let path = dir.join("history.jsonl");
    let _ = std::fs::remove_dir_all(&dir);
    pi_tui::history_store::append(&path, "persisted prompt").unwrap();

    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let config = AppConfig {
        session_id: "history-file".into(),
        history_path: Some(path.clone()),
        ..AppConfig::default()
    };
    let mut app = App::new(&agent, config);
    assert_eq!(app.prompt().editor().history_path(), Some(path.as_path()));

    app.step_key(ctrl('r'));
    for ch in "persisted".chars() {
        app.step_key(plain(ch));
    }
    assert_eq!(app.prompt().text(), "persisted prompt");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_search_query_survives_a_narrow_footer() {
    // LUM-1367 gives the footer a whole-part budget whose first sacrifice is
    // the hint — right for the transient `? for help`, wrong for a live search
    // query. At 44 columns the query must still be on screen, even at the cost
    // of the token counters.
    let mut app = app();
    app.prompt_mut().push_history("deploy the release");
    app.step_key(ctrl('r'));
    for ch in "deploy".chars() {
        app.step_key(plain(ch));
    }

    let narrow = hint_row(&app, 44, 14);
    assert!(
        narrow.contains("reverse-i-search"),
        "the query must survive the narrow budget:
{narrow}"
    );
    // The less valuable counters are what gives way instead.
    assert!(
        !narrow.contains("in 0 out 0"),
        "counters yield first:\n{narrow}"
    );

    // Once the search closes, the ordinary budget (hint first) is back.
    app.step_key(key(KeyCode::Esc, KeyModifiers::NONE));
    let idle = hint_row(&app, 80, 24);
    assert!(!idle.contains("reverse-i-search"));
}

#[test]
fn a_search_survives_a_resize_and_a_repaint() {
    // The query lives in the editor, not in a rendered frame, so a repaint at
    // a different size cannot lose it (the footer chrome is derived state).
    let mut app = app();
    app.prompt_mut().push_history("resize safe");
    app.step_key(ctrl('r'));
    for ch in "resize".chars() {
        app.step_key(plain(ch));
    }
    let _ = app.render_snapshot(40, 12);
    let _ = app.render_snapshot(120, 40);
    assert!(app.history_search_active());
    assert_eq!(app.prompt().text(), "resize safe");
    assert!(hint_row(&app, 120, 40).contains("reverse-i-search: resize"));
}
