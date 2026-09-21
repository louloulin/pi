//! The shipped advertisement surfaces, built from the **coding-agent** table.
//!
//! `pi-tui`'s own `tests/startup_header.rs` installs a hand-written subset of
//! the `app.*` ids; this binary installs the real merged table — every id in
//! [`APP_KEYBINDING_IDS`], the ones with consumers and the dead ones alike —
//! so the rows a user actually sees are the rows under test here.
//!
//! LUM-1240 measured three surfaces advertising chords that no consumer
//! answered: the startup header, `/hotkeys`, and the legend inside `/help`.
//! The header and `/hotkeys` are both driven by
//! [`pi_tui::keybindings::app_action_is_consumed`]; `/help` is a hand-written
//! string, and it stayed wrong the longest (it is pinned by
//! `commands::slash::tests::the_help_legend_describes_ctrl_l_as_the_model_selector`).
//!
//! The second half is the driver's launch surface (Stage 66 / LUM-1228 and
//! Stage 71 / LUM-1239): which options turn the header on, and whether the
//! extension summary row the driver projects is the row the App renders.
//!
//! Kept in its own integration binary: `set_keybindings` mutates process
//! state, and cargo gives every `tests/*.rs` file its own process.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use clap::Parser;
use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_coding_agent::cli::Cli;
use pi_coding_agent::commands::slash::hotkeys_text_with;
use pi_coding_agent::extensions::wiring::ExtensionReport;
use pi_coding_agent::install_keybindings_from;
use pi_coding_agent::interactive::{interactive_app_config, InteractiveOptions};
use pi_coding_agent::keybindings::{merged_definitions, process_env, Platform, APP_KEYBINDING_IDS};
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, ExtensionHeader};
use pi_tui::keybindings::{
    app_action_is_consumed, reset_keybindings, set_keybindings, KeybindingsConfig,
    KeybindingsManager, CONSUMED_APP_ACTIONS,
};

const WIDTH: u16 = 100;
const HEIGHT: u16 = 34;

/// The `app.*` ids that are advertised with a default chord but answered by no
/// consumer.
///
/// **Emptied by LUM-1308**, which wired the last two: the driver now claims
/// `Ctrl+G` (`app.editor.external` — the tty is handed to `$EDITOR`, see
/// `pi-coding-agent/src/external_editor.rs`) and `Ctrl+Z` (`app.suspend` — the
/// terminal is restored before `SIGTSTP`). The list is kept as the tripwire for
/// the next false ad: a new hint whose action has no consumer fails
/// `every_advertised_app_hint_is_wired_or_a_known_dead_chord` until it is wired
/// or listed here.
///
/// A function rather than a `const &[]` because `clippy::const_is_empty`
/// rejects `KNOWN_UNWIRED.is_empty()` on a const that is *literally* empty —
/// and the assertion is the whole point of the tripwire.
fn known_unwired() -> &'static [&'static str] {
    &[]
}

/// Consumed, but only while one of the App's own overlays owns the keyboard,
/// so it is not a global shortcut and the `app` group stays silent about it:
/// the thinking selector answers `Ctrl+S` (`app.thinking.save`). Upstream
/// omits its `saveThinking` from the same table
/// (`packages/coding-agent/src/core/interactive-mode.ts:6343-6352`), and its
/// own legend documents it.
///
/// The same is true of the picker-scoped chords Stage 68 (LUM-1255) wired:
/// `app.session.toggleSort` … `app.session.deleteNoninvasive` only act while
/// the `/resume` picker is open, and the `app.tree.*` filters / folds / label
/// toggle only act inside the `/tree` overlay. Upstream advertises them in
/// those components' own footers, which is where the port shows them too.
const SELECTOR_SCOPED: &[&str] = &[
    "app.thinking.save",
    "app.session.toggleSort",
    "app.session.togglePath",
    "app.session.toggleNamedFilter",
    "app.session.rename",
    "app.session.delete",
    "app.session.deleteNoninvasive",
    "app.tree.foldOrUp",
    "app.tree.unfoldOrDown",
    "app.tree.toggleLabelTimestamp",
    "app.tree.filter.default",
    "app.tree.filter.noTools",
    "app.tree.filter.userOnly",
    "app.tree.filter.labeledOnly",
    "app.tree.filter.all",
    "app.tree.filter.cycleForward",
    "app.tree.filter.cycleBackward",
];

