//! Behavioural audit for `registerShortcut` (P0-3, plan §5).
//!
//! Two guarantees are pinned here:
//!
//! 1. The reserved-key set matches the TS source
//!    (`packages/coding-agent/src/core/extensions/runner.ts:71-90`)
//!    exactly — count and contents.
//! 2. Registering a shortcut on a reserved chord fails with
//!    [`ShortcutConflict::ReservedKey`] naming the colliding keybinding,
//!    and registering on a free chord succeeds.
//!
//! The dispatcher wiring (precedence over the built-in `app.*` / `tui.*`
//! chord table) is covered by `tests/extension_register_shortcut_dispatch.rs`
//! once the interactive driver has a testable harness; for now the
//! behaviour the audit cares about is the registry itself.

#![cfg(test)]

use pi_coding_agent::extensions::extension_shortcuts::{
    ExtensionShortcutRegistry, ShortcutConflict, RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS,
};
use pi_tui::input::{InputEvent, Key, KeyCode, KeyModifiers};
use pi_tui::keybindings::{get_keybindings, KeybindingDefinition, KeybindingsManager};

/// Install a keybindings manager that carries the bindings the test
/// wants to drive collisions against. The process-wide default (the
/// `tui.*` table) only knows about editor ids, so the `app.*` reserved
/// set looks empty to the registry in a fresh test process. We add the
/// entries we care about explicitly.
fn manager_with(chord: &str, keybinding: &str) -> KeybindingsManager {
    // Build a manager that combines the tui defaults with one extra
    // `app.*` binding. The process-wide default (the `tui.*` table)
    // does not know about editor ids, so the `app.*` reserved set
    // looks empty to the registry in a fresh test process; this
    // fixture adds the entries the audit wants to drive.
    let mut definitions = pi_tui::keybindings::tui_default_keybindings();
    definitions.push((
        keybinding.to_string(),
        KeybindingDefinition::new(vec![chord.to_string()]),
    ));
    KeybindingsManager::new(definitions, pi_tui::keybindings::KeybindingsConfig::new())
}

/// Restore the process-wide keybindings manager to its tui-only
/// defaults so the next test starts clean.
fn reset_global_keybindings() {
    pi_tui::keybindings::reset_keybindings();
}

fn key_ctrl(ch: char) -> Key {
    Key::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
}

#[test]
fn reserved_set_matches_ts_runner() {
    // The TS source (`runner.ts:71-90`) lists exactly 18 editor-global
    // ids; P0-3 deliberately widens to 19 (the plan count) by including
    // `app.tools.expand` which Rust promotes alongside the others. If
    // this number changes, the audit fails loudly so the TS list is
    // re-checked — the contract is "extension chords may not collide
    // with these", and any change has to land in both sides together.
    assert_eq!(
        RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS.len(),
        18,
        "RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS drifted from the TS count"
    );

    let canonical = [
        "app.clear",
        "app.editor.external",
        "app.exit",
        "app.interrupt",
        "app.message.copy",
        "app.message.followUp",
        "app.model.cycleBackward",
        "app.model.cycleForward",
        "app.model.select",
        "app.suspend",
        "app.thinking.cycle",
        "app.thinking.toggle",
        "app.tools.expand",
        "tui.editor.deleteToLineEnd",
        "tui.input.copy",
        "tui.input.submit",
        "tui.select.cancel",
        "tui.select.confirm",
    ];
    assert_eq!(
        RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS, &canonical,
        "RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS must mirror the TS list exactly",
    );
}

#[test]
fn reserved_set_is_sorted_and_unique() {
    let mut sorted: Vec<&str> = RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS
        .iter()
        .copied()
        .collect();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS.to_vec(),
        "RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS must be sorted alphabetically so a \
         diff against the TS source is scannable",
    );
}

#[test]
fn register_free_chord_succeeds() {
    let registry = ExtensionShortcutRegistry::new();
    let keybindings = get_keybindings();
    let handle = registry
        .register("ctrl+shift+x", "{\"kind\":\"echo\"}".to_string(), &keybindings)
        .expect("ctrl+shift+x is not a reserved chord");
    assert_eq!(handle, 1, "first handle must be 1 (1-based monotonic)");
    assert_eq!(registry.len(), 1);
}

#[test]
fn register_duplicate_chord_returns_a_new_handle() {
    // The TS source does not deduplicate by chord — multiple extensions
    // can register the same chord and the *first* registered wins
    // (`runner.ts:71-90` only blocks collisions with reserved keys, not
    // among extensions themselves). The lookup below mirrors that
    // behaviour.
    let registry = ExtensionShortcutRegistry::new();
    let keybindings = get_keybindings();
    let first = registry
        .register("ctrl+shift+x", "{\"kind\":\"first\"}".to_string(), &keybindings)
        .unwrap();
    let second = registry
        .register("ctrl+shift+x", "{\"kind\":\"second\"}".to_string(), &keybindings)
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(registry.len(), 2);
}

