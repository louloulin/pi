//! Context-size estimation — port of `packages/ai/src/utils/estimate.ts`.
//!
//! The chars/4 heuristic sizes the current context: `pi-coding-agent` uses it
//! for the compaction threshold and for `find_cut_point`, and this module is
//! the single source of the arithmetic for both.
//!
//! # Deliberate differences from the TypeScript implementation
//!
//! * **Usage is supplied by the caller.** Upstream scans the message list for
//!   the newest assistant message that carries a usable `usage` block. Rust
//!   [`Message`] rows carry no `usage`, `timestamp` or `stopReason`, so the
//!   caller passes the applicable usage in as a [`UsageAnchor`] (the
//!   provider-usage counterpart of upstream's `{ usage, index }`). With no
//!   anchor available the estimate is the pure size walk, which is what
//!   [`estimate_context_usage`] does with `anchor == None`.
//! * **No `stopReason` / timestamp guards.** Upstream refuses usage from an
//!   `aborted` / `error` turn and from an assistant message older than the
//!   newest prefix message (a compaction summary inserted later). Neither
//!   signal exists on the Rust message type, so the caller owns that decision:
//!   `pi-coding-agent` only has a `TurnUsage` for a turn that finished, and it
//!   passes `None` for a zero-usage turn.
//! * **No transcript-added tool tokens (yet).** Upstream adds the tokens of
//!   tools named by `toolResult.addedToolNames` when usage exists.
//!   `pi_protocol::ToolResult::added_tool_names` now carries that list (see
//!   [`crate::utils::deferred_tools`]), but this util still skips the
//!   adjustment: wiring it belongs with the deferred-tools provider stage.
//!   The system-prompt + tool tokens of the no-usage path are unaffected.
//! * **Characters are Unicode scalar values**, not JavaScript UTF-16 code
//!   units: `str::chars().count()`. That is the metric
//!   `pi-coding-agent::compaction` already used, so sharing this module keeps
//!   the existing compaction numbers and its tests unchanged. For text
//!   outside the BMP (emoji) it counts one where JS counts two, which slightly
//!   *under*-estimates those strings.

use pi_protocol::{Content, Context, Message, ToolDefinition, Usage};

/// Characters charged per estimated token (`CHARS_PER_TOKEN`).
pub const CHARS_PER_TOKEN: usize = 4;

/// Characters charged for an image block, regardless of its encoded size
/// (`ESTIMATED_IMAGE_CHARS`).
pub const ESTIMATED_IMAGE_CHARS: usize = 4800;

/// `ContextUsageEstimate` — token estimate plus the usage it was anchored on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContextUsageEstimate {
    /// Estimated total context tokens.
    pub tokens: u32,
    /// Tokens reported by the most recent applicable assistant usage block.
    pub usage_tokens: u32,
    /// Estimated tokens after the most recent applicable assistant usage block.
    pub trailing_tokens: u32,
    /// Index of the applicable message that provided usage, or `None` when
    /// none exists.
    pub last_usage_index: Option<usize>,
}

/// Provider usage attributed to the message at [`UsageAnchor::index`].
///
/// The Rust stand-in for the `{ usage, index }` pair upstream derives by
/// scanning the message list for the newest usable assistant usage.
#[derive(Debug, Clone, Copy)]
pub struct UsageAnchor<'a> {
    /// Usage reported by the assistant turn at [`UsageAnchor::index`].
    pub usage: &'a Usage,
    /// Index of that assistant message in the message list. Messages after it
    /// are the trailing estimate.
    pub index: usize,
}

/// `calculateContextTokens(usage)` — prefer the provider's total.
pub fn calculate_context_tokens(usage: &Usage) -> u32 {
    if usage.total > 0 {
        usage.total
    } else {
        usage.input + usage.output + usage.cache_read + usage.cache_write
    }
}

/// `estimateTextTokens(text)` — `ceil(chars / CHARS_PER_TOKEN)`.
pub fn estimate_text_tokens(text: &str) -> u32 {
    text.chars().count().div_ceil(CHARS_PER_TOKEN) as u32
}

