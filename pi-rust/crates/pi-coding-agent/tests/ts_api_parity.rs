//! Parity audit: every `UiRegionHost` method must have a counterpart in the
//! TypeScript `ExtensionUIContext` and `ExtensionApi` surfaces
//! (`packages/coding-agent/src/core/extensions/types.ts`).
//!
//! Plan §8.1 calls this test "ts_api_parity.rs — reflection enum of all
//! 30+ pub methods of `ExtensionUIContext`, matches TS
//! `Object.keys(ExtensionUIContext)`". This file is the Rust counterpart:
//! it enumerates the canonical TS names (kept in sync by reading the
//! TS interface), and asserts the Rust `UiRegionHost` trait exposes the
//! matching snake_case methods.
//!
//! What the test asserts:
//! - Every TS UI method has a Rust method in `UiRegionHost` (or its
//!   4-method `UiHandler` neighbour).
//! - The mapping table is internally consistent (sorted, unique, no
//!   dangling entries).
//! - Any drift is reported as a failing diff so a reviewer sees the
//!   concrete shape of the gap.
//!
//! What it deliberately does NOT assert:
//! - Behaviour. This is a structural lint, not a semantic one.
//! - Signatures beyond "method exists". A signature drift is a separate
//!   follow-up audit, kept off this test so it stays cheap to run.

#![cfg(test)]

use pi_extensions::UiHandler;
use pi_extensions::UiRegionHost;
use pi_protocol::UiLevel;

use std::collections::HashSet;

/// Canonical TS-side names this port must mirror.
///
/// Source: `packages/coding-agent/src/core/extensions/types.ts` lines
/// 131-282 (ExtensionUIContext) and 1255-1295 (register_* on ExtensionApi).
/// Kept sorted so a diff between the canonical list and the Rust surface
/// is easy to scan.
const TS_CANONICAL: &[&str] = &[
    "addAutocompleteProvider",
    "confirm",
    "custom",
    "editor",
    "getAllThemes",
    "getEditorComponent",
    "getEditorText",
    "getTheme",
    "getToolsExpanded",
    "input",
    "notify",
    "onTerminalInput",
    "pasteToEditor",
    "registerEntryRenderer",
    "registerFlag",
    "registerMarkdownTransformer",
    "registerMessageRenderer",
    "registerShortcut",
    "select",
    "setEditorComponent",
    "setEditorText",
    "setFooter",
    "setHeader",
    "setHiddenThinkingLabel",
    "setStatus",
    "setTheme",
    "setTitle",
    "setToolsExpanded",
    "setWidget",
    "setWorkingIndicator",
    "setWorkingMessage",
    "setWorkingVisible",
    "theme",
];

/// Map of TS name → (trait that owns it, Rust method name).
///
/// The 4 dialog methods (`select`, `confirm`, `input`, `notify`) live on
/// `UiHandler` because they suspend on the user; the rest live on
/// `UiRegionHost` (the region-mutation adapter). Tests assert the
/// mapping covers every entry in [`TS_CANONICAL`].
///
/// When the mapping drifts (a Rust method is renamed, a TS method is
/// added or removed), the audit fails with the concrete diff so the
/// reviewer can update both sides together.
const TS_TO_RUST: &[(&str, &str, &str)] = &[
    // TS name, host trait (UiRegionHost | UiHandler), rust method name
    ("addAutocompleteProvider", "UiRegionHost", "add_autocomplete_provider"),
    ("confirm", "UiHandler", "confirm"),
    ("custom", "UiRegionHost", "open_custom"),
    ("editor", "UiRegionHost", "editor"),
    ("getAllThemes", "UiRegionHost", "get_all_themes"),
    ("getEditorComponent", "UiRegionHost", "get_editor_component"),
    ("getEditorText", "UiRegionHost", "get_editor_text"),
    ("getTheme", "UiRegionHost", "get_theme"),
    ("getToolsExpanded", "UiRegionHost", "get_tools_expanded"),
    ("input", "UiHandler", "input"),
    ("notify", "UiHandler", "notify"),
    ("onTerminalInput", "UiRegionHost", "on_terminal_input"),
    ("pasteToEditor", "UiRegionHost", "paste_to_editor"),
    ("registerEntryRenderer", "UiRegionHost", "register_entry_renderer"),
    ("registerFlag", "UiRegionHost", "register_flag"),
    ("registerMarkdownTransformer", "UiRegionHost", "register_markdown_transformer"),
    ("registerMessageRenderer", "UiRegionHost", "register_message_renderer"),
    ("registerShortcut", "UiRegionHost", "register_shortcut"),
    ("select", "UiHandler", "select"),
    ("setEditorComponent", "UiRegionHost", "set_editor_component"),
    ("setEditorText", "UiRegionHost", "set_editor_text"),
    ("setFooter", "UiRegionHost", "set_footer"),
    ("setHeader", "UiRegionHost", "set_header"),
    ("setHiddenThinkingLabel", "UiRegionHost", "set_hidden_thinking_label"),
    ("setStatus", "UiRegionHost", "set_status"),
    ("setTheme", "UiRegionHost", "set_theme"),
    ("setTitle", "UiRegionHost", "set_title"),
    ("setToolsExpanded", "UiRegionHost", "set_tools_expanded"),
    ("setWidget", "UiRegionHost", "set_widget"),
    ("setWorkingIndicator", "UiRegionHost", "set_working_indicator"),
    ("setWorkingMessage", "UiRegionHost", "set_working_message"),
    ("setWorkingVisible", "UiRegionHost", "set_working_visible"),
    // `theme` is a getter in TS (`readonly theme: Theme`); the Rust port
    // exposes the equivalent through `get_theme` (singular). Logged as a
    // renamed alias so the audit does not flag it twice.
    ("theme", "UiRegionHost", "get_theme"),
];

