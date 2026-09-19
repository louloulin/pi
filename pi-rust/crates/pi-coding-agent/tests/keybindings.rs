//! Integration tests for the Stage 38 coding-agent keybindings config layer.
//!
//! Mirrors the two upstream suites
//! (`packages/coding-agent/test/keybindings.test.ts` and
//! `packages/coding-agent/test/keybindings-migration.test.ts`) plus the file
//! loading / reload paths, which upstream only exercises indirectly.

use std::path::Path;

use pi_coding_agent::keybindings::{
    app_default_keybindings, is_legacy_keybinding_name, load_from_file, load_from_file_with_table,
    load_raw_config, merged_definitions, migrate_keybindings_config_with_table,
    order_keybindings_config, to_keybindings_config, windows_keybindings, Env, KeybindingsManager,
    Platform, APP_KEYBINDING_IDS, KEYBINDING_NAME_MIGRATIONS,
};
use pi_tui::input::{InputEvent, KeyCode, KeyModifiers};
use pi_tui::keybindings::{tui_default_keybindings, KeybindingDefinition, KeybindingsConfig};
use serde_json::{json, Map, Value};

/// Build an [`Env`] from `("NAME", "value")` pairs.
fn env(pairs: &[(&str, &str)]) -> Env {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

/// A definition table with two known ids, used to make ordering assertions
/// independent of the real table's size.
fn stub_table() -> Vec<(String, KeybindingDefinition)> {
    vec![
        ("a.first".to_string(), KeybindingDefinition::new(["1"])),
        ("b.second".to_string(), KeybindingDefinition::new(["2"])),
    ]
}

/// Parse a JSON object literal into the raw config shape.
fn raw(value: Value) -> Map<String, Value> {
    value.as_object().expect("an object literal").clone()
}

/// The default chords of `id`, panicking when the id is missing.
fn keys(definitions: &[(String, KeybindingDefinition)], id: &str) -> Vec<String> {
    definitions
        .iter()
        .find(|(key, _)| key == id)
        .map(|(_, definition)| definition.default_keys.clone())
        .unwrap_or_else(|| panic!("{id} is part of the table"))
}

/// The configured chords of `id` in a user config.
fn binding_keys(config: &KeybindingsConfig, id: &str) -> Vec<String> {
    config.get(id).map(<[String]>::to_vec).unwrap_or_default()
}

/// The effective chords of `id` in `get_effective_config()` output.
fn effective_keys(config: &[(String, Vec<String>)], id: &str) -> Vec<String> {
    config
        .iter()
        .find(|(key, _)| key == id)
        .map(|(_, keys)| keys.clone())
        .unwrap_or_default()
}

// --- platform / WSL detection ------------------------------------------------

#[test]
fn detects_windows_and_wsl_platforms() {
    let empty = Env::new();

    assert!(windows_keybindings(&Platform::Win32, &empty));
    assert!(windows_keybindings(
        &Platform::Linux,
        &env(&[("WSL_DISTRO_NAME", "Ubuntu")])
    ));
    assert!(windows_keybindings(
        &Platform::Linux,
        &env(&[("WSL_INTEROP", "/run/WSL/123_interop")])
    ));

    // Windows Terminal alone is not WSL.
    assert!(!windows_keybindings(
        &Platform::Linux,
        &env(&[("WT_SESSION", "session")])
    ));

    assert!(!windows_keybindings(&Platform::Linux, &empty));
    assert!(!windows_keybindings(
        &Platform::Darwin,
        &env(&[("WSL_DISTRO_NAME", "Ubuntu")])
    ));
    assert!(!windows_keybindings(
        &Platform::Other("freebsd".to_string()),
        &empty
    ));

    // Upstream's `Boolean(env.X)` treats an empty value as absent.
    assert!(!windows_keybindings(
        &Platform::Linux,
        &env(&[("WSL_DISTRO_NAME", "")])
    ));
    assert!(!windows_keybindings(
        &Platform::Linux,
        &env(&[("WSL_INTEROP", "")])
    ));
}

// --- default tables ----------------------------------------------------------

#[test]
fn app_table_matches_the_declared_ids() {
    let definitions = app_default_keybindings(&Platform::Linux, &Env::new());
    let ids: Vec<&str> = definitions.iter().map(|(id, _)| id.as_str()).collect();

    assert_eq!(ids, APP_KEYBINDING_IDS);
    assert_eq!(definitions.len(), 43);
    for (id, definition) in &definitions {
        assert!(
            definition.description.is_some(),
            "{id} carries a description"
        );
    }
}

#[test]
fn merged_table_appends_app_ids_after_the_tui_ids() {
    let definitions = merged_definitions(&Platform::Linux, &Env::new());
    let tui_count = tui_default_keybindings().len();

    assert_eq!(definitions.len(), tui_count + APP_KEYBINDING_IDS.len());

    // The override keeps the tui entry in place and keeps its description.
    let undo = definitions
        .iter()
        .find(|(id, _)| id == "tui.editor.undo")
        .expect("tui.editor.undo exists");
    assert_eq!(undo.1.default_keys, ["ctrl+-"]);
    assert_eq!(undo.1.description.as_deref(), Some("Undo"));

    let first_app = definitions
        .iter()
        .position(|(id, _)| id == "app.interrupt")
        .expect("app.interrupt exists");
    assert!(definitions[..first_app]
        .iter()
        .all(|(id, _)| id.starts_with("tui.")));
    assert!(definitions[first_app..]
        .iter()
        .all(|(id, _)| id.starts_with("app.")));
}

#[test]
fn native_windows_defaults() {
    let definitions = merged_definitions(&Platform::Win32, &Env::new());

    assert_eq!(keys(&definitions, "tui.editor.undo"), ["ctrl+z"]);
    assert_eq!(
        keys(&definitions, "tui.altScreen.previousPrompt"),
        ["ctrl+up"]
    );
    assert_eq!(
        keys(&definitions, "tui.altScreen.nextPrompt"),
        ["ctrl+down"]
    );
    assert_eq!(keys(&definitions, "tui.altScreen.search"), ["ctrl+f"]);
    assert_eq!(keys(&definitions, "app.clipboard.pasteImage"), ["alt+v"]);
    assert_eq!(keys(&definitions, "app.message.followUp"), ["ctrl+q"]);
    assert_eq!(keys(&definitions, "app.message.dequeue"), ["alt+q"]);
    assert_eq!(keys(&definitions, "app.model.cycleBackward"), ["alt+p"]);
    assert!(keys(&definitions, "app.suspend").is_empty());
}

#[test]
fn wsl_defaults_use_the_alt_undo_and_ctrl_alt_family() {
    let definitions = merged_definitions(&Platform::Linux, &env(&[("WSL_DISTRO_NAME", "Ubuntu")]));

    assert_eq!(keys(&definitions, "tui.editor.undo"), ["alt+z"]);
    assert_eq!(
        keys(&definitions, "tui.altScreen.previousPrompt"),
        ["ctrl+up"]
    );
    assert_eq!(
        keys(&definitions, "tui.altScreen.nextPrompt"),
        ["ctrl+down"]
    );
    assert_eq!(keys(&definitions, "tui.altScreen.search"), ["ctrl+f"]);
    assert_eq!(keys(&definitions, "app.clipboard.pasteImage"), ["alt+v"]);
    assert_eq!(keys(&definitions, "app.message.followUp"), ["ctrl+q"]);
    assert_eq!(keys(&definitions, "app.message.dequeue"), ["alt+q"]);
    assert_eq!(keys(&definitions, "app.model.cycleBackward"), ["alt+p"]);
    assert_eq!(keys(&definitions, "app.suspend"), ["ctrl+z"]);
}

#[test]
fn darwin_and_plain_linux_defaults() {
    for platform in [Platform::Darwin, Platform::Linux] {
        let definitions = merged_definitions(&platform, &Env::new());

        assert_eq!(keys(&definitions, "tui.editor.undo"), ["ctrl+-"]);
        assert_eq!(
            keys(&definitions, "tui.altScreen.previousPrompt"),
            ["ctrl+shift+up", "ctrl+up"]
        );
        assert_eq!(
            keys(&definitions, "tui.altScreen.nextPrompt"),
            ["ctrl+shift+down", "ctrl+down"]
        );
        assert_eq!(keys(&definitions, "tui.altScreen.search"), ["ctrl+shift+f"]);
        assert_eq!(keys(&definitions, "app.clipboard.pasteImage"), ["ctrl+v"]);
        assert_eq!(keys(&definitions, "app.message.followUp"), ["alt+enter"]);
        assert_eq!(keys(&definitions, "app.message.dequeue"), ["alt+up"]);
        assert_eq!(
            keys(&definitions, "app.model.cycleBackward"),
            ["shift+ctrl+p"]
        );
        assert_eq!(keys(&definitions, "app.suspend"), ["ctrl+z"]);
    }

    let darwin = merged_definitions(&Platform::Darwin, &Env::new());
    assert_eq!(
        keys(&darwin, "app.tree.foldOrUp"),
        ["alt+left", "ctrl+left"]
    );
    assert_eq!(
        keys(&darwin, "app.tree.unfoldOrDown"),
        ["alt+right", "ctrl+right"]
    );

    let linux = merged_definitions(&Platform::Linux, &Env::new());
    assert_eq!(keys(&linux, "app.tree.foldOrUp"), ["ctrl+left", "alt+left"]);
    assert_eq!(
        keys(&linux, "app.tree.unfoldOrDown"),
        ["ctrl+right", "alt+right"]
    );
}

// --- migration ---------------------------------------------------------------

#[test]
fn renames_legacy_ids_to_namespaced_ids() {
    let (config, migrated) = migrate_keybindings_config_with_table(
        &raw(json!({ "undo": ["ctrl+z"], "interrupt": "ctrl+x" })),
        &stub_table(),
    );

    assert!(migrated);
    // Unknown ids come after the table ids, in lexicographic order.
    assert_eq!(
        config,
        vec![
            ("app.interrupt".to_string(), json!("ctrl+x")),
            ("tui.editor.undo".to_string(), json!(["ctrl+z"])),
        ]
    );
}

#[test]
fn keeps_the_namespaced_value_when_old_and_new_names_both_exist() {
    let (config, migrated) = migrate_keybindings_config_with_table(
        &raw(json!({ "expandTools": "ctrl+x", "app.tools.expand": "ctrl+y" })),
        &stub_table(),
    );

    assert!(migrated);
    assert_eq!(
        config,
        vec![("app.tools.expand".to_string(), json!("ctrl+y"))]
    );
}

#[test]
fn reports_when_nothing_was_migrated() {
    let (config, migrated) = migrate_keybindings_config_with_table(
        &raw(json!({ "app.tools.expand": "ctrl+y" })),
        &stub_table(),
    );

    assert!(!migrated);
    assert_eq!(
        config,
        vec![("app.tools.expand".to_string(), json!("ctrl+y"))]
    );
}

#[test]
fn orders_known_ids_by_the_table_then_unknown_ids_alphabetically() {
    let config = vec![
        ("zeta".to_string(), json!("z")),
        ("b.second".to_string(), json!("2")),
        ("alpha".to_string(), json!("a")),
        ("a.first".to_string(), json!("1")),
    ];
    let ordered = order_keybindings_config(config, &stub_table());

    let ids: Vec<&str> = ordered.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["a.first", "b.second", "alpha", "zeta"]);
}

