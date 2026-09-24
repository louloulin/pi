//! Upstream event-name table shared by the host and the JS shim.
//!
//! The plugin-facing API is `pi.on("<name>", handler)`. That name is what the
//! shim keys its handler map on, and what the host compares against when it
//! decides whether an event has any subscriber
//! ([`ExtensionRuntime::has_subscriber_for`](crate::ExtensionRuntime::has_subscriber_for)).
//! Two names were historical exceptions:
//!
//! * `session_end` — Stage 3 shipped the tag before the upstream name was
//!   settled; it is an alias of `session_shutdown`.
//! * `user_message` — a Rust-native tag upstream does not have.
//!
//! Keeping the list here (instead of only in the shim) lets a test assert the
//! two sides agree, which is the same "the doc and the code cannot drift"
//! pattern `pi-rust/scripts/extension_event_coverage.py --check-doc` uses for
//! the parity document.

/// Every event name a plugin may pass to `pi.on`, in upstream declaration
/// order (`packages/coding-agent/src/core/extensions/types.ts:1257-1301`).
///
/// Mirrors the `UPSTREAM_EVENT_NAMES` array in `runtime/pi-ext-shim.mjs`; a
/// unit test fails when the two drift.
pub const UPSTREAM_EVENT_NAMES: [&str; 37] = [
    "project_trust",
    "resources_discover",
    "session_start",
    "session_info_changed",
    "session_before_switch",
    "session_before_fork",
    "session_fork",
    "session_before_compact",
    "session_compact",
    "session_compact_failed",
    "session_shutdown",
    "session_before_tree",
    "session_tree",
    "context",
    "before_provider_request",
    "before_provider_headers",
    "after_provider_response",
    "before_agent_start",
    "agent_start",
    "agent_end",
    "agent_settled",
    "ui_prompt_start",
    "ui_prompt_end",
    "turn_start",
    "turn_end",
    "message_start",
    "message_update",
    "message_end",
    "tool_execution_start",
    "tool_execution_update",
    "tool_execution_end",
    "model_select",
    "thinking_level_select",
    "tool_call",
    "tool_result",
    "user_bash",
    "input",
];

/// Tags the Rust port added that upstream has no equivalent for.
pub const RUST_ONLY_EVENT_NAMES: [&str; 1] = ["user_message"];

/// Name aliases the shim and the host both resolve: the key is what a plugin
/// may write, the value is the canonical name the host emits.
pub const EVENT_ALIASES: [(&str, &str); 1] = [("session_end", "session_shutdown")];

/// Resolve a plugin-supplied event name to the canonical one.
///
/// Both `pi.on` and the host's `has_subscriber_for` go through this, so a
/// subscription and a delivery can never disagree about which key they use.
pub fn canonical_event_name(name: &str) -> &str {
    match EVENT_ALIASES.iter().find(|(alias, _)| *alias == name) {
        Some((_, canonical)) => canonical,
        None => name,
    }
}

/// Whether `name` is an event the host can deliver (upstream name, Rust-only
/// name, or alias).
pub fn is_known_event_name(name: &str) -> bool {
    let canonical = canonical_event_name(name);
    UPSTREAM_EVENT_NAMES.contains(&canonical) || RUST_ONLY_EVENT_NAMES.contains(&canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shim::SHIM_SOURCE;

    /// The names printed inside the shim's `UPSTREAM_EVENT_NAMES` array.
    fn shim_event_names() -> Vec<String> {
        let start = SHIM_SOURCE
            .find("const UPSTREAM_EVENT_NAMES = Object.freeze([")
            .expect("shim must declare UPSTREAM_EVENT_NAMES");
        let rest = &SHIM_SOURCE[start..];
        let end = rest.find("]);").expect("array must be closed");
        rest[..end]
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                let inner = line.strip_prefix('"')?;
                let (name, _) = inner.split_once('"')?;
                Some(name.to_string())
            })
            .collect()
    }

    #[test]
    fn shim_and_rust_event_tables_agree() {
        let shim = shim_event_names();
        assert_eq!(
            shim.len(),
            UPSTREAM_EVENT_NAMES.len(),
            "shim lists {} names, Rust lists {}: {shim:?}",
            shim.len(),
            UPSTREAM_EVENT_NAMES.len()
        );
        for (index, (shim_name, rust_name)) in
            shim.iter().zip(UPSTREAM_EVENT_NAMES.iter()).enumerate()
        {
            assert_eq!(
                shim_name, rust_name,
                "name {index} differs: shim `{shim_name}` vs Rust `{rust_name}`"
            );
        }
    }

    #[test]
    fn aliases_resolve_and_are_known() {
        assert_eq!(canonical_event_name("session_end"), "session_shutdown");
        assert_eq!(canonical_event_name("session_start"), "session_start");
        assert!(is_known_event_name("session_end"));
        assert!(is_known_event_name("user_message"));
        assert!(!is_known_event_name("not_an_event"));
    }
}