/// `install_keybindings_from` and `reset_keybindings` mutate process state,
/// and cargo runs this binary's tests concurrently: every test that renders a
/// frame or reads a surface takes this lock.
static REGISTRY: Mutex<()> = Mutex::new(());

fn lock_registry() -> std::sync::MutexGuard<'static, ()> {
    REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn manager() -> KeybindingsManager {
    KeybindingsManager::new(
        merged_definitions(&Platform::Linux, &process_env()),
        KeybindingsConfig::default(),
    )
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
            session_id: "lum1242-header".into(),
            startup_header: true,
            ..AppConfig::default()
        },
    );
    app.render_snapshot(WIDTH, HEIGHT).lines
}

/// Render the header through the *driver's* configuration — the option →
/// `AppConfig` projection `main.rs` uses, with the driver's real keybinding
/// table installed from `dir`.
fn render(options: &InteractiveOptions, dir: &Path) -> Vec<String> {
    install_keybindings_from(dir);
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let app = App::new(&agent, interactive_app_config(options));
    app.render_snapshot(WIDTH, HEIGHT).lines
}

/// The `app:` group of `/hotkeys`, as printed.
fn app_group(text: &str) -> &str {
    let start = text.find("\napp:\n").expect("the app group exists");
    let rest = &text[start + 1..];
    match rest.find("\n\n") {
        Some(end) => &rest[..end],
        None => rest,
    }
}

/// Does `group` print a row for this action, i.e. the exact chord cell
/// `format!("  {:<16} ", chords.join(" / "))` the table writes?
///
/// Exact-cell rather than substring: `Shift+T`
/// (`app.tree.toggleLabelTimestamp`, unwired) is a prefix of the live
/// `Shift+Tab` row, so `contains("Shift+T")` claims a row that is not printed
/// (LUM-1242).
fn advertised(group: &str, chords: &[String]) -> bool {
    if chords.is_empty() {
        return false;
    }
    let prefix = format!("  {:<16} ", chords.join(" / "));
    group.lines().any(|line| line.starts_with(&prefix))
}

/// The chord cells the group prints, in order.
fn printed_cells(group: &str) -> Vec<String> {
    group
        .lines()
        .filter_map(|line| line.get(2..18).map(|cell| cell.trim_end().to_string()))
        .filter(|cell| !cell.is_empty())
        .collect()
}

#[test]
fn the_shipped_header_advertises_the_recovered_chords_and_keeps_the_live_ones() {
    let _guard = lock_registry();
    set_keybindings(manager());
    let text = header_lines().join("\n");

    // No false ads left: the tripwire list is empty, and both recovered chords
    // now have a consumer...
    assert!(known_unwired().is_empty(), "a dead chord came back");
    for recovered in ["app.suspend", "app.editor.external"] {
        assert!(
            CONSUMED_APP_ACTIONS.contains(&recovered),
            "{recovered} is not consumed any more"
        );
    }
    // ...so the header has to advertise them (LUM-1308).
    assert!(
        text.contains("Ctrl+Z to suspend"),
        "Ctrl+Z lost its header hint:\n{text}"
    );
    assert!(
        text.contains("Ctrl+G for external editor"),
        "Ctrl+G lost its header hint:\n{text}"
    );
    // Live, and advertised because it is: `Shift+Tab` became real when Stage
    // 67 (LUM-1230) landed, and this row must come back with it.
    assert!(text.contains("Shift+Tab to cycle thinking level"), "{text}");
    assert!(text.contains("Ctrl+L to select model"), "{text}");
    // Live `tui.*` rows are never gated: the `is_wired` filter only governs
    // the `app.*` namespace (LUM-1242).
    assert!(text.contains("Ctrl+K to delete to end"), "{text}");

    reset_keybindings();
}