#[test]
fn exposes_the_legacy_name_table() {
    assert_eq!(KEYBINDING_NAME_MIGRATIONS.len(), 59);
    assert!(is_legacy_keybinding_name("undo"));
    assert!(is_legacy_keybinding_name("interrupt"));
    assert!(!is_legacy_keybinding_name("tui.editor.undo"));
    assert!(!is_legacy_keybinding_name("app.interrupt"));
}

// --- type filtering ----------------------------------------------------------

#[test]
fn drops_non_string_and_mixed_bindings_but_keeps_an_empty_array() {
    let (config, _) = migrate_keybindings_config_with_table(
        &raw(json!({
            "a.first": 42,
            "b.second": { "ctrl+p": true },
            "app.clear": ["ctrl+c", 7],
            "app.exit": "ctrl+d",
            "app.interrupt": ["escape"],
            "app.session.new": [],
        })),
        &stub_table(),
    );
    let bindings = to_keybindings_config(&config);

    assert_eq!(bindings.len(), 3);
    assert_eq!(binding_keys(&bindings, "app.exit"), ["ctrl+d"]);
    assert_eq!(binding_keys(&bindings, "app.interrupt"), ["escape"]);
    assert!(binding_keys(&bindings, "app.session.new").is_empty());
    assert!(bindings.get("app.session.new").is_some());
    assert!(bindings.get("a.first").is_none());
    assert!(bindings.get("b.second").is_none());
    assert!(bindings.get("app.clear").is_none());
}

