//! Session compaction (`/compact`) — Stage 24 of the Rust port.
//!
//! Mirrors `packages/coding-agent/src/core/compaction/`:
//!
//! * [`estimate_context_tokens`] / [`calculate_context_tokens`] size the
//!   current context (chars/4 heuristic, falling back to the last
//!   assistant usage).
//! * [`find_cut_point`] walks backwards from the newest message until it
//!   has kept roughly [`CompactionSettings::keep_recent_tokens`], cutting
//!   only at message boundaries that keep a valid provider history.
//! * [`compact`] serializes everything before the cut point and asks the
//!   model for a structured summary through the same [`StreamFn`] the
//!   agent loop uses, then returns the replacement history.
//!
//! # Deliberate differences from the TypeScript implementation
//!
//! The Rust session backend stores a flat `Vec<Message>` instead of the
//! TS session tree, so this module works on messages rather than
//! `SessionEntry` rows:
//!
//! * No `firstKeptEntryId` — the cut point is an index into the message
//!   list, and the retained tail is stored verbatim in the
//!   [`SessionEntry::Compaction`](pi_protocol::SessionEntry::Compaction)
//!   payload.
//! * Rust `Message` rows carry no provider usage (upstream attaches
//!   `AssistantMessage.usage`), so [`estimate_context_tokens`] is the pure
//!   chars/4 estimate. Callers that do have the last assistant usage can
//!   add the trailing estimate with [`context_tokens_with_trailing`].
//! * When the token walk has no valid cut point at or after the boundary,
//!   [`find_cut_point`] falls back to the newest cut point instead of the
//!   oldest. Upstream falls back to the first message, which makes a
//!   session that ends mid-turn (pending tool results) uncompactable.
//! * Branch summarization (`branch-summarization.ts`) is out of scope —
//!   it summarises abandoned session-tree branches, and the Rust port has
//!   no session tree yet.
//! * Each summarization call ([`complete_summarization`], shared by the
//!   main history summary and the split-turn prefix summary) runs inside
//!   the agent-level retry loop from `utils/retry.ts`, bounded by the
//!   [`RetryPolicy`] the caller passes to [`compact`]. `None` or a
//!   disabled policy reproduces the pre-port "fail on the first error"
//!   behaviour. Upstream classifies the failure via
//!   `AssistantMessage.errorMessage`, which `pi-protocol` does not carry;
//!   this port derives the text from the stream's own `Error` event instead
//!   and falls back to the wrapper wording (`provider returned error`) when
//!   a `Done` arrives with `stop_reason: error` and no wording.
//!
//!   The classifier and the backoff schedule are the shared agent-level
//!   primitives (`pi_agent_core::{is_retryable_error_message,
//!   retry_delay_ms}`), so compaction and the agent loop spend the same
//!   [`RetryPolicy`] budget and agree on what counts as a transient
//!   failure. Only the loop itself is local to this module: the agent
//!   loop's [`pi_agent_core::retry_assistant_call`] is shaped around
//!   `AssistantMessage` / `AgentError`, while a summarization attempt
//!   yields text and keeps its [`CompactionError`] variant. The backoff
//!   sleep is already signal-aware, but compaction is not cancellable today
//!   ([`SimpleStreamOptions::signal`] is unset), so an abort can only arrive
//!   from the stream itself.
//! * The compaction summary is rendered as a `user` message wrapped in
//!   the same `<summary>` tags upstream uses when it converts a
//!   `compactionSummary` message for the provider.

use std::collections::BTreeSet;
use std::time::Duration;

use futures::StreamExt;
use pi_agent_core::{is_retryable_error_message, retry_delay_ms, RetryPolicy};
use pi_ai::{AbortSignal, SharedStreamFn, SimpleStreamOptions, StreamError};
use pi_protocol::{AssistantMessageEvent, Content, Message, Model, Role, StopReason, Usage};

/// Prefix of the synthetic user message that replaces the compacted history.
pub const COMPACTION_SUMMARY_PREFIX: &str =
    "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";

/// Suffix of the synthetic user message that replaces the compacted history.
pub const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";

/// System prompt for the summarization request (`utils.ts`).
pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

/// Initial summarization prompt (`compaction.ts`).
const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or \"(none)\" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or \"(none)\" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

/// Shared body of the update-summary prompt (`compaction.ts`).
const UPDATE_SUMMARIZATION_INSTRUCTIONS: &str =
    "Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from \"In Progress\" to \"Done\" when completed
- UPDATE \"Next Steps\" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

/// Lead-in of the update-summary prompt; `UPDATE_SUMMARIZATION_INSTRUCTIONS`
/// carries the format rules.
const UPDATE_SUMMARIZATION_PROMPT_HEADER: &str =
    "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.";

/// Prompt used to summarize the discarded prefix of a split turn.
const TURN_PREFIX_SUMMARIZATION_PROMPT: &str =
    "This is the PREFIX of a turn that was too large to keep. The SUFFIX (recent work) is retained.

Summarize the prefix to provide context for the retained suffix:

## Original Request
[What did the user ask for in this turn?]

## Early Progress
- [Key decisions and work done in the prefix]

## Context for Suffix
- [Information needed to understand the retained recent work]

Be concise. Focus on what's needed to understand the kept suffix.";

/// Character budget charged for an image block when estimating tokens.
const ESTIMATED_IMAGE_CHARS: usize = 4800;

/// Maximum characters kept per tool result in the serialized conversation.
const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// Automatic-compaction thresholds (`DEFAULT_COMPACTION_SETTINGS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionSettings {
    /// Whether automatic compaction may trigger. Manual `/compact` ignores
    /// this flag, matching upstream.
    pub enabled: bool,
    /// Head-room kept free below the model context window.
    pub reserve_tokens: u32,
    /// Approximate number of recent tokens kept verbatim.
    pub keep_recent_tokens: u32,
}

/// Upstream defaults: `{ enabled: true, reserveTokens: 16384, keepRecentTokens: 20000 }`.
pub const DEFAULT_COMPACTION_SETTINGS: CompactionSettings = CompactionSettings {
    enabled: true,
    reserve_tokens: 16_384,
    keep_recent_tokens: 20_000,
};

impl Default for CompactionSettings {
    fn default() -> Self {
        DEFAULT_COMPACTION_SETTINGS
    }
}

/// Where the conversation is split into "summarize" and "keep".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutPoint {
    /// Index of the first message kept verbatim.
    pub first_kept_index: usize,
    /// Index of the user message that started the split turn, when the cut
    /// lands in the middle of a turn.
    pub turn_start_index: Option<usize>,
    /// True when `first_kept_index` is not a turn start.
    pub is_split_turn: bool,
}

