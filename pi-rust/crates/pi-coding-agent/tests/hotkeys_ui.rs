//! Integration tests for `/hotkeys` markdown-table UI.
//!
//! Mirrors the layout built by
//! `packages/coding-agent/src/modes/interactive/interactive-mode.ts:6315-6429`,
//! which feeds a single `Markdown(hotkeys)` widget:
//!
//! ```text
//! # Keyboard Shortcuts
//!
//! **Navigation**
//! | Key | Action |
//! |-----|--------|
//! | `Ctrl+U` / `Ctrl+P` | previous line / prompt history |
//! ...
//!
//! **Extensions**
//! | Key | Action |
//! |-----|--------|
//! | `Ctrl+T` | Toggle my widget |
//!
//! **Commands**
//! | Key | Action |
//! |-----|--------|
//! | `/` | Slash commands (`/help`, `/model`, `/hotkeys`, …) |
//! | `Esc` | Close selector / overlay first |
//! ```
//!
//! These tests pin the public surface — the formatter is the contract a
//! downstream `Markdown(hotkeys)` widget will rely on, so changes here
//! have to either keep the format or update the contract test.

#![cfg(not(target_arch = "wasm32"))]

use pi_coding_agent::commands::slash::hotkeys_text_with;
use pi_coding_agent::extensions::extension_shortcuts::{
    ExtensionShortcut, ExtensionShortcutRegistry,
};
use pi_tui::keybindings::{KeybindingsConfig, KeybindingsManager};

fn manager() -> KeybindingsManager {
    KeybindingsManager::new(
        pi_coding_agent::keybindings::merged_definitions(
            &pi_coding_agent::keybindings::Platform::Linux,
            &pi_coding_agent::keybindings::process_env(),
        ),
        KeybindingsConfig::default(),
    )
}

fn install_one(
    registry: &ExtensionShortcutRegistry,
    chord: &str,
    description: Option<&str>,
    extension_path: Option<&str>,
) -> u64 {
    let keybindings = pi_tui::keybindings::get_keybindings();
    registry
        .register_with_meta(
            chord,
            "cb".into(),
            description.map(str::to_owned),
            extension_path.map(str::to_owned),
            &keybindings,
        )
        .expect("register")
}

#[test]
fn header_is_h1_keyboard_shortcuts() {
    let text = hotkeys_text_with(&manager(), None);
    assert!(
        text.starts_with("# Keyboard Shortcuts\n"),
        "the markdown body must start with an H1 to match the TS widget:\n{text}"
    );
}

#[test]
fn every_section_is_a_markdown_table_with_key_and_action_columns() {
    let text = hotkeys_text_with(&manager(), None);
    for heading in ["Navigation", "Editing", "Chat log", "Other"] {
        let table = table_for_heading(&text, heading)
            .unwrap_or_else(|| panic!("missing `{heading}` table:\n{text}"));
        assert_eq!(
            table.lines().next().unwrap_or(""),
            format!("**{heading}**"),
            "the `{heading}` row must be a bold heading, not a bullet"
        );
        let header_line = table.lines().nth(1).unwrap_or("");
        assert_eq!(header_line, "");
        assert_eq!(
            table.lines().nth(2).unwrap_or(""),
            "| Key | Action |",
            "the `{heading}` table must declare `Key | Action` columns"
        );
        assert_eq!(
            table.lines().nth(3).unwrap_or(""),
            "|-----|--------|",
            "the `{heading}` table must declare a markdown separator row"
        );
    }
}

#[test]
fn key_column_uses_backticks_around_every_chord() {
    let text = hotkeys_text_with(&manager(), None);
    // Find the "Other" table and assert every data row starts with `| ` and
    // contains a backtick-enclosed key cell.
    let table = table_for_heading(&text, "Other").expect("Other table");
    let rows: Vec<&str> = table
        .lines()
        .skip(3)
        .filter(|line| line.starts_with("| `"))
        .collect();
    assert!(!rows.is_empty(), "Other table has no data rows:\n{table}");
    for row in rows {
        // The Key column must be the first backtick-enclosed token.
        let key_cell = row
            .split("|")
            .nth(1)
            .unwrap_or("")
            .trim();
        assert!(
            key_cell.starts_with('`') && key_cell.ends_with('`'),
            "key cell must be backtick-quoted, got {key_cell:?} in row {row:?}"
        );
    }
}

