//! Coverage for the keybinding registry (`packages/tui/src/keybindings.ts`).
//!
//! These tests pin the parts downstream code depends on — the default table,
//! user overrides (including "unbind"), conflict reporting, resolution order —
//! and the key-id vocabulary that `matches` runs on now that `crossterm`
//! decodes the terminal instead of the TUI parsing raw sequences:
//!
//! 1. The defaults match upstream `TUI_KEYBINDINGS` chord for chord.
//! 2. Rebinding one action never evicts another action's default chords.
//! 3. Two *user* bindings on the same chord are reported as a conflict.
//! 4. An empty override list unbinds an action; unknown ids are ignored and
//!    duplicate chords are dropped.
//! 5. Matching follows the Rust input model: case stands in for Shift on
//!    letters, symbols carry their own shift, `Shift+Tab` arrives as `BackTab`,
//!    `super` is `meta`, and non-key events never match.

use pi_tui::input::{InputEvent, KeyCode, KeyModifiers};
use pi_tui::keybindings::{
    get_keybindings, key_matches, parse_key_id, reset_keybindings, set_keybindings,
    tui_default_keybindings, KeybindingsConfig, KeybindingsManager,
};

fn key(code: KeyCode, modifiers: KeyModifiers) -> InputEvent {
    InputEvent::key(code, modifiers)
}

