//! Stage 66 (LUM-1228): the interactive driver's launch surface for the
//! startup header.
//!
//! `busy_feedback.rs` / `startup_header.rs` in `pi-tui` cover the rendering
//! and the chord resolution. What is left — and what only the driver can get
//! wrong — is the wiring: which options turn the header on, and whether the
//! rows resolve against the table the driver actually installs.
//!
//! Own integration binary: `set_keybindings` mutates process state.

use std::sync::Arc;

use clap::Parser;
use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_coding_agent::cli::Cli;
use pi_coding_agent::install_keybindings_from;
use pi_coding_agent::interactive::{interactive_app_config, InteractiveOptions};
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::App;
use pi_tui::keybindings::reset_keybindings;

const WIDTH: u16 = 100;
const HEIGHT: u16 = 32;

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

fn render(options: &InteractiveOptions, dir: &std::path::Path) -> Vec<String> {
    install_keybindings_from(dir);
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let app = App::new(&agent, interactive_app_config(options));
    app.render_snapshot(WIDTH, HEIGHT).lines
}

#[test]
fn interactive_mode_shows_the_header_with_the_shipped_chords() {
    let dir = tempfile::tempdir().expect("temp dir");
    let options = InteractiveOptions::default();
    let lines = render(&options, dir.path());

    assert!(lines[0].starts_with("pi v"), "{:?}", lines[0]);
    let text = lines.join("\n");
    // The driver's real table (`merged_definitions`), not a fixture: these
    // are the chords the coding agent ships.
    assert!(text.contains("Ctrl+C"), "{text}");
    assert!(text.contains("Ctrl+O"), "{text}");
    assert!(text.contains("Alt+H"), "{text}");
    assert!(text.contains("/ for commands"), "{text}");

    reset_keybindings();
}

#[test]
fn a_quiet_startup_renders_no_header() {
    let dir = tempfile::tempdir().expect("temp dir");
    let options = InteractiveOptions {
        quiet_startup: true,
        ..InteractiveOptions::default()
    };
    let lines = render(&options, dir.path());

    // No title row, and the transcript starts at the top of the frame.
    assert!(!lines[0].starts_with("pi v"), "{:?}", lines[0]);
    assert!(!lines.join("\n").contains("to interrupt"));

    reset_keybindings();
}

#[test]
fn the_no_header_flag_maps_onto_the_quiet_startup_option() {
    // `main.rs` forwards `cli.no_header` into `InteractiveOptions::quiet_startup`.
    let cli = Cli::try_parse_from(["pi", "--no-header"]).expect("parses");
    assert!(cli.no_header);
    let cli = Cli::try_parse_from(["pi"]).expect("parses");
    assert!(!cli.no_header);
}
