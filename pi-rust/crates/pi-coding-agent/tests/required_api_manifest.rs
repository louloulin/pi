//! Integration tests for the extension `required_api` manifest contract.
//!
//! Mirrors the loader behaviour in
//! `packages/coding-agent/src/core/extensions/runner.ts`: an extension's
//! manifest declares the API surface it depends on, and the loader
//! fail-fasts on the first entry that the running build does not
//! implement.
//!
//! The Rust side splits the work across two modules:
//!
//! * [`pi_coding_agent::extensions::api_surface`] owns the
//!   [`STABLE_API`](pi_coding_agent::extensions::api_surface::STABLE_API)
//!   list — the source of truth for "what is implemented in this build".
//! * [`pi_coding_agent::extensions::required_api`] owns the
//!   [`validate_required_api`](pi_coding_agent::extensions::required_api::validate_required_api)
//!   entry point that the extension loader calls before instantiating
//!   the JS host.
//!
//! These tests pin both together so a downstream `ts_api_parity` CI run
//! can rely on the contract.

#![cfg(not(target_arch = "wasm32"))]

use pi_coding_agent::extensions::api_surface::{is_stable, STABLE_API};
use pi_coding_agent::extensions::required_api::{validate_required_api, RequiredApiReport};

#[test]
fn stable_api_covers_every_published_event() {
    // The TS list of `ExtensionEvent` variants we promised parity for.
    let expected_events = [
        "agent_end",
        "agent_start",
        "cache_warming_decision",
        "input",
        "message_end",
        "message_start",
        "message_update",
        "model_select",
        "project_trust",
        "provider_stream",
        "resources_discover",
        "session_before_compact",
        "session_before_fork",
        "session_before_switch",
        "session_before_tree",
        "session_compact",
        "session_compact_failed",
        "session_fork",
        "session_info_changed",
        "session_shutdown",
        "session_start",
        "session_tree",
        "thinking_level_select",
        "tool_call",
        "tool_execution_end",
        "tool_execution_start",
        "tool_execution_update",
        "tool_result",
        "turn_end",
        "turn_start",
        "ui_prompt_end",
        "ui_prompt_start",
        "user_bash",
        "user_message",
    ];
    for name in expected_events {
        assert!(
            is_stable(name),
            "ExtensionEvent variant {name} must be present in STABLE_API"
        );
    }
}

#[test]
fn stable_api_covers_every_published_top_level_method() {
    // The TS `ExtensionAPI` top-level methods beyond the `ui.*` namespace.
    // These are extension-facing actions (abort / send_user_message / fork
    // / switch_session / ...) plus read-only accessors
    // (get_model / get_thinking_level / ...). They live directly on the
    // extension context, not on `ExtensionUIContext`, so the pin uses
    // snake_case names (matching the Rust method names).
    let expected_top_level = [
        "abort",
        "append_entry",
        "compact",
        "follow_up",
        "fork",
        "get_active_tools",
        "get_all_tools",
        "get_flag",
        "get_model",
        "get_session_name",
        "get_thinking_level",
        "is_streaming",
        "navigate_tree",
        "new_session",
        "register_tool",
        "send_message",
        "send_user_message",
        "set_active_tools",
        "set_model",
        "set_session_name",
        "set_thinking_level",
        "switch_session",
        "wait_for_idle",
    ];
    for name in expected_top_level {
        assert!(
            is_stable(name),
            "ExtensionAPI top-level method {name} must be present in STABLE_API"
        );
    }
}

#[test]
fn stable_api_covers_every_published_ui_method() {
    let expected_ui = [
        "ui.addAutocompleteProvider",
        "ui.confirm",
        "ui.custom",
        "ui.editor",
        "ui.getAllThemes",
        "ui.getEditorComponent",
        "ui.getEditorText",
        "ui.getTheme",
        "ui.getToolsExpanded",
        "ui.input",
        "ui.notify",
        "ui.onTerminalInput",
        "ui.pasteToEditor",
        "ui.registerEntryRenderer",
        "ui.registerFlag",
        "ui.registerMarkdownTransformer",
        "ui.registerMessageRenderer",
        "ui.registerShortcut",
        "ui.select",
        "ui.setEditorComponent",
        "ui.setEditorText",
        "ui.setFooter",
        "ui.setHeader",
        "ui.setHiddenThinkingLabel",
        "ui.setStatus",
        "ui.setTheme",
        "ui.setTitle",
        "ui.setToolsExpanded",
        "ui.setWidget",
        "ui.setWorkingIndicator",
        "ui.setWorkingMessage",
        "ui.setWorkingVisible",
        "ui.theme",
    ];
    for name in expected_ui {
        assert!(
            is_stable(name),
            "ExtensionUIContext method {name} must be present in STABLE_API"
        );
    }
}

#[test]
fn empty_required_api_loads_against_the_full_surface() {
    let report = validate_required_api(&[]);
    assert!(report.is_satisfied(), "{report}");
    assert_eq!(report.declared, Vec::<String>::new());
    assert!(report.missing.is_empty());
}

#[test]
fn mixed_real_and_unknown_entries_report_only_the_unknown() {
    let declared = vec![
        "turn_start".to_string(),
        "ui.setWidget".to_string(),
        "ui.notARealMethod".to_string(),
        "totally-made-up".to_string(),
    ];
    let report = validate_required_api(&declared);
    assert!(!report.is_satisfied());
    assert_eq!(
        report.missing,
        vec!["totally-made-up".to_string(), "ui.notARealMethod".to_string()],
        "missing entries are sorted and contain only the unknown names"
    );
    assert_eq!(report.declared.len(), 4, "all declared entries survive");
}

#[test]
fn unknown_entries_drive_a_clear_error() {
    let report = validate_required_api(&["ui.noSuchMethod".to_string()]);
    let display = format!("{report}");
    assert!(
        display.contains("ui.noSuchMethod"),
        "the error must surface the missing entry name; got {display}"
    );
    let report: Box<dyn std::error::Error> = Box::new(report);
    assert!(report.source().is_none(), "RequiredApiReport is its own root cause");
}

#[test]
fn stable_api_is_self_consistent() {
    // Defensive: every entry in STABLE_API must round-trip through
    // `is_stable` (a misspelled name would silently drop a method from
    // the manifest validator).
    for name in STABLE_API {
        assert!(is_stable(name), "{name} must round-trip through is_stable");
    }
    // The list must be sorted and de-duplicated — enforced by the
    // unit tests inside `api_surface`, but pinned here so a future
    // `pub const` shim that drops the sort still fails the contract
    // test.
    let mut sorted = STABLE_API.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, STABLE_API);
}

#[test]
fn required_api_report_is_cloneable_and_comparable() {
    let report: RequiredApiReport = validate_required_api(&["ui.setWidget".to_string()]);
    let cloned = report.clone();
    assert_eq!(report, cloned);
}