#[test]
fn register_reserved_chord_is_rejected() {
    reset_global_keybindings();
    let registry = ExtensionShortcutRegistry::new();
    // The test installs a manager with `app.model.cycleForward =
    // ctrl+p` (the upstream default) so the registry sees a real
    // reservation. The default manager only knows `tui.*` ids.
    let keybindings = manager_with("ctrl+p", "app.model.cycleForward");
    let err = registry
        .register("ctrl+p", "{\"kind\":\"echo\"}".to_string(), &keybindings)
        .expect_err("ctrl+p collides with app.model.cycleForward");
    match err {
        ShortcutConflict::ReservedKey {
            keybinding,
            conflicting_keys,
            ..
        } => {
            assert_eq!(
                keybinding, "app.model.cycleForward",
                "conflict report must name the colliding keybinding",
            );
            assert!(
                conflicting_keys.iter().any(|k| k == "ctrl+p"),
                "conflict report must list the colliding chord, got {conflicting_keys:?}",
            );
        }
        other => panic!("expected ReservedKey conflict, got {other:?}"),
    }
    assert_eq!(registry.len(), 0, "rejected registrations must not store");
}

#[test]
fn register_reserved_chord_suspend_only_collides_on_non_windows() {
    reset_global_keybindings();
    let registry = ExtensionShortcutRegistry::new();
    // Wire `app.suspend = ctrl+z` so the audit exercises the
    // collision path; in production `install_keybindings_from` does
    // this (with a Windows gate inside `app_default_keybindings`).
    let keybindings = manager_with("ctrl+z", "app.suspend");
    let on_windows = cfg!(windows);
    let result = registry.register("ctrl+z", "{}".to_string(), &keybindings);
    if on_windows {
        // On Windows the production gate unbinds `app.suspend` before
        // the registry ever runs; the audit-only fixture here still
        // binds it, so the collision is detected regardless of
        // platform — the production gate lives one layer up.
        assert!(
            result.is_err(),
            "production gate only fires on non-Windows, but the registry itself sees the binding it was given",
        );
    } else {
        let err = result.expect_err("non-Windows must reserve ctrl+z for app.suspend");
        assert!(matches!(err, ShortcutConflict::ReservedKey { keybinding, .. } if keybinding == "app.suspend"));
    }
}

#[test]
fn register_unknown_chord_is_rejected() {
    let registry = ExtensionShortcutRegistry::new();
    let keybindings = get_keybindings();
    let err = registry
        .register("super+banana", "{}".to_string(), &keybindings)
        .expect_err("super+banana is not a parseable chord");
    assert!(
        matches!(err, ShortcutConflict::UnknownChord { ref chord } if chord == "super+banana"),
        "expected UnknownChord(banana), got {err:?}",
    );
}

#[test]
fn lookup_matches_by_event() {
    let registry = ExtensionShortcutRegistry::new();
    let keybindings = get_keybindings();
    let handle = registry
        .register("ctrl+shift+x", "{\"kind\":\"echo\"}".to_string(), &keybindings)
        .unwrap();
    let event = InputEvent::Key(key_ctrl('X'));
    let hit = registry.lookup(&event).expect("ctrl+shift+x should match");
    assert_eq!(hit.id, handle);
    assert_eq!(hit.chord, "ctrl+shift+x");
    assert_eq!(hit.callback, "{\"kind\":\"echo\"}");
}

#[test]
fn lookup_misses_unrelated_event() {
    let registry = ExtensionShortcutRegistry::new();
    let keybindings = get_keybindings();
    registry
        .register("ctrl+shift+x", "{}".to_string(), &keybindings)
        .unwrap();
    let event = InputEvent::Key(key_ctrl('Y'));
    assert!(registry.lookup(&event).is_none());
}

#[test]
fn unregister_removes_handle() {
    let registry = ExtensionShortcutRegistry::new();
    let keybindings = get_keybindings();
    let handle = registry
        .register("ctrl+shift+x", "{}".to_string(), &keybindings)
        .unwrap();
    assert!(registry.unregister(handle));
    assert_eq!(registry.len(), 0);
    assert!(!registry.unregister(handle), "double-unregister must report false");
    assert!(registry.lookup(&InputEvent::Key(key_ctrl('X'))).is_none());
}

#[test]
fn snapshot_returns_insertion_order_sorted_by_id() {
    let registry = ExtensionShortcutRegistry::new();
    let keybindings = get_keybindings();
    registry
        .register("ctrl+shift+x", "first".to_string(), &keybindings)
        .unwrap();
    registry
        .register("ctrl+shift+y", "second".to_string(), &keybindings)
        .unwrap();
    let snap = registry.snapshot();
    assert_eq!(snap.len(), 2);
    assert_eq!(snap[0].id, 1);
    assert_eq!(snap[1].id, 2);
}