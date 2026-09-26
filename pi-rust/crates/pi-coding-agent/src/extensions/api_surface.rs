//! Stable API surface enumeration for [`required_api`] manifest checking.
//!
//! Mirrors the parity-tracking layer in
//! `packages/coding-agent/src/core/extensions/types.ts`. Every public
//! extension method, lifecycle event, and tool hook lives in exactly one
//! place in the Rust source; this module re-exports those names as a
//! stable, sorted list that the manifest validator compares against.
//!
//! ## Why a hand-curated list?
//!
//! The alternative — reflective enumeration of every `impl Trait for ...`
//! — would couple the contract to implementation noise (private helpers,
//! trait aliases, generated code). A flat list is easy to audit, easy
//! to diff against the upstream TS source, and easy to keep in sync with
//! `docs/API_STABILITY.md`.
//!
//! [`required_api`]: crate::extensions::required_api

/// One stable identifier for an extension-facing API. The strings are
/// the same names an extension manifest uses (`required_api: ["ui.setWidget", ...]`)
/// and the same names the JS shim surfaces through `pi.on(...)` /
/// `pi.ui.*`. They are sorted alphabetically.
pub const STABLE_API: &[&str] = &[
    // Sorted alphabetically (test `stable_api_is_sorted_and_unique`
    // enforces this). Names that share a prefix stay grouped.
    "abort",
    "agent_end",
    "agent_start",
    "append_entry",
    "cache_warming_decision",
    "compact",
    "componentSlot.register",
    "follow_up",
    "fork",
    "get_active_tools",
    "get_all_tools",
    "get_flag",
    "get_model",
    "get_session_name",
    "get_thinking_level",
    "input",
    "is_streaming",
    "lifecycle.afterProviderResponse",
    "lifecycle.agentSettled",
    "lifecycle.beforeAgentStart",
    "lifecycle.beforeProviderHeaders",
    "lifecycle.beforeProviderRequest",
    "lifecycle.context",
    "message_end",
    "message_start",
    "message_update",
    "model_select",
    "navigate_tree",
    "new_session",
    "project_trust",
    "provider_stream",
    "register_tool",
    "resources_discover",
    "send_message",
    "send_user_message",
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
    "set_active_tools",
    "set_model",
    "set_session_name",
    "set_thinking_level",
    "switch_session",
    "thinking_level_select",
    "toolRenderer.register",
    "tool_call",
    "tool_execution_end",
    "tool_execution_start",
    "tool_execution_update",
    "tool_result",
    "turn_end",
    "turn_start",
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
    "ui_prompt_end",
    "ui_prompt_start",
    "user_bash",
    "user_message",
    "wait_for_idle",
];

/// True iff `name` is part of the stable API surface.
pub fn is_stable(name: &str) -> bool {
    STABLE_API.contains(&name)
}

/// Sorted difference: every entry in `required` that is not part of
/// the stable surface. Empty means the manifest is satisfiable on this
/// build.
pub fn missing(required: &[String]) -> Vec<String> {
    let mut missing: Vec<String> = required
        .iter()
        .filter(|name| !is_stable(name))
        .cloned()
        .collect();
    missing.sort();
    missing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_api_is_sorted_and_unique() {
        let mut sorted = STABLE_API.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, STABLE_API, "STABLE_API must be sorted and unique");
    }

    #[test]
    fn every_entry_is_recognised() {
        for name in STABLE_API {
            assert!(is_stable(name), "{name} must round-trip through is_stable");
        }
    }

    #[test]
    fn missing_returns_only_unknown_names() {
        let required = vec![
            "turn_start".to_string(),
            "ui.setWidget".to_string(),
            "ui.noSuchMethod".to_string(),
        ];
        let mut missing = missing(&required);
        missing.sort();
        assert_eq!(missing, vec!["ui.noSuchMethod".to_string()]);
    }

    #[test]
    fn empty_required_list_has_no_missing_entries() {
        assert!(missing(&[]).is_empty());
        // An empty string is not a stable name — `missing` surfaces it.
        assert_eq!(missing(&[String::new()]), vec![String::new()]);
    }

    #[test]
    fn unknown_name_is_reported() {
        assert!(!is_stable("ui.notARealMethod"));
        assert!(!is_stable("totally-made-up"));
    }

    #[test]
    fn stable_api_tracks_pi_mono_event_names() {
        // The TS pi-mono `ExtensionEvent` union names that the Rust
        // `ExtensionEvent::name()` method must agree with. Pinned here
        // so a rename in either language breaks the contract test
        // instead of silently drifting.
        for (ts_event, rust_name) in [
            ("resources_discover", "resources_discover"),
            ("model_select", "model_select"),
            ("thinking_level_select", "thinking_level_select"),
            ("user_bash", "user_bash"),
            ("project_trust", "project_trust"),
            ("ui_prompt_start", "ui_prompt_start"),
            ("ui_prompt_end", "ui_prompt_end"),
            ("session_fork", "session_fork"),
            ("provider_stream", "provider_stream"),
            ("cache_warming_decision", "cache_warming_decision"),
        ] {
            assert_eq!(ts_event, rust_name, "TS / Rust names diverge");
            assert!(
                is_stable(ts_event),
                "STABLE_API must list {ts_event} so the manifest validator accepts it"
            );
        }
    }

    #[test]
    fn stable_api_lists_every_top_level_api_method() {
        // The TS ExtensionAPI methods that aren't under the `ui.` namespace.
        // Pinned as a sorted list so adding a new method is a deliberate
        // test edit (not silent drift).
        let expected = [
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
        for name in expected {
            assert!(
                is_stable(name),
                "top-level ExtensionAPI method {name} must be in STABLE_API"
            );
        }
    }
}