/// File operations observed while scanning assistant tool calls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileOperations {
    read: BTreeSet<String>,
    written: BTreeSet<String>,
    edited: BTreeSet<String>,
}

impl FileOperations {
    /// Record one tool call, when it is `read` / `write` / `edit` and carries
    /// a string `path` argument.
    pub fn record_tool_call(&mut self, name: &str, arguments: &serde_json::Value) {
        let Some(path) = arguments.get("path").and_then(|value| value.as_str()) else {
            return;
        };
        match name {
            "read" => {
                self.read.insert(path.to_string());
            }
            "write" => {
                self.written.insert(path.to_string());
            }
            "edit" => {
                self.edited.insert(path.to_string());
            }
            _ => {}
        }
    }

    /// Record every tool call in an assistant message.
    pub fn record_message(&mut self, message: &Message) {
        if message.role != Role::Assistant {
            return;
        }
        for block in &message.content {
            if let Content::ToolCall(call) = block {
                self.record_tool_call(&call.name, &call.arguments);
            }
        }
    }

    /// Files only read (never modified), sorted; then modified files, sorted.
    pub fn compute_file_lists(&self) -> (Vec<String>, Vec<String>) {
        let modified: BTreeSet<&String> = self.edited.union(&self.written).collect();
        let read_only = self
            .read
            .iter()
            .filter(|path| !modified.contains(path))
            .cloned()
            .collect();
        let modified_files = modified.into_iter().cloned().collect();
        (read_only, modified_files)
    }
}

/// Everything [`compact`] needs, produced by [`prepare_compaction`].
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionPreparation {
    /// Index of the first message kept verbatim.
    pub first_kept_index: usize,
    /// Messages replaced by the summary.
    pub messages_to_summarize: Vec<Message>,
    /// Prefix of a split turn, summarized with its own prompt.
    pub turn_prefix_messages: Vec<Message>,
    /// Whether the cut split a turn.
    pub is_split_turn: bool,
    /// Estimated context tokens before compaction.
    pub tokens_before: u32,
    /// Summary produced by an earlier compaction, for iterative updates.
    pub previous_summary: Option<String>,
    /// File operations observed in the summarized messages.
    pub file_ops: FileOperations,
}

/// A completed compaction: the summary plus the messages kept verbatim.
#[derive(Debug, Clone, PartialEq)]
pub struct Compaction {
    /// Structured summary text (without the `<summary>` wrapper).
    pub summary: String,
    /// Messages kept verbatim after the summary.
    pub retained_tail: Vec<Message>,
    /// Estimated context tokens before compaction.
    pub tokens_before: u32,
    /// Usage reported by the summarization call(s).
    pub usage: Usage,
    /// Files only read during the compacted span.
    pub read_files: Vec<String>,
    /// Files modified during the compacted span.
    pub modified_files: Vec<String>,
}

impl Compaction {
    /// The conversation after compaction: summary message + retained tail.
    pub fn into_history(self) -> Vec<Message> {
        replace_with_compaction(&self.retained_tail, &self.summary)
    }

    /// Details object stored alongside the session entry.
    pub fn details(&self) -> serde_json::Value {
        serde_json::json!({
            "readFiles": self.read_files,
            "modifiedFiles": self.modified_files,
        })
    }

    /// Session entry for this compaction.
    pub fn to_entry(&self) -> pi_protocol::SessionEntry {
        pi_protocol::SessionEntry::Compaction {
            summary: self.summary.clone(),
            retained_tail: self.retained_tail.clone(),
            tokens_before: self.tokens_before,
            usage: Some(self.usage),
            details: Some(self.details()),
        }
    }
}

/// Errors surfaced by [`compact`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompactionError {
    /// Nothing is old enough to summarize (already compacted, or the whole
    /// conversation fits inside `keep_recent_tokens`).
    #[error("nothing to compact: conversation already fits in the retained window")]
    NothingToCompact,
    /// The summarization stream failed before producing a message.
    #[error("summarization failed: {0}")]
    Stream(String),
    /// The provider reported an error mid-stream.
    #[error("summarization failed: {0}")]
    Provider(String),
    /// The model hit the output-token cap, so the summary is incomplete.
    #[error("summarization failed: generation hit the token cap and the summary is incomplete")]
    Incomplete,
    /// The model returned no text (or tried to call a tool).
    #[error("summarization failed: no summary text returned")]
    EmptySummary,
}

/// Build the user-visible conversation history for a summary + tail.
///
/// The summary becomes a `user` message wrapped in `<summary>` tags, which
/// is exactly how upstream `convertToLlm` renders a `compactionSummary`.
pub fn replace_with_compaction(retained_tail: &[Message], summary: &str) -> Vec<Message> {
    let mut history = Vec::with_capacity(retained_tail.len() + 1);
    history.push(summary_message(summary));
    history.extend_from_slice(retained_tail);
    history
}

/// Render the synthetic user message carrying a compaction summary.
pub fn summary_message(summary: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![Content::text(format!(
            "{COMPACTION_SUMMARY_PREFIX}{summary}{COMPACTION_SUMMARY_SUFFIX}"
        ))],
        model: None,
    }
}

/// Extract the summary text from a [`summary_message`], if `message` is one.
pub fn extract_summary(message: &Message) -> Option<&str> {
    if message.role != Role::User || message.content.len() != 1 {
        return None;
    }
    let text = match &message.content[0] {
        Content::Text(text) => &text.text,
        _ => return None,
    };
    text.strip_prefix(COMPACTION_SUMMARY_PREFIX)?
        .strip_suffix(COMPACTION_SUMMARY_SUFFIX)
}

/// `calculateContextTokens(usage)` — prefer the provider's total.
pub fn calculate_context_tokens(usage: &Usage) -> u32 {
    if usage.total > 0 {
        usage.total
    } else {
        usage.input + usage.output + usage.cache_read + usage.cache_write
    }
}

/// `estimateTokens(message)` — conservative chars/4 heuristic.
pub fn estimate_tokens(message: &Message) -> u32 {
    let chars = match message.role {
        Role::Assistant => message.content.iter().map(block_chars).sum(),
        Role::User | Role::System | Role::Tool => content_chars(&message.content),
    };
    chars.div_ceil(4) as u32
}

/// Sum [`estimate_tokens`] across the whole conversation.
pub fn estimate_message_tokens(messages: &[Message]) -> u32 {
    messages.iter().map(estimate_tokens).sum()
}

/// Estimate the context size of a message list.
///
/// Rust [`Message`] rows do not carry provider usage, so this is the pure
/// chars/4 estimate over every message. Use
/// [`context_tokens_with_trailing`] when the last assistant usage is known.
pub fn estimate_context_tokens(messages: &[Message]) -> u32 {
    estimate_message_tokens(messages)
}