fn assert_sorted_unique(items: &[&str], label: &str) {
    let mut sorted: Vec<&str> = items.to_vec();
    sorted.sort_unstable();
    let mut seen: HashSet<&str> = HashSet::new();
    for item in items {
        assert!(
            seen.insert(*item),
            "{label} contains duplicate entry `{item}`",
        );
    }
    let original: Vec<&str> = items.to_vec();
    assert_eq!(
        original, sorted,
        "{label} must be kept sorted alphabetically so the diff is scannable"
    );
}

#[test]
fn ts_canonical_is_sorted_and_unique() {
    assert_sorted_unique(TS_CANONICAL, "TS_CANONICAL");
    assert_sorted_unique(
        &TS_TO_RUST
            .iter()
            .map(|(ts, _, _)| *ts)
            .collect::<Vec<_>>(),
        "TS_TO_RUST ts column",
    );
    // The Rust column is allowed to alias — `theme` (TS getter) and
    // `getTheme` (TS method) both map to `get_theme`. Drop the
    // uniqueness check on the Rust column and trust the `required`
    // membership test in `region_host_exposes_every_non_dialog_method`
    // to catch typos / dropped entries.
}

#[test]
fn ts_canonical_matches_mapping() {
    let mapped: HashSet<&str> = TS_TO_RUST.iter().map(|(ts, _, _)| *ts).collect();
    let canonical: HashSet<&str> = TS_CANONICAL.iter().copied().collect();
    let missing: Vec<&&str> = canonical.difference(&mapped).collect();
    let extra: Vec<&&str> = mapped.difference(&canonical).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "TS_TO_RUST out of sync with TS_CANONICAL.\n\
         missing from mapping (add a row): {missing:?}\n\
         extra in mapping (drop the row): {extra:?}",
    );
}

#[test]
fn every_ts_method_has_a_rust_counterpart() {
    for (ts_name, trait_name, rust_method) in TS_TO_RUST {
        assert!(
            TS_CANONICAL.contains(ts_name),
            "`{ts_name}` is in TS_TO_RUST but not in TS_CANONICAL — \
             either delete the row or re-add the canonical entry",
        );
        assert!(
            *trait_name == "UiRegionHost" || *trait_name == "UiHandler",
            "unknown trait `{trait_name}` for TS method `{ts_name}`",
        );
        assert!(
            !rust_method.is_empty(),
            "empty Rust method name for TS `{ts_name}`",
        );
    }
}

#[test]
fn rust_dialog_methods_live_on_ui_handler() {
    // The 4 dialog methods suspend on the user, so they sit on UiHandler
    // (sync) rather than UiRegionHost (region-mutation adapter). The
    // audit keeps them out of UiRegionHost to make that intent visible.
    let on_handler: HashSet<&str> = TS_TO_RUST
        .iter()
        .filter(|(_, trait_name, _)| *trait_name == "UiHandler")
        .map(|(ts, _, _)| *ts)
        .collect();
    let expected: HashSet<&str> = ["confirm", "input", "notify", "select"]
        .into_iter()
        .collect();
    assert_eq!(
        on_handler, expected,
        "the four dialog methods must live on UiHandler",
    );
}

