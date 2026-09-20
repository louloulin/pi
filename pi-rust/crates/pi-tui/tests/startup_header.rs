//! The startup header's key hints, resolved against a live keybinding table.
//!
//! `pi-tui`'s own registry ships only the `tui.*` ids — the coding-agent
//! driver installs `app.*` (`APP_KEYBINDING_IDS`) before the App runs. These
//! tests install a table with the ids the header names, so the rows show real
//! chords, an override moves one, and an unbound action drops out.
//!
//! Kept in its own integration binary on purpose: `set_keybindings` mutates
//! process state, and cargo gives every `tests/*.rs` file its own process, so
//! this suite cannot race the header assertions in `busy_feedback.rs`.

use std::sync::{Arc, Mutex};

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, ExtensionHeader};
use pi_tui::component::TextComponent;
use pi_tui::keybindings::{
    reset_keybindings, set_keybindings, tui_default_keybindings, KeybindingDefinition,
    KeybindingsConfig, KeybindingsManager,
};

const WIDTH: u16 = 100;
const HEIGHT: u16 = 32;

/// A terminal too short for the expanded hint list — the 120×23 PTY capture
/// of LUM-1260 §3.
const SHORT_HEIGHT: u16 = 23;

/// The chords the header should resolve, one row per id it names.
const APP_CHORDS: &[(&str, &str)] = &[
    ("app.interrupt", "escape"),
    ("app.clear", "ctrl+c"),
    ("app.exit", "ctrl+d"),
    ("app.suspend", "ctrl+z"),
    ("app.thinking.cycle", "shift+tab"),
    ("app.model.cycleForward", "ctrl+p"),
    ("app.model.cycleBackward", "shift+ctrl+p"),
    ("app.model.select", "ctrl+l"),
    ("app.tools.expand", "ctrl+o"),
    ("app.header", "alt+h"),
    ("app.thinking.toggle", "ctrl+t"),
    ("app.editor.external", "ctrl+e"),
    ("app.message.followUp", "alt+enter"),
    ("app.message.dequeue", "alt+up"),
    ("app.clipboard.pasteImage", "ctrl+v"),
];

/// `set_keybindings` is process-global, and cargo runs the tests in this
/// binary concurrently: every test that installs a table takes this lock.
static REGISTRY: Mutex<()> = Mutex::new(());

fn lock_registry() -> std::sync::MutexGuard<'static, ()> {
    REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Install the `tui.*` table plus `APP_CHORDS`, with `overrides` applied on
