//! Port of the `splitDeferredTools` unit vectors from
//! `packages/ai/test/deferred-tools.test.ts`.
//!
//! Upstream drives the split through the provider payloads; this port has no
//! wiring yet (`Compat::supports_additional_tools` does not exist), so the
//! util is pinned directly. Most vectors hand the transcript's `addedToolNames`
//! to [`split_deferred_tools`] as a slice; the `*_from_context` vectors below
//! instead carry them on `pi_protocol::ToolResult::added_tool_names` and let
//! [`split_deferred_tools_from_context`] read them back out.

use pi_ai::utils::deferred_tools::{
    added_tool_names_from_messages, identity_tool_name, split_deferred_tools,
    split_deferred_tools_from_context,
};
use pi_protocol::{Content, Context, Message, Role, ToolCall, ToolDefinition};

fn tool(name: &str, description: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        label: name.into(),
        description: description.into(),
        parameters: serde_json::json!({"type": "object"}),
        metadata: None,
    }
}

fn user(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![Content::text(text)],
        model: None,
    }
}

fn assistant_tool_call(name: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![Content::ToolCall(ToolCall {
            id: "call_1".into(),
            name: name.into(),
            arguments: serde_json::json!({}),
        })],
        model: None,
    }
}

fn tool_result_message(text: &str) -> Message {
    tool_result_message_with_added(text, None)
}

/// A tool result that advertises the deferred tools it just loaded.
fn tool_result_message_with_added(text: &str, added: Option<Vec<&str>>) -> Message {
    Message {
        role: Role::Tool,
        content: vec![Content::ToolResult(pi_protocol::ToolResult {
            tool_call_id: "call_1".into(),
            content: Box::new(Content::text(text)),
            is_error: false,
            details: None,
            added_tool_names: added.map(|names| names.into_iter().map(String::from).collect()),
        })],
        model: None,
    }
}

/// Upstream `makeContext(tools, addedToolNames)`: user -> assistant tool call
/// -> tool result -> user. The added names travel as a separate argument
/// because the Rust tool result has no `addedToolNames` field.
fn context(tools: Vec<ToolDefinition>) -> Context {
    Context {
        system_prompt: String::new(),
        messages: vec![
            user("Hello"),
            assistant_tool_call("base_tool"),
            tool_result_message("done"),
            user("next"),
        ],
        tools,
    }
}

fn immediate_names(split: &pi_ai::utils::deferred_tools::SplitDeferredTools<'_>) -> Vec<String> {
    split
        .immediate
        .iter()
        .map(|tool| tool.name.clone())
        .collect()
}

fn deferred_names(split: &pi_ai::utils::deferred_tools::SplitDeferredTools<'_>) -> Vec<String> {
    split
        .deferred
        .iter()
        .map(|(name, _)| name.clone())
        .collect()
}

#[test]
fn defers_a_tool_marked_by_the_transcript() {
    let context = context(vec![tool("base_tool", "base"), tool("late_tool", "late")]);

    let split = split_deferred_tools(
        &context,
        true,
        &["late_tool".to_string()],
        identity_tool_name,
    );

    assert_eq!(immediate_names(&split), vec!["base_tool"]);
    assert_eq!(deferred_names(&split), vec!["late_tool"]);
    assert_eq!(split.deferred[0].1.description, "late");
}

#[test]
fn returns_everything_immediately_when_deferred_loading_is_disabled() {
    let context = context(vec![tool("base_tool", "base"), tool("late_tool", "late")]);

    let split = split_deferred_tools(
        &context,
        false,
        &["late_tool".to_string()],
        identity_tool_name,
    );

    assert_eq!(immediate_names(&split), vec!["base_tool", "late_tool"]);
    assert!(split.deferred.is_empty());
}

#[test]
fn keeps_a_tool_immediate_when_the_assistant_already_called_it() {
    let mut context = context(vec![tool("base_tool", "base"), tool("late_tool", "late")]);
    context.messages[1] = assistant_tool_call("late_tool");

    let split = split_deferred_tools(
        &context,
        true,
        &["late_tool".to_string()],
        identity_tool_name,
    );

    assert_eq!(immediate_names(&split), vec!["base_tool", "late_tool"]);
    assert!(split.deferred.is_empty());
}

#[test]
fn ignores_added_names_with_no_matching_tool() {
    let context = context(vec![tool("base_tool", "base")]);

    let split = split_deferred_tools(
        &context,
        true,
        &["late_tool".to_string()],
        identity_tool_name,
    );

    assert_eq!(immediate_names(&split), vec!["base_tool"]);
    assert!(split.deferred.is_empty());
}

