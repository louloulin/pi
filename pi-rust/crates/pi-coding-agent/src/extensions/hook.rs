//! `tool_call` / `tool_result` hooks for the coding agent (LUM-1330).
//!
//! Before this module the port delivered both events from the agent
//! fan-out — *after* the tool had already run — so a plugin could observe
//! them but never act on them. Upstream, both are hooks:
//!
//! | event | upstream contract |
//! | --- | --- |
//! | `tool_call` | fired before execution; a handler may block the call (`{ block, reason, terminate }`) or patch the arguments by mutating `event.input` |
//! | `tool_result` | fired after execution; a handler may replace `content` / `details` / `isError` before the model sees it |
//!
//! [`ExtensionToolHooks`] implements the agent loop's
//! [`BeforeToolCall`](pi_agent_core::BeforeToolCall) /
//! [`AfterToolCall`](pi_agent_core::AfterToolCall) surfaces on top of
//! [`ExtensionRuntime`], so the loop's existing block / terminate /
//! result-rewrite machinery applies unchanged.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_agent_core::{AfterToolCall, BeforeToolCall, BeforeToolCallDecision};
use pi_protocol::{Content, TextContent, ToolCall, ToolResult};

use crate::extensions::wiring::ExtensionRuntime;

/// Bridges the extension host into the agent loop's tool hooks.
#[derive(Debug)]
pub struct ExtensionToolHooks {
    runtime: Arc<ExtensionRuntime>,
    /// Calls observed by `before_tool_call`, keyed by tool call id.
    ///
    /// `AfterToolCall` only receives the result, while upstream's
    /// `tool_result` event also carries `toolName` and `input`; the before
    /// hook stashes them here (a plain `Mutex`, never held across an
    /// `await`) and the after hook takes them back out.
    calls: Mutex<HashMap<String, ToolCall>>,
}

impl ExtensionToolHooks {
    /// Wrap a runtime.
    pub fn new(runtime: Arc<ExtensionRuntime>) -> Self {
        Self {
            runtime,
            calls: Mutex::new(HashMap::new()),
        }
    }
}

/// Install the tool hooks on `agent` — but only when a loaded extension
/// actually subscribed to one of them.
///
/// The hooks sit on the hot path of every tool call, so a runtime with no
/// `tool_call` / `tool_result` subscriber gets no hook at all rather than a
/// hook that immediately returns.
pub fn install_tool_hooks(
    agent: &mut pi_agent_core::Agent,
    runtime: Option<&Arc<ExtensionRuntime>>,
) {
    let Some(runtime) = runtime else {
        return;
    };
    let wants_call = runtime.has_subscriber_for("tool_call");
    let wants_result = runtime.has_subscriber_for("tool_result");
    if !wants_call && !wants_result {
        return;
    }
    let hooks = Arc::new(ExtensionToolHooks::new(runtime.clone()));
    let adapter = agent.hooks_mut();
    if wants_call {
        adapter.before_tool_call = Some(hooks.clone());
    }
    if wants_result {
        adapter.after_tool_call = Some(hooks);
    }
}

#[async_trait]
impl BeforeToolCall for ExtensionToolHooks {
    async fn before_tool_call(&self, call: &ToolCall) -> BeforeToolCallDecision {
        if let Ok(mut calls) = self.calls.lock() {
            calls.insert(call.id.clone(), call.clone());
        }
        let outcome = self.runtime.dispatch_tool_call(call).await;
        if outcome.blocked {
            let mut decision = BeforeToolCallDecision::block(
                outcome
                    .reason
                    .clone()
                    .unwrap_or_else(|| "blocked by extension".to_string()),
            );
            decision.terminate = outcome.terminate;
            return decision;
        }
        let mut decision = BeforeToolCallDecision::allow();
        decision.terminate = outcome.terminate;
        decision.input = Some(outcome.input);
        decision
    }
}

#[async_trait]
impl AfterToolCall for ExtensionToolHooks {
    async fn after_tool_call(&self, result: &mut ToolResult) {
        let call = self
            .calls
            .lock()
            .ok()
            .and_then(|mut calls| calls.remove(&result.tool_call_id))
            // A call nobody prepared (an unknown id from a custom executor)
            // still gets the hook, with the payload upstream would send for
            // a tool that reported no arguments.
            .unwrap_or_else(|| ToolCall {
                id: result.tool_call_id.clone(),
                name: String::new(),
                arguments: serde_json::Value::Object(serde_json::Map::new()),
            });
        let Some(patch) = self.runtime.dispatch_tool_result(&call, result).await else {
            return;
        };
        if let Some(content) = patch.content {
            match content.len() {
                0 => *result.content = Content::Text(TextContent::default()),
                1 => {
                    if let Some(block) = content.into_iter().next() {
                        *result.content = block;
                    }
                }
                // `ToolResult::content` carries one block, upstream's array
                // carries many. Replacing a multi-block result with its first
                // block would silently drop output, so the original stands and
                // the divergence is reported instead.
                many => {
                    tracing::warn!(
                        target: "pi_extension",
                        blocks = many,
                        tool_call_id = %result.tool_call_id,
                        "tool_result handler returned {many} content blocks; \
                         this port can carry one — keeping the original content"
                    );
                }
            }
        }
        if let Some(is_error) = patch.is_error {
            result.is_error = is_error;
        }
        if let Some(details) = patch.details {
            result.details = Some(details);
        }
    }
}