#[test]
fn extensions_section_uses_registry_snapshot_in_insertion_order() {
    let registry = ExtensionShortcutRegistry::new();
    install_one(&registry, "ctrl+t", Some("Toggle widget"), Some("/ext/a.js"));
    install_one(
        &registry,
        "ctrl+y",
        Some("Run second action"),
        Some("/ext/b.js"),
    );
    let shortcuts = registry.snapshot();
    let text = hotkeys_text_with(&manager(), Some(&shortcuts));
    let table = table_for_heading(&text, "Extensions")
        .expect("Extensions table must appear when at least one shortcut is registered");
    // Order matches snapshot order (sorted by handle — the registry
    // already sorts by handle ascending).
    let ctrl_t_pos = table.find("`Ctrl+T`").expect("Ctrl+T row");
    let ctrl_y_pos = table.find("`Ctrl+Y`").expect("Ctrl+Y row");
    assert!(
        ctrl_t_pos < ctrl_y_pos,
        "snapshot order must be preserved:\n{table}"
    );
    assert!(
        table.contains("Toggle widget"),
        "description must surface, not the extension path:\n{table}"
    );
}

#[test]
fn extensions_section_falls_back_to_extension_path_when_no_description() {
    let registry = ExtensionShortcutRegistry::new();
    install_one(&registry, "ctrl+q", None, Some("/ext/quiet.js"));
    let shortcuts = registry.snapshot();
    let text = hotkeys_text_with(&manager(), Some(&shortcuts));
    let table = table_for_heading(&text, "Extensions").expect("Extensions table");
    assert!(
        table.contains("/ext/quiet.js"),
        "missing description must fall back to extension_path:\n{table}"
    );
}

#[test]
fn extensions_section_is_absent_when_registry_is_empty() {
    let shortcuts = ExtensionShortcutRegistry::new().snapshot();
    let text = hotkeys_text_with(&manager(), Some(&shortcuts));
    assert!(
        !text.contains("**Extensions**"),
        "no shortcuts → no Extensions section, got:\n{text}"
    );
}

#[test]
fn commands_section_lists_slash_and_escape_rows() {
    let text = hotkeys_text_with(&manager(), None);
    let table = table_for_heading(&text, "Commands").expect("Commands table");
    assert!(table.contains("Slash commands"), "{table}");
    assert!(table.contains("Close selector / overlay first"), "{table}");
}

#[test]
fn unbound_action_drops_its_row() {
    let mut config = KeybindingsConfig::default();
    config.set("app.session.fork", Vec::<String>::new());
    let manager = KeybindingsManager::new(
        pi_coding_agent::keybindings::merged_definitions(
            &pi_coding_agent::keybindings::Platform::Linux,
            &pi_coding_agent::keybindings::process_env(),
        ),
        config,
    );
    let text = hotkeys_text_with(&manager, None);
    assert!(
        !text.contains("fork a session from a message"),
        "unbound action must be dropped, got:\n{text}"
    );
}

#[test]
fn snapshot_of_empty_registry_drops_extensions_section() {
    let shortcuts: Vec<ExtensionShortcut> = Vec::new();
    let text = hotkeys_text_with(&manager(), Some(&shortcuts));
    assert!(!text.contains("**Extensions**"), "{text}");
}

/// Return the markdown table that follows a `**Heading**\n` line. The
/// table is the contiguous block of lines until the next blank line or
/// the next `**` heading.
fn table_for_heading(text: &str, heading: &str) -> Option<String> {
    let marker = format!("**{heading}**\n");
    let after = text.split_once(&marker)?.1;
    let mut out = String::new();
    out.push_str(&marker);
    for line in after.lines() {
        if line.starts_with("**") {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}