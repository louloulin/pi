//! Tool descriptor shared between the agent runtime and the extension host.
//!
//! Mirrors `packages/coding-agent/src/extensions/types.ts`. The
//! `parameters` field is a JSON Schema object so a single type serves the
//! runtime validation layer (`jsonschema`) and the WASM ABI.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// Tool execution mode — see `packages/agent/src/types.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionMode {
    /// Each tool call is prepared, executed, and finalized before the next one starts.
    Sequential,
    /// Tool calls are prepared sequentially, then allowed tools execute concurrently.
    Parallel,
}

/// Definition of a tool callable by the LLM.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Stable identifier the model uses to invoke the tool.
    pub name: String,
    /// Human-readable label, used in the TUI.
    pub label: String,
    /// Description shown to the model so it knows when to call the tool.
    pub description: String,
    /// JSON Schema describing the tool's parameters object.
    pub parameters: serde_json::Value,
    /// Optional extension-provided metadata (e.g. `{"group": "fs"}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Per-extension state attached to a tool definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolRegistration {
    /// The underlying tool definition.
    #[serde(flatten)]
    pub definition: ToolDefinition,
    /// Extension that owns this tool (used for permission checks).
    pub owner_extension: String,
}

/// Lightweight key/value bag used by extension config.
pub type ExtensionConfig = IndexMap<String, serde_json::Value>;
