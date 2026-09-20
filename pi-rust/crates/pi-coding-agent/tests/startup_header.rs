//! Stage 66 (LUM-1228): the interactive driver's launch surface for the
//! startup header.
//!
//! `busy_feedback.rs` / `startup_header.rs` in `pi-tui` cover the rendering
//! and the chord resolution. What is left — and what only the driver can get
//! wrong — is the wiring: which options turn the header on, and whether the
//! rows resolve against the table the driver actually installs.
//!
//! Own integration binary: `set_keybindings` mutates process state.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_coding_agent::cli::Cli;
use pi_coding_agent::extensions::wiring::ExtensionReport;
use pi_coding_agent::install_keybindings_from;
use pi_coding_agent::interactive::{interactive_app_config, InteractiveOptions};
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, ExtensionHeader};
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

/// `install_keybindings_from` and `reset_keybindings` mutate process state,
/// and cargo runs this binary's tests concurrently: every test that renders
/// a frame takes this lock, exactly like `pi-tui`'s `startup_header.rs`.
static REGISTRY: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_registry() -> std::sync::MutexGuard<'static, ()> {
    REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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
    let _guard = lock_registry();
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
    let _guard = lock_registry();
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

// --- Stage 71 (LUM-1239): the extension summary row ---------------------

#[test]
fn the_driver_projects_loaded_extensions_onto_the_header() {
    // Absolute paths outside home and cwd stay absolute, so this is stable
    // on every machine.
    let options = InteractiveOptions {
        extension_report: ExtensionReport {
            loaded: vec![
                PathBuf::from("/opt/ext/fixture-ext.mjs"),
                PathBuf::from("/etc/pi/foo.mjs"),
            ],
            ..ExtensionReport::default()
        },
        ..InteractiveOptions::default()
    };
    let config = interactive_app_config(&options);
    assert_eq!(
        config.extension_header,
        ExtensionHeader::Loaded {
            count: 2,
            names: vec!["/opt/ext/fixture-ext.mjs".into(), "/etc/pi/foo.mjs".into()],
        }
    );
}

#[test]
fn the_driver_maps_no_extensions_flag_onto_the_header() {
    let options = InteractiveOptions {
        extension_report: ExtensionReport {
            disabled: true,
            ..ExtensionReport::default()
        },
        ..InteractiveOptions::default()
    };
    assert_eq!(
        interactive_app_config(&options).extension_header,
        ExtensionHeader::Disabled
    );
}

#[test]
fn a_run_without_extensions_keeps_the_header_hidden() {
    assert_eq!(
        interactive_app_config(&InteractiveOptions::default()).extension_header,
        ExtensionHeader::Hidden
    );
}

#[test]
fn the_extension_row_is_rendered_below_the_title() {
    let _guard = lock_registry();
    let dir = tempfile::tempdir().expect("temp dir");
    let options = InteractiveOptions {
        extension_report: ExtensionReport {
            loaded: vec![PathBuf::from("/opt/ext/fixture-ext.mjs")],
            commands: vec![pi_extensions::RegisteredCommand {
                name: "ext-echo".into(),
                description: "echo".into(),
            }],
            ..ExtensionReport::default()
        },
        ..InteractiveOptions::default()
    };
    let lines = render(&options, dir.path());

    assert!(lines[0].starts_with("pi v"), "{:?}", lines[0]);
    assert_eq!(lines[1], "1 extension(s): /opt/ext/fixture-ext.mjs");

    reset_keybindings();
}
