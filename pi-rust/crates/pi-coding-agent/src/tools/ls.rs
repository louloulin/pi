//! `LsTool` — list a directory's contents.
//!
//! Mirrors `createLsTool` from `packages/coding-agent/src/core/tools/ls.ts`.
//! Single-layer listing (no recursion), alphabetical sort, optional
//! hidden-file inclusion, optional detailed listing (`ls -l` style with
//! size + mtime).
//!
//! Behaviour:
//!
//! - `path` must be a directory and must be relative to the agent's
//!   cwd; absolute paths and `..` are rejected with a sandbox error.
//! - `all = false` (default) filters entries that start with `.`.
//! - `detail = false` (default) prints one name per line; `detail = true`
//!   prints `<type> <size> <mtime> <name>` per line, mirroring `ls -l`.
//! - Directories are sorted before files, then alphabetically
//!   (case-sensitive). The model gets a stable, predictable order.

#![cfg(not(target_arch = "wasm32"))]

use std::fs;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;

use super::mod_ignore::relativize_for_search;
use super::truncate::{format_size, truncate_head, TruncationOptions, DEFAULT_MAX_BYTES};
use super::{AbortLike, AgentTool, ToolError, ToolOutput};
use pi_protocol::ToolExecutionMode;

/// `ls` tool — list a single directory.
#[derive(Debug, Default)]
pub struct LsTool;

#[derive(Debug, Deserialize)]
struct LsArgs {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    all: Option<bool>,
    #[serde(default)]
    detail: Option<bool>,
}

#[async_trait]
impl AgentTool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }

    fn label(&self) -> &str {
        "ls"
    }

    fn description(&self) -> &str {
        "List directory contents. Single-layer (no recursion). By default \
         hides dotfiles; pass `all: true` to show them. With `detail: \
         true`, each entry prints as '<type> <size> <mtime> <name>' \
         (similar to `ls -l`). Directories sort before files, then \
         alphabetically. The listing is truncated to 50KB. `path` must be \
         relative to cwd and may not contain '..' or be absolute."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to list (relative to cwd). Defaults to '.'."
                },
                "all": {
                    "type": "boolean",
                    "description": "Include dotfiles. Defaults to false."
                },
                "detail": {
                    "type": "boolean",
                    "description": "Show size and mtime columns (ls -l style). Defaults to false."
                }
            }
        })
    }

    fn execution_mode(&self) -> Option<ToolExecutionMode> {
        Some(ToolExecutionMode::Sequential)
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        abort: AbortLike,
    ) -> Result<ToolOutput, ToolError> {
        if abort.is_cancelled() {
            return Err(ToolError::Aborted);
        }

        let parsed: LsArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidArguments(e.to_string()))?;

        let cwd = std::env::current_dir().map_err(|e| {
            ToolError::Execution(format!("failed to resolve current directory: {}", e))
        })?;

        let (dir, _) =
            relativize_for_search(parsed.path.as_deref().unwrap_or("."), &cwd)?;

        let meta = fs::metadata(&dir).map_err(|e| {
            ToolError::Execution(format!("path not found: {} ({})", dir.display(), e))
        })?;
        if !meta.is_dir() {
            return Err(ToolError::Execution(format!(
                "not a directory: {}",
                dir.display()
            )));
        }

        let show_all = parsed.all.unwrap_or(false);
        let detail = parsed.detail.unwrap_or(false);

        let entries = fs::read_dir(&dir).map_err(|e| {
            ToolError::Execution(format!(
                "cannot read directory {}: {}",
                dir.display(),
                e
            ))
        })?;

        let mut rows: Vec<Entry> = Vec::new();
        for entry in entries {
            if abort.is_cancelled() {
                return Err(ToolError::Aborted);
            }
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if !show_all && name.starts_with('.') {
                continue;
            }
            let entry_meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            rows.push(Entry::new(name, entry_meta));
        }

        rows.sort();

        if rows.is_empty() {
            return Ok(ToolOutput::text("(empty directory)"));
        }

        let text = if detail {
            render_detail(&rows)
        } else {
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join("\n")
        };

        // Byte-limit the listing. There is no separate line limit because the
        // entry count is already capped (`ls.ts:139-140`).
        let truncation = truncate_head(&text, TruncationOptions::bytes_only(DEFAULT_MAX_BYTES));
        let mut text = truncation.content.clone();
        let mut notices: Vec<String> = Vec::new();
        let mut details = serde_json::json!({});
        if truncation.truncated {
            notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
            details["truncation"] = serde_json::to_value(&truncation)
                .map_err(|e| ToolError::Execution(format!("details encode: {}", e)))?;
        }
        if !notices.is_empty() {
            text.push_str("\n[");
            text.push_str(&notices.join(". "));
            text.push(']');
        }

        let details = if details.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            None
        } else {
            Some(details)
        };

        Ok(match details {
            Some(d) => ToolOutput::text(text).with_details(d),
            None => ToolOutput::text(text),
        })
    }
}

/// Single directory entry, with everything `ls -l` needs.
struct Entry {
    name: String,
    is_dir: bool,
    /// Size in bytes; meaningless for directories.
    size: u64,
    /// Modification time as Unix epoch seconds.
    mtime: i64,
}

impl Entry {
    fn new(name: String, meta: fs::Metadata) -> Self {
        let is_dir = meta.is_dir();
        let size = if is_dir { 0 } else { meta.len() };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Self {
            name,
            is_dir,
            size,
            mtime,
        }
    }
}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Directories before files, then alphabetically by name
        // (case-sensitive). `is_dir: true > false`, so we invert the
        // comparison so directories sort first.
        other
            .is_dir
            .cmp(&self.is_dir)
            .then_with(|| self.name.cmp(&other.name))
    }
}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Eq for Entry {}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.is_dir == other.is_dir && self.name == other.name
    }
}

/// Render `ls -l` style lines. The first character is `d` for
/// directories, `-` for files (matches POSIX conventions).
fn render_detail(rows: &[Entry]) -> String {
    let mut out = String::new();
    for r in rows {
        let type_char = if r.is_dir { 'd' } else { '-' };
        let mtime = format_mtime(r.mtime);
        out.push_str(&format!(
            "{} {:>10} {} {}\n",
            type_char,
            r.size,
            mtime,
            r.name
        ));
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Format a Unix epoch (seconds) as ISO 8601 in UTC, e.g. `2024-04-29 12:34:56`.
fn format_mtime(epoch_secs: i64) -> String {
    if epoch_secs <= 0 {
        return "1970-01-01 00:00:00".to_string();
    }
    let dt = DateTime::<Utc>::from_timestamp(epoch_secs, 0)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).unwrap());
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_mtime_known_value() {
        // 2024-01-15T12:34:56Z = 1705322096
        assert_eq!(format_mtime(1705322096), "2024-01-15 12:34:56");
    }

    #[test]
    fn format_mtime_zero() {
        assert_eq!(format_mtime(0), "1970-01-01 00:00:00");
    }
}
