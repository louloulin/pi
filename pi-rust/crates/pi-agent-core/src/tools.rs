//! Tool execution abstraction.
//!
//! `pi-agent-core` deliberately does not depend on `pi-coding-agent`: this
//! crate knows *how* to drive the loop, not which concrete tools exist. The
//! host therefore injects a [`ToolExecutor`] through
//! [`AgentConfig`](crate::AgentConfig). The loop uses it for two things:
//!
//! 1. advertising the available tool definitions to the model
//!    (`Context::tools`), and
//! 2. dispatching each `Content::ToolCall` the model emits.
//!
//! A batch of tool calls is dispatched according to [`ToolExecutionMode`],
//! mirroring `executeToolCalls` in `packages/agent/src/agent-loop.ts`: the
//! batch runs sequentially when the loop is configured that way *or* any one
//! of its calls belongs to a `Sequential` tool; otherwise the calls are
//! prepared in source order and executed concurrently, with their results
//! still appended in source order. [`ToolExecutor::execution_mode`] is how a
//! host advertises the per-tool mode.

use async_trait::async_trait;
use pi_protocol::{ToolCall, ToolDefinition, ToolExecutionMode, ToolResult};
use tokio_util::sync::CancellationToken;

use crate::agent_loop::AgentError;

/// Dispatches the tool calls a provider emits.
///
/// Implementations live in the host crate (`pi-coding-agent` adapts its
/// built-in tool bundle), which keeps the dependency direction acyclic:
/// `pi-agent-core` → `pi-protocol`, and the host → `pi-agent-core`.
///
/// Implementors are stored as `Arc<dyn ToolExecutor>` inside
/// [`AgentConfig`](crate::AgentConfig), so `Send + Sync + 'static` is
/// required.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Tool definitions the model sees on every turn.
    fn definitions(&self) -> Vec<ToolDefinition>;

    /// Execute a single tool call.
    ///
    /// `signal` is the loop's cooperative cancellation handle; executors
    /// should surface cancellation rather than returning a normal result.
    /// A returned [`Err`] is not fatal to the turn — the loop folds it into
    /// an `is_error: true` tool result so the model can react.
    async fn execute(
        &self,
        call: &ToolCall,
        signal: CancellationToken,
    ) -> Result<ToolResult, AgentError>;

    /// Execution mode for the named tool.
    ///
    /// Drives the batch dispatch in
    /// [`AgentLoop::run`](crate::AgentLoop::run): returning
    /// [`ToolExecutionMode::Sequential`] for any call in a batch serializes
    /// the whole batch, exactly like the TypeScript loop's
    /// `hasSequentialToolCall` check. A tool the executor does not know
    /// about cannot force serialization.
    ///
    /// The default is deliberately [`ToolExecutionMode::Sequential`], which
    /// is *not* the TypeScript default: an executor written before this
    /// method existed keeps the one-call-at-a-time behaviour it was tested
    /// against. Executors that can run tools concurrently opt in, which is
    /// what the built-in [`crate::tools`] bundle does (an unset per-tool mode
    /// maps to [`ToolExecutionMode::Parallel`], matching the upstream
    /// `executionMode === undefined` default).
    fn execution_mode(&self, _tool_name: &str) -> ToolExecutionMode {
        ToolExecutionMode::Sequential
    }
}
