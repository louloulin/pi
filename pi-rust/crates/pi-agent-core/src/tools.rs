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
//! Tool calls run sequentially in this stage — every built-in tool is
//! `Sequential` and the parallel path is deferred to a later stage. The
//! [`ToolExecutor::execute`] signature already takes a
//! [`CancellationToken`] so a parallel implementation can fan out without
//! changing the trait.

use async_trait::async_trait;
use pi_protocol::{ToolCall, ToolDefinition, ToolResult};
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
}
