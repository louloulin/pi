//! `FindTool` — search for files by glob pattern.
//!
//! Mirrors `createFindTool` from
//! `packages/coding-agent/src/core/tools/find.ts`. The TS port delegates
//! the actual search to `fd`, but we run a pure-Rust walk over `walkdir`
//! filtered by a glob pattern compiled with the [`regex`] crate — same
//! surface the model sees, no shelling out.
//!
//! Glob support is intentionally minimal but covers the patterns the
//! model actually emits:
//!
//! - `*`  — any sequence of characters except `/`
//! - `**` — any sequence of characters including `/` (must be a whole path
//!   segment, i.e. surrounded by `/` or at the ends)
//! - `?`  — any single character except `/`
//! - everything else is a literal
//!
//! Output is one matching path per line, relative to the search root,
//! using POSIX separators. Results are sorted lexicographically;
//! `offset` skips the first N matches; `limit` caps the total number
//! returned.
//!
//! The `path` argument must be relative to the agent's current working
//! directory and must not contain `..` — same sandbox model as `bash`.
//!
//! Built-in skip list (`.git`, `node_modules`, `target`, `dist`,
//! `build`, `.pi`) is shared with the other navigation tools via
//! [`super::mod_ignore`].

#![cfg(not(target_arch = "wasm32"))]

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use walkdir::WalkDir;

use super::mod_ignore::{relativize_for_search, to_posix_relative, DEFAULT_IGNORE_NAMES};
use super::truncate::{format_size, truncate_head, TruncationOptions, DEFAULT_MAX_BYTES};
use super::{AbortLike, AgentTool, ToolError, ToolOutput};
use pi_protocol::ToolExecutionMode;

/// `find` tool — search for files by glob pattern.
#[derive(Debug, Default)]
pub struct FindTool;

/// What kind of filesystem entries to include.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FindType {
    /// Only regular files (default).
    #[default]
    File,
    /// Only directories.
    Directory,
    /// Either files or directories.
    Any,
}

const DEFAULT_LIMIT: usize = 1000;

#[derive(Debug, Deserialize)]
struct FindArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    r#type: Option<FindType>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
}

#[async_trait]
impl AgentTool for FindTool {
    fn name(&self) -> &str {
        "find"
    }

    fn label(&self) -> &str {
        "find"
    }

    fn description(&self) -> &str {
        "Search for files by glob pattern. Returns matching paths relative \
         to the search directory, one per line. Common patterns: '*.rs', \
         'src/**/*.toml', '**/*.json'. `path` must be relative to the \
         agent's current working directory (no '..', no absolute paths). \
         Ignores build / VCS directories (.git, node_modules, target, \
         dist, build, .pi). Results are sorted lexicographically and \
         truncated to 50KB."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["pattern"],
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern, e.g. '*.rs' or 'src/**/*.toml'. Supports * (any non-slash), ** (any including /), ? (single non-slash)."
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (relative to cwd). Defaults to '.'."
                },
                "type": {
                    "type": "string",
                    "enum": ["file", "directory", "any"],
                    "description": "Filter by filesystem entry type. Defaults to 'file'."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum number of results. Defaults to 1000."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Skip the first N results. Defaults to 0."
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

        let parsed: FindArgs = serde_json::from_value(args)
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

        let kind = parsed.r#type.unwrap_or_default();
        let effective_limit = parsed.limit.unwrap_or(DEFAULT_LIMIT);
        let offset = parsed.offset.unwrap_or(0);
        let matcher = GlobMatcher::compile(&parsed.pattern)?;

        // Walk the full tree, collecting every match. Sorting and
        // pagination happen *after* the walk so `offset` / `limit`
        // semantics are stable regardless of the order `walkdir`
        // returns entries in. If the caller only cared about the first
        // N matches they can pass `limit`; we still walk everything
        // because making `offset` deterministic requires it.
        let mut collected: Vec<String> = Vec::new();

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

            let file_type = entry.file_type();
            match kind {
                FindType::File if !file_type.is_file() => continue,
                FindType::Directory if !file_type.is_dir() => continue,
                FindType::Any => {}
                FindType::File | FindType::Directory => {}
            }

            // The root itself is reported as "." by walkdir when the
            // search path is a directory; skip it so the caller can
            // treat the result list as "files inside the root".
            if entry.path() == root && entry.depth() == 0 {
                continue;
            }

            let rel = to_posix_relative(&root, entry.path());
            // For directory matches, surface the trailing slash so the
            // caller can tell `find -type d src` from `find -type f src`.
            let candidate = if file_type.is_dir() {
                format!("{}/", rel)
            } else {
                rel
            };

            if !matcher.is_match(&candidate) {
                continue;
            }

            collected.push(candidate);
        }

        collected.sort();

        let total = collected.len();
        let skipped = offset.min(total);
        let page: Vec<String> = collected.into_iter().skip(skipped).take(effective_limit).collect();

        if page.is_empty() {
            let text = if total == 0 {
                "No files found matching pattern".to_string()
            } else {
                // offset was past the end of the result set.
                format!("No results after offset {}", offset)
            };
            return Ok(ToolOutput::text(text));
        }