/// `estimateTextAndImageContentTokens(content)` — text by its length, every
/// image by [`ESTIMATED_IMAGE_CHARS`], then one `chars/4` rounding.
///
/// Upstream also accepts a bare string; Rust message content is always a block
/// list, so only the block form exists (the tool-result variant recurses into
/// its inner content exactly like [`estimate_message_tokens`]).
pub fn estimate_text_and_image_content_tokens(content: &[Content]) -> u32 {
    content_chars(content).div_ceil(CHARS_PER_TOKEN) as u32
}

/// `estimateMessageTokens(message)` — one `chars/4` rounding per message.
///
/// Every role walks the same block list: text by its length, images by
/// [`ESTIMATED_IMAGE_CHARS`], tool calls by `name + JSON(arguments)`, and a
/// nested tool result by its inner block. Upstream keeps separate
/// `user` / `toolResult` branches only because its content may be a bare
/// string; that shape does not exist in Rust. Upstream's `thinking` block has
/// no Rust content variant either — provider thinking text is not part of the
/// protocol yet.
pub fn estimate_message_tokens(message: &Message) -> u32 {
    content_chars(&message.content).div_ceil(CHARS_PER_TOKEN) as u32
}

/// Sum [`estimate_message_tokens`] over a message list (upstream's private
/// `estimateMessages`, minus the usage scan).
pub fn estimate_messages_tokens(messages: &[Message]) -> u32 {
    messages.iter().fold(0u32, |total, message| {
        total.saturating_add(estimate_message_tokens(message))
    })
}

/// `estimateToolsTokens(tools)` — the serialized tool definitions at chars/4.
pub fn estimate_tools_tokens(tools: &[ToolDefinition]) -> u32 {
    if tools.is_empty() {
        return 0;
    }
    match serde_json::to_string(tools) {
        Ok(json) => estimate_text_tokens(&json),
        // `to_string` only fails on non-string-keyed maps, which a tool
        // schema cannot produce; treat it like the empty case rather than
        // charging a guess.
        Err(_) => 0,
    }
}

/// `estimateContextTokens(context)` — the full estimate for a
/// [`Context`], with the system prompt and tool definitions added when no
/// usage anchor exists.
///
/// * `anchor == Some(..)`: `tokens = usage + trailing`, where `trailing`
///   covers the messages after the anchored assistant message.
/// * `anchor == None`: `tokens = messages + system prompt + tools`,
///   `trailing_tokens == tokens` and `usage_tokens == 0` — upstream's
///   "no usage yet" shape.
///
/// An out-of-range anchor index contributes no trailing messages instead of
/// panicking; the anchor is caller-supplied, and a log that shrank after the
/// turn was recorded should degrade rather than abort.
pub fn estimate_context_usage(
    context: &Context,
    anchor: Option<UsageAnchor<'_>>,
) -> ContextUsageEstimate {
    match anchor {
        Some(anchor) => {
            let usage_tokens = calculate_context_tokens(anchor.usage);
            let trailing = context
                .messages
                .get(anchor.index.saturating_add(1)..)
                .unwrap_or(&[]);
            let trailing_tokens = estimate_messages_tokens(trailing);
            ContextUsageEstimate {
                tokens: usage_tokens.saturating_add(trailing_tokens),
                usage_tokens,
                trailing_tokens,
                last_usage_index: Some(anchor.index),
            }
        }
        None => {
            let tokens = estimate_messages_tokens(&context.messages)
                .saturating_add(estimate_text_tokens(&context.system_prompt))
                .saturating_add(estimate_tools_tokens(&context.tools));
            ContextUsageEstimate {
                tokens,
                usage_tokens: 0,
                trailing_tokens: tokens,
                last_usage_index: None,
            }
        }
    }
}

/// Characters in one content block — the Rust counterpart of upstream's
/// per-block accounting in `estimateMessageTokens`.
fn block_chars(block: &Content) -> usize {
    match block {
        Content::Text(text) => text.text.chars().count(),
        Content::Image(_) => ESTIMATED_IMAGE_CHARS,
        Content::ToolCall(call) => {
            call.name.chars().count()
                + serde_json::to_string(&call.arguments)
                    .map(|json| json.chars().count())
                    .unwrap_or(0)
        }
        Content::ToolResult(result) => block_chars(&result.content),
    }
}

fn content_chars(content: &[Content]) -> usize {
    content.iter().map(block_chars).sum()
}
