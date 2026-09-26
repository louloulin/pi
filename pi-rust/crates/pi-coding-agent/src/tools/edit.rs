//! `EditTool` — exact-text replacement with multi-edit support.
//!
//! Port of `createEditTool` from
//! `packages/coding-agent/src/core/tools/edit.ts`. The matcher and diff
//! generation live in [`edit_diff`](super::edit_diff); this module owns the
//! tool contract: argument preparation (including the legacy single-edit
//! form), the read → normalize → match → write pipeline, and the
//! `{ diff, patch, firstChangedLine }` details the TUI renders.
//!
//! Deliberate deviations from upstream, documented at the call site:
//! upstream validates file readability with `fs.access(R_OK | W_OK)` and
//! reports the Node error code (`Error code: EACCES.`); Rust's
//! [`std::io::Error`] has no portable string code, so the `Display` form is
//! used in the `Could not edit file: <path>. <error>.` message. The legacy
//! `old_text` / `new_text` / `replace_all` single-edit arguments are kept as
//! a Rust extension (upstream only accepts `edits[]`); the upstream
//! `edits[]` form is the documented one.

#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::edit_diff::{
    apply_edits_to_normalized_content, detect_line_ending, generate_diff_string,
    generate_unified_patch, normalize_to_lf, restore_line_endings, split_bom, Edit,
};
use super::file_mutation_queue::with_file_mutation_queue;
use super::{AbortLike, AgentTool, ToolError, ToolOutput};

/// `edit` tool — exact-text replacement of one or more disjoint regions.
#[derive(Debug, Default)]
pub struct EditTool;

/// Structured details attached to an [`EditTool`] result.
///
/// Field names match upstream `EditToolDetails` (`diff`, `patch`,
/// `firstChangedLine`) so a renderer can decode either implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolDetails {
    /// Display-oriented, line-numbered diff of the changes made.
    pub diff: String,
    /// Standard unified patch of the changes made.
    pub patch: String,
    /// 1-based line number of the first change in the new file, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_changed_line: Option<usize>,
}

/// Raw arguments as they arrive from the model.
#[derive(Debug, Deserialize)]
struct EditArgs {
    path: String,
    #[serde(default)]
    edits: Option<Value>,
    #[serde(default, alias = "oldText")]
    old_text: Option<String>,
    #[serde(default, alias = "newText")]
    new_text: Option<String>,
    #[serde(default, alias = "replaceAll")]
    replace_all: Option<bool>,
}

/// Turn one JSON value into a single [`Edit`].
fn parse_single_edit(value: &Value) -> Result<Edit, ToolError> {
    serde_json::from_value(value.clone())
        .map_err(|error| ToolError::InvalidArguments(format!("invalid edit entry: {error}")))
}

/// Read either an array of edits or one edit object into `edits`.
fn collect_edits(value: &Value, edits: &mut Vec<Edit>) -> Result<(), ToolError> {
    match value {
        Value::Array(items) => {
            for item in items {
                edits.push(parse_single_edit(item)?);
            }
        }
        Value::Object(_) => edits.push(parse_single_edit(value)?),
        _ => {
            return Err(ToolError::InvalidArguments(
                "Edit tool input is invalid. edits must contain at least one replacement."
                    .to_string(),
            ))
        }
    }
    Ok(())
}

/// Upstream `prepareEditArguments` + `validateEditInput`.
///
/// Accepts `edits` as an array, a single edit object, or a JSON string of
/// either (some models stringify it), then folds in the legacy
/// `old_text` / `new_text` pair.
fn prepare_edits(args: &EditArgs) -> Result<Vec<Edit>, ToolError> {
    let mut edits: Vec<Edit> = Vec::new();

    match &args.edits {
        None | Some(Value::Null) => {}
        Some(Value::String(raw)) => {
            // A failed parse mirrors upstream's swallowed exception: the
            // call then fails the same "at least one replacement" check.
            if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
                collect_edits(&parsed, &mut edits)?;
            }
        }
        Some(value) => collect_edits(value, &mut edits)?,
    }

    if let (Some(old_text), Some(new_text)) = (&args.old_text, &args.new_text) {
        edits.push(Edit {
            old_text: old_text.clone(),
            new_text: new_text.clone(),
        });
    }

    if edits.is_empty() {
        return Err(ToolError::InvalidArguments(
            "Edit tool input is invalid. edits must contain at least one replacement.".to_string(),
        ));
    }

    Ok(edits)
}

fn could_not_edit(path: &str, error: &std::io::Error) -> ToolError {
    ToolError::Execution(format!("Could not edit file: {path}. {error}."))
}

