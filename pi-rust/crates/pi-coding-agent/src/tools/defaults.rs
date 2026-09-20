//! Default tool bundle exposed to the agent.
//!
//! [`default_tool_bundle`] is the canonical list the agent registers by
//! default. The order matches the order the model sees in its system
//! prompt — keep it stable so the host's tool registry and the model's
//! expectations stay in sync.
//!
//! Currently seven tools are registered: `read`, `write`, `edit`,
//! `bash`, `find`, `grep`, `ls`. The first four land in Stage 2; the
//! navigation tools land in Stage 9.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use super::{BashTool, DynAgentTool, EditTool, FindTool, GrepTool, LsTool, ReadTool, WriteTool};

/// Return the default set of agent tools in registration order.
///
/// On `wasm32-unknown-unknown` this is an empty vector — the file /
/// shell tools cannot run natively, and the JS-extension host provides
/// equivalents through the WASM ABI.
pub fn default_tool_bundle() -> Vec<DynAgentTool> {
    vec![
        Arc::new(ReadTool) as DynAgentTool,
        Arc::new(WriteTool),
        Arc::new(EditTool),
        Arc::new(BashTool),
        Arc::new(FindTool),
        Arc::new(GrepTool),
        Arc::new(LsTool),
    ]
}
