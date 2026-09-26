//! Parity audit: every wire tag emitted by `ExtensionEvent::name()` must
//! have a counterpart in the TypeScript `ExtensionEvent` union
//! (`packages/coding-agent/src/core/extensions/types.ts`).
//!
//! Plan §8.1 calls this test "ts_event_parity.rs — 40+ ExtensionEvent
//! variants match TS string literals exactly". This file is the Rust
//! counterpart: it enumerates the canonical TS event tags (kept in sync
//! by reading the TS union), then walks `ExtensionEvent::name()` for
//! every variant the Rust enum declares and asserts:
//!
//! - Every wire tag Rust emits is one the TS union knows about, **or**
//!   is in the documented `RUST_EXTENSIONS` list (the Rust-native events
//!   that have no TS counterpart yet: `user_message`, `session_fork`,
//!   `session_compact_failed`, `ui_prompt_start`, `ui_prompt_end`).
//! - Every TS event tag has a Rust variant that produces it.
//!
//! What the test deliberately does NOT assert:
//! - Payload field names. The Rust enum carries `serde(rename = "...")`
//!   annotations on every field; an audit of those lives in
//!   `extension_event_payload_parity.rs` (future work) so this file
//!   stays cheap to run.

#![cfg(test)]

use pi_protocol::ExtensionEvent;
use pi_protocol::{CompactReason, Content, ForkPosition, InputSource, Message, ModelSelectSource};
use pi_protocol::{ResourcesDiscoverReason, Role, SessionBeforeSwitchReason, SessionShutdownReason};
use pi_protocol::{ToolResult, UiPromptKind};

use std::collections::HashSet;

/// Canonical TS event wire tags the port must mirror.
///
/// Source: `packages/coding-agent/src/core/extensions/types.ts:1034-1059`
/// (the `ExtensionEvent` union) plus the snake_case form of every
/// nested event interface (`*Event` suffix stripped, PascalCase →
/// snake_case).
///
/// Kept sorted so a diff against the Rust surface is scannable.
const TS_EVENT_TAGS: &[&str] = &[
    "after_provider_response",
    "agent_end",
    "agent_settled",
    "agent_start",
    "before_agent_start",
    "before_provider_headers",
    "before_provider_request",
    "context",
    "input",
    "message_end",
    "message_start",
    "message_update",
    "model_select",
    "project_trust",
    "resources_discover",
    "session_before_compact",
    "session_before_fork",
    "session_before_switch",
    "session_before_tree",
    "session_compact",
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
    "user_bash",
];

/// Rust-native event tags with no TS counterpart.
///
/// These are documented additions: `user_message` lets the Rust loop
/// hand the JS side a typed `Message` before the agent processes it;
/// `session_fork` is the success-side sibling of `session_before_fork`;
/// `session_compact_failed` carries the abort / error distinction TS
/// folds into a single `session_compact` event; `ui_prompt_start` /
/// `ui_prompt_end` are the visibility pair for blocking extension UI
/// prompts. They are *additions*, not gaps — they cannot be removed
/// without breaking Rust-native plugins.
const RUST_EXTENSIONS: &[&str] = &[
    "session_compact_failed",
    "session_fork",
    "ui_prompt_end",
    "ui_prompt_start",
    "user_message",
];

fn empty_user_message() -> Message {
    Message {
        role: Role::User,
        content: Vec::new(),
        model: None,
    }
}

fn empty_tool_result() -> ToolResult {
    ToolResult::default()
}

