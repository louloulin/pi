//! `ExtensionAPI` shape — the surface the host exposes to extensions.
//!
//! Mirrors `packages/coding-agent/src/extensions/types.ts`.

use std::path::PathBuf;

use pi_protocol::{ExtensionEvent, ToolDefinition, UiRequest};

/// Methods an extension can call on the host.
#[derive(Debug, Clone, Default)]
pub struct ExtensionApiStub {
    _private: (),
}

/// One extension loaded from disk.
#[derive(Debug, Clone)]
pub struct ExtensionEntry {
    /// Absolute path of the extension source (or compiled `.wasm`).
    pub source: PathBuf,
    /// Resolved extension identifier (default: file stem).
    pub id: String,
    /// Optional display label.
    pub label: Option<String>,
}

/// Capability surface the extension has registered.
#[derive(Debug, Clone, Default)]
pub struct ExtensionCapabilities {
    /// Tools registered via `pi.registerTool`.
    pub tools: Vec<ToolDefinition>,
}

/// Bridge between the agent runtime and a loaded extension.
#[async_trait::async_trait]
pub trait ExtensionBridge: Send + Sync {
    /// Deliver an event to the extension. Returns `true` when the
    /// extension subscribed to that event.
    async fn deliver(&self, event: &ExtensionEvent) -> bool;

    /// Ask the extension to handle a UI request and resolve with the
    /// answer. Defaults to a deny/ignore answer.
    async fn ui_request(&self, _request: UiRequest) -> Option<pi_protocol::UiResponse> {
        None
    }
}
