//! `GrepTool` — search file contents with a regex.
//!
//! Mirrors `createGrepTool` from
//! `packages/coding-agent/src/core/tools/grep.ts`. The TS port shells out
//! to `ripgrep` (`rg --json`); we run a pure-Rust walk over `walkdir`
//! and match each line with the [`regex`] crate. Same wire shape, no
//! shelling out, easier to test in CI.
//!
//! Behaviour:
//!
//! - `pattern` is a Rust-flavored regex (same surface area as
//!   `ripgrep --regexp` for the patterns we care about — anchors,
//!   character classes, alternation, repetitions).
//! - `path` may point to a file or directory; directories are walked
//!   recursively with the same ignore list as [`find`](super::find).
//! - `include` is a glob (same syntax as `find`) to restrict which
//!   files are scanned.
//! - `ignore_case` toggles case-insensitive matching.
//! - `context` is the number of context lines printed before and after
//!   each match. Context uses a per-file state machine so we never
//!   have to keep the whole file in memory.
//! - `limit` caps the number of matches returned; `offset` skips the
//!   first N matches. These compose: at most `offset + limit` matches
//!   are scanned.
//! - Files that look binary (a NUL byte in the first 8 KiB) are skipped
//!   without surfacing an error, matching the TS port's behaviour.
//!
//! Output format is `<relpath>:<line>:<content>` for matched lines and
//! `<relpath>-<line>-<content>` for context lines — same as ripgrep's
//! default presentation when context is requested.

#![cfg(not(target_arch = "wasm32"))]

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use async_trait::async_trait;
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_json::json;
use walkdir::WalkDir;

use super::find::GlobMatcher;
use super::mod_ignore::{relativize_for_search, to_posix_relative, DEFAULT_IGNORE_NAMES};
use super::truncate::{
    format_size, truncate_head, truncate_line, TruncationOptions, DEFAULT_MAX_BYTES,
    GREP_MAX_LINE_LENGTH,
};
use super::{AbortLike, AgentTool, ToolError, ToolOutput};
use pi_protocol::ToolExecutionMode;

/// `grep` tool — regex search over files.
#[derive(Debug, Default)]
pub struct GrepTool;

/// Soft byte budget for the binary-file sniff. Mirrors the TS port's
/// behaviour: read up to this many bytes from the top of the file; if
/// any NUL byte shows up, skip the file.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

const DEFAULT_LIMIT: usize = 100;

#[derive(Debug, Deserialize)]
struct GrepArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    include: Option<String>,
    #[serde(default)]
    ignore_case: Option<bool>,
    #[serde(default)]
    context: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
}