/// Provider-reported usage plus the estimate for the messages after it.
pub fn context_tokens_with_trailing(usage: &Usage, trailing: &[Message]) -> u32 {
    calculate_context_tokens(usage) + estimate_message_tokens(trailing)
}

/// `shouldCompact(contextTokens, contextWindow, settings)`.
pub fn should_compact(
    context_tokens: u32,
    context_window: u32,
    settings: CompactionSettings,
) -> bool {
    if !settings.enabled {
        return false;
    }
    context_tokens.saturating_add(settings.reserve_tokens) > context_window
}

/// Find the boundary that keeps roughly `keep_recent_tokens` of recent
/// messages, cutting only where the retained history stays valid.
pub fn find_cut_point(
    messages: &[Message],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: u32,
) -> CutPoint {
    let cut_points = valid_cut_points(messages, start_index, end_index);
    if cut_points.is_empty() {
        return CutPoint {
            first_kept_index: start_index,
            turn_start_index: None,
            is_split_turn: false,
        };
    }

    let mut accumulated = 0u32;
    let mut cut_index = cut_points[0];
    for index in (start_index..end_index).rev() {
        let tokens = estimate_tokens(&messages[index]);
        if tokens == 0 {
            continue;
        }
        accumulated = accumulated.saturating_add(tokens);
        if accumulated >= keep_recent_tokens {
            cut_index = *cut_points
                .iter()
                .find(|&&candidate| candidate >= index)
                .unwrap_or(&cut_points[cut_points.len() - 1]);
            break;
        }
    }

    let starts_turn = is_turn_start_message(&messages[cut_index]);
    let turn_start_index = if starts_turn {
        None
    } else {
        find_turn_start_index(messages, cut_index, start_index)
    };
    CutPoint {
        first_kept_index: cut_index,
        turn_start_index,
        is_split_turn: !starts_turn && turn_start_index.is_some(),
    }
}

/// Index of the user message that starts the turn containing `entry_index`.
pub fn find_turn_start_index(
    messages: &[Message],
    entry_index: usize,
    start_index: usize,
) -> Option<usize> {
    (start_index..=entry_index)
        .rev()
        .find(|&index| is_turn_start_message(&messages[index]))
}

/// Prepare a compaction, or `None` when there is nothing to summarize.
pub fn prepare_compaction(
    messages: &[Message],
    settings: CompactionSettings,
) -> Option<CompactionPreparation> {
    // A previous compaction left a summary message at the head of the
    // history; start the boundary after it and reuse its text as the
    // previous summary so the next compaction updates rather than restarts.
    let previous_summary = messages.first().and_then(extract_summary);
    let boundary_start = usize::from(previous_summary.is_some());
    if messages.len() <= boundary_start {
        return None;
    }

    let tokens_before = estimate_context_tokens(messages);
    let cut_point = find_cut_point(
        messages,
        boundary_start,
        messages.len(),
        settings.keep_recent_tokens,
    );

    let history_end = if cut_point.is_split_turn {
        cut_point.turn_start_index?
    } else {
        cut_point.first_kept_index
    };
    let messages_to_summarize = messages[boundary_start..history_end].to_vec();
    let turn_prefix_messages = if cut_point.is_split_turn {
        messages[cut_point.turn_start_index?..cut_point.first_kept_index].to_vec()
    } else {
        Vec::new()
    };
    if messages_to_summarize.is_empty() && turn_prefix_messages.is_empty() {
        return None;
    }

    let mut file_ops = FileOperations::default();
    for message in messages_to_summarize.iter().chain(&turn_prefix_messages) {
        file_ops.record_message(message);
    }

    Some(CompactionPreparation {
        first_kept_index: cut_point.first_kept_index,
        messages_to_summarize,
        turn_prefix_messages,
        is_split_turn: cut_point.is_split_turn,
        tokens_before,
        previous_summary: previous_summary.map(str::to_string),
        file_ops,
    })
}

/// Summarize the prepared span and return the compacted history.
///
/// `retry` is the agent-level retry budget applied to each summarization
/// call ([`RetryPolicy`]); `None` (or a disabled policy) keeps the
/// pre-port behaviour of failing on the first transient provider error.
pub async fn compact(
    messages: &[Message],
    model: &Model,
    stream_fn: &SharedStreamFn,
    settings: CompactionSettings,
    custom_instructions: Option<&str>,
    retry: Option<RetryPolicy>,
) -> Result<Compaction, CompactionError> {
    let preparation =
        prepare_compaction(messages, settings).ok_or(CompactionError::NothingToCompact)?;
    let retained_tail = messages[preparation.first_kept_index..].to_vec();

    let (summary, usage) =
        if preparation.is_split_turn && !preparation.turn_prefix_messages.is_empty() {
            let (history_text, history_usage) = if preparation.messages_to_summarize.is_empty() {
                ("No prior history.".to_string(), None)
            } else {
                let (text, usage) = generate_summary(
                    &preparation.messages_to_summarize,
                    model,
                    stream_fn,
                    settings.reserve_tokens,
                    custom_instructions,
                    preparation.previous_summary.as_deref(),
                    retry,
                )
                .await?;
                (text, Some(usage))
            };
            let (prefix_text, prefix_usage) = generate_turn_prefix_summary(
                &preparation.turn_prefix_messages,
                model,
                stream_fn,
                settings.reserve_tokens,
                retry,
            )
            .await?;
            let merged =
                format!("{history_text}\n\n---\n\n**Turn Context (split turn):**\n\n{prefix_text}");
            let usage =
                history_usage.map_or(prefix_usage, |first| combine_usage(&first, &prefix_usage));
            (merged, usage)
        } else {
            generate_summary(
                &preparation.messages_to_summarize,
                model,
                stream_fn,
                settings.reserve_tokens,
                custom_instructions,
                preparation.previous_summary.as_deref(),
                retry,
            )
            .await?
        };

    let (read_files, modified_files) = preparation.file_ops.compute_file_lists();
    let summary = format!(
        "{summary}{}",
        format_file_operations(&read_files, &modified_files)
    );

    Ok(Compaction {
        summary,
        retained_tail,
        tokens_before: preparation.tokens_before,
        usage,
        read_files,
        modified_files,
    })
}

/// Convenience wrapper: summarize and immediately produce the new history.
pub async fn compact_history(
    messages: &[Message],
    model: &Model,
    stream_fn: &SharedStreamFn,
    settings: CompactionSettings,
    custom_instructions: Option<&str>,
    retry: Option<RetryPolicy>,
) -> Result<Vec<Message>, CompactionError> {
    let compaction = compact(
        messages,
        model,
        stream_fn,
        settings,
        custom_instructions,
        retry,
    )
    .await?;
    Ok(compaction.into_history())
}