// --- file loading ------------------------------------------------------------

fn write_config(path: &Path, content: &str) {
    std::fs::write(path, content).expect("write config fixture");
}

#[test]
fn load_raw_config_returns_none_for_missing_invalid_and_non_object_files() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("keybindings.json");

    assert!(load_raw_config(&path).is_none());

    for content in ["{not json", "[]", "42", "\"ctrl+c\"", "null", ""] {
        write_config(&path, content);
        assert!(
            load_raw_config(&path).is_none(),
            "{content:?} is not a top-level object"
        );
    }

    write_config(&path, r#"{"app.exit":"ctrl+d"}"#);
    let raw = load_raw_config(&path).expect("a top-level object");
    assert_eq!(raw.get("app.exit"), Some(&json!("ctrl+d")));
}

#[test]
fn loads_and_migrates_a_config_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("keybindings.json");
    write_config(
        &path,
        r#"{"cursorUp":["up","ctrl+p"],"expandTools":"ctrl+x","app.tree.filter.all":12}"#,
    );

    let bindings = load_from_file_with_table(&path, &stub_table());

    assert_eq!(
        binding_keys(&bindings, "tui.editor.cursorUp"),
        ["up", "ctrl+p"]
    );
    assert_eq!(binding_keys(&bindings, "app.tools.expand"), ["ctrl+x"]);
    assert!(bindings.get("app.tree.filter.all").is_none());
}