        let mut text = page.join("\n");
        // Byte-limit the result list; the result limit already capped the row
        // count, so there is no separate line limit (`find.ts:277`).
        let truncation = truncate_head(&text, TruncationOptions::bytes_only(DEFAULT_MAX_BYTES));
        text = truncation.content.clone();
        let mut notices: Vec<String> = Vec::new();
        let mut details = serde_json::json!({});
        // Limit is reached when the caller asked for one and we have
        // *more* matches left after applying offset + limit.
        if parsed.limit.is_some() && skipped + page.len() < total {
            notices.push(format!("{} results limit reached", effective_limit));
            details["resultLimitReached"] = serde_json::json!(effective_limit);
        }
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

/// Decide whether a `walkdir` entry should be skipped entirely.
///
/// We skip the directory itself (so its children are still visited) when
/// its file name matches the hardcoded ignore list. `walkdir` will not
/// descend into the directory after we return `false` from the filter.
fn should_skip_entry(entry: &walkdir::DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return false;
    }
    let name = entry.file_name().to_string_lossy();
    DEFAULT_IGNORE_NAMES.iter().any(|i| *i == name.as_ref())
}

// ---------------------------------------------------------------------------
// Glob matcher
// ---------------------------------------------------------------------------

/// Compiled glob pattern backed by a [`Regex`].
pub(crate) struct GlobMatcher {
    regex: Regex,
    /// Whether the pattern can match directories (we surface this for
    /// tests; the matcher itself is path-shape agnostic).
    #[allow(dead_code)]
    raw: String,
}

impl std::fmt::Debug for GlobMatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobMatcher")
            .field("raw", &self.raw)
            .finish_non_exhaustive()
    }
}

impl GlobMatcher {
    /// Parse `pattern` into a matcher, returning
    /// [`ToolError::InvalidArgument`] if it cannot be compiled.
    pub(crate) fn compile(pattern: &str) -> Result<Self, ToolError> {
        let regex_src = glob_to_regex(pattern);
        let regex = Regex::new(&regex_src).map_err(|e| {
            ToolError::InvalidArgument(format!("invalid glob '{}': {}", pattern, e))
        })?;
        Ok(Self {
            regex,
            raw: pattern.to_string(),
        })
    }

    /// True if `path` (POSIX-separated, may end with `/` for directories)
    /// matches the compiled pattern.
    pub(crate) fn is_match(&self, path: &str) -> bool {
        // Strip the trailing slash that find emits for directory matches
        // so a pattern like `src/**` matches both `src/lib` and
        // `src/lib/`. The pattern itself is already anchored.
        let trimmed = path.trim_end_matches('/');
        let target = if trimmed.is_empty() { "." } else { trimmed };
        self.regex.is_match(target)
    }
}

/// Convert a glob pattern to an anchored regex source string.
///
/// Supported wildcards:
/// - `*` → `[^/]*`
/// - `**` → `.*`
/// - `?` → `[^/]`
/// - everything else literal (regex-special characters are escaped)
fn glob_to_regex(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 4);
    out.push('^');
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    // Collapse `**/` into just `.*/`, which matches
                    // "zero or more segments then a slash". Bare `**`
                    // matches anything (including the empty path).
                    if chars.peek() == Some(&'/') {
                        chars.next();
                        out.push_str("(?:.*/)?");
                    } else {
                        out.push_str(".*");
                    }
                } else {
                    out.push_str("[^/]*");
                }
            }
            '?' => out.push_str("[^/]"),
            // Regex meta characters that need escaping. We escape them
            // even inside "obvious" positions to keep the translation
            // simple and predictable.
            '.' | '(' | ')' | '+' | '|' | '^' | '$' | '{' | '}' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '[' => {
                // Treat character classes as literal `[...]` — we
                // already documented the simple wildcard set. Find a
                // matching `]` so we consume the whole class as literal.
                out.push_str("\\[");
                while let Some(nc) = chars.next() {
                    if nc == ']' {
                        out.push(']');
                        break;
                    } else if nc == '\\' {
                        if let Some(escaped) = chars.next() {
                            out.push_str(&regex::escape(&escaped.to_string()));
                        }
                    } else {
                        out.push_str(&regex::escape(&nc.to_string()));
                    }
                }
            }
            other => out.push(other),
        }
    }
    out.push('$');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_star_matches_basename() {
        let m = GlobMatcher::compile("*.rs").unwrap();
        assert!(m.is_match("foo.rs"));
        // `*` does not cross `/` — use `**/*.rs` for nested.
        assert!(!m.is_match("src/lib.rs"));
        assert!(!m.is_match("foo.txt"));
        assert!(!m.is_match("a/foo.rs"));
    }

    #[test]
    fn glob_doublestar_matches_nested() {
        let m = GlobMatcher::compile("**/*.toml").unwrap();
        assert!(m.is_match("Cargo.toml"));
        assert!(m.is_match("crates/foo/Cargo.toml"));
        assert!(m.is_match("a/b/c.toml"));
        assert!(!m.is_match("Cargo.toml.bak"));
    }

    #[test]
    fn glob_question_matches_single_char() {
        let m = GlobMatcher::compile("file?.txt").unwrap();
        assert!(m.is_match("file1.txt"));
        assert!(m.is_match("file_.txt"));
        assert!(!m.is_match("file12.txt"));
        assert!(!m.is_match("file.txt"));
    }

    #[test]
    fn glob_doublestar_bare() {
        let m = GlobMatcher::compile("**").unwrap();
        assert!(m.is_match("a"));
        assert!(m.is_match("a/b/c"));
        assert!(m.is_match("a.txt"));
    }

    #[test]
    fn glob_literal_path() {
        let m = GlobMatcher::compile("src/lib").unwrap();
        assert!(m.is_match("src/lib"));
        assert!(!m.is_match("src/lib.rs"));
    }

    #[test]
    fn glob_handles_trailing_slash() {
        let m = GlobMatcher::compile("src/**").unwrap();
        assert!(m.is_match("src/lib"));
        assert!(m.is_match("src/lib/"));
    }
}
