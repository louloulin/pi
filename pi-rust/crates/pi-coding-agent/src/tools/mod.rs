//! Built-in agent tools: read, write, edit, bash, find, grep, ls.
//!
//! Mirrors the file-mutation / shell tools exposed by
//! `packages/coding-agent/src/core/tools` in the TypeScript implementation.
//! Each tool implements [`AgentTool`] and contributes a [`ToolDefinition`]
//! that the host can register with the agent loop. JSON Schemas are written
//! explicitly with [`serde_json::json!`] so we don't pull in `schemars` —
//! keeping the wire surface close to what the model actually sees.
//!
//! Native (`std::process`, `std::fs`, `walkdir`) is gated on
//! `#[cfg(not(target_arch = "wasm32"))]` so the `wasm32-unknown-unknown`
//! build keeps compiling for the JS-extension host.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[cfg(not(target_arch = "wasm32"))]
mod bash;
#[cfg(not(target_arch = "wasm32"))]
mod defaults;
#[cfg(not(target_arch = "wasm32"))]
mod edit;
#[cfg(not(target_arch = "wasm32"))]
pub mod edit_diff;
#[cfg(not(target_arch = "wasm32"))]
mod find;
#[cfg(not(target_arch = "wasm32"))]
mod grep;
#[cfg(not(target_arch = "wasm32"))]
mod ls;
#[cfg(not(target_arch = "wasm32"))]
mod mod_ignore;
#[cfg(not(target_arch = "wasm32"))]
mod read;
#[cfg(not(target_arch = "wasm32"))]
pub mod render;
#[cfg(not(target_arch = "wasm32"))]
pub mod text_diff;
#[cfg(not(target_arch = "wasm32"))]
pub mod truncate;
#[cfg(not(target_arch = "wasm32"))]
mod write;

#[cfg(not(target_arch = "wasm32"))]
pub use bash::BashTool;
#[cfg(not(target_arch = "wasm32"))]
pub use defaults::default_tool_bundle;
#[cfg(not(target_arch = "wasm32"))]
pub use edit::{EditTool, EditToolDetails};
#[cfg(not(target_arch = "wasm32"))]
pub use edit_diff::{
    apply_edits_to_normalized_content, compute_edits_diff, generate_diff_string,
    generate_unified_patch, DiffResult, Edit,
};
#[cfg(not(target_arch = "wasm32"))]
pub use find::{FindTool, FindType};
#[cfg(not(target_arch = "wasm32"))]
pub use grep::GrepTool;
#[cfg(not(target_arch = "wasm32"))]
pub use ls::LsTool;
#[cfg(not(target_arch = "wasm32"))]
pub use mod_ignore::{is_ignored_dir_name, relativize_for_search, DEFAULT_IGNORE_NAMES};
#[cfg(not(target_arch = "wasm32"))]
pub use read::ReadTool;
#[cfg(not(target_arch = "wasm32"))]
pub use render::{
    get_text_output, render_diff, render_lines_ansi, render_lines_plain, renderer_for,
    BashRenderer, EditRenderer, FindRenderer, GrepRenderer, LsRenderer, ReadRenderer,
    ToolRenderContext, ToolRenderOptions, ToolRenderSession, ToolRenderer, WriteHighlightCache,
    WriteHighlightStats, WriteRenderer, BASH_PREVIEW_LINES, FIND_FOLD_LINES, GREP_FOLD_LINES,
    LS_FOLD_LINES, READ_FOLD_LINES, WRITE_FOLD_LINES, WRITE_PARTIAL_FULL_HIGHLIGHT_LINES,
};
#[cfg(not(target_arch = "wasm32"))]
pub use write::WriteTool;

use std::sync::Arc;

use async_trait::async_trait;
use pi_protocol::{Content, TextContent, ToolDefinition, ToolExecutionMode};
use thiserror::Error;

/// Cooperative cancellation handle passed into [`AgentTool::execute`].
///
/// Mirrors the role of `AbortSignal` in the TypeScript runtime. The host
/// creates one per tool call and shares it with the tool so the tool can
/// observe user-initiated cancellation between awaits. Bash captures it
/// around `child.wait()` so Ctrl-C still tears the subprocess down.
#[derive(Debug, Clone, Default)]
pub struct AbortLike {
    inner: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl AbortLike {
    /// Construct a handle that never fires.
    pub fn none() -> Self {
        Self::default()
    }

    /// Construct a handle that is already cancelled. Useful in tests and
    /// for tools that want to fail fast when invoked after the deadline.
    pub fn cancelled() -> Self {
        Self {
            inner: Some(std::sync::Arc::new(
                std::sync::atomic::AtomicBool::new(true),
            )),
        }
    }

    /// Wrap a shared atomic flag. The host sets it to `true` to cancel.
    pub fn from_flag(flag: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self { inner: Some(flag) }
    }

    /// True once cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|b| b.load(std::sync::atomic::Ordering::SeqCst))
    }
}

