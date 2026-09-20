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
//! Kept in its own integration binary: `set_keybindings` mutates process
//! state, and cargo gives every `tests/*.rs` file its own process.

use std::sync::{Arc, Mutex};

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_coding_agent::commands::slash::hotkeys_text_with;
use pi_coding_agent::keybindings::{merged_definitions, process_env, Platform, APP_KEYBINDING_IDS};
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::keybindings::{
    app_action_is_consumed, reset_keybindings, set_keybindings, KeybindingsConfig,
    KeybindingsManager, CONSUMED_APP_ACTIONS,
};

const WIDTH: u16 = 100;
const HEIGHT: u16 = 34;

/// The `app.*` ids that are bound with a default chord but answered by no
/// consumer — the two actions that need terminal hand-over semantics the port
/// does not have (`SIGTSTP` + restore for `Ctrl+Z`, an `$EDITOR` hand-over for
/// `Ctrl+G`).
///
/// This is the tripwire: every other advertised `app.*` id has to be in
/// [`CONSUMED_APP_ACTIONS`], so wiring one of these (or un-wiring a live one)
/// forces this list to be updated rather than leaving a surface lying. When a
/// rebase brings in a sibling branch that wires an action — `Shift+Tab` did
/// exactly that on the way to `feature/pi.rs` (Stage 67 / LUM-1230) — this
/// test fails until the id moves across.
const KNOWN_UNWIRED: &[&str] = &["app.suspend", "app.editor.external"];

/// Consumed, but only while one of the App's own overlays owns the keyboard,
/// so it is not a global shortcut and the `app` group stays silent about it:
/// the thinking selector answers `Ctrl+S` (`app.thinking.save`). Upstream
/// omits its `saveThinking` from the same table
/// (`packages/coding-agent/src/core/interactive-mode.ts:6343-6352`), and its
/// own legend documents it.
const SELECTOR_SCOPED: &[&str] = &["app.thinking.save"];

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
fn the_shipped_header_hides_the_dead_chords_and_keeps_the_live_ones() {
    let _guard = lock_registry();
    set_keybindings(manager());
    let text = header_lines().join("\n");

    // Dead: bound in this very table, answered by nobody.
    for dead in KNOWN_UNWIRED {
        assert!(
            !CONSUMED_APP_ACTIONS.contains(dead),
            "{dead} is classified dead but is consumed"
        );
    }
    assert!(!text.contains("to suspend"), "{text}");
    assert!(!text.contains("for external editor"), "{text}");
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

    // The two dead chords are gone from the group that lists `app.*` actions.
    // `Ctrl+G` is checked inside that group only: it is still a live chord for
    // the *transcript* action (`tui.altScreen.searchNext`), and this group is
    // about `app.*`.
    assert!(!advertised(app, &["Ctrl+Z".into()]), "{app}");
    assert!(!advertised(app, &["Ctrl+G".into()]), "{app}");
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
            app_action_is_consumed(id) || KNOWN_UNWIRED.contains(&id),
            "{id} is advertised but neither wired nor a known dead chord: wire it and add \
             it to CONSUMED_APP_ACTIONS, or list it as unwired, or stop advertising it"
        );
    }
}

#[test]
fn the_known_dead_chords_are_bound_but_not_consumed() {
    let manager = manager();
    for id in KNOWN_UNWIRED {
        assert!(
            APP_KEYBINDING_IDS.contains(id),
            "{id} is a coding-agent keybinding"
        );
        assert!(
            !manager.get_keys(id).is_empty(),
            "{id} is hidden from the surfaces, so it should still be bound"
        );
        assert!(
            !app_action_is_consumed(id),
            "{id} is listed as unwired but something consumes it now"
        );
    }
}
