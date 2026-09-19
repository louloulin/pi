//! Adapts the built-in tool bundle to the agent loop's tool executor.
//!
//! `pi-agent-core` knows how to drive tools but not which ones exist; this
//! module is the bridge. [`BuiltinToolExecutor`] owns the default bundle
//! (`read`, `write`, `edit`, `bash`, `find`, `grep`, `ls`) and implements
//! `pi_agent_core::ToolExecutor`, so the loop can advertise those tools to
//! the model and execute the calls it emits.

use std::sync::Arc;

use async_trait::async_trait;
use pi_agent_core::tools::ToolExecutor;
use pi_agent_core::AgentError;
use pi_protocol::{Content, ToolCall, ToolDefinition, ToolResult};
use tokio_util::sync::CancellationToken;

use crate::tools::{default_tool_bundle, AbortLike, DynAgentTool, ToolError};

/// [`ToolExecutor`] backed by the built-in [`AgentTool`] bundle.
///
/// The tool list is held by registration order; [`definitions`](ToolExecutor::definitions)
/// and execution both preserve that order.
pub struct BuiltinToolExecutor {
    tools: Vec<DynAgentTool>,
}

impl BuiltinToolExecutor {
    /// Wrap an explicit tool list.
    pub fn new(tools: Vec<DynAgentTool>) -> Self {
        Self { tools }
    }

    /// Wrap [`default_tool_bundle`].
    pub fn with_default_tools() -> Self {
        Self::new(default_tool_bundle())
    }

    /// The tools this executor owns, in registration order.
    pub fn tools(&self) -> &[DynAgentTool] {
        &self.tools
    }
}

impl std::fmt::Debug for BuiltinToolExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltinToolExecutor")
            .field(
                "tools",
                &self.tools.iter().map(|tool| tool.name()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// Build the default executor as the trait object the agent loop expects.
pub fn default_executor() -> Arc<dyn ToolExecutor> {
    Arc::new(BuiltinToolExecutor::with_default_tools())
}

#[async_trait]
impl ToolExecutor for BuiltinToolExecutor {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|tool| tool.definition()).collect()
    }

    async fn execute(
        &self,
        call: &ToolCall,
        signal: CancellationToken,
    ) -> Result<ToolResult, AgentError> {
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.name() == call.name)
            .ok_or_else(|| AgentError::Tool {
                tool: call.name.clone(),
                message: "unknown tool".to_string(),
            })?;

        // Best-effort bridge between the loop's async cancellation token and
        // the tools' synchronous `AbortLike` flag: a token that has already
        // fired maps to a cancelled handle, and a token that fires mid-call
        // is observed the next time the tool polls the flag.
        let abort = if signal.is_cancelled() {
            AbortLike::cancelled()
        } else {
            AbortLike::none()
        };

        match tool.execute(call.arguments.clone(), abort).await {
            Ok(output) => Ok(ToolResult {
                tool_call_id: call.id.clone(),
                content: Box::new(fold_content(output.content)),
                is_error: false,
                details: output.details,
            }),
            // Cancellation keeps a dedicated error path so callers can tell
            // an abort apart from a tool that legitimately failed.
            Err(ToolError::Aborted) => Err(AgentError::Tool {
                tool: call.name.clone(),
                message: "operation aborted".to_string(),
            }),
            Err(err) => Ok(ToolResult {
                tool_call_id: call.id.clone(),
                content: Box::new(Content::text(err.to_string())),
                is_error: true,
                details: None,
            }),
        }
    }
}

/// Collapse a tool's content blocks into the single block
/// [`ToolResult::content`] carries.
///
/// Every built-in tool emits exactly one text block, so the common path just
/// moves it. Multi-block outputs (an image plus a caption, say) are flattened
/// into one text block so nothing is silently dropped.
fn fold_content(blocks: Vec<Content>) -> Content {
    let mut iter = blocks.into_iter();
    match (iter.next(), iter.next()) {
        (None, _) => Content::text(""),
        (Some(only), None) => only,
        (Some(first), Some(second)) => {
            let mut text = block_text(&first);
            for block in std::iter::once(second).chain(iter) {
                text.push('\n');
                text.push_str(&block_text(&block));
            }
            Content::text(text)
        }
    }
}

/// Render a single content block as text for [`fold_content`].
fn block_text(block: &Content) -> String {
    match block {
        Content::Text(text) => text.text.clone(),
        Content::Image(image) => format!("[image {}]", image.mime_type),
        Content::ToolCall(call) => format!("[tool call {}]", call.name),
        Content::ToolResult(result) => format!("[tool result {}]", result.tool_call_id),
    }
}