#[test]
fn the_shipped_hotkeys_group_lists_only_consumed_actions() {
    let _guard = lock_registry();
    let text = hotkeys_text_with(&manager());
    let app = app_group(&text);

    // Both recovered chords are in the group that lists `app.*` actions.
    // `Ctrl+G` is checked inside that group only: it is still a live chord for
    // the *transcript* action (`tui.altScreen.searchNext`), and this group is
    // about `app.*`.
    assert!(advertised(app, &["Ctrl+Z".into()]), "{app}");
    assert!(advertised(app, &["Ctrl+G".into()]), "{app}");
    assert!(advertised(app, &["Ctrl+L".into()]), "{app}");
    assert!(advertised(app, &["Shift+Tab".into()]), "{app}");

    // The invariant of LUM-1240, not just the two rows: the group's chord
    // cells are *exactly* the cells of the bound, consumed, non-selector-scoped
    // `app.*` ids. Comparing the two sets rather than id-by-id is what makes
    // this decidable — `ctrl+p` is bound to `app.model.cycleForward` *and*
    // `app.session.togglePath`, so no per-id lookup can tell which one printed
    // a row (LUM-1242).
    let manager = manager();
    let mut expected: Vec<String> = APP_KEYBINDING_IDS
        .iter()
        .filter(|id| app_action_is_consumed(id) && !SELECTOR_SCOPED.contains(id))
        .filter_map(|id| {
            let chords = manager.get_keys(id);
            if chords.is_empty() {
                return None;
            }
            Some(
                chords
                    .iter()
                    .map(|chord| pi_tui::locale::format_chord(chord))
                    .collect::<Vec<_>>()
                    .join(" / "),
            )
        })
        .collect();
    expected.sort();
    let mut printed = printed_cells(app);
    printed.sort();
    assert_eq!(printed, expected, "the app group is not the consumed set");

    reset_keybindings();
}

#[test]
fn every_advertised_app_hint_is_wired_or_a_known_dead_chord() {
    use pi_tui::locale::{HeaderKey, STARTUP_HINTS};

    let mut ids: Vec<&'static str> = Vec::new();
    for hint in STARTUP_HINTS {
        match hint.key {
            HeaderKey::Chord(id) | HeaderKey::ChordTwice(id) => ids.push(id),
            HeaderKey::ChordPair(first, second) => ids.extend([first, second]),
            HeaderKey::Literal(_) => {}
        }
    }
    assert!(!ids.is_empty(), "the hint table named no actions at all");
    for id in ids {
        assert!(
            app_action_is_consumed(id) || known_unwired().contains(&id),
            "{id} is advertised but neither wired nor a known dead chord: wire it and add \
             it to CONSUMED_APP_ACTIONS, or list it as unwired, or stop advertising it"
        );
    }
}

#[test]
fn the_recovered_chords_are_bound_and_consumed() {
    // LUM-1308 successor of `the_known_dead_chords_are_bound_but_not_consumed`:
    // with the list empty, the property that matters is that the two ids the
    // list used to hold are now on the consuming side, and that no advertised
    // hint is left without a consumer.
    assert!(known_unwired().is_empty());
    let manager = manager();
    for id in ["app.suspend", "app.editor.external"] {
        assert!(
            APP_KEYBINDING_IDS.contains(&id),
            "{id} is a coding-agent keybinding"
        );
        assert!(
            !manager.get_keys(id).is_empty(),
            "{id} is advertised, so it has to be bound"
        );
        assert!(app_action_is_consumed(id), "{id} has no consumer again");
    }
}

// --- Stage 66 (LUM-1228) / Stage 71 (LUM-1239): the driver's launch surface

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
