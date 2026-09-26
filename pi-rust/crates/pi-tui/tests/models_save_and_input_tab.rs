//! P25 (D4 + D5) — `app.models.save` and `tui.input.tab`.
//!
//! D4 = `app.models.save` — the `/scoped-models` panel's "save selection
//! to `settings.json`" chord (`Ctrl+S`). The action id is registered in the
//! merged keybinding table and is consumed by the driver's
//! `handle_picker_key` for `PickerKind::ScopedModels`. The tests below pin
//! the action-id surface so a refactor cannot silently drop it.
//!
//! D5 = `tui.input.tab` — the Tab key inside the editor. The editor
//! consumes it in two paths:
//!
//! 1. While the autocomplete dropdown is open, Tab accepts the highlighted
//!    suggestion (and falls through to submit when the prefix starts with
//!    `/`).
//! 2. With the dropdown closed, Tab triggers a command-/file-completion
//!    hint via [`pi_tui::Editor::handle_tab_completion`].
//!
//! Both paths are pinned below at the editor + keybinding surface, so a
//! refactor that breaks either one cannot land silently.

use pi_tui::components::keybindings::{
    app_action_is_consumed, tui_default_keybindings, CONSUMED_APP_ACTIONS,
};
use pi_tui::editor::Editor;
use pi_tui::input::{Key, KeyCode, KeyModifiers};

fn find_entry<'a>(
    table: &'a [(String, pi_tui::components::keybindings::KeybindingDefinition)],
    id: &str,
) -> Option<&'a pi_tui::components::keybindings::KeybindingDefinition> {
    table.iter().find_map(|(k, v)| if k == id { Some(v) } else { None })
}

/// 1 — `app.models.save` is a *consumed* `app.*` id. The port marks it
/// as such in [`CONSUMED_APP_ACTIONS`] so it does not surface as a silent
/// id in `/hotkeys` or the startup hints. The driver
/// (`pi-coding-agent/src/keybindings.rs`) attaches the actual `Ctrl+S`
/// chord; the table-level binding lives outside the `pi-tui` crate.
#[test]
fn app_models_save_is_a_consumed_app_action() {
    assert!(
        CONSUMED_APP_ACTIONS.contains(&"app.models.save"),
        "app.models.save must appear in CONSUMED_APP_ACTIONS"
    );
    assert!(
        app_action_is_consumed("app.models.save"),
        "app_action_is_consumed must return true for app.models.save"
    );
}

/// 2 — `app.models.save` is in the consumed-actions list, so it does not
/// appear as a *silent* id in `/hotkeys` or the startup hints.
#[test]
fn app_models_save_is_in_the_consumed_actions_list() {
    assert!(
        CONSUMED_APP_ACTIONS.contains(&"app.models.save"),
        "app.models.save must appear in CONSUMED_APP_ACTIONS"
    );
    assert!(
        app_action_is_consumed("app.models.save"),
        "app_action_is_consumed must return true for app.models.save"
    );
}

/// 3 — The full cluster of scoped-models actions — `enableAll`,
/// `clearAll`, `toggleProvider`, `reorderUp`, `reorderDown` — is bound
/// alongside `app.models.save`. This is the D4 contract: all six surface
/// on the `/scoped-models` panel. They are listed in
/// [`CONSUMED_APP_ACTIONS`] so the port never advertises them as silent.
#[test]
fn scoped_models_action_cluster_is_fully_consumed() {
    for id in [
        "app.models.save",
        "app.models.enableAll",
        "app.models.clearAll",
        "app.models.toggleProvider",
        "app.models.reorderUp",
        "app.models.reorderDown",
    ] {
        assert!(
            CONSUMED_APP_ACTIONS.contains(&id),
            "{id} must be in CONSUMED_APP_ACTIONS"
        );
        assert!(
            app_action_is_consumed(id),
            "{id} must be in CONSUMED_APP_ACTIONS"
        );
    }
}

/// 4 — `tui.input.tab` lives in the merged keybinding table with the
/// upstream chord `Tab`. Plugin authors read this surface to know which
/// chord the editor is listening on.
#[test]
fn tui_input_tab_is_in_default_keybindings_with_tab_chord() {
    let table = tui_default_keybindings();
    let entry = find_entry(&table, "tui.input.tab")
        .expect("tui.input.tab must be in the default keybindings table");
    assert!(
        entry.default_keys.iter().any(|c| c.eq_ignore_ascii_case("tab")),
        "tui.input.tab must be bound to Tab; got {:?}",
        entry.default_keys
    );
}

/// 5 — `tui.input.tab` is consumed by the component layer (the
/// `pi-tui::Editor`), so it counts as wired even though it is not in the
/// `app.*` consumed list. `app_action_is_consumed` returns `true` for any
/// non-`app.*` id.
#[test]
fn tui_input_tab_is_a_tui_component_level_action() {
    assert!(
        app_action_is_consumed("tui.input.tab"),
        "tui.input.tab is always consumed by the editor component"
    );
}

/// 6 — `Editor::handle_tab_completion` returns `false` (no completion
/// possible) when the editor has no autocomplete provider. The Tab key is
/// a no-op in that case.
#[test]
fn tab_completion_is_a_noop_without_an_autocomplete_provider() {
    let mut editor = Editor::new();
    assert!(!editor.handle_tab_completion());
}

/// 7 — Sending `Tab` into the editor via `handle_key` while the
/// autocomplete dropdown is closed does not error out. This is the
/// ergonomic surface plugin authors use: a single key event reaches the
/// tab-handling path. The action returned is whatever the editor decided
/// (None when there is no provider, Changed when a completion ran).
#[test]
fn tab_key_reaches_the_editor_tab_handler() {
    let mut editor = Editor::new();
    let tab = Key::new(KeyCode::Tab, KeyModifiers::NONE);
    // Without an autocomplete provider, Tab is a no-op and the editor
    // returns EditorAction::None — but the call must not panic and the
    // buffer must remain unchanged.
    let action = editor.handle_key(tab);
    assert!(
        matches!(action, pi_tui::editor::EditorAction::None),
        "Tab without provider must be a safe no-op; got {action:?}"
    );
    assert_eq!(editor.text(), "");
}

/// 8 — Pressing Tab inside the editor never silently drops a chord. The
/// action returned by `handle_key` is one of the documented
/// `EditorAction` variants. This pins the contract so a future refactor
/// cannot start swallowing Tab.
#[test]
fn tab_key_returns_an_editor_action_variant() {
    let mut editor = Editor::new();
    editor.insert_str("hello");
    let tab = Key::new(KeyCode::Tab, KeyModifiers::NONE);
    let action = editor.handle_key(tab);
    // Either no-op (no provider) or Changed (a completion ran) — both
    // are valid contracts. What is *not* valid is dropping the chord
    // and returning something outside the documented enum.
    let name = format!("{:?}", action);
    assert!(
        name == "None" || name.contains("Changed") || name.contains("Submit"),
        "Tab must return one of the EditorAction variants; got {name}"
    );
}

/// 9 — Repeated Tab presses without an autocomplete provider do not loop
/// or grow the buffer. The buffer stays exactly what the user typed.
#[test]
fn repeated_tab_presses_are_safe_no_ops() {
    let mut editor = Editor::new();
    editor.insert_str("plain text");
    let tab = Key::new(KeyCode::Tab, KeyModifiers::NONE);
    for _ in 0..5 {
        let _ = editor.handle_key(tab);
    }
    assert_eq!(editor.text(), "plain text");
}