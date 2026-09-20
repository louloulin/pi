//! Port of `packages/ai/test/context-estimate.test.ts` plus the estimate
//! assertions from `packages/ai/test/deferred-tools.test.ts`.
//!
//! # Adaptations
//!
//! Upstream's two `context-estimate` vectors exercise the *usage scan*: a
//! compaction summary inserted after an assistant message invalidates its
//! usage, and a response to that inserted context makes usage applicable
//! again. Rust `Message` rows carry no `timestamp` / `usage` / `stopReason`,
//! so there is nothing to scan and nothing to invalidate — the caller passes
//! the applicable usage as a [`UsageAnchor`]. These tests therefore pin the
//! anchor semantics (hit / miss / out-of-range) instead of the scan.
//!
//! The "counts definitions marked after the latest usage checkpoint" vector is
//! likewise unreachable: it relies on `toolResult.addedToolNames`, which the
//! Rust protocol does not carry (see `pi_ai::utils::deferred_tools`).

use pi_ai::utils::estimate::{
    calculate_context_tokens, estimate_context_usage, estimate_message_tokens,
    estimate_messages_tokens, estimate_text_and_image_content_tokens, estimate_text_tokens,
    estimate_tools_tokens, ContextUsageEstimate, UsageAnchor, CHARS_PER_TOKEN,
    ESTIMATED_IMAGE_CHARS,
};
use pi_protocol::{
    Content, Context, ImageContent, Message, Role, ToolCall, ToolDefinition, ToolResult, Usage,
};

fn user(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![Content::text(text)],
        model: None,
    }
}

fn assistant(blocks: Vec<Content>) -> Message {
    Message {
        role: Role::Assistant,
        content: blocks,
        model: None,
    }
}

fn tool_result_message(text: &str) -> Message {
    Message {
        role: Role::Tool,
        content: vec![Content::ToolResult(ToolResult {
            tool_call_id: "call_1".into(),
            content: Box::new(Content::text(text)),
            is_error: false,
            details: None,
        })],
        model: None,
    }
}

fn tool_call(name: &str, arguments: serde_json::Value) -> Content {
    Content::ToolCall(ToolCall {
        id: "call_1".into(),
        name: name.into(),
        arguments,
    })
}

fn image() -> Content {
    Content::Image(ImageContent {
        mime_type: "image/png".into(),
        data: "aW1hZ2U=".into(),
    })
}

fn tool(name: &str, description: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        label: name.into(),
        description: description.into(),
        parameters: serde_json::json!({"type": "object"}),
        metadata: None,
    }
}

fn usage_of_total(total: u32) -> Usage {
    Usage {
        input: total,
        total,
        ..Usage::default()
    }
}

#[test]
fn estimates_text_tokens_with_chars_over_four_rounding_up() {
    assert_eq!(CHARS_PER_TOKEN, 4);
    assert_eq!(estimate_text_tokens(""), 0);
    assert_eq!(estimate_text_tokens("abcd"), 1);
    assert_eq!(estimate_text_tokens("abcde"), 2);
    // 9 / 4 -> 3, the same direction as the compaction call sites used.
    assert_eq!(estimate_text_tokens(&"a".repeat(9)), 3);
}

#[test]
fn charges_every_image_a_fixed_character_budget() {
    assert_eq!(ESTIMATED_IMAGE_CHARS, 4800);
    // 3 text chars + one image -> (3 + 4800) / 4 rounded up.
    assert_eq!(
        estimate_text_and_image_content_tokens(&[Content::text("abc"), image()]),
        (3 + 4800usize).div_ceil(4) as u32
    );
    assert_eq!(
        estimate_message_tokens(&user("abcd")),
        1,
        "plain user text still uses chars/4"
    );
    let mixed = Message {
        role: Role::User,
        content: vec![Content::text("abcd"), image()],
        model: None,
    };
    assert_eq!(estimate_message_tokens(&mixed), 1201);
    let image_only = Message {
        role: Role::User,
        content: vec![image()],
        model: None,
    };
    assert_eq!(estimate_message_tokens(&image_only), 1200);
}

#[test]
fn estimates_message_tokens_per_role() {
    // user: one rounding over the message's characters.
    assert_eq!(estimate_message_tokens(&user("hello")), 2);

    // tool result: the inner text block is what counts.
    assert_eq!(estimate_message_tokens(&tool_result_message("abcd")), 1);
    assert_eq!(estimate_message_tokens(&tool_result_message("abcde")), 2);

    // assistant: text + toolCall (name + JSON arguments), one rounding.
    let arguments = serde_json::json!({"a": 1});
    let json_len = serde_json::to_string(&arguments).unwrap().chars().count();
    let expected = ("abcd".len() + "get".len() + json_len).div_ceil(4) as u32;
    assert_eq!(
        estimate_message_tokens(&assistant(vec![
            Content::text("abcd"),
            tool_call("get", arguments),
        ])),
        expected
    );
}