/// Serialize messages to text for the summarization prompt.
pub fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for message in messages {
        match message.role {
            Role::User | Role::System => {
                let text = content_text(&message.content);
                if !text.is_empty() {
                    parts.push(format!("[User]: {text}"));
                }
            }
            Role::Assistant => {
                let text = content_text(&message.content);
                if !text.is_empty() {
                    parts.push(format!("[Assistant]: {text}"));
                }
                let tool_calls: Vec<String> = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        Content::ToolCall(call) => Some(format!(
                            "{}({})",
                            call.name,
                            arguments_to_string(&call.arguments)
                        )),
                        _ => None,
                    })
                    .collect();
                if !tool_calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", tool_calls.join("; ")));
                }
            }
            Role::Tool => {
                for block in &message.content {
                    if let Content::ToolResult(result) = block {
                        let text = content_text(std::slice::from_ref(&result.content));
                        if !text.is_empty() {
                            parts.push(format!(
                                "[Tool result]: {}",
                                truncate_for_summary(&text, TOOL_RESULT_MAX_CHARS)
                            ));
                        }
                    }
                }
            }
        }
    }
    parts.join("\n\n")
}

/// Join the text blocks of a content array, mirroring `contentText`.
pub fn content_text(content: &[Content]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            Content::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `formatFileOperations(readFiles, modifiedFiles)`.
pub fn format_file_operations(read_files: &[String], modified_files: &[String]) -> String {
    let mut sections: Vec<String> = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!(
            "<read-files>\n{}\n</read-files>",
            read_files.join("\n")
        ));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files.join("\n")
        ));
    }
    if sections.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", sections.join("\n\n"))
    }
}

// ---------------------------------------------------------------------------
// Summarization calls
// ---------------------------------------------------------------------------

/// Run one summarization completion and return its text plus usage.
async fn generate_summary(
    messages: &[Message],
    model: &Model,
    stream_fn: &SharedStreamFn,
    reserve_tokens: u32,
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
    retry: Option<RetryPolicy>,
) -> Result<(String, Usage), CompactionError> {
    let max_tokens = summary_max_tokens(model, reserve_tokens, 0.8);
    let conversation = serialize_conversation(messages);

    let mut prompt = format!("<conversation>\n{conversation}\n</conversation>\n\n");
    let mut base = match previous_summary {
        Some(_) => {
            format!("{UPDATE_SUMMARIZATION_PROMPT_HEADER}\n\n{UPDATE_SUMMARIZATION_INSTRUCTIONS}")
        }
        None => SUMMARIZATION_PROMPT.to_string(),
    };
    if let Some(instructions) = custom_instructions {
        base.push_str(&format!("\n\nAdditional focus: {instructions}"));
    }
    if let Some(previous) = previous_summary {
        prompt.push_str(&format!(
            "<previous-summary>\n{previous}\n</previous-summary>\n\n"
        ));
    }
    prompt.push_str(&base);

    let (text, usage) =
        complete_summarization(model, stream_fn, &prompt, max_tokens, retry).await?;
    Ok((text, usage))
}

/// Summarize the discarded prefix of a split turn.
async fn generate_turn_prefix_summary(
    messages: &[Message],
    model: &Model,
    stream_fn: &SharedStreamFn,
    reserve_tokens: u32,
    retry: Option<RetryPolicy>,
) -> Result<(String, Usage), CompactionError> {
    let max_tokens = summary_max_tokens(model, reserve_tokens, 0.5);
    let conversation = serialize_conversation(messages);
    let prompt = format!(
        "<conversation>\n{conversation}\n</conversation>\n\n{TURN_PREFIX_SUMMARIZATION_PROMPT}"
    );
    complete_summarization(model, stream_fn, &prompt, max_tokens, retry).await
}

/// `min(floor(fraction * reserveTokens), model.maxTokens)`.
fn summary_max_tokens(model: &Model, reserve_tokens: u32, fraction: f32) -> u32 {
    let budget = (fraction * reserve_tokens as f32).floor() as u32;
    if model.max_output_tokens > 0 {
        budget.min(model.max_output_tokens)
    } else {
        budget
    }
}

/// One completed summarization stream, before its content is validated.
struct SummarizationResponse {
    content: Vec<Content>,
    stop_reason: StopReason,
    usage: Usage,
}

/// Drive the summarization stream to completion, retrying transient provider
/// failures within `retry` — upstream `completeSummarization`, which wraps
/// `retryAssistantCall` around the single stream call.
///
/// The classifier ([`is_retryable_error_message`]) and the backoff schedule
/// ([`retry_delay_ms`]) are shared with the agent loop; the loop is local
/// because a summarization attempt produces text and a [`CompactionError`]
/// rather than an `AssistantMessage`.
async fn complete_summarization(
    model: &Model,
    stream_fn: &SharedStreamFn,
    prompt: &str,
    max_tokens: u32,
    retry: Option<RetryPolicy>,
) -> Result<(String, Usage), CompactionError> {
    let context = pi_protocol::Context {
        system_prompt: SUMMARIZATION_SYSTEM_PROMPT.to_string(),
        messages: vec![Message {
            role: Role::User,
            content: vec![Content::text(prompt)],
            model: None,
        }],
        tools: Vec::new(),
    };
    let options = SimpleStreamOptions {
        max_tokens: Some(max_tokens),
        ..SimpleStreamOptions::default()
    };

    // Every attempt opens a fresh stream — the loop re-runs `summarize_once`,
    // so a half-consumed stream is never resumed.
    let policy = retry.filter(|policy| policy.enabled);
    let max_attempts = policy.map_or(0, |policy| policy.max_retries);
    let mut attempt = 0u32;
    let response = loop {
        match summarize_once(model, stream_fn, &context, &options).await {
            AttemptOutcome::Done(response) => break response,
            AttemptOutcome::Failed { error, error_text } => {
                // Non-retryable, or budget exhausted: the original error goes
                // back unchanged, variant included.
                if attempt >= max_attempts || !is_retryable_error_message(&error_text) {
                    return Err(error);
                }
                attempt += 1;
                let policy = policy.expect("a retry implies an enabled policy");
                let delay_ms = retry_delay_ms(&policy, attempt);
                if !sleep_or_abort(delay_ms, options.signal.as_ref()).await {
                    // Cancelled during the backoff: same error shape as a
                    // stream that aborts mid-flight.
                    return Err(CompactionError::Provider("aborted".to_string()));
                }
            }
            // An abort during the stream is terminal and never retried.
            AttemptOutcome::Aborted => {
                return Err(CompactionError::Provider("aborted".to_string()))
            }
        }
    };

    // A truncated summary is deterministic — retrying cannot help, and the
    // caller reports it as an incomplete compaction (never retried).
    if response.stop_reason == StopReason::MaxTokens {
        return Err(CompactionError::Incomplete);
    }
    if response.content.iter().any(|block| block.is_tool_call()) {
        return Err(CompactionError::Provider(
            "summarization attempted to call a tool".to_string(),
        ));
    }
    let text = content_text(&response.content);
    if text.trim().is_empty() {
        return Err(CompactionError::EmptySummary);
    }
    Ok((text, response.usage))
}