/// top (`(id, chord)` rows replace every chord of that id).
fn install(overrides: &[(&str, &str)]) {
    let mut definitions = tui_default_keybindings();
    let mut config = KeybindingsConfig::new();
    for (id, chord) in APP_CHORDS {
        definitions.push(((*id).to_string(), KeybindingDefinition::new([*chord])));
    }
    for (id, chord) in overrides {
        config.set(*id, [*chord]);
    }
    let mut manager = KeybindingsManager::new(definitions, KeybindingsConfig::new());
    manager.set_user_bindings(config);
    set_keybindings(manager);
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

fn header_lines() -> Vec<String> {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let app = App::new(
        &agent,
        AppConfig {
            session_id: "startup-header".into(),
            startup_header: true,
            ..AppConfig::default()
        },
    );
    app.render_snapshot(WIDTH, HEIGHT).lines
}

/// The header for one [`AppConfig`], with the startup header forced on.
fn header_lines_with(extension_header: ExtensionHeader) -> Vec<String> {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let app = App::new(
        &agent,
        AppConfig {
            session_id: "startup-header".into(),
            startup_header: true,
            extension_header,
            ..AppConfig::default()
        },
    );
    app.render_snapshot(WIDTH, HEIGHT).lines
}

#[test]
fn the_header_resolves_the_live_chords() {
    let _guard = lock_registry();
    install(&[]);
    let text = header_lines().join("\n");

    // One row per `STARTUP_HINTS` entry that is bound, in table order, with
    // the effective chord — the same text `/hotkeys` prints.
    assert!(text.contains("Esc to interrupt"), "{text}");
    assert!(text.contains("Ctrl+C to clear"), "{text}");
    assert!(text.contains("Ctrl+C twice to exit"), "{text}");
    assert!(text.contains("Ctrl+D to exit (empty)"), "{text}");
    // `app.suspend` and `app.editor.external` are bound but not implemented,
    // so the header must not advertise them (LUM-1240 / LUM-1245).
    assert!(!text.contains("to suspend"), "{text}");
    assert!(!text.contains("for external editor"), "{text}");
    assert!(text.contains("Shift+Tab to cycle thinking level"), "{text}");
    assert!(
        text.contains("Ctrl+P/Shift+Ctrl+P to cycle models"),
        "{text}"
    );
    assert!(text.contains("Ctrl+L to select model"), "{text}");
    assert!(text.contains("Ctrl+O to expand tools"), "{text}");
    assert!(text.contains("Alt+H to hide this header"), "{text}");
    assert!(text.contains("Ctrl+T to expand thinking"), "{text}");
    assert!(text.contains("/ for commands"), "{text}");
    assert!(text.contains("! to run bash"), "{text}");
    assert!(text.contains("!! to run bash (no context)"), "{text}");
    assert!(text.contains("Alt+Enter to queue follow-up"), "{text}");
    assert!(
        text.contains("Alt+Up to edit all queued messages"),
        "{text}"
    );
    assert!(
        text.contains("Ctrl+V to paste image (with text fallback)"),
        "{text}"
    );
    assert!(text.contains("drop files to attach"), "{text}");

    reset_keybindings();
}

#[test]
fn an_override_moves_the_chord_on_the_header() {
    let _guard = lock_registry();
    install(&[("app.header", "ctrl+g"), ("app.tools.expand", "ctrl+y")]);
    let text = header_lines().join("\n");

    assert!(text.contains("Ctrl+G to hide this header"), "{text}");
    assert!(text.contains("Ctrl+Y to expand tools"), "{text}");
    // The released defaults are gone with them.
    assert!(!text.contains("Alt+H"), "{text}");
    assert!(!text.contains("Ctrl+O"), "{text}");

    reset_keybindings();
}

#[test]
fn an_unbound_action_is_dropped_from_the_header() {
    let _guard = lock_registry();
    // `app.clear` drives two rows (clear / exit); unregistering it removes
    // both rather than printing a blank key column.
    let mut definitions = tui_default_keybindings();
    for (id, chord) in APP_CHORDS {
        if *id == "app.clear" {
            continue;
        }
        definitions.push(((*id).to_string(), KeybindingDefinition::new([*chord])));
    }
    set_keybindings(KeybindingsManager::new(
        definitions,
        KeybindingsConfig::new(),
    ));
    let text = header_lines().join("\n");

    assert!(!text.contains("to clear"), "{text}");
    assert!(!text.contains("twice to exit"), "{text}");
    // Its neighbours are untouched.
    assert!(text.contains("Esc to interrupt"), "{text}");
    assert!(text.contains("Ctrl+D to exit (empty)"), "{text}");

    reset_keybindings();
}

#[test]
fn the_header_keeps_the_live_component_rows() {
    let _guard = lock_registry();
    install(&[]);
    let text = header_lines().join("\n");

    // The `is_wired` filter only governs the `app.*` namespace. The `tui.*`
    // rows are consumed by the `pi-tui` components themselves, so gating them
    // on a hand-kept list would delete live shortcuts — the first cut of the
    // filter did exactly that and silently dropped this row, which only the
    // real PTY capture caught (LUM-1242). Pin it from now on.
    assert!(
        text.contains("Ctrl+K to delete to end"),
        "the live `tui.editor.deleteToLineEnd` row disappeared:\n{text}"
    );
    assert!(
        text.contains("Ctrl+C to clear"),
        "the live `app.clear` row disappeared:\n{text}"
    );

    reset_keybindings();
}

// --- Stage 71 (LUM-1239): the extension summary row ---------------------

#[test]
fn the_header_lists_the_loaded_extensions() {
    let lines = header_lines_with(ExtensionHeader::Loaded {
        count: 2,
        names: vec![
            "./fixture-ext.mjs".into(),
            "~/.pi/agent/extensions/foo.mjs".into(),
        ],
    });

    // Directly under the title row, so `pi -e ./ext.mjs` is discoverable
    // without scrolling.
    assert!(lines[0].starts_with("pi v"), "{:?}", lines[0]);
    assert_eq!(
        lines[1],
        "2 extension(s): ./fixture-ext.mjs, ~/.pi/agent/extensions/foo.mjs"
    );
}

#[test]
fn the_header_says_extensions_none_when_they_are_disabled() {
    let text = header_lines_with(ExtensionHeader::Disabled).join("\n");
    assert!(
        text.contains("extensions: none (--no-extensions)"),
        "{text}"
    );
}

#[test]
fn a_run_without_extensions_has_no_extension_row() {
    // Default config = `ExtensionHeader::Hidden`: the pre-Stage-71 header.
    let text = header_lines().join("\n");
    assert!(!text.contains("extension"), "{text}");
}

#[test]
fn a_loaded_row_with_no_names_is_hidden() {
    // Defensive: a count with an empty name list carries no information.
    let text = header_lines_with(ExtensionHeader::Loaded {
        count: 0,
        names: Vec::new(),
    })
    .join("\n");
    assert!(!text.contains("extension"), "{text}");
}

#[test]
fn an_extension_header_still_overrides_the_extension_summary() {
    // `ctx.ui.setHeader` replaces the whole built-in header
    // (`interactive-mode.ts:958`), including the new summary row.
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "startup-header".into(),
            startup_header: true,
            extension_header: ExtensionHeader::Loaded {
                count: 1,
                names: vec!["./fixture-ext.mjs".into()],
            },
            ..AppConfig::default()
        },
    );
    app.set_header(Some(Box::new(TextComponent::new(["-- custom header --"]))));
    let text = app.render_snapshot(WIDTH, HEIGHT).lines.join("\n");

    assert!(text.contains("-- custom header --"), "{text}");
    assert!(!text.contains("extension(s)"), "{text}");
}

