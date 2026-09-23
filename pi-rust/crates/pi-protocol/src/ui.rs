//! UI-friendly type aliases. Stage 4 of the Rust port wires these into
//! the `pi-tui` component layer; downstream crates re-export them as
//! needed.
//!
//! The aliases keep the wire types (`Message`, `Content`, …) the source
//! of truth while letting the TUI write `Message::text("hi")` style
//! helpers without bringing in the full `crate::Message` path.

use crate::{AssistantMessage, Message, ToolCall, ToolResult, Usage};

/// A user-authored text message.
///
/// Convenience alias used by the TUI editor component to hand a single
/// string to the agent loop without allocating a full [`Message`].
pub type UserText = String;

/// Plain text content block — re-exported under a UI-friendly name so
/// the TUI does not need to depend on the wire-type module path.
pub use crate::content::TextContent as UiText;

/// Resolved turn outcome — the final assistant message plus the tool
/// results the loop emitted.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnSummary {
    /// Final assistant message of the turn.
    pub message: AssistantMessage,
    /// Tool result messages produced by the turn (one per tool call).
    pub tool_results: Vec<Message>,
}

/// Summary of a single tool execution — produced by the agent loop for
/// the TUI to render a tool execution block.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecutionRecord {
    /// The originating tool call.
    pub call: ToolCall,
    /// Final tool result.
    pub result: ToolResult,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
}

/// Totals used by the TUI status bar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageTotals {
    /// Cumulative input tokens across the session.
    pub input: u32,
    /// Cumulative output tokens across the session.
    pub output: u32,
    /// Cumulative cached input tokens.
    pub cache_read: u32,
    /// Cumulative cache write tokens.
    pub cache_write: u32,
}

impl UsageTotals {
    /// Empty totals — the initial state of a session.
    pub const fn zero() -> Self {
        Self {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
        }
    }

    /// Accumulate a single turn's usage into the totals.
    pub fn add(&mut self, usage: Usage) {
        self.input = self.input.saturating_add(usage.input);
        self.output = self.output.saturating_add(usage.output);
        self.cache_read = self.cache_read.saturating_add(usage.cache_read);
        self.cache_write = self.cache_write.saturating_add(usage.cache_write);
    }
}