/// What one summarization attempt reports to the retry loop.
enum AttemptOutcome {
    /// The stream produced a terminal response.
    Done(SummarizationResponse),
    /// The attempt failed; `error_text` is the provider wording the shared
    /// classifier inspects.
    Failed {
        /// The error to return unchanged once retrying stops.
        error: CompactionError,
        /// The wording used for the retry decision.
        error_text: String,
    },
    /// Cancelled — never retried.
    Aborted,
}

/// One summarization attempt: open a fresh stream and drive it to a
/// conclusion.
///
/// The terminal stream events are mapped onto [`AttemptOutcome`] so the retry
/// loop sees the provider's wording, while the error carried alongside it
/// keeps the [`CompactionError`] variant the pre-port code used (`Stream`
/// for transport failures, `Provider` for in-stream provider errors).
async fn summarize_once(
    model: &Model,
    stream_fn: &SharedStreamFn,
    context: &pi_protocol::Context,
    options: &SimpleStreamOptions,
) -> AttemptOutcome {
    let mut stream = match stream_fn.stream_simple(model, context, options).await {
        Ok(stream) => stream,
        Err(StreamError::Aborted) => return AttemptOutcome::Aborted,
        Err(err) => {
            let text = err.to_string();
            return AttemptOutcome::Failed {
                error: CompactionError::Stream(text.clone()),
                error_text: text,
            };
        }
    };

    let mut response: Option<SummarizationResponse> = None;
    while let Some(event) = stream.next().await {
        match event {
            Ok(AssistantMessageEvent::Done {
                content,
                stop_reason,
                usage,
            }) => {
                response = Some(SummarizationResponse {
                    content,
                    stop_reason,
                    usage,
                });
            }
            Ok(AssistantMessageEvent::Error { message }) => {
                return AttemptOutcome::Failed {
                    error: CompactionError::Provider(message.clone()),
                    error_text: message,
                };
            }
            Ok(AssistantMessageEvent::Aborted) => return AttemptOutcome::Aborted,
            Ok(
                AssistantMessageEvent::Start { .. }
                | AssistantMessageEvent::TextDelta { .. }
                | AssistantMessageEvent::ThinkingDelta { .. }
                | AssistantMessageEvent::ToolCallDelta { .. },
            ) => {}
            Err(StreamError::Aborted) => return AttemptOutcome::Aborted,
            Err(err) => {
                let text = err.to_string();
                return AttemptOutcome::Failed {
                    error: CompactionError::Stream(text.clone()),
                    error_text: text,
                };
            }
        }
    }

    match response {
        // `stop_reason: Error` with no `Error` event. `pi-protocol` has no
        // `AssistantMessage.errorMessage`, so the provider's wording either
        // rides in the content or is missing entirely; the fallback matches
        // upstream's wrapper wording, which the classifier treats as
        // retryable (see the module docs).
        Some(SummarizationResponse {
            stop_reason: StopReason::Error,
            content,
            ..
        }) => {
            let text = content_text(&content);
            let text = if text.trim().is_empty() {
                "provider returned error".to_string()
            } else {
                text
            };
            AttemptOutcome::Failed {
                error: CompactionError::Provider(text.clone()),
                error_text: text,
            }
        }
        Some(response) => AttemptOutcome::Done(response),
        // The stream ended without a `Done` event. Deterministic, and the
        // text matches no retryable pattern, so it is reported as-is.
        None => AttemptOutcome::Failed {
            error: CompactionError::EmptySummary,
            error_text: "summarization stream produced no result".to_string(),
        },
    }
}

/// Sleep `delay_ms` for a backoff, returning `false` when `signal` aborts
/// first. Mirrors the provider layer's `sleep_or_abort`.
async fn sleep_or_abort(delay_ms: u64, signal: Option<&AbortSignal>) -> bool {
    let sleep = tokio::time::sleep(Duration::from_millis(delay_ms));
    match signal {
        Some(signal) => tokio::select! {
            () = sleep => true,
            () = signal.cancelled() => false,
        },
        None => {
            sleep.await;
            true
        }
    }
}

/// `combineUsage(first, second)` for the two summary calls of a split turn.
fn combine_usage(first: &Usage, second: &Usage) -> Usage {
    Usage {
        input: first.input + second.input,
        output: first.output + second.output,
        cache_read: first.cache_read + second.cache_read,
        cache_write: first.cache_write + second.cache_write,
        total: first.total + second.total,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn valid_cut_points(messages: &[Message], start_index: usize, end_index: usize) -> Vec<usize> {
    (start_index..end_index)
        .filter(|&index| is_cut_point_message(&messages[index]))
        .collect()
}

/// User or assistant messages are valid cut points; tool results are not
/// (they must follow their tool call). A compaction summary is never a cut
/// point — it is already the head of the summarizable region.
fn is_cut_point_message(message: &Message) -> bool {
    if extract_summary(message).is_some() {
        return false;
    }
    matches!(message.role, Role::User | Role::Assistant)
}

fn is_turn_start_message(message: &Message) -> bool {
    message.role == Role::User && extract_summary(message).is_none()
}

fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars).collect();
    let truncated = text.chars().count() - max_chars;
    format!("{kept}\n\n[... {truncated} more characters truncated]")
}