fn ctrl(c: char) -> InputEvent {
    key(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn alt(c: char) -> InputEvent {
    key(KeyCode::Char(c), KeyModifiers::ALT)
}

fn config(pairs: &[(&str, &[&str])]) -> KeybindingsConfig {
    let mut config = KeybindingsConfig::new();
    for (id, keys) in pairs {
        config.set(*id, keys.iter().copied());
    }
    config
}

#[test]
fn binds_ctrl_j_as_a_default_newline_alias() {
    let kb = KeybindingsManager::tui_defaults();

    assert_eq!(
        kb.get_keys("tui.input.newLine"),
        vec!["shift+enter".to_string(), "ctrl+j".to_string()]
    );
    assert!(kb.matches(&ctrl('j'), "tui.input.newLine"));
    assert!(kb.matches(
        &key(KeyCode::Enter, KeyModifiers::SHIFT),
        "tui.input.newLine"
    ));
    assert!(!kb.matches(
        &key(KeyCode::Enter, KeyModifiers::NONE),
        "tui.input.newLine"
    ));
}

#[test]
fn binds_modified_and_unmodified_editor_viewport_navigation() {
    let kb = KeybindingsManager::tui_defaults();

    assert_eq!(
        kb.get_keys("tui.editor.cursorLineStart"),
        vec!["home", "ctrl+home", "ctrl+a"]
    );
    assert_eq!(
        kb.get_keys("tui.editor.cursorLineEnd"),
        vec!["end", "ctrl+end", "ctrl+e"]
    );
    assert_eq!(
        kb.get_keys("tui.editor.pageUp"),
        vec!["pageUp", "ctrl+pageUp"]
    );
    assert_eq!(
        kb.get_keys("tui.editor.pageDown"),
        vec!["pageDown", "ctrl+pageDown"]
    );
}

#[test]
fn leaves_dedicated_prompt_history_navigation_unbound_by_default() {
    let kb = KeybindingsManager::tui_defaults();

    assert!(kb.get_keys("tui.editor.historyPrevious").is_empty());
    assert!(kb.get_keys("tui.editor.historyNext").is_empty());
    assert!(kb
        .get_definition("tui.editor.historyPrevious")
        .expect("history definition")
        .is_unbound());
}

#[test]
fn binds_unmodified_terminal_viewport_shortcuts_to_alternate_screen_navigation() {
    let kb = KeybindingsManager::tui_defaults();

    assert_eq!(kb.get_keys("tui.altScreen.pageUp"), vec!["pageUp"]);
    assert_eq!(kb.get_keys("tui.altScreen.pageDown"), vec!["pageDown"]);
    assert!(kb.get_keys("tui.altScreen.halfPageUp").is_empty());
    assert!(kb.get_keys("tui.altScreen.halfPageDown").is_empty());
    assert!(kb.get_keys("tui.altScreen.lineUp").is_empty());
    assert!(kb.get_keys("tui.altScreen.lineDown").is_empty());
    assert_eq!(
        kb.get_keys("tui.altScreen.previousPrompt"),
        vec!["ctrl+shift+up", "ctrl+up"]
    );
    assert_eq!(
        kb.get_keys("tui.altScreen.nextPrompt"),
        vec!["ctrl+shift+down", "ctrl+down"]
    );
    assert_eq!(kb.get_keys("tui.altScreen.search"), vec!["ctrl+shift+f"]);
    assert_eq!(
        kb.get_keys("tui.altScreen.searchNext"),
        vec!["enter", "ctrl+g"]
    );
    assert_eq!(
        kb.get_keys("tui.altScreen.searchPrevious"),
        vec!["shift+enter", "ctrl+shift+g"]
    );
    assert_eq!(kb.get_keys("tui.altScreen.searchClose"), vec!["escape"]);
    assert_eq!(kb.get_keys("tui.altScreen.top"), vec!["home"]);
    assert_eq!(kb.get_keys("tui.altScreen.bottom"), vec!["end"]);
}

#[test]
fn does_not_evict_selector_confirm_when_input_submit_is_rebound() {
    let kb = KeybindingsManager::new(
        tui_default_keybindings(),
        config(&[("tui.input.submit", &["enter", "ctrl+enter"])]),
    );

    assert_eq!(kb.get_keys("tui.input.submit"), vec!["enter", "ctrl+enter"]);
    assert_eq!(kb.get_keys("tui.select.confirm"), vec!["enter"]);
}

#[test]
fn does_not_evict_cursor_bindings_when_another_action_reuses_the_key() {
    let kb = KeybindingsManager::new(
        tui_default_keybindings(),
        config(&[("tui.select.up", &["up", "ctrl+p"])]),
    );

    assert_eq!(kb.get_keys("tui.select.up"), vec!["up", "ctrl+p"]);
    assert_eq!(kb.get_keys("tui.editor.cursorUp"), vec!["up"]);
}

#[test]
fn reports_direct_user_binding_conflicts_without_evicting_defaults() {
    let kb = KeybindingsManager::new(
        tui_default_keybindings(),
        config(&[
            ("tui.input.submit", &["ctrl+x"]),
            ("tui.select.confirm", &["ctrl+x"]),
        ]),
    );

    let conflicts = kb.get_conflicts();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].key, "ctrl+x");
    assert_eq!(
        conflicts[0].keybindings,
        vec!["tui.input.submit", "tui.select.confirm"]
    );
    assert_eq!(kb.get_keys("tui.editor.cursorLeft"), vec!["left", "ctrl+b"]);
    // The manager keeps the conflicting overrides in effect; reporting is the
    // only thing upstream does about them.
    assert_eq!(kb.get_keys("tui.input.submit"), vec!["ctrl+x"]);
}

#[test]
fn empty_override_unbinds_and_duplicates_are_dropped() {
    let kb = KeybindingsManager::new(
        tui_default_keybindings(),
        config(&[
            ("tui.input.submit", &[]),
            ("tui.editor.cursorLeft", &["ctrl+b", "ctrl+b", "left"]),
        ]),
    );

    assert!(kb.get_keys("tui.input.submit").is_empty());
    assert_eq!(kb.get_keys("tui.editor.cursorLeft"), vec!["ctrl+b", "left"]);
    assert!(!kb.matches(&key(KeyCode::Enter, KeyModifiers::NONE), "tui.input.submit"));
}

#[test]
fn ignores_overrides_for_unknown_ids() {
    let kb = KeybindingsManager::new(
        tui_default_keybindings(),
        config(&[("app.not.a.tui.binding", &["ctrl+x"])]),
    );

    assert!(kb.get_definition("app.not.a.tui.binding").is_none());
    assert!(kb.get_keys("app.not.a.tui.binding").is_empty());
    assert!(kb.get_conflicts().is_empty());
    // The unknown id did not disturb the defaults.
    assert_eq!(kb.get_keys("tui.input.submit"), vec!["enter"]);
}