#[async_trait]
impl AgentTool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn label(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents for a regex pattern. Returns matching lines \
         as `<relpath>:<line>:<content>` (with `-line-` separators for \
         context). `path` may be a file or directory (recursive); `include` \
         is a glob like '*.rs' that restricts which files are scanned. \
         `ignore_case` toggles case-insensitive matching. `context` adds \
         N lines before/after each match. `limit` caps results (default \
         100); `offset` skips the first N matches. Binary files (those \
         containing a NUL byte in the first 8 KiB) are skipped silently. \
         Long match lines are truncated to 500 chars, and the whole result \
         block is truncated to 50KB. \
         `path` must be relative to cwd and may not contain '..'."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["pattern"],
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for. Uses Rust regex syntax (compatible with ripgrep's --regexp)."
                },
                "path": {
                    "type": "string",
                    "description": "Directory or file to search (relative to cwd). Defaults to '.'."
                },
                "include": {
                    "type": "string",
                    "description": "Glob restricting which files are scanned, e.g. '*.rs' or '**/*.toml'."
                },
                "ignore_case": {
                    "type": "boolean",
                    "description": "Case-insensitive matching. Defaults to false."
                },
                "context": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Number of context lines to show before and after each match. Defaults to 0."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum number of matches. Defaults to 100."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Skip the first N matches. Defaults to 0."
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

        let parsed: GrepArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidArguments(e.to_string()))?;

        let cwd = std::env::current_dir().map_err(|e| {
            ToolError::Execution(format!("failed to resolve current directory: {}", e))
        })?;

        let (root, _) = relativize_for_search(parsed.path.as_deref().unwrap_or("."), &cwd)?;

        if !root.exists() {
            return Err(ToolError::Execution(format!(
                "path not found: {}",
                root.display()
            )));
        }

        let matcher = RegexBuilder::new(&parsed.pattern)
            .case_insensitive(parsed.ignore_case.unwrap_or(false))
            .build()
            .map_err(|e| {
                ToolError::InvalidArgument(format!("invalid regex '{}': {}", parsed.pattern, e))
            })?;

        let include_glob = match parsed.include.as_deref() {
            Some(p) if !p.is_empty() => Some(GlobMatcher::compile(p)?),
            _ => None,
        };

        let effective_limit = parsed.limit.unwrap_or(DEFAULT_LIMIT);
        let offset = parsed.offset.unwrap_or(0);
        let context = parsed.context.unwrap_or(0);

        // Walk every matching file and collect all matches. Sorting
        // and pagination happen after the walk so `offset` semantics
        // are stable regardless of walkdir's visit order.
        let mut collected: Vec<Match> = Vec::new();

        let is_dir = root.is_dir();
        if is_dir {
            let walker = WalkDir::new(&root)
                .follow_links(false)
                .into_iter()
                .filter_entry(|entry| !should_skip_entry(entry));

            for entry in walker {
                if abort.is_cancelled() {
                    return Err(ToolError::Aborted);
                }
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.path().to_path_buf();
                let rel = to_posix_relative(&root, &path);

                if let Some(glob) = &include_glob {
                    if !glob.is_match(&rel) {
                        continue;
                    }
                }

                let outcome = scan_file(&path, &rel, &matcher, context, &abort, usize::MAX);
                if let ScanOutcome::Matches(mut ms, _) = outcome {
                    collected.append(&mut ms);
                }
            }
        } else {
            let basename = root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Some(glob) = &include_glob {
                if !glob.is_match(&basename) {
                    // Allow include to be permissive for a single
                    // explicit file path — the caller named it
                    // explicitly, so honour the request.
                }
            }
            let rel = basename;
            let outcome = scan_file(&root, &rel, &matcher, context, &abort, usize::MAX);
            if let ScanOutcome::Matches(mut ms, _) = outcome {
                collected.append(&mut ms);
            }
        }

        // Stable sort by (file, line) so pagination across multiple
        // files is deterministic.
        collected.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));

        let total = collected.len();
        let skipped = offset.min(total);
        let page: Vec<Match> = collected
            .into_iter()
            .skip(skipped)
            .take(effective_limit)
            .collect();
        let limit_reached = parsed.limit.is_some() && skipped + page.len() < total;

        if page.is_empty() {
            let text = if total == 0 {
                "No matches found".to_string()
            } else {
                format!("No results after offset {}", offset)
            };
            return Ok(ToolOutput::text(text));
        }

        let (mut text, lines_truncated) = render_matches(&page, context == 0);
        // Byte-limit the rendered block. There is no line limit here because
        // the match limit already capped the row count
        // (`grep.ts:281-282`).
        let truncation = truncate_head(&text, TruncationOptions::bytes_only(DEFAULT_MAX_BYTES));
        text = truncation.content.clone();
        let mut notices: Vec<String> = Vec::new();
        let mut details = serde_json::json!({});
        if limit_reached && parsed.limit.is_some() {
            notices.push(format!("{} matches limit reached", effective_limit));
            details["matchLimitReached"] = serde_json::json!(effective_limit);
        }
        if truncation.truncated {
            notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
            details["truncation"] = serde_json::to_value(&truncation)
                .map_err(|e| ToolError::Execution(format!("details encode: {}", e)))?;
        }
        if lines_truncated {
            notices.push(format!(
                "Some lines truncated to {} chars. Use read tool to see full lines",
                GREP_MAX_LINE_LENGTH
            ));
            // Mirror upstream's `details.linesTruncated` so the presentation
            // layer can warn about it without re-deriving the notice text.
            details["linesTruncated"] = serde_json::json!(true);
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

/// One grep match (or context line) with everything we need to render.
#[derive(Debug, Clone)]
struct Match {
    file: String,
    line: usize,
    text: String,
    /// `false` for context lines (printed with `-` separators).
    is_match: bool,
}

enum ScanOutcome {
    /// `exhausted` is `true` when the scan stopped because the budget
    /// ran out (so the caller should stop walking further files).
    Matches(Vec<Match>, #[allow(dead_code)] bool),
    /// File could not be opened or read; treat as a no-op.
    Error,
}

/// Scan a single file for regex matches, honouring the binary-file
/// sniff and the `context` ring buffer.
///
/// `budget` caps how many *match* lines we may emit; context lines do
/// not count. We stop early when the budget runs out and signal
/// `exhausted = true` so the caller can stop walking.
fn scan_file(
    path: &Path,
    rel: &str,
    matcher: &Regex,
    context: usize,
    abort: &AbortLike,
    budget: usize,
) -> ScanOutcome {
    if budget == 0 {
        return ScanOutcome::Matches(Vec::new(), true);
    }

    // Binary sniff — open the file, read up to BINARY_SNIFF_BYTES,
    // bail out if we see a NUL byte.
    {
        let mut f = match File::open(path) {
            Ok(f) => f,
            Err(_) => return ScanOutcome::Error,
        };
        let mut sniff = vec![0u8; BINARY_SNIFF_BYTES];
        match f.read(&mut sniff) {
            Ok(0) => return ScanOutcome::Error,
            Ok(n) => {
                if sniff[..n].contains(&0) {
                    return ScanOutcome::Error;
                }
            }
            Err(_) => return ScanOutcome::Error,
        }
    }

    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return ScanOutcome::Error,
    };
    let reader = BufReader::new(file);

    // Context state:
    //
    // `ring` is a sliding window of the last `context` lines. When we
    // emit a match, we drain the most recent `context` lines from the
    // ring as backward context (unless the previous match already
    // attached them).
    //
    // `forward_left` counts how many post-match context lines we still
    // owe. It is set to `context` every time we emit a match and
    // decremented for each subsequent non-matching line until it
    // reaches zero.
    let mut ring: std::collections::VecDeque<(usize, String)> =
        std::collections::VecDeque::with_capacity(context.saturating_add(1));
    let mut forward_left: usize = 0;

    let mut matches: Vec<Match> = Vec::new();
    let mut emitted: usize = 0;

    for (idx, line_result) in reader.lines().enumerate() {
        if abort.is_cancelled() {
            return ScanOutcome::Error;
        }
        let line_no = idx + 1;
        let line = match line_result {
            Ok(l) => strip_cr(&l),
            Err(_) => return ScanOutcome::Error,
        };

        let matched = matcher.is_match(&line);

        if matched {
            // Backward context: pull the most recent `context` lines
            // out of the ring and attach them as context entries,
            // skipping any that were already attached as forward
            // context for the previous match (those are recorded with
            // `is_match = false` in the result list).
            if context > 0 && !ring.is_empty() {
                let take = context.min(ring.len());
                let start = ring.len() - take;
                for (ln, txt) in ring.iter().skip(start).cloned().collect::<Vec<_>>() {
                    let already = matches
                        .iter()
                        .any(|m| !m.is_match && m.line == ln && m.file == rel);
                    if !already {
                        matches.push(Match {
                            file: rel.to_string(),
                            line: ln,
                            text: txt,
                            is_match: false,
                        });
                    }
                }
            }

            // The match itself.
            matches.push(Match {
                file: rel.to_string(),
                line: line_no,
                text: line.clone(),
                is_match: true,
            });
            emitted += 1;
            forward_left = context;

            // Don't push this line into the ring — it's a match, so
            // subsequent backward-context for a *later* match should
            // start from lines after this one, not include the match
            // line itself (matches always render with `:`).
            ring.clear();

            if emitted >= budget {
                return ScanOutcome::Matches(matches, true);
            }
        } else if forward_left > 0 {
            // Forward context for the previous match.
            matches.push(Match {
                file: rel.to_string(),
                line: line_no,
                text: line.clone(),
                is_match: false,
            });
            forward_left -= 1;
            // Don't push forward-context lines into the ring either;
            // they've already been emitted so attaching them again as
            // backward context would duplicate them.
        } else if context > 0 {
            ring.push_back((line_no, line));
            if ring.len() > context {
                ring.pop_front();
            }
        }
    }

    ScanOutcome::Matches(matches, false)
}

/// Strip a trailing CR (for CRLF line endings).
fn strip_cr(line: &str) -> String {
    if let Some(stripped) = line.strip_suffix('\r') {
        stripped.to_string()
    } else {
        line.to_string()
    }
}

/// Render the matches to the wire format expected by the model:
/// `<relpath>:<line>:<content>` for matches, `<relpath>-<line>-<content>`
/// for context lines.
/// Render the match list, optionally truncating each line to
/// [`GREP_MAX_LINE_LENGTH`]. Upstream only truncates in the
/// `context === 0` case (with context it renders whole blocks), so the
/// caller passes `truncate_lines = context == 0`.
///
/// Returns the rendered text plus whether any line was cut, which drives the
/// `Some lines truncated to 500 chars` notice.
fn render_matches(matches: &[Match], truncate_lines: bool) -> (String, bool) {
    let mut out = String::new();
    let mut any_truncated = false;
    for m in matches {
        let sep = if m.is_match { ':' } else { '-' };
        let (text, was_truncated) = if truncate_lines {
            truncate_line(&m.text, GREP_MAX_LINE_LENGTH)
        } else {
            (m.text.clone(), false)
        };
        any_truncated |= was_truncated;
        out.push_str(&format!("{}{}{}{}{}\n", m.file, sep, m.line, sep, text));
    }
    while out.ends_with('\n') {
        out.pop();
    }
    (out, any_truncated)
}

/// Decide whether a `walkdir` entry should be skipped (same logic as
/// `find`, copied here to keep the modules decoupled).
fn should_skip_entry(entry: &walkdir::DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return false;
    }
    let name = entry.file_name().to_string_lossy();
    DEFAULT_IGNORE_NAMES.iter().any(|i| *i == name.as_ref())
}