/// Result returned by an [`AgentTool`] invocation.
///
/// This is the tool-local payload; the host wraps it in a [`ToolResult`]
/// (`pi_protocol::ToolResult`) and stamps the tool call id. Tools report
/// failures as `Err(ToolError)` so the host can decide whether to mark the
/// tool result as `is_error`.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// Content blocks returned to the model. Plain tools emit a single text
    /// block; image-returning tools add `Content::Image` alongside.
    pub content: Vec<Content>,
    /// Optional structured details (diff metadata, exit codes, …) the UI
    /// can render without parsing the human-readable content.
    pub details: Option<serde_json::Value>,
}

impl ToolOutput {
    /// Convenience constructor for a single-text-block result.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![Content::Text(TextContent { text: text.into() })],
            details: None,
        }
    }

    /// Attach structured details to an existing output.
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

/// Errors a tool can return to the host.
#[derive(Debug, Error)]
pub enum ToolError {
    /// The tool was cancelled via the [`AbortLike`] handle.
    #[error("operation aborted")]
    Aborted,
    /// Caller-supplied arguments were malformed (wrong types, missing
    /// required fields, etc). The host turns this into an `is_error: true`
    /// tool result so the model can correct the call.
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    /// I/O or runtime failure (file not found, command exited non-zero, …).
    #[error("{0}")]
    Execution(String),
    /// The caller asked for a path outside the workspace sandbox
    /// (e.g. an absolute path or one that escapes via `..`). Mirrors the
    /// sandbox checks `bash` and `read` apply for relative-path inputs.
    #[error("sandbox violation: {0}")]
    SandboxViolation(String),
    /// The tool's input regex / pattern failed to compile. Used by `grep`
    /// when `pattern` is not a valid regex.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}

/// Pluggable tool the agent can invoke.
///
/// All seven built-in tools (`read`, `write`, `edit`, `bash`, `find`,
/// `grep`, `ls`) implement this trait. Extension tools can also implement
/// it directly, although the canonical extension path goes through
/// `pi-extensions` + WASM.
#[async_trait]
pub trait AgentTool: Send + Sync {
    /// Stable identifier the model uses to call the tool (e.g. `"bash"`).
    fn name(&self) -> &str;

    /// Human-readable label, surfaced in the TUI.
    fn label(&self) -> &str;

    /// Description shown to the model so it knows when to call the tool.
    fn description(&self) -> &str;

    /// JSON Schema describing the tool's parameter object.
    fn parameters(&self) -> serde_json::Value;

    /// Optional execution-mode override. Defaults to `None` (parallel),
    /// matching the default [`ToolExecutionMode::Parallel`] applied by the
    /// agent loop.
    fn execution_mode(&self) -> Option<ToolExecutionMode> {
        None
    }

    /// Execute the tool with the parsed arguments. `args` is the JSON
    /// object the host extracted from the model's tool call; it has not yet
    /// been validated against [`parameters`](Self::parameters).
    async fn execute(
        &self,
        args: serde_json::Value,
        abort: AbortLike,
    ) -> Result<ToolOutput, ToolError>;

    /// Convenience builder for the wire [`ToolDefinition`].
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            label: self.label().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters(),
            metadata: None,
        }
    }
}

/// Boxed, type-erased [`AgentTool`] used by the host's tool registry.
pub type DynAgentTool = Arc<dyn AgentTool>;

/// Built-in tool bundle: `read`, `write`, `edit`, `bash`, `find`, `grep`,
/// `ls`.
///
/// The order is the order the model sees in its system prompt and the
/// order the host registers them. `read` leads because it is the safest
/// tool the model uses most often; `ls` trails because it is the most
/// navigation-only.
///
/// On `wasm32-unknown-unknown` the file / shell tools cannot run natively,
/// so this returns an empty bundle; the JS-extension host provides
/// equivalents via the WASM ABI in Stage 3.
pub fn standard_tools() -> Vec<DynAgentTool> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        vec![
            Arc::new(ReadTool),
            Arc::new(WriteTool),
            Arc::new(EditTool),
            Arc::new(BashTool),
            Arc::new(FindTool),
            Arc::new(GrepTool),
            Arc::new(LsTool),
        ]
    }
    #[cfg(target_arch = "wasm32")]
    {
        Vec::new()
    }
}