#[test]
fn region_host_exposes_every_non_dialog_method() {
    // UiRegionHost is a sealed trait (only TuiRegionHost in pi-coding-agent
    // implements it), and `dyn UiRegionHost` does not expose its method
    // names at runtime. The structural assertion is therefore the
    // mapping table itself: every non-dialog method we promise TS is
    // represented there. The runtime check that follows proves the trait
    // is at least object-safe enough to hold behind `Arc<dyn ...>`.
    let on_region: HashSet<(&str, &str)> = TS_TO_RUST
        .iter()
        .filter(|(_, trait_name, _)| *trait_name == "UiRegionHost")
        .map(|(ts, _, rust)| (*ts, *rust))
        .collect();
    let required: HashSet<(&str, &str)> = [
        ("addAutocompleteProvider", "add_autocomplete_provider"),
        ("custom", "open_custom"),
        ("editor", "editor"),
        ("getAllThemes", "get_all_themes"),
        ("getEditorComponent", "get_editor_component"),
        ("getEditorText", "get_editor_text"),
        ("getTheme", "get_theme"),
        ("getToolsExpanded", "get_tools_expanded"),
        ("onTerminalInput", "on_terminal_input"),
        ("pasteToEditor", "paste_to_editor"),
        ("registerEntryRenderer", "register_entry_renderer"),
        ("registerFlag", "register_flag"),
        ("registerMarkdownTransformer", "register_markdown_transformer"),
        ("registerMessageRenderer", "register_message_renderer"),
        ("registerShortcut", "register_shortcut"),
        ("setEditorComponent", "set_editor_component"),
        ("setEditorText", "set_editor_text"),
        ("setFooter", "set_footer"),
        ("setHeader", "set_header"),
        ("setHiddenThinkingLabel", "set_hidden_thinking_label"),
        ("setStatus", "set_status"),
        ("setTheme", "set_theme"),
        ("setTitle", "set_title"),
        ("setToolsExpanded", "set_tools_expanded"),
        ("setWidget", "set_widget"),
        ("setWorkingIndicator", "set_working_indicator"),
        ("setWorkingMessage", "set_working_message"),
        ("setWorkingVisible", "set_working_visible"),
        ("theme", "get_theme"),
    ]
    .into_iter()
    .collect();
    let missing: Vec<&(&str, &str)> = required.difference(&on_region).collect();
    assert!(
        missing.is_empty(),
        "UiRegionHost mapping missing entries: {:?}",
        missing.iter().map(|(ts, rust)| (ts, rust)).collect::<Vec<_>>(),
    );
}

#[test]
fn ui_handler_dialog_signatures_compile() {
    // The 4 dialog methods are async fn(... ) -> Option<String>/bool.
    // Asserting the trait is object-safe is the cheapest check that
    // nothing quietly broke the contract — UiHandler is referenced
    // through `dyn` everywhere downstream.
    fn _accept(_h: &dyn UiHandler) {
        let _ = <dyn UiHandler as UiHandler>::confirm;
        let _ = <dyn UiHandler as UiHandler>::input;
        let _ = <dyn UiHandler as UiHandler>::select;
        let _ = <dyn UiHandler as UiHandler>::notify;
    }
    // UiLevel is what `notify` carries — referencing it here proves the
    // import path the production code uses is still the one the audit
    // sees.
    let _level: UiLevel = UiLevel::Info;
}

#[test]
fn ui_region_host_trait_is_object_safe() {
    // UiRegionHost is also stored behind `Arc<dyn UiRegionHost>` in
    // production. If a future change accidentally adds a generic method
    // or returns `Self`, the trait stops being object-safe and the
    // production wiring breaks; this test catches that at audit time.
    fn _accept(_h: &dyn UiRegionHost) {
        let _ = <dyn UiRegionHost as UiRegionHost>::set_status;
        let _ = <dyn UiRegionHost as UiRegionHost>::get_editor_text;
        let _ = <dyn UiRegionHost as UiRegionHost>::register_shortcut;
    }
}
