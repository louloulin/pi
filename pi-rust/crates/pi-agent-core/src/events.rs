//! Agent event surface consumed by the TUI and the extension bridge.
//!
//! The variants mirror the `agent.subscribe` event union in
//! `packages/agent/src/agent.ts`. The agent emits these to subscribers
//! via the channel returned by [`Agent::subscribe`](crate::Agent::subscribe).

use pi_protocol::{AssistantMessage, Message, ToolCall, ToolResult};

/// Per-event payload delivered to subscribers of an
/// [`Agent`](crate::Agent).
///
/// The full union aligns with the TS `agent.subscribe` surface:
///
/// * `TurnStart` / `TurnEnd` bracket a single assistant turn.
/// * `MessageStart` / `MessageUpdate` / `MessageEnd` bracket an
///   assistant message and stream text / thinking / tool-call deltas.
/// * `ToolExecutionStart` / `ToolExecutionUpdate` / `ToolExecutionEnd`
///   bracket the execution of a single tool call.
/// * `AgentStart` / `AgentEnd` bracket a single
///   [`Agent::prompt`](crate::Agent::prompt) call.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// A new turn began.
    ///
    /// Fired after the loop has drained pending messages and is about to
    /// call the streaming layer.
    TurnStart,

    /// An assistant message started streaming.
    ///
    /// Followed by zero or more `MessageUpdate` events and a single
    /// `MessageEnd`.
    MessageStart {
        /// Model identifier producing the message.
        model: String,
    },

    /// Incremental assistant message update.
    ///
    /// Carries the latest text delta, thinking delta, or tool-call
    /// delta. Tool-call deltas with the same `index` belong to the same
    /// tool call; the TUI appends them in `index` order.
    MessageUpdate(AssistantMessageUpdate),

    /// Assistant message finished streaming.
    ///
    /// `message` carries the final `AssistantMessage` as the provider
    /// produced it. The TUI uses it to swap the streaming placeholder
    /// for the final block.
    MessageEnd {
        /// Final message produced by the provider.
        message: AssistantMessage,
    },

    /// Tool execution started.
    ToolExecutionStart {
        /// Tool call about to run.
        call: ToolCall,
    },

    /// Incremental tool execution update.
    ///
    /// Today the agent emits a single `ToolExecutionUpdate` with the
    /// latest stdout/stderr text the tool produced; Stage 5 may switch
    /// to a richer streaming surface.
    ToolExecutionUpdate {
        /// Tool call the update belongs to.
        tool_call_id: String,
        /// New partial output the tool produced.
        delta: String,
    },

    /// Tool execution finished.
    ToolExecutionEnd {
        /// Final tool result.
        result: ToolResult,
        /// Wall-clock duration in milliseconds.
        duration_ms: u64,
    },

    /// Turn finished — emitted once per [`AgentLoop::run`](crate::AgentLoop::run)
    /// exit, regardless of stop reason.
    TurnEnd {
        /// Final assistant message of the turn.
        message: AssistantMessage,
        /// Tool result messages produced during the turn.
        tool_results: Vec<Message>,
    },

    /// A user message was enqueued at the start of a turn.
    UserMessage(Message),

    /// Agent surface error (stream / tool / provider). The TUI
    /// surfaces this as a banner and falls back to print mode when the
    /// error indicates the renderer is unusable.
    Error(String),
}

/// One incremental update emitted between `MessageStart` and
/// `MessageEnd`.
#[derive(Debug, Clone)]
pub enum AssistantMessageUpdate {
    /// Text delta — append to the current assistant block.
    TextDelta(String),
    /// Thinking/reasoning delta — append to a (possibly collapsed)
    /// thinking block.
    ThinkingDelta(String),
    /// Tool-call delta — index identifies which tool call the delta
    /// belongs to. `id` and `name` are populated on the first delta
    /// for a given index; subsequent deltas carry only `arguments_delta`.
    ToolCallDelta {
        /// Provider-assigned tool call index.
        index: u32,
        /// Provider-issued identifier (first delta only).
        id: Option<String>,
        /// Tool name (first delta only).
        name: Option<String>,
        /// Argument fragment to append to the JSON arguments string.
        arguments_delta: Option<String>,
    },
}
