//! Session entry types — mirrors `packages/coding-agent/docs/session-format.md`.
//!
//! The Rust session backend in Stage 5 round-trips these types with the
//! SQLite session backend in the TS monorepo.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AssistantMessage, Message, ToolCall, ToolResult};

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
}