fn details_from(base: &str, updated: &str, path: &str) -> EditToolDetails {
    let diff = generate_diff_string(base, updated, 4);
    EditToolDetails {
        diff: diff.diff,
        patch: generate_unified_patch(path, base, updated, 4),
        first_changed_line: diff.first_changed_line,
    }
}

/// Legacy `replace_all: true` behaviour: exact-match replace every
/// occurrence, keeping the file's original line endings and BOM.
fn execute_replace_all(args: &EditArgs, absolute: &Path) -> Result<ToolOutput, ToolError> {
    let old_text = args.old_text.clone().unwrap_or_default();
    let new_text = args.new_text.clone().unwrap_or_default();

    let raw_content =
        std::fs::read_to_string(absolute).map_err(|error| could_not_edit(&args.path, &error))?;
    let (bom, content) = split_bom(&raw_content);
    let ending = detect_line_ending(content);
    let normalized = normalize_to_lf(content);

    let count = normalized.matches(&old_text).count();
    if old_text.is_empty() || count == 0 {
        return Err(ToolError::Execution(format!(
            "old_text not found in '{}'",
            args.path
        )));
    }
    let updated = normalized.replace(&old_text, &new_text);
    if updated == normalized {
        return Err(ToolError::Execution(format!(
            "edit would not change '{}'",
            args.path
        )));
    }

    let final_content = format!("{bom}{}", restore_line_endings(&updated, ending));
    std::fs::write(absolute, &final_content).map_err(|error| could_not_edit(&args.path, &error))?;

    let details = details_from(&normalized, &updated, &args.path);
    Ok(
        ToolOutput::text(format!("Replaced {count} occurrence(s) in {}.", args.path)).with_details(
            serde_json::to_value(details).expect("EditToolDetails is JSON-serializable"),
        ),
    )
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
        "Edit a single file using exact text replacement. Every edits[].oldText must match a \
         unique, non-overlapping region of the original file. Each edits[].oldText is matched \
         against the original file, not after earlier edits are applied. If two changes affect \
         the same block or nearby lines, merge them into one edit instead of emitting overlapping \
         edits. Do not include large unchanged regions just to connect distant changes."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["path"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit (relative or absolute)"
                },
                "edits": {
                    "type": "array",
                    "minItems": 1,
                    "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["oldText", "newText"],
                        "properties": {
                            "oldText": {
                                "type": "string",
                                "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call."
                            },
                            "newText": {
                                "type": "string",
                                "description": "Replacement text for this targeted edit."
                            }
                        }
                    }
                },
                "old_text": {
                    "type": "string",
                    "description": "Legacy single-edit form: exact text to match. Prefer edits[]."
                },
                "new_text": {
                    "type": "string",
                    "description": "Legacy single-edit form: replacement text. Prefer edits[]."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Legacy: replace every occurrence of old_text instead of requiring a unique match. Defaults to false.",
                    "default": false
                }
            }
        })
    }

    async fn execute(&self, args: Value, abort: AbortLike) -> Result<ToolOutput, ToolError> {
        if abort.is_cancelled() {
            return Err(ToolError::Aborted);
        }

        let parsed: EditArgs = serde_json::from_value(args)
            .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;

        // Hold the per-path mutation lock for the whole read → match → write
        // pipeline so a parallel `write` on the same path cannot race our
        // edit and silently overwrite the result (TS `createEditTool` wraps
        // the whole pipeline in `withFileMutationQueue`).
        with_file_mutation_queue(&parsed.path, async {
            let absolute = crate::paths::absolute(Path::new(&parsed.path));

            if parsed.replace_all.unwrap_or(false) && parsed.edits.is_none() {
                return execute_replace_all(&parsed, &absolute);
            }

            let edits = prepare_edits(&parsed)?;

            let raw_content = std::fs::read_to_string(&absolute)
                .map_err(|error| could_not_edit(&parsed.path, &error))?;
            let (bom, content) = split_bom(&raw_content);
            let ending = detect_line_ending(content);
            let normalized_content = normalize_to_lf(content);

            let applied =
                apply_edits_to_normalized_content(&normalized_content, &edits, &parsed.path)
                    .map_err(ToolError::Execution)?;

            let final_content = format!(
                "{bom}{}",
                restore_line_endings(&applied.new_content, ending)
            );
            std::fs::write(&absolute, &final_content)
                .map_err(|error| could_not_edit(&parsed.path, &error))?;

            let details =
                details_from(&applied.base_content, &applied.new_content, &parsed.path);
            Ok(ToolOutput::text(format!(
                "Successfully replaced {} block(s) in {}.",
                edits.len(),
                parsed.path
            ))
            .with_details(
                serde_json::to_value(details).expect("EditToolDetails is JSON-serializable"),
            ))
        })
        .await
    }
}