#[test]
fn sums_message_estimates_without_extra_rounding() {
    let messages = vec![user("hello"), assistant(vec![Content::text("world")])];
    assert_eq!(
        estimate_messages_tokens(&messages),
        estimate_message_tokens(&messages[0]) + estimate_message_tokens(&messages[1])
    );
    assert_eq!(estimate_messages_tokens(&[]), 0);
}

#[test]
fn calculates_context_tokens_from_total_or_components() {
    let with_total = Usage {
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 4,
        total: 99,
    };
    assert_eq!(calculate_context_tokens(&with_total), 99);

    let without_total = Usage {
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 4,
        total: 0,
    };
    assert_eq!(calculate_context_tokens(&without_total), 10);
}

#[test]
fn estimates_tools_tokens_from_the_serialized_definitions() {
    assert_eq!(estimate_tools_tokens(&[]), 0);
    let tools = vec![tool("base_tool", "The base tool")];
    let json = serde_json::to_string(&tools).unwrap();
    assert_eq!(estimate_tools_tokens(&tools), estimate_text_tokens(&json));
    assert!(estimate_tools_tokens(&tools) > 0);
}

#[test]
fn context_usage_prefers_the_anchored_usage_and_counts_the_trailing_tail() {
    // Upstream's "uses assistant usage again after a response to the inserted
    // context" shape, with the usage anchor standing in for the scan.
    let context = Context {
        system_prompt: "system".into(),
        messages: vec![
            user("summary"),
            assistant(vec![Content::text("kept")]),
            user(&"x".repeat(4000)),
        ],
        tools: vec![],
    };

    let estimate = estimate_context_usage(
        &context,
        Some(UsageAnchor {
            usage: &usage_of_total(9_500),
            index: 1,
        }),
    );

    assert_eq!(
        estimate,
        ContextUsageEstimate {
            tokens: 10_500,
            usage_tokens: 9_500,
            trailing_tokens: 1_000,
            last_usage_index: Some(1),
        }
    );
}

#[test]
fn context_usage_without_an_anchor_adds_the_system_prompt_and_tools() {
    // Upstream's "ignores stale assistant usage" shape: with no applicable
    // usage the estimate is the pure walk plus the request prefix.
    let context = Context {
        system_prompt: "system".into(),
        messages: vec![user("summary"), user(&"x".repeat(4000))],
        tools: vec![tool("base_tool", "The base tool")],
    };

    let estimate = estimate_context_usage(&context, None);

    let messages_tokens = estimate_messages_tokens(&context.messages);
    let prefix_tokens = estimate_text_tokens("system") + estimate_tools_tokens(&context.tools);
    assert_eq!(
        estimate,
        ContextUsageEstimate {
            tokens: messages_tokens + prefix_tokens,
            usage_tokens: 0,
            trailing_tokens: messages_tokens + prefix_tokens,
            last_usage_index: None,
        }
    );
}

#[test]
fn context_usage_with_an_anchor_at_the_last_message_has_no_trailing_tokens() {
    let context = Context {
        system_prompt: "system".into(),
        messages: vec![user("summary"), assistant(vec![Content::text("kept")])],
        tools: vec![],
    };

    let estimate = estimate_context_usage(
        &context,
        Some(UsageAnchor {
            usage: &usage_of_total(100),
            index: 1,
        }),
    );

    assert_eq!(estimate.usage_tokens, 100);
    assert_eq!(estimate.trailing_tokens, 0);
    assert_eq!(estimate.tokens, 100);
    assert_eq!(estimate.last_usage_index, Some(1));
}

#[test]
fn context_usage_with_an_out_of_range_anchor_degrades_to_no_trailing() {
    let context = Context {
        system_prompt: String::new(),
        messages: vec![user("summary")],
        tools: vec![],
    };

    let estimate = estimate_context_usage(
        &context,
        Some(UsageAnchor {
            usage: &usage_of_total(42),
            index: 99,
        }),
    );

    assert_eq!(estimate.usage_tokens, 42);
    assert_eq!(estimate.trailing_tokens, 0);
    assert_eq!(estimate.tokens, 42);
    assert_eq!(estimate.last_usage_index, Some(99));
}
