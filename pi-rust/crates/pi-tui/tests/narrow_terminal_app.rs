//! P29 (G1) — narrow-terminal degradation wiring into the App.
//!
//! [`App::narrow_options`] surfaces [`pi_tui::NarrowOptions`] for the
//! viewport the App last rendered, and
//! [`App::composer_max_rows_effective`] threads the extreme-narrow row
//! cap (1 row) through the configured `composer_max_rows`. These are the
//! single observation points every narrow-aware consumer reaches for so
//! the thresholds stay in one place. The unit tests below pin the
//! behaviour; the rendering integration is covered by the lib tests.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::narrow_terminal::{
    is_extreme_narrow, is_narrow, NarrowOptions, EXTREME_NARROW_WIDTH, NARROW_TERMINAL_WIDTH,
};

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        api: Api::Faux,
        id: "faux-model".into(),
        label: Some("Faux".into()),
        context_window: 1024,
        max_output_tokens: 256,
    }
}

fn fresh_app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    App::new(&agent, AppConfig::default())
}

/// 1 — Unmeasured viewport (width == 0) classifies as wide so the very
/// first frame does not flip into the narrow band.
#[test]
fn unmeasured_viewport_classifies_as_wide() {
    let app = fresh_app();
    app.set_viewport_size(0, 0);
    let opts = app.narrow_options();
    assert!(!opts.narrow);
    assert!(!opts.extreme_narrow);
    assert!(opts.spinner_animated);
    assert!(opts.show_secondary_segments);
}

/// 2 — A 120-column viewport stays wide; the configured composer cap
/// survives unchanged through `composer_max_rows_effective`.
#[test]
fn wide_viewport_returns_configured_composer_rows() {
    let app = fresh_app();
    app.set_viewport_size(120, 40);
    let opts = app.narrow_options();
    assert!(!opts.narrow);
    assert_eq!(app.composer_max_rows_effective(), app.composer_max_rows_configured());
}

/// 3 — A narrow viewport (29 columns) collapses the secondary segments
/// but leaves the composer cap alone — only the extreme-narrow band
/// touches the cap.
#[test]
fn narrow_viewport_drops_segments_but_leaves_composer() {
    let app = fresh_app();
    app.set_viewport_size(29, 24);
    let opts = app.narrow_options();
    assert!(opts.narrow);
    assert!(!opts.extreme_narrow);
    assert!(!opts.show_secondary_segments);
    assert_eq!(
        app.composer_max_rows_effective(),
        app.composer_max_rows_configured(),
    );
}

/// 4 — Extreme-narrow viewport (15 columns) freezes the spinner and
/// forces the composer to a single row, regardless of the configured cap.
#[test]
fn extreme_narrow_viewport_caps_composer_at_one_row() {
    let app = fresh_app();
    app.set_viewport_size(15, 24);
    let opts = app.narrow_options();
    assert!(opts.extreme_narrow);
    assert!(!opts.spinner_animated);
    assert_eq!(app.composer_max_rows_effective(), 1);
}

/// 5 — Threshold boundaries: `is_narrow(30) == false`,
/// `is_narrow(29) == true`, `is_extreme_narrow(20) == false`,
/// `is_extreme_narrow(19) == true` — direct re-export sanity checks.
#[test]
fn threshold_boundaries_match_helpers() {
    assert!(!is_narrow(NARROW_TERMINAL_WIDTH));
    assert!(is_narrow(NARROW_TERMINAL_WIDTH - 1));
    assert!(!is_extreme_narrow(EXTREME_NARROW_WIDTH));
    assert!(is_extreme_narrow(EXTREME_NARROW_WIDTH - 1));
}

/// 6 — `narrow_options()` returns `NarrowOptions` and is a pure function
/// of the cached viewport width — a synthetic round-trip confirms the
/// App reads the same atomic the helpers consume.
#[test]
fn narrow_options_round_trip() {
    let app = fresh_app();
    for w in [10u16, 19, 20, 29, 30, 80, 200] {
        app.set_viewport_size(w, 24);
        let opts: NarrowOptions = app.narrow_options();
        assert_eq!(opts.narrow, w < NARROW_TERMINAL_WIDTH, "width={}", w);
        assert_eq!(
            opts.extreme_narrow,
            w < EXTREME_NARROW_WIDTH,
            "width={}",
            w,
        );
    }
}

/// 7 — `composer_max_rows_effective` returns at least 1 even on the most
/// extreme case, so callers that divide by it never panic.
#[test]
fn composer_max_rows_effective_is_at_least_one() {
    let app = fresh_app();
    app.set_viewport_size(0, 0);
    assert!(app.composer_max_rows_effective() >= 1);
}