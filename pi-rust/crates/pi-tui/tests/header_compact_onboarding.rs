//! Phase 7 / G4 — the compactOnboarding row upstream prints when the
//! user has folded the header (`interactive-mode.ts:948`).
//!
//! Three things this file pins:
//!
//! * The folded / compact-mode row uses the upstream wording:
//!   `Press <chord> to show full startup help and loaded resources.`
//! * The same row also appears when the terminal is too short to hold the
//!   expanded hint list (LUM-1266 — the fold row uses the upstream string
//!   so both states point at the same chord).
//! * Both English and Chinese variants are wired through
//!   [`locale::header_folded_line`].

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::input::{Key, KeyCode, KeyModifiers};
use pi_tui::locale::{header_folded_line, Locale, HEADER_COMPACT_ONBOARDING_EN_TEMPLATE};

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

fn app_with(agent: &Agent, tweak: impl FnOnce(&mut AppConfig)) -> App {
    let mut config = AppConfig {
        session_id: "compact-onboarding".into(),
        ..AppConfig::default()
    };
    tweak(&mut config);
    App::new(agent, config)
}

fn alt_h() -> Key {
    Key::new(KeyCode::Char('h'), KeyModifiers::ALT)
}

#[test]
fn header_folded_line_uses_the_upstream_compact_onboarding_string() {
    // Upstream wording is the source of truth — the Rust port just plugs the
    // resolved chord into `{keys}`.
    let line = header_folded_line(Locale::En, "Alt+H");
    assert_eq!(
        line,
        "Press Alt+H to show full startup help and loaded resources."
    );
    assert_eq!(
        HEADER_COMPACT_ONBOARDING_EN_TEMPLATE,
        "Press {keys} to show full startup help and loaded resources."
    );
}

#[test]
fn header_folded_line_chinese_renders_a_translated_compact_onboarding_row() {
    let line = header_folded_line(Locale::Zh, "Alt+H");
    assert!(line.starts_with("按 Alt+H"), "{line}");
    assert!(line.contains("完整启动帮助"), "{line}");
}

#[test]
fn folded_header_renders_the_compact_onboarding_row_after_the_title() {
    // User presses `app.header` (`Alt+H`) to fold it. The header must now
    // carry the title *plus* the compactOnboarding row, matching TS pi-tui's
    // `logo + compactOnboarding` compact layout (`interactive-mode.ts:948`).
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = app_with(&agent, |config| {
        config.startup_header = true;
        config.startup_header_expanded = true;
    });
    let _ = app.render_snapshot(100, 32);

    assert!(app.header_expanded());
    assert_eq!(app.step_key(alt_h()), StepOutcome::Redraw);
    assert!(!app.header_expanded());

    let folded = app.render_snapshot(100, 32).lines;
    assert!(folded[0].starts_with("pi v"), "title still shows: {folded:?}");
    assert!(
        folded[1].contains("to show full startup help and loaded resources"),
        "compactOnboarding row right under the title: {folded:?}"
    );
    assert!(
        !folded.join("\n").contains("to delete to end"),
        "the expanded hint list must not bleed into compact mode: {folded:?}"
    );
}