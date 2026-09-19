//! Session entry types — mirrors `packages/coding-agent/docs/session-format.md`.
//!
//! The Rust session backend in Stage 5 round-trips these types with the
//! SQLite session backend in the TS monorepo.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AssistantMessage, Message, ToolCall, ToolResult, Usage};

/// A single row in a session log.
///
/// Variants cover the same shape the TS format uses; new variants land when
/// we port the session writer/reader in Stage 5.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEntry {
    /// Session header — written once at the start of a session.
    Header {
        /// Session identifier.
        id: String,
        /// Wall-clock time the session started.
        created_at: DateTime<Utc>,
        /// Pi version that produced the session.
        version: String,
    },
    /// User message.
    UserMessage(Message),
    /// Assistant message.
    AssistantMessage(AssistantMessage),
    /// Tool call (denormalised for replay).
    ToolCall(ToolCall),
    /// Tool result (denormalised for replay).
    ToolResult(ToolResult),
    /// Free-form extension entry (`pi.appendEntry("foo", payload)`).
    Extension {
        /// Extension that wrote the entry.
        extension: String,
        /// User-supplied kind label.
        kind: String,
        /// Arbitrary JSON payload.
        payload: serde_json::Value,
    },
    /// Compaction checkpoint written by `/compact`.
    ///
    /// Mirrors the `compaction` entry in
    /// `packages/coding-agent/src/core/compaction`: everything up to
    /// this point is replaced by `summary`, while `retained_tail` holds
    /// the recent messages kept verbatim. Replaying a session resets
    /// the conversation to the summary plus that tail.
    Compaction {
        /// Structured summary produced by the summarization model.
        summary: String,
        /// Recent messages kept verbatim after the summary.
        #[serde(default)]
        retained_tail: Vec<Message>,
        /// Estimated context tokens before compaction.
        tokens_before: u32,
        /// Usage reported by the summarization call, when available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        /// Extra details (read / modified file lists).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
    },
}
