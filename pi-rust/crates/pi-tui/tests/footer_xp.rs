//! The footer's `• xp` experimental-features indicator (Phase 4 / G2).
//!
//! Upstream paints a single dim bullet followed by a bold warning-coloured
//! `xp` token whenever the driver reports
//! `areExperimentalFeaturesEnabled()` is true
//! (`packages/coding-agent/src/modes/interactive/components/footer.ts:162-164`).
//! The Rust port wires the same flag through [`StatusData::experimental`] and
//! surfaces it on [`App`] via [`App::set_experimental`] / [`App::experimental`].
//! When the flag is false the indicator must not appear at all — the segment
//! is part of [`NARROW_SACRIFICE_ORDER`] so it yields its row to the model
//! name when the bar is squeezed.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::status::{StatusBar, StatusData};
use pi_tui::styles::SelectListStyles;
use pi_tui::theme::{builtin_theme, ColorMode, Theme};

// dark.json: warning = "yellow" → 255,255,0; dim = "dimGray" → 102,102,102.
const WARNING: &str = "\x1b[38;2;255;255;0m";
const DIM: &str = "\x1b[38;2;102;102;102m";
const BOLD: &str = "\x1b[1m";

fn dark() -> Theme {
    builtin_theme("dark", ColorMode::TrueColor).expect("built-in dark theme")
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

fn app_with_status(setup: impl FnOnce(&mut App)) -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(&agent, AppConfig::default());
    setup(&mut app);
    app
}

#[test]
fn footer_omits_xp_when_disabled() {
    let theme = dark();
    let styles = SelectListStyles::new(&theme);
    let bar = StatusBar::new();
    let data = StatusData::new("gpt-4o", "abc-123").with_hint("? for help");

    let themed = bar.render_themed(&data, 80, &styles);

    assert!(
        !themed.contains("xp"),
        "disabled footer must not render the `xp` token: {themed:?}"
    );
    assert!(
        !themed.contains(&format!("{WARNING}xp")),
        "disabled footer must not leak the warning prefix either: {themed:?}"
    );
}

#[test]
fn footer_shows_xp_when_experimental_is_enabled() {
    let theme = dark();
    let styles = SelectListStyles::new(&theme);
    let bar = StatusBar::new();
    let data = StatusData::new("gpt-4o", "abc-123")
        .with_hint("? for help")
        .with_experimental(true);

    let themed = bar.render_themed(&data, 80, &styles);

    // The bullet is dim and the `xp` token is warning-coloured + bold, mirroring
    // upstream's `[[ "•" (dim), "xp" (warning+bold) ]]` join.
    assert!(
        themed.contains(&format!("{DIM} • \x1b[39m{WARNING}{BOLD}xp\x1b[22m\x1b[39m")),
        "experimental footer must render ` • ` dim then `xp` warning+bold: {themed:?}"
    );
}

#[test]
fn status_data_experimental_round_trips() {
    let mut data = StatusData::new("gpt-4o", "abc-123");
    assert!(!data.experimental());
    assert!(!data.clone().experimental());

    data.set_experimental(true);
    assert!(data.experimental());

    let built = StatusData::new("gpt-4o", "abc-123").with_experimental(true);
    assert!(built.experimental());
}

#[test]
fn xp_zone_is_sacrificed_before_the_model_name_when_the_bar_is_narrow() {
    // When the row is squeezed, the indicator must be the first zone to drop
    // so the model name stays visible. Upstream applies the same priority
    // (`footer.ts:64-70`). We probe two widths: at 80 cols both segments fit;
    // at 20 cols only the model survives.
    let theme = dark();
    let styles = SelectListStyles::new(&theme);
    let bar = StatusBar::new();
    let data = StatusData::new("gpt-4o", "abc-123")
        .with_hint("? for help")
        .with_experimental(true);

    let wide = bar.render_themed(&data, 80, &styles);
    assert!(wide.contains("gpt-4o"));
    assert!(
        wide.contains(&format!("{BOLD}xp")),
        "wide footer must include the xp indicator: {wide:?}"
    );

    let narrow = bar.render_themed(&data, 20, &styles);
    assert!(
        narrow.contains("gpt-4o"),
        "the model segment is the last to fall off: {narrow:?}"
    );
    assert!(
        !narrow.contains("xp"),
        "the xp indicator is sacrificed before the model: {narrow:?}"
    );
}

#[test]
fn app_set_experimental_round_trips_and_reaches_the_footer() {
    let theme = dark();
    let styles = SelectListStyles::new(&theme);
    let bar = StatusBar::new();

    let mut app = app_with_status(|_| {});

    // Default state: flag off, footer does not advertise the indicator.
    assert!(!app.experimental());
    assert!(
        !bar.render_themed(app.status_data(), 80, &styles).contains("xp"),
        "default footer must not render `xp`"
    );

    // Toggle on; getter mirrors the new state and the footer grows the segment.
    app.set_experimental(true);
    assert!(app.experimental());
    assert!(
        bar.render_themed(app.status_data(), 80, &styles)
            .contains(&format!("{BOLD}xp")),
        "after App::set_experimental(true) the footer must contain `xp`: {rendered:?}",
        rendered = bar.render_themed(app.status_data(), 80, &styles),
    );

    // Toggle off again — getter and footer return to the original layout.
    app.set_experimental(false);
    assert!(!app.experimental());
    assert!(
        !bar.render_themed(app.status_data(), 80, &styles).contains("xp"),
        "after App::set_experimental(false) the footer must drop `xp` again"
    );
}