/// The driver's table makes the expanded header ~21 rows tall. A 23-row
/// terminal cannot hold it *and* the composer *and* the status bar, so the
/// App folds the hint list for that frame rather than budget it all to the
/// header and clip the prompt off the bottom (LUM-1260 §3, LUM-1266).
#[test]
fn the_header_folds_on_a_terminal_that_cannot_hold_it() {
    let _guard = lock_registry();
    install(&[]);
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let app = App::new(
        &agent,
        AppConfig {
            session_id: "startup-header".into(),
            startup_header: true,
            ..AppConfig::default()
        },
    );
    let lines = app.render_snapshot(WIDTH, SHORT_HEIGHT).lines;
    let text = lines.join("\n");

    // The title and the way back to the hints survive; the hints themselves
    // do not (they cost 19 of the 23 rows).
    assert!(text.contains("pi v"), "{text}");
    assert!(
        text.contains("hints hidden on a short terminal — Alt+H shows them"),
        "{text}"
    );
    assert!(!text.contains("to interrupt"), "folded:\n{text}");

    // The composer is painted on the row above the status bar, and the
    // transcript keeps a real viewport instead of a single row.
    let prompt_row = lines
        .iter()
        .position(|line| line.contains("type a prompt"))
        .unwrap_or_else(|| panic!("no composer painted:\n{text}"));
    assert_eq!(prompt_row, SHORT_HEIGHT as usize - 2);
    assert!(
        !lines[SHORT_HEIGHT as usize - 1].trim().is_empty(),
        "the status bar is painted below the composer:\n{text}"
    );
    assert!(
        app.viewport().1 >= 3,
        "the transcript keeps at least the floor of rows: {:?}",
        app.viewport()
    );

    reset_keybindings();
}