#[test]
fn resolved_bindings_cover_every_definition_in_table_order() {
    let definitions = tui_default_keybindings();
    let kb = KeybindingsManager::new(
        definitions.clone(),
        config(&[("tui.editor.yank", &["ctrl+y", "alt+v"])]),
    );

    let resolved = kb.get_resolved_bindings();
    assert_eq!(resolved.len(), definitions.len());
    assert_eq!(
        kb.keybindings().collect::<Vec<_>>(),
        definitions
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>()
    );
    assert!(resolved.iter().any(|(id, keys)| id == "tui.editor.yank"
        && keys == &vec!["ctrl+y".to_string(), "alt+v".to_string()]));
    assert!(resolved
        .iter()
        .any(|(id, keys)| id == "tui.editor.undo" && keys == &vec!["ctrl+-".to_string()]));
}

#[test]
fn set_user_bindings_replaces_the_override_set() {
    let mut kb = KeybindingsManager::tui_defaults();
    kb.set_user_bindings(config(&[("tui.select.cancel", &["ctrl+q"])]));
    assert_eq!(kb.get_keys("tui.select.cancel"), vec!["ctrl+q"]);
    assert!(kb.get_user_bindings().get("tui.select.cancel").is_some());

    kb.set_user_bindings(KeybindingsConfig::new());
    assert_eq!(kb.get_keys("tui.select.cancel"), vec!["escape", "ctrl+c"]);
    assert!(kb.get_user_bindings().is_empty());
}

#[test]
fn parses_the_key_id_vocabulary() {
    assert_eq!(
        parse_key_id("ctrl+shift+up"),
        Some(pi_tui::input::Key::new(
            KeyCode::Up,
            KeyModifiers {
                shift: true,
                control: true,
                alt: false,
                meta: false,
            }
        ))
    );
    assert_eq!(parse_key_id("pageUp"), parse_key_id("pageup"));
    assert_eq!(parse_key_id("escape"), parse_key_id("esc"));
    assert_eq!(parse_key_id("return"), parse_key_id("enter"));
    assert_eq!(parse_key_id("space"), Some(pi_tui::input::Key::char(' ')));
    assert_eq!(
        parse_key_id("f12"),
        Some(pi_tui::input::Key::new(KeyCode::F(12), KeyModifiers::NONE))
    );
    assert_eq!(parse_key_id("super+k"), {
        let mut modifiers = KeyModifiers::NONE;
        modifiers.meta = true;
        Some(pi_tui::input::Key::new(KeyCode::Char('k'), modifiers))
    });
    // Unknown modifier names are ignored, like upstream's `parseKeyId`.
    assert_eq!(parse_key_id("hyper+a"), parse_key_id("a"));
    // Out of vocabulary.
    assert_eq!(parse_key_id(""), None);
    assert_eq!(parse_key_id("clear"), None);
    assert_eq!(parse_key_id("f13"), None);
    assert_eq!(parse_key_id("+"), None);
}