fn arguments_to_string(arguments: &serde_json::Value) -> String {
    match arguments.as_object() {
        Some(map) => map
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(", "),
        None => arguments.to_string(),
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::stream::{AssistantMessageEventStream, StreamFn};
    use pi_ai::StreamError;
    use pi_protocol::{AssistantMessage, Role};
    use std::sync::Mutex;

    fn user(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![Content::text(text)],
            model: None,
        }
    }

    fn assistant(text: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![Content::text(text)],
            model: Some("faux".into()),
        }
    }

    fn assistant_with_tool_call(name: &str, path: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![Content::ToolCall(pi_protocol::ToolCall {
                id: "call-1".into(),
                name: name.into(),
                arguments: serde_json::json!({ "path": path }),
            })],
            model: Some("faux".into()),
        }
    }

    fn tool_result(text: &str) -> Message {
        Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(pi_protocol::ToolResult {
                tool_call_id: "call-1".into(),
                content: Box::new(Content::text(text)),
                is_error: false,
                details: None,
            })],
            model: None,
        }
    }

    /// Stream that always answers summarization with `text`.
    struct SummaryStream {
        text: String,
        stop_reason: StopReason,
        calls: Mutex<Vec<String>>,
    }

    impl SummaryStream {
        fn new(text: &str) -> Self {
            Self {
                text: text.to_string(),
                stop_reason: StopReason::Stop,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn prompts(&self) -> Vec<String> {
            self.calls.lock().expect("lock").clone()
        }
    }

    /// One scripted summarization attempt for the agent-level retry tests.
    #[derive(Clone)]
    enum Attempt {
        /// Fail before the stream starts; `StreamError::Malformed` stands in
        /// for a dropped transport because `StreamError::Transport` wraps a
        /// `reqwest::Error` this crate cannot build in a test.
        TransportFailure(&'static str),
        /// Stream a single in-stream provider error and stop.
        ProviderError(&'static str),
        /// Stream `Done` with `stop_reason: error` and `text` as content.
        DoneWithErrorStopReason(&'static str),
        /// Stream a usable summary.
        Summary(&'static str),
    }

    /// Stream that replays a script of [`Attempt`]s, repeating the last one
    /// forever once the script drains.
    struct ScriptedSummaryStream {
        script: Mutex<std::collections::VecDeque<Attempt>>,
        calls: Mutex<usize>,
    }

    impl ScriptedSummaryStream {
        fn new(script: Vec<Attempt>) -> Self {
            Self {
                script: Mutex::new(script.into()),
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().expect("lock")
        }

        fn next_attempt(&self) -> Attempt {
            let mut script = self.script.lock().expect("lock");
            if script.len() > 1 {
                script.pop_front().expect("non-empty")
            } else {
                script.front().cloned().expect("a scripted attempt")
            }
        }
    }

    fn done_events(
        model: &Model,
        content: Vec<Content>,
        stop_reason: StopReason,
    ) -> Vec<Result<AssistantMessageEvent, StreamError>> {
        vec![
            Ok(AssistantMessageEvent::Start {
                model: model.id.clone(),
            }),
            Ok(AssistantMessageEvent::Done {
                content,
                stop_reason,
                usage: Usage {
                    input: 10,
                    output: 5,
                    total: 15,
                    ..Usage::default()
                },
            }),
        ]
    }

    #[async_trait::async_trait]
    impl StreamFn for ScriptedSummaryStream {
        async fn stream_simple(
            &self,
            model: &Model,
            _ctx: &pi_protocol::Context,
            _options: &SimpleStreamOptions,
        ) -> Result<AssistantMessageEventStream, StreamError> {
            *self.calls.lock().expect("lock") += 1;
            match self.next_attempt() {
                Attempt::TransportFailure(text) => Err(StreamError::Malformed(text.to_string())),
                Attempt::ProviderError(text) => Ok(Box::pin(futures::stream::iter(vec![
                    Ok(AssistantMessageEvent::Start {
                        model: model.id.clone(),
                    }),
                    Ok(AssistantMessageEvent::Error {
                        message: text.to_string(),
                    }),
                ]))),
                Attempt::DoneWithErrorStopReason(text) => {
                    let content = if text.is_empty() {
                        Vec::new()
                    } else {
                        vec![Content::text(text)]
                    };
                    Ok(Box::pin(futures::stream::iter(done_events(
                        model,
                        content,
                        StopReason::Error,
                    ))))
                }
                Attempt::Summary(text) => Ok(Box::pin(futures::stream::iter(done_events(
                    model,
                    vec![Content::text(text)],
                    StopReason::Stop,
                )))),
            }
        }
    }

    #[async_trait::async_trait]
    impl StreamFn for SummaryStream {
        async fn stream_simple(
            &self,
            model: &Model,
            ctx: &pi_protocol::Context,
            _options: &SimpleStreamOptions,
        ) -> Result<AssistantMessageEventStream, StreamError> {
            self.calls
                .lock()
                .expect("lock")
                .push(content_text(&ctx.messages[0].content));
            let message = AssistantMessage {
                model: model.id.clone(),
                content: vec![Content::text(self.text.clone())],
                stop_reason: self.stop_reason,
                usage: Usage {
                    input: 100,
                    output: 50,
                    total: 150,
                    ..Usage::default()
                },
            };
            let events = vec![
                Ok(AssistantMessageEvent::Start {
                    model: model.id.clone(),
                }),
                Ok(AssistantMessageEvent::Done {
                    content: message.content,
                    stop_reason: message.stop_reason,
                    usage: message.usage,
                }),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    fn model() -> Model {
        Model {
            provider: pi_protocol::ProviderId::new("faux"),
            id: "faux-model".into(),
            api: pi_protocol::Api::Faux,
            label: None,
            context_window: 200_000,
            max_output_tokens: 0,
        }
    }

    #[test]
    fn calculates_context_tokens_from_total_or_components() {
        let with_total = Usage {
            input: 1,
            output: 2,
            total: 99,
            ..Usage::default()
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
    fn estimates_tokens_with_chars_over_four() {
        assert_eq!(estimate_tokens(&user("abcd")), 1);
        assert_eq!(estimate_tokens(&user(&"a".repeat(9))), 3);
        let with_call = assistant_with_tool_call("read", "src/lib.rs");
        assert!(estimate_tokens(&with_call) > 0);
        assert_eq!(
            estimate_context_tokens(&[user("hello"), assistant("world")]),
            estimate_message_tokens(&[user("hello"), assistant("world")])
        );
    }

    #[test]
    fn context_estimate_is_pure_heuristic_without_usage() {
        let messages = vec![user("hello"), assistant("world")];
        assert_eq!(
            estimate_context_tokens(&messages),
            estimate_message_tokens(&messages)
        );
    }

    #[test]
    fn context_estimate_prefers_provider_usage_when_available() {
        let usage = Usage {
            input: 1_000,
            output: 200,
            total: 1_200,
            ..Usage::default()
        };
        assert_eq!(context_tokens_with_trailing(&usage, &[user("abcd")]), 1_201);
    }

    #[test]
    fn should_compact_respects_enabled_and_reserve() {
        let settings = CompactionSettings::default();
        assert!(!should_compact(1_000, 200_000, settings));
        assert!(should_compact(190_000, 200_000, settings));
        assert!(!should_compact(
            190_000,
            200_000,
            CompactionSettings {
                enabled: false,
                ..settings
            }
        ));
    }

    #[test]
    fn cut_point_keeps_recent_tokens_at_turn_boundary() {
        let mut messages = Vec::new();
        for index in 0..10 {
            messages.push(user(&format!("question {index} {}", "x".repeat(100))));
            messages.push(assistant(&format!("answer {index} {}", "y".repeat(100))));
        }
        let cut = find_cut_point(&messages, 0, messages.len(), 100);
        assert!(!cut.is_split_turn);
        assert_eq!(messages[cut.first_kept_index].role, Role::User);
        assert!(cut.first_kept_index > 0);
    }

    #[test]
    fn cut_point_splits_a_single_oversized_turn() {
        let messages = vec![
            user("do the thing"),
            assistant_with_tool_call("read", "a.rs"),
            tool_result(&"z".repeat(4_000)),
        ];
        let cut = find_cut_point(&messages, 0, messages.len(), 100);
        assert!(cut.is_split_turn);
        assert_eq!(cut.turn_start_index, Some(0));
        assert_eq!(cut.first_kept_index, 1);
    }

    #[test]
    fn prepare_returns_none_when_conversation_fits() {
        let messages = vec![user("hi"), assistant("hello")];
        assert!(prepare_compaction(&messages, CompactionSettings::default()).is_none());
    }

    #[test]
    fn prepare_extracts_files_and_previous_summary() {
        let mut messages = vec![user("start")];
        for index in 0..8 {
            messages.push(assistant_with_tool_call("read", &format!("r{index}.rs")));
            messages.push(tool_result(&"z".repeat(2_000)));
        }
        let settings = CompactionSettings {
            keep_recent_tokens: 300,
            ..CompactionSettings::default()
        };
        let preparation = prepare_compaction(&messages, settings).expect("preparation");
        assert!(preparation.previous_summary.is_none());
        let (read_files, modified_files) = preparation.file_ops.compute_file_lists();
        assert!(!read_files.is_empty());
        assert!(modified_files.is_empty());
    }

    #[tokio::test]
    async fn compact_replaces_prefix_with_summary() {
        let mut messages = vec![user("first question")];
        for index in 0..8 {
            messages.push(assistant(&format!("answer {index} {}", "y".repeat(400))));
            messages.push(user(&format!("follow-up {index} {}", "x".repeat(400))));
        }
        let stream = std::sync::Arc::new(SummaryStream::new("## Goal\nship it"));
        let compacted = compact_history(
            &messages,
            &model(),
            &(stream.clone() as SharedStreamFn),
            CompactionSettings {
                keep_recent_tokens: 100,
                ..CompactionSettings::default()
            },
            Some("keep the API notes"),
            None,
        )
        .await
        .expect("compact");

        let summary = extract_summary(&compacted[0]).expect("summary message");
        assert!(summary.contains("ship it"));
        assert!(!summary.contains("<read-files>"));
        assert_eq!(compacted.len(), 2);
        // The retained tail starts at a valid cut point (never a tool result).
        assert_eq!(compacted[1].role, Role::User);

        let prompts = stream.prompts();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("<conversation>"));
        assert!(prompts[0].contains("Additional focus: keep the API notes"));
    }

    #[tokio::test]
    async fn compact_splits_an_oversized_turn_into_two_summaries() {
        let mut messages = vec![user("first question")];
        for index in 0..8 {
            messages.push(assistant(&format!("answer {index} {}", "y".repeat(400))));
            messages.push(user(&format!("follow-up {index} {}", "x".repeat(400))));
        }
        let stream = std::sync::Arc::new(SummaryStream::new("## Goal\nship it"));
        let compacted = compact_history(
            &messages,
            &model(),
            &(stream.clone() as SharedStreamFn),
            CompactionSettings {
                keep_recent_tokens: 200,
                ..CompactionSettings::default()
            },
            None,
            None,
        )
        .await
        .expect("compact");

        let summary = extract_summary(&compacted[0]).expect("summary message");
        assert!(summary.contains("**Turn Context (split turn):**"));
        // Summary + the split turn's retained suffix (assistant + user).
        assert_eq!(compacted.len(), 3);
        assert_eq!(compacted[1].role, Role::Assistant);

        let prompts = stream.prompts();
        assert_eq!(prompts.len(), 2);
        assert!(prompts[0].contains("<conversation>"));
        assert!(prompts[1].contains("PREFIX of a turn that was too large"));
    }

    #[tokio::test]
    async fn compact_updates_previous_summary_iteratively() {
        let mut messages = vec![summary_message("## Goal\nexisting goal")];
        for index in 0..8 {
            messages.push(user(&format!("question {index} {}", "x".repeat(400))));
            messages.push(assistant(&format!("answer {index} {}", "y".repeat(400))));
        }
        let stream = std::sync::Arc::new(SummaryStream::new("## Goal\nupdated goal"));
        let compacted = compact_history(
            &messages,
            &model(),
            &(stream.clone() as SharedStreamFn),
            CompactionSettings {
                keep_recent_tokens: 150,
                ..CompactionSettings::default()
            },
            None,
            None,
        )
        .await
        .expect("compact");

        let prompts = stream.prompts();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("<previous-summary>\n## Goal\nexisting goal"));
        assert!(prompts[0].contains("NEW conversation messages"));
        assert!(extract_summary(&compacted[0])
            .expect("summary")
            .contains("updated goal"));
    }

    #[tokio::test]
    async fn compact_fails_on_incomplete_summary() {
        let mut messages = vec![user("first")];
        for index in 0..8 {
            messages.push(assistant(&format!("answer {index} {}", "y".repeat(400))));
            messages.push(user(&format!("follow-up {index} {}", "x".repeat(400))));
        }
        let mut stream = SummaryStream::new("partial");
        stream.stop_reason = StopReason::MaxTokens;
        let result = compact_history(
            &messages,
            &model(),
            &(std::sync::Arc::new(stream) as SharedStreamFn),
            CompactionSettings {
                keep_recent_tokens: 200,
                ..CompactionSettings::default()
            },
            None,
            None,
        )
        .await;
        assert_eq!(result.unwrap_err(), CompactionError::Incomplete);
    }

    #[tokio::test]
    async fn compact_reports_nothing_to_do() {
        let messages = vec![user("hi"), assistant("hello")];
        let stream = std::sync::Arc::new(SummaryStream::new("nope"));
        let result = compact_history(
            &messages,
            &model(),
            &(stream as SharedStreamFn),
            CompactionSettings::default(),
            None,
            None,
        )
        .await;
        assert_eq!(result.unwrap_err(), CompactionError::NothingToCompact);
    }

    /// A history long enough that `prepare_compaction` always finds work.
    fn long_history() -> Vec<Message> {
        let mut messages = vec![user("first question")];
        for index in 0..8 {
            messages.push(assistant(&format!("answer {index} {}", "y".repeat(400))));
            messages.push(user(&format!("follow-up {index} {}", "x".repeat(400))));
        }
        messages
    }

    fn compact_settings() -> CompactionSettings {
        // A low retention keeps the cut at a turn boundary, so the run makes
        // exactly one summarization call and the call count in the retry
        // tests is not confused with the two calls of a split turn.
        CompactionSettings {
            keep_recent_tokens: 100,
            ..CompactionSettings::default()
        }
    }

    fn agent_retry(max_retries: u32) -> Option<RetryPolicy> {
        // Zero base delay keeps the test's retries sleep-free.
        Some(RetryPolicy::new(max_retries, 0))
    }

    #[tokio::test]
    async fn compact_retries_a_transient_stream_failure() {
        let stream = std::sync::Arc::new(ScriptedSummaryStream::new(vec![
            Attempt::TransportFailure("socket connection was closed unexpectedly"),
            Attempt::Summary("## Goal\nrecovered"),
        ]));
        let compacted = compact_history(
            &long_history(),
            &model(),
            &(stream.clone() as SharedStreamFn),
            compact_settings(),
            None,
            agent_retry(1),
        )
        .await
        .expect("the retried compaction succeeds");

        assert_eq!(stream.calls(), 2);
        assert!(extract_summary(&compacted[0])
            .expect("summary")
            .contains("recovered"));
    }

    #[tokio::test]
    async fn compact_fails_immediately_when_retrying_is_disabled() {
        // Same transient wording as the test above, but no retry budget: the
        // pre-port behaviour must be unchanged.
        let stream = std::sync::Arc::new(ScriptedSummaryStream::new(vec![
            Attempt::TransportFailure("socket connection was closed unexpectedly"),
            Attempt::Summary("## Goal\nrecovered"),
        ]));
        let result = compact_history(
            &long_history(),
            &model(),
            &(stream.clone() as SharedStreamFn),
            compact_settings(),
            None,
            None,
        )
        .await;

        assert_eq!(stream.calls(), 1);
        let error = result.unwrap_err();
        assert_eq!(
            error,
            CompactionError::Stream(
                "malformed stream: socket connection was closed unexpectedly".to_string()
            )
        );
    }

    #[tokio::test]
    async fn compact_retries_an_in_stream_provider_error() {
        let stream = std::sync::Arc::new(ScriptedSummaryStream::new(vec![
            Attempt::ProviderError("provider returned error"),
            Attempt::Summary("## Goal\nrecovered"),
        ]));
        let compacted = compact_history(
            &long_history(),
            &model(),
            &(stream.clone() as SharedStreamFn),
            compact_settings(),
            None,
            agent_retry(2),
        )
        .await
        .expect("the retried compaction succeeds");

        assert_eq!(stream.calls(), 2);
        assert!(extract_summary(&compacted[0])
            .expect("summary")
            .contains("recovered"));
    }

    #[tokio::test]
    async fn compact_does_not_retry_a_non_retryable_provider_error() {
        let stream = std::sync::Arc::new(ScriptedSummaryStream::new(vec![
            Attempt::ProviderError("insufficient_quota"),
            Attempt::Summary("## Goal\nrecovered"),
        ]));
        let result = compact_history(
            &long_history(),
            &model(),
            &(stream.clone() as SharedStreamFn),
            compact_settings(),
            None,
            agent_retry(3),
        )
        .await;

        assert_eq!(stream.calls(), 1, "quota exhaustion is terminal");
        assert_eq!(
            result.unwrap_err(),
            CompactionError::Provider("insufficient_quota".to_string())
        );
    }

    #[tokio::test]
    async fn compact_retries_an_error_stop_reason_without_wording() {
        // `Done` with `stop_reason: error` and no content text has no
        // provider wording to classify, so the wrapper wording (retryable)
        // is used and the call is retried.
        let stream = std::sync::Arc::new(ScriptedSummaryStream::new(vec![
            Attempt::DoneWithErrorStopReason(""),
            Attempt::Summary("## Goal\nrecovered"),
        ]));
        let compacted = compact_history(
            &long_history(),
            &model(),
            &(stream.clone() as SharedStreamFn),
            compact_settings(),
            None,
            agent_retry(1),
        )
        .await
        .expect("the retried compaction succeeds");

        assert_eq!(stream.calls(), 2);
        assert!(extract_summary(&compacted[0])
            .expect("summary")
            .contains("recovered"));
    }

    #[tokio::test]
    async fn compact_returns_the_error_stop_reason_wording_once_retries_run_out() {
        let stream = std::sync::Arc::new(ScriptedSummaryStream::new(vec![
            Attempt::DoneWithErrorStopReason("overloaded"),
        ]));
        let result = compact_history(
            &long_history(),
            &model(),
            &(stream.clone() as SharedStreamFn),
            compact_settings(),
            None,
            agent_retry(2),
        )
        .await;

        assert_eq!(stream.calls(), 3, "1 initial + 2 retries");
        assert_eq!(
            result.unwrap_err(),
            CompactionError::Provider("overloaded".to_string())
        );
    }

    #[tokio::test]
    async fn compact_does_not_retry_a_truncated_summary() {
        // A `stop_reason: max_tokens` summary is deterministic, so the retry
        // budget must not be spent on it.
        let mut truncating = SummaryStream::new("partial");
        truncating.stop_reason = StopReason::MaxTokens;
        let stream = std::sync::Arc::new(truncating);
        let result = compact_history(
            &long_history(),
            &model(),
            &(stream.clone() as SharedStreamFn),
            compact_settings(),
            None,
            agent_retry(3),
        )
        .await;
        assert_eq!(result.unwrap_err(), CompactionError::Incomplete);
        assert_eq!(stream.prompts().len(), 1, "no retry was attempted");
    }

    #[test]
    fn serializes_tool_results_with_truncation() {
        let long = "z".repeat(TOOL_RESULT_MAX_CHARS + 100);
        let messages = vec![
            user("hi"),
            assistant("calling"),
            assistant_with_tool_call("read", "src/lib.rs"),
            tool_result(&long),
        ];
        let serialized = serialize_conversation(&messages);
        assert!(serialized.contains("[User]: hi"));
        assert!(serialized.contains("[Assistant tool calls]: read(path=\"src/lib.rs\")"));
        assert!(serialized.contains("more characters truncated"));
    }
}
