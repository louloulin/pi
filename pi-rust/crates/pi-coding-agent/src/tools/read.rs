//! `ReadTool` — read file contents with offset/limit and truncation.
//!
//! Mirrors the text half of `createReadTool` from
//! `packages/coding-agent/src/core/tools/read.ts`. Image support
//! (`utils/image-process.ts`, MIME sniffing) belongs to the image subsystem
//! and is not ported yet: a binary file surfaces as a UTF-8 read error
//! instead of being attached as an image.
//!
//! For text files this port matches upstream byte for byte, including the
//! three continuation notices:
//!
//! * `[Showing lines a-b of N. Use offset=a+1 to continue.]` when the head
//!   truncation stopped on the line limit,
//! * `[Showing lines a-b of N (50.0KB limit). Use offset=…]` when it stopped
//!   on the byte limit,
//! * `[N more lines in file. Use offset=…]` when the caller's own `limit`
//!   stopped early even though truncation had not fired,
//! * `[Line N is xB, exceeds 50.0KB limit. Use bash: sed -n 'Np' <path> | head -c 51200]`
//!   when a single over-long first line cannot be shown at all.

#![cfg(not(target_arch = "wasm32"))]

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::truncate::{
    format_size, truncate_head, TruncatedBy, TruncationOptions, TruncationResult, DEFAULT_MAX_BYTES,
};
use super::{AbortLike, AgentTool, ToolError, ToolOutput};

/// `read` tool — read the contents of a file.
#[derive(Debug, Default)]
pub struct ReadTool;

#[derive(Debug, Deserialize)]
struct ReadArgs {
    path: String,
    /// 1-indexed first line to read (upstream `readSchema.offset`).
    #[serde(default)]
    offset: Option<i64>,
    /// Maximum number of lines to read (upstream `readSchema.limit`).
    #[serde(default)]
    limit: Option<i64>,
}

/// Structured `details` payload for `read`, mirroring upstream
/// `ReadToolDetails`. Only present when the content was actually truncated.
#[derive(Debug, Clone, serde::Serialize)]
struct ReadToolDetails {
    truncation: TruncationResult,
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
         Output is truncated to 2000 lines or 50KB (whichever is hit first). \
         Use offset/limit for large files. When you need the full file, \
         continue with offset until complete."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["path"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative or absolute)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (1-indexed)"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Maximum number of lines to read"
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

        let parsed: ReadArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArguments(e.to_string()))?;

        let content = std::fs::read_to_string(&parsed.path).map_err(|e| {
            ToolError::Execution(format!("failed to read '{}': {}", parsed.path, e))
        })?;

        // Upstream splits on `\n` *without* dropping the trailing empty entry:
        // a file that ends with a newline counts one extra (empty) line, which
        // is what `offset` is validated against. `truncate_head` uses the
        // other convention (trailing newline ignored) via
        // `split_lines_for_counting`.
        let all_lines: Vec<&str> = content.split('\n').collect();
        let total_file_lines = all_lines.len();

        // Convert the 1-indexed input into a 0-indexed array access, clamping
        // negatives to 0 exactly like upstream `Math.max(0, offset - 1)`.
        let start_line = parsed
            .offset
            .map(|o| o.saturating_sub(1).max(0) as usize)
            .unwrap_or(0);
        let start_line_display = start_line + 1;

        if start_line >= all_lines.len() {
            return Err(ToolError::Execution(format!(
                "Offset {} is beyond end of file ({} lines total)",
                parsed.offset.unwrap_or(0),
                total_file_lines
            )));
        }

        let mut user_limited_lines: Option<usize> = None;
        let selected_content = match parsed.limit {
            Some(limit) => {
                let limit = limit.max(0) as usize;
                let end_line = (start_line + limit).min(all_lines.len());
                user_limited_lines = Some(end_line - start_line);
                all_lines[start_line..end_line].join("\n")
            }
            None => all_lines[start_line..].join("\n"),
        };

        let truncation = truncate_head(&selected_content, TruncationOptions::default());

        let output_text = if truncation.first_line_exceeds_limit {
            // The first line alone blows the byte budget; an empty body with a
            // hint is more useful than silence.
            let first_line_size = format_size(all_lines[start_line].len());
            format!(
                "[Line {} is {}, exceeds {} limit. Use bash: sed -n '{}p' {} | head -c {}]",
                start_line_display,
                first_line_size,
                format_size(DEFAULT_MAX_BYTES),
                start_line_display,
                parsed.path,
                DEFAULT_MAX_BYTES
            )
        } else if truncation.truncated {
            let end_line_display = start_line_display + truncation.output_lines - 1;
            let next_offset = end_line_display + 1;
            let mut text = truncation.content.clone();
            match truncation.truncated_by {
                Some(TruncatedBy::Lines) => text.push_str(&format!(
                    "\n\n[Showing lines {}-{} of {}. Use offset={} to continue.]",
                    start_line_display, end_line_display, total_file_lines, next_offset
                )),
                _ => text.push_str(&format!(
                    "\n\n[Showing lines {}-{} of {} ({} limit). Use offset={} to continue.]",
                    start_line_display,
                    end_line_display,
                    total_file_lines,
                    format_size(DEFAULT_MAX_BYTES),
                    next_offset
                )),
            }
            text
        } else if let Some(limited) =
            user_limited_lines.filter(|limited| start_line + limited < all_lines.len())
        {
            let remaining = all_lines.len() - (start_line + limited);
            let next_offset = start_line + limited + 1;
            format!(
                "{}\n\n[{} more lines in file. Use offset={} to continue.]",
                truncation.content, remaining, next_offset
            )
        } else {
            truncation.content.clone()
        };

        let mut output = ToolOutput::text(output_text);
        if truncation.truncated {
            output = output.with_details(
                serde_json::to_value(ReadToolDetails { truncation })
                    .map_err(|e| ToolError::Execution(format!("details encode: {}", e)))?,
            );
        }
        Ok(output)
    }
}
