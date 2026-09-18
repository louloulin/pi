//! `ReadTool` — read file contents.
//!
//! Mirrors `createReadTool` from `packages/coding-agent/src/core/tools/read.ts`.
//! Truncation and image handling are deliberately omitted in the first cut;
//! the tool returns the full file as a single text block. Stage 2 wires
//! the model-side truncation hints.

#![cfg(not(target_arch = "wasm32"))]

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::{AbortLike, AgentTool, ToolError, ToolOutput};

/// `read` tool — read the contents of a file.
#[derive(Debug, Default)]
pub struct ReadTool;

#[derive(Debug, Deserialize)]
struct ReadArgs {
    path: String,
}

#[async_trait]
impl AgentTool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn label(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read the contents of a file as UTF-8 text. \
         The file must exist and be readable; relative paths resolve \
         against the agent's current working directory."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["path"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative or absolute)."
                }
            }
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        abort: AbortLike,
    ) -> Result<ToolOutput, ToolError> {
        if abort.is_cancelled() {
            return Err(ToolError::Aborted);
        }

        let parsed: ReadArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidArguments(e.to_string()))?;

        let content = std::fs::read_to_string(&parsed.path).map_err(|e| {
            ToolError::Execution(format!("failed to read '{}': {}", parsed.path, e))
        })?;

        Ok(ToolOutput::text(content))
    }
}