#[test]
fn strips_a_leading_bom_before_parsing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("keybindings.json");
    write_config(&path, "\u{feff}{\"app.exit\":\"ctrl+d\"}");

    // Both the explicit-table and the current-platform entry points BOM-strip.
    assert_eq!(
        binding_keys(&load_from_file_with_table(&path, &stub_table()), "app.exit"),
        ["ctrl+d"]
    );
    assert_eq!(binding_keys(&load_from_file(&path), "app.exit"), ["ctrl+d"]);
}

#[test]
fn missing_config_file_yields_empty_user_bindings() {
    let dir = tempfile::tempdir().expect("temp dir");
    let bindings = load_from_file_with_table(&dir.path().join("keybindings.json"), &stub_table());

    assert!(bindings.is_empty());
}

// --- KeybindingsManager ------------------------------------------------------

#[test]
fn manager_migrates_user_bindings_and_resolves_defaults() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("keybindings.json");
    write_config(&path, r#"{"selectConfirm":"enter","interrupt":"ctrl+x"}"#);

    let manager =
        KeybindingsManager::create_with_platform(dir.path(), &Platform::Linux, &Env::new());

    assert_eq!(manager.config_path(), Some(path.as_path()));
    assert_eq!(
        binding_keys(manager.get_user_bindings(), "tui.select.confirm"),
        ["enter"]
    );
    assert_eq!(
        binding_keys(manager.get_user_bindings(), "app.interrupt"),
        ["ctrl+x"]
    );

    let effective = manager.get_effective_config();
    assert_eq!(effective_keys(&effective, "tui.select.confirm"), ["enter"]);
    assert_eq!(effective_keys(&effective, "app.interrupt"), ["ctrl+x"]);
    // An override does not disturb the other defaults.
    assert_eq!(effective_keys(&effective, "app.clear"), ["ctrl+c"]);
    assert_eq!(effective_keys(&effective, "tui.editor.undo"), ["ctrl+-"]);
}

#[test]
fn manager_reload_rereads_the_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("keybindings.json");
    write_config(&path, r#"{"interrupt":"ctrl+x"}"#);

    let mut manager =
        KeybindingsManager::create_with_platform(dir.path(), &Platform::Darwin, &Env::new());
    assert_eq!(
        binding_keys(manager.get_user_bindings(), "app.interrupt"),
        ["ctrl+x"]
    );

    write_config(&path, r#"{"app.clear":"ctrl+y"}"#);
    manager.reload();

    assert!(manager.get_user_bindings().get("app.interrupt").is_none());
    assert_eq!(
        binding_keys(manager.get_user_bindings(), "app.clear"),
        ["ctrl+y"]
    );
    assert_eq!(
        effective_keys(&manager.get_effective_config(), "app.clear"),
        ["ctrl+y"]
    );
}

#[test]
fn manager_without_a_config_file_keeps_the_defaults() {
    let dir = tempfile::tempdir().expect("temp dir");
    let manager =
        KeybindingsManager::create_with_platform(dir.path(), &Platform::Win32, &Env::new());

    assert!(manager.get_user_bindings().is_empty());
    let effective = manager.get_effective_config();
    assert_eq!(
        effective.len(),
        merged_definitions(&Platform::Win32, &Env::new()).len()
    );
    assert_eq!(
        effective_keys(&effective, "app.clipboard.pasteImage"),
        ["alt+v"]
    );
}

#[test]
fn manager_matches_events_against_the_effective_table() {
    let dir = tempfile::tempdir().expect("temp dir");
    let manager =
        KeybindingsManager::create_with_platform(dir.path(), &Platform::Linux, &Env::new());
    let ctrl_c = InputEvent::key(KeyCode::Char('c'), KeyModifiers::CONTROL);

    assert!(manager.matches(&ctrl_c, "app.clear"));
    assert!(!manager.matches(&ctrl_c, "app.exit"));
    assert!(!manager.matches(&ctrl_c, "not.a.binding"));
}
