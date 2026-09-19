//! `WriteTool` — create or overwrite a file.
//!
//! Mirrors `createWriteTool` from
//! `packages/coding-agent/src/core/tools/write.ts`. Parent directories are
//! created on demand so the model does not have to mkdir + write as two
//! separate calls.

#![cfg(not(target_arch = "wasm32"))]

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::{AbortLike, AgentTool, ToolError, ToolOutput};

/// `write` tool — create or overwrite a file.
#[derive(Debug, Default)]
pub struct WriteTool;

#[derive(Debug, Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

#[async_trait]
impl AgentTool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn label(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write content to a file. Creates the file if it doesn't exist, \
         overwrites if it does. Parent directories are created automatically."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["path", "content"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write (relative or absolute)."
                },
                "content": {
                    "type": "string",
                    "description": "Full file contents to write."
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

        let parsed: WriteArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidArguments(e.to_string()))?;

        if let Some(parent) = std::path::Path::new(&parsed.path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    ToolError::Execution(format!(
                        "failed to create parent directories for '{}': {}",
                        parsed.path, e
                    ))
                })?;
            }
        }

        std::fs::write(&parsed.path, &parsed.content).map_err(|e| {
            ToolError::Execution(format!("failed to write '{}': {}", parsed.path, e))
        })?;

        Ok(ToolOutput::text(format!(
            "Successfully wrote {} bytes to {}",
            parsed.content.len(),
            parsed.path
        )))
    }
}