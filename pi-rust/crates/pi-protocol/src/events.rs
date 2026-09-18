//! Agent + extension event surface.
//!
//! Mirrors the typed event union in `packages/agent/src/types.ts` and the
//! extension event surface in `packages/coding-agent/src/extensions/types.ts`.

use serde::{Deserialize, Serialize};

use crate::{Content, Message, ToolCall, ToolResult, Usage};

/// Final stop reason of an assistant turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Model emitted text/tool calls and stopped naturally.
    Stop,
    /// Model requested a tool invocation; the loop will execute it.
    ToolUse,
    /// Max output tokens hit before a natural stop.
    MaxTokens,
    /// Generation aborted by the user.
    Aborted,
    /// Generation failed with an error message.
    Error,
    /// Model finished its turn with an empty content array.
    Empty,
}

/// Per-token event stream emitted during assistant generation.
///
/// This is the Rust analogue of `AssistantMessageEvent` in
/// `packages/ai/src/types.ts`. The full set of variants will be filled in
/// during Stage 1 alongside the provider ports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantMessageEvent {
    /// Generation started.
    Start {
        /// Model identifier.
        model: String,
    },
    /// A text delta arrived.
    TextDelta {
        /// The new text.
        delta: String,
    },
    /// A reasoning/thinking delta arrived.
    ThinkingDelta {
        /// The new thinking text.
        delta: String,
    },
    /// A tool call arrived (possibly partial).
    ToolCallDelta {
        /// Tool call index.
        index: u32,
        /// Provider-issued identifier (when first seen).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Tool name (when first seen).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Argument fragment.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments_delta: Option<String>,
    },
    /// Generation finished.
    Done {
        /// Final message content.
        content: Vec<Content>,
        /// Stop reason.
        stop_reason: StopReason,
        /// Token usage.
        usage: Usage,
    },
    /// Generation aborted.
    Aborted,
    /// Generation failed.
    Error {
        /// Provider-specific error message.
        message: String,
    },
}

/// Full assistant message returned at the end of a stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantMessage {
    /// Model identifier.
    pub model: String,
    /// Final content blocks.
    pub content: Vec<Content>,
    /// Stop reason.
    pub stop_reason: StopReason,
    /// Token usage.
    pub usage: Usage,
}

/// Lifecycle event delivered to extensions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtensionEvent {
    /// Session started.
    SessionStart,
    /// Session is about to end.
    SessionEnd,
    /// Agent received a user message.
    UserMessage {
        /// The user message.
        message: Message,
    },
    /// Agent is about to call a tool.
    ToolCall {
        /// Tool call about to be executed.
        call: ToolCall,
    },
    /// Tool finished executing.
    ToolResult {
        /// Tool result.
        result: ToolResult,
    },
    /// Agent finished a turn.
    AgentEnd {
        /// The final assistant message.
        message: AssistantMessage,
    },
}

/// UI request delivered to extensions (notify, confirm, input, select, custom).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiRequest {
    /// Non-blocking notification.
    Notify {
        /// Message body.
        message: String,
        /// Severity.
        #[serde(default)]
        level: UiLevel,
    },
    /// Yes/no confirmation.
    Confirm {
        /// Title of the dialog.
        title: String,
        /// Body text.
        body: String,
    },
    /// Free-form text input.
    Input {
        /// Title of the dialog.
        title: String,
        /// Placeholder text.
        #[serde(default)]
        placeholder: Option<String>,
    },
    /// Single-select from a list.
    Select {
        /// Title of the dialog.
        title: String,
        /// Options to choose from.
        options: Vec<String>,
    },
}

/// UI severity — drives colour and icon in the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiLevel {
    /// Informational.
    Info,
    /// Success.
    Success,
    /// Warning.
    Warning,
    /// Error.
    Error,
}

/// Response to a [`UiRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiResponse {
    /// Notify acknowledgment (no payload).
    NotifyAck,
    /// Confirm answer.
    Confirm {
        /// Whether the user accepted.
        accepted: bool,
    },
    /// Input answer.
    Input {
        /// Text the user entered.
        value: String,
    },
    /// Select answer.
    Select {
        /// Selected option (matches one of `UiRequest::Select::options`).
        value: String,
    },
}