#[test]
fn deduplicates_tools_by_normalized_name_keeping_the_last_definition() {
    // Upstream's `uniqueTools` Map: the first occurrence fixes the position,
    // the later definition wins.
    let context = Context {
        system_prompt: String::new(),
        messages: vec![user("hi")],
        tools: vec![
            tool("read", "lowercase definition"),
            tool("base_tool", "base"),
            tool("Read", "canonical definition"),
        ],
    };

    let split = split_deferred_tools(&context, true, &[], |name: &str| name.to_lowercase());

    // First occurrence fixes the position (`read` was seen first), the later
    // definition wins.
    assert_eq!(immediate_names(&split), vec!["Read", "base_tool"]);
    assert_eq!(split.immediate[0].description, "canonical definition");
    assert!(split.deferred.is_empty());
}

#[test]
fn normalizes_names_before_checking_prior_tool_usage() {
    // Upstream "normalizes OAuth names before checking prior tool usage": the
    // active tool is `read`, the assistant called `Read`, the marker says
    // `read` — the normalized names match, so nothing is deferred.
    let context = Context {
        system_prompt: String::new(),
        messages: vec![user("hi"), assistant_tool_call("Read")],
        tools: vec![tool("base_tool", "base"), tool("read", "read")],
    };

    let split = split_deferred_tools(&context, true, &["read".to_string()], |name: &str| {
        name.to_lowercase()
    });

    assert_eq!(immediate_names(&split), vec!["base_tool", "read"]);
    assert!(split.deferred.is_empty());
}

#[test]
fn matches_a_normalized_marker_to_an_active_tool() {
    let context = Context {
        system_prompt: String::new(),
        messages: vec![user("hi"), assistant_tool_call("base_tool")],
        tools: vec![tool("base_tool", "base"), tool("read", "read")],
    };

    let split = split_deferred_tools(&context, true, &["Read".to_string()], |name: &str| {
        name.to_lowercase()
    });

    assert_eq!(immediate_names(&split), vec!["base_tool"]);
    assert_eq!(deferred_names(&split), vec!["read"]);
}

#[test]
fn deduplicates_repeated_added_names() {
    let context = context(vec![tool("base_tool", "base"), tool("late_tool", "late")]);

    let split = split_deferred_tools(
        &context,
        true,
        &["late_tool".to_string(), "late_tool".to_string()],
        identity_tool_name,
    );

    assert_eq!(immediate_names(&split), vec!["base_tool"]);
    assert_eq!(deferred_names(&split), vec!["late_tool"]);
}

#[test]
fn keeps_the_original_tool_order_for_immediate_tools() {
    let context = Context {
        system_prompt: String::new(),
        messages: vec![user("hi")],
        tools: vec![
            tool("a", "a"),
            tool("b", "b"),
            tool("c", "c"),
            tool("d", "d"),
        ],
    };

    let split = split_deferred_tools(&context, true, &[], identity_tool_name);

    assert_eq!(immediate_names(&split), vec!["a", "b", "c", "d"]);
}

// ---------------------------------------------------------------------------
// Transcript-driven entry point: `added_tool_names` on the real protocol field.
// ---------------------------------------------------------------------------

/// The context-shaped entry point reads `added_tool_names` off the transcript,
/// like upstream's `getDeferredToolNames`.
#[test]
fn reads_added_names_from_the_transcript() {
    let mut context = context(vec![tool("base_tool", "base"), tool("late_tool", "late")]);
    context.messages[2] = tool_result_message_with_added("done", Some(vec!["late_tool"]));

    let split = split_deferred_tools_from_context(&context, true, identity_tool_name);

    assert_eq!(immediate_names(&split), vec!["base_tool"]);
    assert_eq!(deferred_names(&split), vec!["late_tool"]);
}

/// Names are collected in first-seen order and de-duplicated, and only
/// `Role::Tool` messages contribute.
#[test]
fn extracts_added_names_in_order_without_duplicates() {
    let messages = vec![
        user("hi"),
        tool_result_message_with_added("first", Some(vec!["b", "a"])),
        assistant_tool_call("a"),
        tool_result_message_with_added("second", Some(vec!["a", "c"])),
        tool_result_message("third"),
    ];

    assert_eq!(
        added_tool_names_from_messages(&messages),
        vec!["b", "a", "c"]
    );
}

/// With no transcript marker the split defers nothing, even with the
/// context-shaped entry point.
#[test]
fn adds_nothing_when_no_result_advertises_tools() {
    let context = context(vec![tool("base_tool", "base"), tool("late_tool", "late")]);

    let split = split_deferred_tools_from_context(&context, true, identity_tool_name);

    assert_eq!(immediate_names(&split), vec!["base_tool", "late_tool"]);
    assert!(split.deferred.is_empty());
}
