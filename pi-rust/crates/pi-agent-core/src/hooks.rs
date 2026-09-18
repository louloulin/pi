//! Hook trait surfaces — `BeforeToolCall` / `AfterToolCall` / `BeforeTurn` /
//! `AfterTurn` — that match the extension-event hook points in
//! `packages/agent/src/types.ts` and the extension hook points in
//! `packages/coding-agent/src/extensions/types.ts`.

use async_trait::async_trait;
use pi_protocol::{AssistantMessage, Message, ToolCall, ToolResult};

/// Hook called before a tool executes.
///
/// Returning `BeforeToolCallDecision::Block` prevents the tool from running;
/// the loop emits an error tool result instead.
#[async_trait]
pub trait BeforeToolCall: Send + Sync {
    /// Inspect / block a tool call.
    async fn before_tool_call(&self, call: &ToolCall) -> BeforeToolCallDecision;
}

/// Hook called after a tool executes. May rewrite the result before the
/// loop emits it.
#[async_trait]
pub trait AfterToolCall: Send + Sync {
    /// Mutate (or replace) a tool result.
    async fn after_tool_call(&self, result: &mut ToolResult);
}

/// Hook called before each assistant turn starts.
#[async_trait]
pub trait BeforeTurn: Send + Sync {
    /// Observe (or rewrite) the messages handed to the model.
    async fn before_turn(&self, messages: &mut Vec<Message>);
}

/// Hook called after each assistant turn finishes.
#[async_trait]
pub trait AfterTurn: Send + Sync {
    /// Observe the final assistant message.
    async fn after_turn(&self, message: &AssistantMessage);
}

/// Decision returned from [`BeforeToolCall::before_tool_call`].
#[derive(Debug, Clone, Default)]
pub struct BeforeToolCallDecision {
    /// True if the tool should be blocked.
    pub block: bool,
    /// Human-readable reason shown in the error tool result.
    pub reason: Option<String>,
    /// Hint that the loop should stop after this tool batch.
    pub terminate: bool,
}

impl BeforeToolCallDecision {
    /// Allow the tool to execute.
    pub fn allow() -> Self {
        Self::default()
    }

    /// Block the tool from executing with the given reason.
    pub fn block(reason: impl Into<String>) -> Self {
        Self {
            block: true,
            reason: Some(reason.into()),
            terminate: false,
        }
    }
}
