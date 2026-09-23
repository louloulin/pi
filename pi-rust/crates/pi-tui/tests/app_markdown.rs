//! Integration coverage for the App-level markdown switch.
//!
//! `MessageView` has had an opt-in markdown renderer since LUM-1117
//! (`crates/pi-tui/src/markdown.rs`), but for a while nothing outside the
//! crate turned it on, so the interactive TUI kept rendering assistant
//! bodies as plain text. These tests pin the wiring:
//!
//! * [`AppConfig::markdown`] defaults to `true`, matching upstream, whose
//!   assistant message body is a `Markdown` component
//!   (`packages/coding-agent/src/modes/interactive/components/assistant-message.ts`).
//! * Turning it off falls back to the plain-text path.
//! * `App::set_markdown` flips it at runtime, and `/clear` does not reset
//!   it.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::MessageItem;

const WIDTH: u16 = 40;
const HEIGHT: u16 = 6;

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

fn app(config: AppConfig) -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    App::new(&agent, config)
}

/// Visible text of the rendered App, joined for `contains` assertions.
fn rendered(app: &App) -> String {
    app.render_snapshot(WIDTH, HEIGHT).lines.join("\n")
}

/// An assistant body that only differs between the two paths: markdown strips
/// the `#` heading marker and the `**` emphasis markers, plain text keeps them.
const BODY: &str = "# Heading\n\nbody with **bold** text";

#[test]
fn markdown_is_on_by_default() {
    let mut app = app(AppConfig {
        session_id: "md-default".into(),
        ..AppConfig::default()
    });
    assert!(app.markdown(), "AppConfig::markdown defaults to true");
    app.messages_mut().push(MessageItem::assistant(BODY));

    let joined = rendered(&app);
    assert!(
        joined.contains("Heading"),
        "heading text missing: {joined:?}"
    );
    assert!(
        !joined.contains("# Heading"),
        "heading marker was not consumed: {joined:?}"
    );
    assert!(
        joined.contains("body with bold text"),
        "emphasis not rendered: {joined:?}"
    );
    assert!(
        !joined.contains("**bold**"),
        "emphasis markers were not consumed: {joined:?}"
    );
}

#[test]
fn markdown_can_be_disabled() {
    let mut app = app(AppConfig {
        session_id: "md-off".into(),
        markdown: false,
        ..AppConfig::default()
    });
    assert!(!app.markdown());
    app.messages_mut().push(MessageItem::assistant(BODY));

    let joined = rendered(&app);
    assert!(
        joined.contains("# Heading"),
        "plain-text path should keep the marker: {joined:?}"
    );
    assert!(
        joined.contains("**bold**"),
        "plain-text path should keep the emphasis markers: {joined:?}"
    );
}

#[test]
fn set_markdown_switches_paths_without_losing_the_transcript() {
    let mut app = app(AppConfig {
        session_id: "md-toggle".into(),
        ..AppConfig::default()
    });
    app.messages_mut().push(MessageItem::assistant(BODY));
    assert!(rendered(&app).contains("Heading"));

    app.set_markdown(false);
    assert!(!app.markdown());
    let plain = rendered(&app);
    assert!(plain.contains("# Heading"));
    assert!(plain.contains("**bold**"));

    // The transcript items were never touched, so switching back re-renders
    // the same body as markdown.
    app.set_markdown(true);
    assert!(app.markdown());
    let md = rendered(&app);
    assert!(!md.contains("# Heading"));
    assert!(!md.contains("**bold**"));
}

#[test]
fn clear_keeps_the_markdown_switch() {
    let mut app = app(AppConfig {
        session_id: "md-clear".into(),
        ..AppConfig::default()
    });
    app.messages_mut().push(MessageItem::assistant(BODY));
    assert!(app.markdown());

    // `/clear` clears the message log and must leave the rendering mode
    // alone. The chord that used to reach it (`Ctrl+L`) belongs to
    // `app.model.select` upstream and is claimed by the driver, not the App
    // (LUM-1245).
    app.clear_transcript();
    assert!(app.messages().is_empty(), "clear_transcript clears the log");
    assert!(
        app.markdown(),
        "clear_transcript must not reset the markdown switch"
    );

    // New output still renders as markdown after the clear.
    app.messages_mut().push(MessageItem::assistant(BODY));
    assert!(!rendered(&app).contains("# Heading"));
}