fn rust_event_tags() -> Vec<&'static str> {
    // The full set of wire tags `ExtensionEvent::name()` can return.
    // The `Default` impl produces an event with empty fields, but every
    // variant has a stable wire tag, so listing every variant by name
    // here is the cheapest way to enumerate them.
    [
        ExtensionEvent::SessionStart,
        ExtensionEvent::UserMessage {
            message: empty_user_message(),
        },
        ExtensionEvent::ToolCall {
            tool_call_id: String::new(),
            tool_name: String::new(),
            input: serde_json::Value::Null,
        },
        ExtensionEvent::ToolResult {
            tool_call_id: String::new(),
            tool_name: String::new(),
            input: serde_json::Value::Null,
            content: Vec::<Content>::new(),
            is_error: false,
            details: None,
        },
        ExtensionEvent::ResourcesDiscover {
            cwd: String::new(),
            reason: ResourcesDiscoverReason::Startup,
        },
        ExtensionEvent::SessionShutdown {
            reason: SessionShutdownReason::Quit,
            target_session_file: None,
        },
        ExtensionEvent::SessionInfoChanged { name: None },
        ExtensionEvent::SessionCompact {
            reason: CompactReason::Manual,
            tokens_before: 0,
            tokens_after: 0,
            retained: 0,
        },
        ExtensionEvent::SessionBeforeSwitch {
            reason: SessionBeforeSwitchReason::New,
            target_session_file: None,
        },
        ExtensionEvent::SessionBeforeFork {
            entry_id: String::new(),
            position: ForkPosition::Before,
        },
        ExtensionEvent::SessionFork {
            session_id: String::new(),
            parent_id: String::new(),
            entry_id: String::new(),
        },
        ExtensionEvent::SessionBeforeCompact {
            reason: CompactReason::Manual,
            will_retry: false,
            custom_instructions: None,
        },
        ExtensionEvent::SessionCompactFailed {
            reason: CompactReason::Manual,
            error_message: None,
            aborted: false,
            will_retry: false,
            from_extension: false,
        },
        ExtensionEvent::SessionBeforeTree {
            target_id: String::new(),
            old_leaf_id: None,
            user_wants_summary: false,
            custom_instructions: None,
        },
        ExtensionEvent::SessionTree {
            new_leaf_id: None,
            old_leaf_id: None,
            from_extension: false,
        },
        ExtensionEvent::AgentStart,
        ExtensionEvent::AgentEnd { messages: Vec::new() },
        ExtensionEvent::TurnStart {
            turn_index: 0,
            timestamp: 0,
        },
        ExtensionEvent::TurnEnd {
            turn_index: 0,
            message: empty_user_message(),
            tool_results: Vec::new(),
        },
        ExtensionEvent::MessageStart {
            message: empty_user_message(),
        },
        ExtensionEvent::MessageUpdate {
            assistant_message_event: serde_json::Value::Null,
        },
        ExtensionEvent::MessageEnd {
            message: empty_user_message(),
        },
        ExtensionEvent::Context {
            messages: Vec::new(),
        },
        ExtensionEvent::BeforeProviderRequest {
            payload: serde_json::Value::Null,
        },
        ExtensionEvent::BeforeProviderHeaders {
            headers: serde_json::Map::new(),
        },
        ExtensionEvent::AfterProviderResponse {
            status: 0,
            headers: serde_json::Map::new(),
        },
        ExtensionEvent::BeforeAgentStart {
            prompt: String::new(),
            system_prompt: String::new(),
        },
        ExtensionEvent::AgentSettled,
        ExtensionEvent::ToolExecutionStart {
            tool_call_id: String::new(),
            tool_name: String::new(),
            args: serde_json::Value::Null,
        },
        ExtensionEvent::ToolExecutionUpdate {
            tool_call_id: String::new(),
            tool_name: String::new(),
            args: serde_json::Value::Null,
            partial_result: String::new(),
        },
        ExtensionEvent::ToolExecutionEnd {
            tool_call_id: String::new(),
            tool_name: String::new(),
            result: empty_tool_result(),
            is_error: false,
        },
        ExtensionEvent::ModelSelect {
            model: String::new(),
            previous_model: None,
            source: ModelSelectSource::Set,
        },
        ExtensionEvent::ThinkingLevelSelect {
            level: String::new(),
            previous_level: String::new(),
        },
        ExtensionEvent::UserBash {
            command: String::new(),
            exclude_from_context: false,
            cwd: String::new(),
        },
        ExtensionEvent::Input {
            text: String::new(),
            source: InputSource::Interactive,
        },
        ExtensionEvent::ProjectTrust { cwd: String::new() },
        ExtensionEvent::UiPromptStart {
            kind: UiPromptKind::Confirm,
            title: None,
        },
        ExtensionEvent::UiPromptEnd {
            kind: UiPromptKind::Confirm,
            title: None,
        },
    ]
    .iter()
    .map(|event| event.name())
    .collect()
}

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
fn canonical_lists_are_sorted_and_unique() {
    assert_sorted_unique(TS_EVENT_TAGS, "TS_EVENT_TAGS");
    assert_sorted_unique(RUST_EXTENSIONS, "RUST_EXTENSIONS");
}

#[test]
fn ts_and_rust_extensions_do_not_overlap() {
    let ts: HashSet<&str> = TS_EVENT_TAGS.iter().copied().collect();
    let rust: HashSet<&str> = RUST_EXTENSIONS.iter().copied().collect();
    let overlap: Vec<&&str> = ts.intersection(&rust).collect();
    assert!(
        overlap.is_empty(),
        "an event tag appears in both TS_EVENT_TAGS and RUST_EXTENSIONS: {overlap:?} \
         — pick one side",
    );
}

#[test]
fn every_ts_event_tag_has_a_rust_variant() {
    let rust = rust_event_tags();
    let rust_set: HashSet<&str> = rust.iter().copied().collect();
    let ts_set: HashSet<&str> = TS_EVENT_TAGS.iter().copied().collect();
    let missing: Vec<&&str> = ts_set.difference(&rust_set).collect();
    assert!(
        missing.is_empty(),
        "TS event tags with no Rust variant: {missing:?} — add the variant \
         to ExtensionEvent or move the tag from TS_EVENT_TAGS to RUST_EXTENSIONS",
    );
}

#[test]
fn every_rust_event_tag_is_in_canonical_lists() {
    let rust = rust_event_tags();
    let canonical: HashSet<&str> = TS_EVENT_TAGS
        .iter()
        .chain(RUST_EXTENSIONS.iter())
        .copied()
        .collect();
    let unexpected: Vec<&&str> = rust
        .iter()
        .filter(|tag| !canonical.contains(*tag))
        .collect();
    assert!(
        unexpected.is_empty(),
        "Rust emits event tags not listed in either TS_EVENT_TAGS or \
         RUST_EXTENSIONS: {unexpected:?} — add them to one of the two lists",
    );
}

#[test]
fn no_duplicate_wire_tags() {
    let mut seen: HashSet<&str> = HashSet::new();
    let rust = rust_event_tags();
    let mut dupes: Vec<&str> = Vec::new();
    for tag in &rust {
        if !seen.insert(*tag) {
            dupes.push(*tag);
        }
    }
    assert!(
        dupes.is_empty(),
        "ExtensionEvent has variants that emit the same wire tag: {dupes:?} \
         — pick one variant per tag and delete the rest",
    );
}
