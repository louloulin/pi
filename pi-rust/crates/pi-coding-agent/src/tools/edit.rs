//! `EditTool` — replace a single text occurrence (or all of them) in a file.
//!
//! Mirrors the legacy single-edit form of `createEditTool` from
//! `packages/coding-agent/src/core/tools/edit.ts`. By default `old_text` must
//! match the file exactly once; pass `replace_all: true` to allow zero-or-more
//! matches. The tool surfaces a `diff_summary` + replacement count in
//! [`ToolOutput::details`] so the TUI can render an inline diff without
//! re-running the model.

#![cfg(not(target_arch = "wasm32"))]

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{AbortLike, AgentTool, ToolError, ToolOutput};

/// `edit` tool — exact-text replacement with optional replace-all.
#[derive(Debug, Default)]
pub struct EditTool;

/// Structured details attached to an [`EditTool`] result. Returned as
/// `ToolOutput::details` so the TUI can render a concise diff without
/// having to reconstruct it from the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditToolDetails {
    /// `"2 occurrences replaced"` / `"1 occurrence replaced"`.
    pub diff_summary: String,
    /// Number of replacements applied (always `>= 1` on success).
    pub replaced: usize,
}

#[derive(Debug, Deserialize)]
struct EditArgs {
    path: String,
    old_text: String,
    new_text: String,
    #[serde(default)]
    replace_all: bool,
}

#[async_trait]
impl AgentTool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn label(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace exact text in a file. By default `old_text` must occur \
         exactly once; pass `replace_all: true` to replace every occurrence. \
         Use `write` instead when you want to overwrite the whole file."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["path", "old_text", "new_text"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit (relative or absolute)."
                },
                "old_text": {
                    "type": "string",
                    "description": "Exact text to match. Must be unique in the file unless replace_all is true."
                },
                "new_text": {
                    "type": "string",
                    "description": "Replacement text."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace every occurrence instead of requiring a single match. Defaults to false.",
                    "default": false
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

        let parsed: EditArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidArguments(e.to_string()))?;

        let original = std::fs::read_to_string(&parsed.path).map_err(|e| {
            ToolError::Execution(format!("failed to read '{}': {}", parsed.path, e))
        })?;

        let (updated, replaced) = if parsed.replace_all {
            let count = original.matches(&parsed.old_text).count();
            if count == 0 {
                return Err(ToolError::Execution(format!(
                    "old_text not found in '{}'",
                    parsed.path
                )));
            }
            let new = original.replace(&parsed.old_text, &parsed.new_text);
            (new, count)
        } else {
            let occurrences = original.matches(&parsed.old_text).count();
            if occurrences == 0 {
                return Err(ToolError::Execution(format!(
                    "old_text not found in '{}'",
                    parsed.path
                )));
            }
            if occurrences > 1 {
                return Err(ToolError::Execution(format!(
                    "old_text matches {} places in '{}'; pass replace_all=true or supply more context",
                    occurrences, parsed.path
                )));
            }
            // Safe: we just confirmed exactly one occurrence.
            let new = original.replacen(&parsed.old_text, &parsed.new_text, 1);
            (new, 1)
        };

        if updated == original {
            return Err(ToolError::Execution(format!(
                "edit would not change '{}'",
                parsed.path
            )));
        }

        std::fs::write(&parsed.path, &updated).map_err(|e| {
            ToolError::Execution(format!("failed to write '{}': {}", parsed.path, e))
        })?;

        let diff_summary = if replaced == 1 {
            "1 occurrence replaced".to_string()
        } else {
            format!("{} occurrences replaced", replaced)
        };

        let details = EditToolDetails {
            diff_summary: diff_summary.clone(),
            replaced,
        };

        Ok(ToolOutput::text(diff_summary).with_details(serde_json::to_value(details)
            .expect("EditToolDetails is JSON-serializable")))
    }
}