#[test]
fn matches_with_case_as_shift_and_symbols_carrying_their_own_shift() {
    // Letters: the case stands in for the Shift bit.
    assert!(key_matches("a", &pi_tui::input::Key::char('a')));
    assert!(!key_matches("a", &pi_tui::input::Key::char('A')));
    assert!(key_matches("shift+a", &pi_tui::input::Key::char('A')));
    assert!(key_matches(
        "shift+a",
        &pi_tui::input::Key::new(KeyCode::Char('a'), KeyModifiers::SHIFT)
    ));
    assert!(key_matches(
        "ctrl+b",
        &pi_tui::input::Key::new(KeyCode::Char('b'), KeyModifiers::CONTROL)
    ));
    assert!(!key_matches(
        "ctrl+b",
        &pi_tui::input::Key::new(KeyCode::Char('b'), KeyModifiers::ALT)
    ));

    // Symbols already carry their shifted identity, so a stray Shift bit does
    // not break the match.
    assert!(key_matches(
        "!",
        &pi_tui::input::Key::new(KeyCode::Char('!'), KeyModifiers::SHIFT)
    ));
    assert!(!key_matches("1", &pi_tui::input::Key::char('!')));

    // Shift+Tab arrives as BackTab without a Shift bit.
    assert!(key_matches(
        "shift+tab",
        &pi_tui::input::Key::new(KeyCode::BackTab, KeyModifiers::NONE)
    ));
    assert!(!key_matches(
        "tab",
        &pi_tui::input::Key::new(KeyCode::BackTab, KeyModifiers::NONE)
    ));
    assert!(key_matches(
        "tab",
        &pi_tui::input::Key::new(KeyCode::Tab, KeyModifiers::NONE)
    ));

    // super is surfaced as `meta` by the crossterm conversion.
    let mut meta = KeyModifiers::NONE;
    meta.meta = true;
    assert!(key_matches(
        "super+k",
        &pi_tui::input::Key::new(KeyCode::Char('k'), meta)
    ));

    // Unknown ids never match.
    assert!(!key_matches("clear", &pi_tui::input::Key::char('c')));
    assert!(!key_matches("", &pi_tui::input::Key::char('c')));
}

#[test]
fn matches_only_key_events() {
    let kb = KeybindingsManager::tui_defaults();

    assert!(!kb.matches(
        &InputEvent::wheel(true, false, 0, 0),
        "tui.altScreen.pageUp"
    ));
    assert!(!kb.matches(
        &InputEvent::gesture(pi_tui::input::MouseGesture::left_press(1, 1)),
        "tui.input.submit"
    ));
    assert!(!kb.matches(
        &InputEvent::Resize {
            width: 80,
            height: 24
        },
        "tui.input.submit"
    ));
    assert!(!kb.matches(&InputEvent::Ignored, "tui.input.submit"));
    assert!(!kb.matches(&ctrl('j'), "tui.no.such.binding"));
}

#[test]
fn matches_the_chords_the_editor_and_alt_screen_use() {
    let kb = KeybindingsManager::tui_defaults();

    assert!(kb.matches(&ctrl('d'), "tui.editor.deleteCharForward"));
    assert!(kb.matches(
        &key(KeyCode::Delete, KeyModifiers::NONE),
        "tui.editor.deleteCharForward"
    ));
    assert!(kb.matches(&alt('b'), "tui.editor.cursorWordLeft"));
    assert!(kb.matches(
        &key(KeyCode::Left, KeyModifiers::CONTROL),
        "tui.editor.cursorWordLeft"
    ));
    assert!(kb.matches(
        &key(KeyCode::Char('-'), KeyModifiers::CONTROL),
        "tui.editor.undo"
    ));
    assert!(kb.matches(
        &key(KeyCode::Char(']'), KeyModifiers::CONTROL),
        "tui.editor.jumpForward"
    ));
    assert!(kb.matches(
        &key(KeyCode::PageUp, KeyModifiers::NONE),
        "tui.altScreen.pageUp"
    ));
    assert!(kb.matches(
        &key(
            KeyCode::Char('f'),
            KeyModifiers::SHIFT | KeyModifiers::CONTROL
        ),
        "tui.altScreen.search"
    ));
    assert!(kb.matches(
        &key(KeyCode::Esc, KeyModifiers::NONE),
        "tui.altScreen.searchClose"
    ));
}

#[test]
fn global_accessors_build_defaults_and_can_be_replaced() {
    reset_keybindings();
    let defaults = get_keybindings();
    assert_eq!(defaults.get_keys("tui.input.submit"), vec!["enter"]);

    set_keybindings(KeybindingsManager::new(
        tui_default_keybindings(),
        config(&[("tui.input.submit", &["ctrl+enter"])]),
    ));
    assert_eq!(
        get_keybindings().get_keys("tui.input.submit"),
        vec!["ctrl+enter"]
    );

    reset_keybindings();
    assert_eq!(
        get_keybindings().get_keys("tui.input.submit"),
        vec!["enter"]
    );
}
