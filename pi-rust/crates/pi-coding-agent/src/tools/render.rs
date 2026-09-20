//! Tool result renderers: presentation for `read` / `write` / `bash` /
//! `find` / `grep` / `ls`.
//!
//! Rust-side port of the upstream `core/tools/renderers/` directory.
//! Upstream splits each tool's presentation (`renderCall` / `renderResult`)
//! from its execution path so a process that only displays tool output does
//! not have to load the execution code or its parameter schema. This module
//! mirrors that split for the six tools whose output benefits from a
//! presentation layer:
//!
//! * [`ReadRenderer`] renders a compact `read <path>` call summary and the
//!   result body with a 10-line fold, delegating highlighting to
//!   [`pi_tui::highlight_code`] when [`pi_tui::get_language_from_path`] maps
//!   the file to a language the highlighter supports.
//! * [`WriteRenderer`] renders `write <path> (<n> lines, <m> bytes)` plus a
//!   highlighted preview of the content being written, backed by
//!   [`WriteHighlightCache`] — an upstream-faithful incremental highlighter
//!   that highlights only the appended delta plus a bounded prefix, never the
//!   whole file again.
//! * [`BashRenderer`] renders `bash <command> (timeout Ns)`, keeps the *tail*
//!   of the captured output when collapsed, folds the tool's own truncation
//!   footer into a single warning line, and reports the wall-clock duration
//!   from the tool's `details.elapsed_ms`.
//! * [`FindRenderer`] / [`GrepRenderer`] / [`LsRenderer`] render the search and
//!   listing tools: a `find <pattern> in <path>` / `grep /<pattern>/ in <path>`
//!   / `ls <path>` call summary and a head-folded result body with the hit-limit
//!   and truncation warnings upstream shows.
//! * [`EditRenderer`] renders `edit <path>` and the tool's `details.diff`
//!   through [`render_diff`], a line-numbered diff with removed/added colours
//!   and word-level inverse-video highlighting on a one-line replacement.
//!
//! Renderers emit [`StyledLine`]s (theme slots, no ANSI). The caller decides
//! how to paint them: [`render_lines_ansi`] paints for a text terminal,
//! [`render_lines_plain`] drops the styling, and the interactive TUI can push
//! the same lines into its buffer. [`ToolRenderSession`] pairs the per-call
//! renderer state with the `ToolCall` / `ToolResult` event stream so the
//! `--print` / text-fallback paths do not re-implement the bookkeeping.
//!
//! Deliberate deviations from upstream, each documented at its call site:
//! upstream's `renderResult` hides a collapsed read entirely and folds errored
//! reads to 10 lines; here a collapsed read shows its first 10 lines and an
//! errored read shows every line unhighlighted (a truncated error message is
//! not useful). The two fold hints also keep upstream's `... (N more lines)`
//! wording but drop its trailing keybinding hint, because this crate has no
//! keybinding-hint component yet; `write`'s `(N lines, M bytes)` summary has no
//! upstream counterpart and is added here because the caller asked for it.
//! The `bash` / `find` / `grep` / `ls` renderers likewise drop that trailing
//! keybinding hint and, unlike upstream's component tree, never emit a leading
//! blank line before a result body or a warning block.

#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_protocol::{Content, ToolCall, ToolResult};
use pi_tui::{SpanStyle, StyledLine, StyledSpan, Theme, ThemeColor};

use super::text_diff::{diff_words, DiffKind};
use super::truncate::{format_size, TruncatedBy, TruncationResult};
use super::ToolOutput;

/// Number of content lines a collapsed `read` result shows before folding.
pub const READ_FOLD_LINES: usize = 10;

/// Number of content lines a collapsed `write` preview shows before folding.
pub const WRITE_FOLD_LINES: usize = 10;

/// Number of leading lines re-highlighted as a block after an incremental
/// `write` update (upstream `WRITE_PARTIAL_FULL_HIGHLIGHT_LINES`).
///
/// Highlights can span several lines (block comments, template literals), so
/// a line that was already highlighted can change meaning when the text after
/// it arrives. Re-running the lexer over this bounded prefix fixes those
/// lines without re-highlighting the whole file on every delta.
pub const WRITE_PARTIAL_FULL_HIGHLIGHT_LINES: usize = 50;

// ---------------------------------------------------------------------------
// Render context / options
// ---------------------------------------------------------------------------

/// Ambient state a renderer needs but that is not part of the tool call or
/// result itself.
///
/// Mirrors the fields upstream's `ToolRenderContext` exposes to
/// `renderCall` / `renderResult`: the working directory paths are shown
/// relative to, whether the block is expanded, whether the result is an error,
/// and whether image attachments may be shown.
#[derive(Debug, Clone)]
pub struct ToolRenderContext {
    /// Working directory `cwd`-relative paths are displayed against.
    pub cwd: PathBuf,
    /// Whether the caller asked for the expanded (unfolded) rendering.
    pub expanded: bool,
    /// Whether the result being rendered is an error.
    pub is_error: bool,
    /// Whether image content may be shown instead of an indicator.
    pub show_images: bool,
}

impl ToolRenderContext {
    /// A collapsed, non-error context rooted at `cwd`.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            expanded: false,
            is_error: false,
            show_images: false,
        }
    }

    /// Toggle the expanded flag.
    pub fn with_expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    /// Toggle the error flag.
    pub fn with_is_error(mut self, is_error: bool) -> Self {
        self.is_error = is_error;
        self
    }

    /// Toggle whether images may be shown.
    pub fn with_show_images(mut self, show_images: bool) -> Self {
        self.show_images = show_images;
        self
    }
}

/// Per-`renderResult` options.
///
/// Kept separate from [`ToolRenderContext`] because upstream passes an
/// `options` object to `renderResult` and the call-site context to
/// `renderCall`; both carry `expanded`, and the result renderer reads this
/// one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolRenderOptions {
    /// Whether the caller asked for the expanded (unfolded) rendering.
    pub expanded: bool,
}

impl ToolRenderOptions {
    /// Collapsed rendering.
    pub fn collapsed() -> Self {
        Self { expanded: false }
    }

    /// Expanded rendering.
    pub fn expanded() -> Self {
        Self { expanded: true }
    }
}

// ---------------------------------------------------------------------------
// Renderer abstraction
// ---------------------------------------------------------------------------

/// Presentation for one tool call and its result.
///
/// The two halves are split exactly like upstream `renderCall` /
/// `renderResult`: `renderCall` runs when the arguments are known (and may
/// cache derived state such as a highlight cache), `renderResult` runs once
/// the tool produced output. An implementation is kept alive per tool-call id
/// by [`ToolRenderSession`].
pub trait ToolRenderer: Send {
    /// Stable tool name this renderer presents (e.g. `"read"`).
    fn name(&self) -> &str;

    /// Render the call summary from the model-supplied arguments.
    fn render_call(&mut self, args: &serde_json::Value, ctx: &ToolRenderContext)
        -> Vec<StyledLine>;

    /// Render the tool result.
    fn render_result(
        &mut self,
        result: &ToolOutput,
        options: &ToolRenderOptions,
        ctx: &ToolRenderContext,
    ) -> Vec<StyledLine>;
}

/// Renderer for a built-in tool name, or `None` when the tool has no
/// presentation layer.
pub fn renderer_for(name: &str) -> Option<Box<dyn ToolRenderer>> {
    match name {
        "read" => Some(Box::new(ReadRenderer::default())),
        "write" => Some(Box::new(WriteRenderer::default())),
        "edit" => Some(Box::new(EditRenderer::default())),
        "bash" => Some(Box::new(BashRenderer)),
        "find" => Some(Box::new(FindRenderer)),
        "grep" => Some(Box::new(GrepRenderer)),
        "ls" => Some(Box::new(LsRenderer)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// read
// ---------------------------------------------------------------------------

/// Presentation for the `read` tool.
///
/// Remembers the path (and offset/limit) from [`ToolRenderer::render_call`] so
/// [`ToolRenderer::render_result`] can pick the highlighting language without
/// re-parsing the call.
#[derive(Debug, Default)]
pub struct ReadRenderer {
    path: Option<String>,
    offset: Option<i64>,
    limit: Option<i64>,
}

impl ReadRenderer {
    /// A renderer with no remembered call state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Path recorded by the last [`ToolRenderer::render_call`].
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }
}

impl ToolRenderer for ReadRenderer {
    fn name(&self) -> &str {
        "read"
    }

    fn render_call(
        &mut self,
        args: &serde_json::Value,
        ctx: &ToolRenderContext,
    ) -> Vec<StyledLine> {
        self.path = path_arg(args);
        self.offset = args.get("offset").and_then(serde_json::Value::as_i64);
        self.limit = args.get("limit").and_then(serde_json::Value::as_i64);

        let mut line = tool_title("read");
        line.push(StyledSpan::new(" ", SpanStyle::PLAIN));
        line.extend(render_tool_path(self.path.as_deref(), ctx));
        if let Some(range) = read_line_range(self.offset, self.limit) {
            line.push(StyledSpan::new(range, SpanStyle::fg(ThemeColor::Warning)));
        }
        vec![line]
    }

    fn render_result(
        &mut self,
        result: &ToolOutput,
        options: &ToolRenderOptions,
        ctx: &ToolRenderContext,
    ) -> Vec<StyledLine> {
        let output = get_text_output(result, ctx.show_images);
        let normalized = replace_tabs(&output);

        // An errored read is never highlighted: the error text is prose, and
        // upstream's `highlightCode` would run the (unreliable) auto-detector
        // over it.
        let language = if ctx.is_error {
            None
        } else {
            self.path.as_deref().and_then(supported_language_for_path)
        };
        let mut lines = match language {
            Some(language) => pi_tui::highlight_code(
                &normalized,
                Some(language),
                SpanStyle::fg(ThemeColor::MdCodeBlock),
            ),
            None => plain_tool_output_lines(&normalized),
        };
        trim_trailing_empty_lines(&mut lines);

        let total = lines.len();
        // Upstream hides a collapsed, non-error read entirely and folds errors
        // to 10 lines; the Rust port shows the first 10 lines and lets errors
        // through unfolded instead.
        let max_lines = if options.expanded || ctx.is_error {
            total
        } else {
            READ_FOLD_LINES
        };
        let mut out: Vec<StyledLine> = lines.into_iter().take(max_lines).collect();
        if total > max_lines {
            out.push(notice_line(format!(
                "... ({} more lines)",
                total - max_lines
            )));
        }
        if let Some(notice) = truncation_notice(result.details.as_ref()) {
            out.push(warning_line(notice));
        }
        out
    }
}

/// `:start` / `:start-end` suffix upstream appends when `offset` or `limit`
/// was passed.
fn read_line_range(offset: Option<i64>, limit: Option<i64>) -> Option<String> {
    if offset.is_none() && limit.is_none() {
        return None;
    }
    let start = offset.unwrap_or(1);
    match limit {
        Some(limit) => Some(format!(":{}-{}", start, start + limit - 1)),
        None => Some(format!(":{start}")),
    }
}

/// Upstream-equivalent truncation notice derived from the `read` tool's
/// `details.truncation` payload.
fn truncation_notice(details: Option<&serde_json::Value>) -> Option<String> {
    let truncation = details?.get("truncation")?.clone();
    let truncation: TruncationResult = serde_json::from_value(truncation).ok()?;
    if !truncation.truncated {
        return None;
    }
    if truncation.first_line_exceeds_limit {
        return Some(format!(
            "[First line exceeds {} limit]",
            format_size(truncation.max_bytes)
        ));
    }
    match truncation.truncated_by {
        Some(TruncatedBy::Lines) => Some(format!(
            "[Truncated: showing {} of {} lines ({} line limit)]",
            truncation.output_lines, truncation.total_lines, truncation.max_lines
        )),
        _ => Some(format!(
            "[Truncated: {} lines shown ({} limit)]",
            truncation.output_lines,
            format_size(truncation.max_bytes)
        )),
    }
}

// ---------------------------------------------------------------------------
// write
// ---------------------------------------------------------------------------

/// Presentation for the `write` tool.
///
/// `render_call` owns the highlighted preview (the content lives in the
/// call's arguments, which is also where upstream streams it from) and keeps
/// the [`WriteHighlightCache`] across repeated calls so a re-render of a
/// growing payload only re-highlights the delta. `render_result` renders the
/// error body — upstream shows nothing for a successful write.
#[derive(Debug, Default)]
pub struct WriteRenderer {
    path: Option<String>,
    cache: Option<WriteHighlightCache>,
}

impl WriteRenderer {
    /// A renderer with no remembered call state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Highlight cache produced by the last `render_call`, if the target path
    /// mapped to a supported language.
    pub fn cache(&self) -> Option<&WriteHighlightCache> {
        self.cache.as_ref()
    }
}

impl ToolRenderer for WriteRenderer {
    fn name(&self) -> &str {
        "write"
    }

    fn render_call(
        &mut self,
        args: &serde_json::Value,
        ctx: &ToolRenderContext,
    ) -> Vec<StyledLine> {
        let path = path_arg(args);
        let content = string_arg(args, "content");
        self.path = path;

        self.cache = match (self.path.as_deref(), content.as_deref()) {
            (Some(path), Some(content)) => {
                WriteHighlightCache::updated(self.cache.take(), path, content)
            }
            _ => None,
        };

        let mut title = tool_title("write");
        title.push(StyledSpan::new(" ", SpanStyle::PLAIN));
        title.extend(render_tool_path(self.path.as_deref(), ctx));

        let Some(content) = content else {
            let mut lines = vec![title];
            lines.push(vec![StyledSpan::new(
                "[invalid content arg - expected string]",
                SpanStyle::fg(ThemeColor::Error),
            )]);
            return lines;
        };

        let mut rendered: Vec<StyledLine> = match self.cache.as_ref() {
            Some(cache) => cache.highlighted_lines().to_vec(),
            None => plain_tool_output_lines(&replace_tabs(&normalize_display_text(&content))),
        };
        trim_trailing_empty_lines(&mut rendered);

        let total = rendered.len();
        title.push(StyledSpan::new(" ", SpanStyle::PLAIN));
        title.push(StyledSpan::new(
            format!("({} lines, {} bytes)", total, content.len()),
            SpanStyle::fg(ThemeColor::Muted),
        ));

        let max_lines = if ctx.expanded {
            total
        } else {
            WRITE_FOLD_LINES
        };
        let mut lines = vec![title];
        lines.extend(rendered.into_iter().take(max_lines));
        if total > max_lines {
            lines.push(notice_line(format!(
                "... ({} more lines, {} total)",
                total - max_lines,
                total
            )));
        }
        lines
    }

    fn render_result(
        &mut self,
        result: &ToolOutput,
        _options: &ToolRenderOptions,
        ctx: &ToolRenderContext,
    ) -> Vec<StyledLine> {
        if !ctx.is_error {
            return Vec::new();
        }
        let output = get_text_output(result, ctx.show_images);
        if output.is_empty() {
            return Vec::new();
        }
        output
            .split('\n')
            .map(|line| vec![StyledSpan::new(line, SpanStyle::fg(ThemeColor::Error))])
            .collect()
    }
}

/// Counters for [`WriteHighlightCache`], exposed so tests (and callers that
/// want to assert on the incremental contract) can tell a full rebuild from
/// an incremental update without inspecting private state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteHighlightStats {
    /// Number of times the cache was built from scratch.
    pub full_rebuilds: u32,
    /// Number of times an appended delta was folded in incrementally.
    pub incremental_updates: u32,
    /// Number of bounded prefix re-highlights performed.
    pub prefix_refreshes: u32,
}

/// Highlighter signature injected into [`WriteHighlightCache`].
///
/// Mirrors `highlightCode(code, lang)`: it is only ever called for languages
/// [`pi_tui::supports_language`] accepted.
pub type HighlightFn = dyn Fn(&str, &str) -> Vec<StyledLine> + Send + Sync;

/// Incremental syntax-highlighting cache for a `write` payload.
///
/// Ports `WriteHighlightCache` from upstream `renderers/write.ts`. The cache
/// is keyed on `(path, language)` and reuses the highlighted lines when the
/// new content is a pure append of the old content:
///
/// * the appended delta is highlighted line by line (the first segment is
///   merged into the previous last line, which upstream also does because a
///   streamed delta can land in the middle of a line), and
/// * the first [`WRITE_PARTIAL_FULL_HIGHLIGHT_LINES`] lines are re-highlighted
///   as one block so multi-line constructs stay correct.
///
/// Anything else (path or language changed, content is not an append) rebuilds
/// from scratch. The cache never re-highlights more than the delta plus the
/// bounded prefix, which is what makes it safe to call on every streaming
/// update.
pub struct WriteHighlightCache {
    raw_path: String,
    lang: &'static str,
    raw_content: String,
    normalized_lines: Vec<String>,
    highlighted_lines: Vec<StyledLine>,
    highlight: Arc<HighlightFn>,
    stats: WriteHighlightStats,
}

impl WriteHighlightCache {
    /// Build a cache for `raw_path` / `content`, or `None` when the path does
    /// not map to a language the highlighter supports.
    pub fn new(raw_path: &str, content: &str) -> Option<Self> {
        Self::with_highlighter(raw_path, content, default_highlight_fn())
    }

    /// Build a cache with an injected highlighter. Used by tests to observe
    /// exactly which slices are re-highlighted, and by callers that want a
    /// different base style.
    pub fn with_highlighter(
        raw_path: &str,
        content: &str,
        highlight: Arc<HighlightFn>,
    ) -> Option<Self> {
        let lang = supported_language_for_path(raw_path)?;
        let normalized = replace_tabs(&normalize_display_text(content));
        let highlighted_lines = highlight(&normalized, lang);
        Some(Self {
            raw_path: raw_path.to_string(),
            lang,
            raw_content: content.to_string(),
            normalized_lines: normalized.split('\n').map(str::to_string).collect(),
            highlighted_lines,
            highlight,
            stats: WriteHighlightStats {
                full_rebuilds: 1,
                ..WriteHighlightStats::default()
            },
        })
    }

    /// Refresh a cache for `content`, reusing `previous` when it can.
    ///
    /// This is the port of upstream `updateWriteHighlightCacheIncremental`:
    /// `None` always rebuilds; a previous cache is extended only for the same
    /// path / language and a purely appended payload. Returns `None` when the
    /// path's language is unsupported (callers fall back to plain text).
    pub fn updated(previous: Option<Self>, raw_path: &str, content: &str) -> Option<Self> {
        let lang = supported_language_for_path(raw_path)?;
        let Some(mut cache) = previous else {
            return Self::with_highlighter(raw_path, content, default_highlight_fn());
        };

        let same_target = cache.lang == lang && cache.raw_path == raw_path;
        if !same_target || !content.starts_with(&cache.raw_content) {
            let rebuilds = cache.stats.full_rebuilds + 1;
            let mut rebuilt =
                Self::with_highlighter(raw_path, content, Arc::clone(&cache.highlight))?;
            rebuilt.stats.full_rebuilds = rebuilds;
            rebuilt.stats.incremental_updates = cache.stats.incremental_updates;
            rebuilt.stats.prefix_refreshes = cache.stats.prefix_refreshes;
            return Some(rebuilt);
        }
        if content.len() == cache.raw_content.len() {
            return Some(cache);
        }
        cache.apply_delta(content);
        Some(cache)
    }

    /// Language the cache highlights with.
    pub fn language(&self) -> &'static str {
        self.lang
    }

    /// Highlighted lines, aligned one-to-one with the source lines.
    pub fn highlighted_lines(&self) -> &[StyledLine] {
        &self.highlighted_lines
    }

    /// Normalized (tabs expanded, `\r` removed) source lines.
    pub fn normalized_lines(&self) -> &[String] {
        &self.normalized_lines
    }

    /// Rebuild / incremental counters.
    pub fn stats(&self) -> WriteHighlightStats {
        self.stats
    }

    /// Fold an appended delta into the cache.
    fn apply_delta(&mut self, content: &str) {
        let delta_raw = &content[self.raw_content.len()..];
        let delta = replace_tabs(&normalize_display_text(delta_raw));
        self.raw_content = content.to_string();

        if self.normalized_lines.is_empty() {
            self.normalized_lines.push(String::new());
            self.highlighted_lines.push(Vec::new());
        }
        if self.highlighted_lines.len() < self.normalized_lines.len() {
            self.highlighted_lines
                .resize(self.normalized_lines.len(), Vec::new());
        }

        let mut segments = delta.split('\n');
        let first = segments.next().unwrap_or_default();
        let last = self.normalized_lines.len() - 1;
        self.normalized_lines[last].push_str(first);
        self.highlighted_lines[last] = self.highlight_line(&self.normalized_lines[last]);
        for segment in segments {
            self.normalized_lines.push(segment.to_string());
            let line = self.highlight_line(segment);
            self.highlighted_lines.push(line);
        }

        self.refresh_prefix();
        self.stats.incremental_updates += 1;
    }

    /// Re-highlight the bounded prefix as one block.
    fn refresh_prefix(&mut self) {
        let count = WRITE_PARTIAL_FULL_HIGHLIGHT_LINES.min(self.normalized_lines.len());
        if count == 0 {
            return;
        }
        let prefix = self.normalized_lines[..count].join("\n");
        let highlight = Arc::clone(&self.highlight);
        let highlighted = highlight(&prefix, self.lang);
        for index in 0..count {
            let line = match highlighted.get(index) {
                Some(line) => line.clone(),
                None => self.highlight_line(&self.normalized_lines[index]),
            };
            self.highlighted_lines[index] = line;
        }
        self.stats.prefix_refreshes += 1;
    }

    /// Highlight a single line, keeping only its first rendered line (a line
    /// never contains a newline).
    fn highlight_line(&self, line: &str) -> StyledLine {
        let mut rendered = (self.highlight)(line, self.lang);
        if rendered.is_empty() {
            return Vec::new();
        }
        rendered.swap_remove(0)
    }
}

/// Default highlighter: `pi_tui::highlight_code` with the markdown code-block
/// base style, matching upstream's `highlightCode` default theme slot.
fn default_highlight_fn() -> Arc<HighlightFn> {
    Arc::new(|code: &str, lang: &str| {
        pi_tui::highlight_code(code, Some(lang), SpanStyle::fg(ThemeColor::MdCodeBlock))
    })
}

impl std::fmt::Debug for WriteHighlightCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteHighlightCache")
            .field("raw_path", &self.raw_path)
            .field("lang", &self.lang)
            .field("content_len", &self.raw_content.len())
            .field("lines", &self.normalized_lines.len())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// bash
// ---------------------------------------------------------------------------

/// Number of trailing output lines a collapsed `bash` result shows
/// (upstream `BASH_PREVIEW_LINES`).
///
/// Unlike [`FIND_FOLD_LINES`] / [`GREP_FOLD_LINES`] / [`LS_FOLD_LINES`], which
/// keep the *head* of the listing, a shell command's interesting output is at
/// the end, so the collapsed preview keeps the tail and reports how many
/// earlier lines were skipped.
pub const BASH_PREVIEW_LINES: usize = 5;

/// Presentation for the `bash` tool.
///
/// Deviation from upstream: upstream grows the elapsed-time label from a
/// wall-clock timer kept in the render context (`startedAt` / `endedAt`,
/// updated by a 1s interval while the call is partial). The Rust port renders a
/// finished result only and takes the duration from the tool's own
/// `details.elapsed_ms`, so it needs neither a timer nor an invalidation hook.
#[derive(Debug, Default)]
pub struct BashRenderer;

impl ToolRenderer for BashRenderer {
    fn name(&self) -> &str {
        "bash"
    }

    fn render_call(
        &mut self,
        args: &serde_json::Value,
        _ctx: &ToolRenderContext,
    ) -> Vec<StyledLine> {
        let command = nullable_str_arg(args, "command");
        let timeout = args
            .get("timeout")
            .and_then(serde_json::Value::as_i64)
            .filter(|timeout| *timeout != 0);

        let mut line = tool_title("bash");
        line.push(StyledSpan::new(" ", SpanStyle::PLAIN));
        match command.as_deref() {
            None => line.push(StyledSpan::new(
                "[invalid arg]",
                SpanStyle::fg(ThemeColor::Error),
            )),
            Some("") => line.push(StyledSpan::new(
                "...",
                SpanStyle::fg(ThemeColor::ToolOutput),
            )),
            Some(command) => line.push(StyledSpan::new(
                command.to_string(),
                SpanStyle::fg(ThemeColor::ToolTitle),
            )),
        }
        if let Some(timeout) = timeout {
            line.push(StyledSpan::new(
                format!(" (timeout {}s)", timeout),
                SpanStyle::fg(ThemeColor::Muted),
            ));
        }
        vec![line]
    }

    fn render_result(
        &mut self,
        result: &ToolOutput,
        options: &ToolRenderOptions,
        ctx: &ToolRenderContext,
    ) -> Vec<StyledLine> {
        let mut output = get_text_output(result, ctx.show_images).trim().to_string();
        let truncation = truncation_from_details(result.details.as_ref(), "truncation");
        let full_output_path = result
            .details
            .as_ref()
            .and_then(|details| details.get("full_output_path"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);

        // The tool already appended its own `[Showing lines … Full output: …]`
        // footer to the captured text; drop it here so the renderer's own
        // warning block is the only one on screen (upstream does the same).
        if let (Some(path), Some(truncation)) = (&full_output_path, &truncation) {
            if truncation.truncated && output.ends_with(']') {
                if let Some(footer_start) = output.rfind("\n\n[") {
                    if output[footer_start..].contains(path) {
                        output = output[..footer_start].trim_end().to_string();
                    }
                }
            }
        }

        let mut lines: Vec<StyledLine> = Vec::new();
        if !output.is_empty() {
            let rendered = plain_tool_output_lines(&output);
            if options.expanded {
                lines.extend(rendered);
            } else {
                let total = rendered.len();
                let start = total.saturating_sub(BASH_PREVIEW_LINES);
                if start > 0 {
                    lines.push(notice_line(format!(
                        "... ({} earlier lines, to expand)",
                        start
                    )));
                }
                lines.extend(rendered.into_iter().skip(start));
            }
        }

        if let Some(truncation) = &truncation {
            let mut warnings: Vec<String> = Vec::new();
            if let Some(path) = &full_output_path {
                warnings.push(format!("Full output: {path}"));
            }
            if truncation.truncated {
                warnings.push(match truncation.truncated_by {
                    Some(TruncatedBy::Lines) => format!(
                        "Truncated: showing {} of {} lines",
                        truncation.output_lines, truncation.total_lines
                    ),
                    _ => format!(
                        "Truncated: {} lines shown ({} limit)",
                        truncation.output_lines,
                        format_size(truncation.max_bytes)
                    ),
                });
            }
            if !warnings.is_empty() {
                lines.push(warning_line(format!("[{}]", warnings.join(". "))));
            }
        } else if let Some(path) = &full_output_path {
            lines.push(warning_line(format!("[Full output: {path}]")));
        }

        if let Some(elapsed_ms) = result
            .details
            .as_ref()
            .and_then(|details| details.get("elapsed_ms"))
            .and_then(serde_json::Value::as_u64)
        {
            lines.push(notice_line(format!(
                "Took {:.1}s",
                elapsed_ms as f64 / 1000.0
            )));
        }
        lines
    }
}

// ---------------------------------------------------------------------------
// find
// ---------------------------------------------------------------------------

/// Number of result lines a collapsed `find` listing shows (upstream `20`).
pub const FIND_FOLD_LINES: usize = 20;

/// Presentation for the `find` tool.
#[derive(Debug, Default)]
pub struct FindRenderer;

impl ToolRenderer for FindRenderer {
    fn name(&self) -> &str {
        "find"
    }

    fn render_call(
        &mut self,
        args: &serde_json::Value,
        _ctx: &ToolRenderContext,
    ) -> Vec<StyledLine> {
        let pattern = nullable_str_arg(args, "pattern");
        let raw_path = nullable_str_arg(args, "path");
        let limit = args.get("limit").and_then(serde_json::Value::as_i64);

        let mut line = tool_title("find");
        line.push(StyledSpan::new(" ", SpanStyle::PLAIN));
        match pattern.as_deref() {
            None => line.push(StyledSpan::new(
                "[invalid arg]",
                SpanStyle::fg(ThemeColor::Error),
            )),
            Some(pattern) => line.push(StyledSpan::new(
                pattern.to_string(),
                SpanStyle::fg(ThemeColor::Accent),
            )),
        }
        line.push(StyledSpan::new(
            " in ",
            SpanStyle::fg(ThemeColor::ToolOutput),
        ));
        line.push(match raw_path.as_deref() {
            None => StyledSpan::new("[invalid arg]", SpanStyle::fg(ThemeColor::Error)),
            Some(raw_path) => {
                let path = if raw_path.is_empty() { "." } else { raw_path };
                StyledSpan::new(shorten_home(path), SpanStyle::fg(ThemeColor::ToolOutput))
            }
        });
        if let Some(limit) = limit {
            line.push(StyledSpan::new(
                format!(" (limit {})", limit),
                SpanStyle::fg(ThemeColor::ToolOutput),
            ));
        }
        vec![line]
    }

    fn render_result(
        &mut self,
        result: &ToolOutput,
        options: &ToolRenderOptions,
        ctx: &ToolRenderContext,
    ) -> Vec<StyledLine> {
        let output = get_text_output(result, ctx.show_images).trim().to_string();
        let mut lines = folded_output_lines(&output, options.expanded, FIND_FOLD_LINES);

        let result_limit = count_from_details(result.details.as_ref(), "resultLimitReached");
        let truncation = truncation_from_details(result.details.as_ref(), "truncation");
        if let Some(warnings) = limit_warnings(
            result_limit.map(|n| format!("{n} results limit")),
            truncation.as_ref(),
            false,
        ) {
            lines.push(warning_line(format!("[Truncated: {warnings}]")));
        }
        lines
    }
}

// ---------------------------------------------------------------------------
// grep
// ---------------------------------------------------------------------------

/// Number of match lines a collapsed `grep` listing shows (upstream `15`).
pub const GREP_FOLD_LINES: usize = 15;

/// Presentation for the `grep` tool.
#[derive(Debug, Default)]
pub struct GrepRenderer;

impl ToolRenderer for GrepRenderer {
    fn name(&self) -> &str {
        "grep"
    }

    fn render_call(
        &mut self,
        args: &serde_json::Value,
        _ctx: &ToolRenderContext,
    ) -> Vec<StyledLine> {
        let pattern = nullable_str_arg(args, "pattern");
        let raw_path = nullable_str_arg(args, "path");
        // Upstream names this argument `glob`; this port's `grep` tool spells it
        // `include`, so accept either spelling.
        let glob = [
            nullable_str_arg(args, "include"),
            nullable_str_arg(args, "glob"),
        ]
        .into_iter()
        .flatten()
        .find(|glob| !glob.is_empty());
        let limit = args.get("limit").and_then(serde_json::Value::as_i64);

        let mut line = tool_title("grep");
        line.push(StyledSpan::new(" ", SpanStyle::PLAIN));
        match pattern.as_deref() {
            None => line.push(StyledSpan::new(
                "[invalid arg]",
                SpanStyle::fg(ThemeColor::Error),
            )),
            Some(pattern) => line.push(StyledSpan::new(
                format!("/{}/", pattern),
                SpanStyle::fg(ThemeColor::Accent),
            )),
        }
        line.push(StyledSpan::new(
            " in ",
            SpanStyle::fg(ThemeColor::ToolOutput),
        ));
        line.push(match raw_path.as_deref() {
            None => StyledSpan::new("[invalid arg]", SpanStyle::fg(ThemeColor::Error)),
            Some(raw_path) => {
                let path = if raw_path.is_empty() { "." } else { raw_path };
                StyledSpan::new(shorten_home(path), SpanStyle::fg(ThemeColor::ToolOutput))
            }
        });
        if let Some(glob) = glob {
            line.push(StyledSpan::new(
                format!(" ({})", glob),
                SpanStyle::fg(ThemeColor::ToolOutput),
            ));
        }
        if let Some(limit) = limit {
            line.push(StyledSpan::new(
                format!(" limit {}", limit),
                SpanStyle::fg(ThemeColor::ToolOutput),
            ));
        }
        vec![line]
    }

    fn render_result(
        &mut self,
        result: &ToolOutput,
        options: &ToolRenderOptions,
        ctx: &ToolRenderContext,
    ) -> Vec<StyledLine> {
        let output = get_text_output(result, ctx.show_images).trim().to_string();
        let mut lines = folded_output_lines(&output, options.expanded, GREP_FOLD_LINES);

        let match_limit = count_from_details(result.details.as_ref(), "matchLimitReached");
        let truncation = truncation_from_details(result.details.as_ref(), "truncation");
        let lines_truncated = result
            .details
            .as_ref()
            .and_then(|details| details.get("linesTruncated"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if let Some(warnings) = limit_warnings(
            match_limit.map(|n| format!("{n} matches limit")),
            truncation.as_ref(),
            lines_truncated,
        ) {
            lines.push(warning_line(format!("[Truncated: {warnings}]")));
        }
        lines
    }
}

// ---------------------------------------------------------------------------
// ls
// ---------------------------------------------------------------------------

/// Number of entry lines a collapsed `ls` listing shows (upstream `20`).
pub const LS_FOLD_LINES: usize = 20;

/// Presentation for the `ls` tool.
#[derive(Debug, Default)]
pub struct LsRenderer;

impl ToolRenderer for LsRenderer {
    fn name(&self) -> &str {
        "ls"
    }

    fn render_call(
        &mut self,
        args: &serde_json::Value,
        ctx: &ToolRenderContext,
    ) -> Vec<StyledLine> {
        let path = nullable_str_arg(args, "path");
        let limit = args.get("limit").and_then(serde_json::Value::as_i64);

        let mut line = tool_title("ls");
        line.push(StyledSpan::new(" ", SpanStyle::PLAIN));
        line.extend(render_tool_path_with_fallback(
            path.as_deref(),
            ctx,
            Some("."),
        ));
        if let Some(limit) = limit {
            line.push(StyledSpan::new(
                format!(" (limit {})", limit),
                SpanStyle::fg(ThemeColor::ToolOutput),
            ));
        }
        vec![line]
    }

    fn render_result(
        &mut self,
        result: &ToolOutput,
        options: &ToolRenderOptions,
        ctx: &ToolRenderContext,
    ) -> Vec<StyledLine> {
        let output = get_text_output(result, ctx.show_images).trim().to_string();
        let mut lines = folded_output_lines(&output, options.expanded, LS_FOLD_LINES);

        // Upstream's `ls` caps entries and records `entryLimitReached`; the Rust
        // tool has no entry cap yet, so this branch only fires if one is added.
        let entry_limit = count_from_details(result.details.as_ref(), "entryLimitReached");
        let truncation = truncation_from_details(result.details.as_ref(), "truncation");
        if let Some(warnings) = limit_warnings(
            entry_limit.map(|n| format!("{n} entries limit")),
            truncation.as_ref(),
            false,
        ) {
            lines.push(warning_line(format!("[Truncated: {warnings}]")));
        }
        lines
    }
}

// ---------------------------------------------------------------------------
// edit
// ---------------------------------------------------------------------------

/// Presentation for the `edit` tool.
///
/// `render_call` shows `edit <path>`; `render_result` renders the
/// `details.diff` string through [`render_diff`], which colours removed and
/// added lines and highlights the changed words inside a one-for-one line
/// replacement with inverse video. Rust port of upstream
/// `renderers/edit.ts`'s `renderCall` / `renderResult` plus
/// `components/diff.ts`'s `renderDiff` / `renderIntraLineDiff`.
///
/// Deviation from upstream: the upstream renderer caches a
/// `computeEditsDiff` preview while the call is streaming and suppresses the
/// result diff when it equals that preview (and suppresses an error that the
/// preview already showed); the Rust port renders a finished result only, so
/// it always shows the `details.diff` / error body.
#[derive(Debug, Default)]
pub struct EditRenderer {
    path: Option<String>,
}

impl EditRenderer {
    /// A renderer with no remembered call state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Path recorded by the last [`ToolRenderer::render_call`].
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }
}

impl ToolRenderer for EditRenderer {
    fn name(&self) -> &str {
        "edit"
    }

    fn render_call(
        &mut self,
        args: &serde_json::Value,
        ctx: &ToolRenderContext,
    ) -> Vec<StyledLine> {
        self.path = path_arg(args);

        let mut line = tool_title("edit");
        line.push(StyledSpan::new(" ", SpanStyle::PLAIN));
        line.extend(render_tool_path(self.path.as_deref(), ctx));
        vec![line]
    }

    fn render_result(
        &mut self,
        result: &ToolOutput,
        _options: &ToolRenderOptions,
        ctx: &ToolRenderContext,
    ) -> Vec<StyledLine> {
        if ctx.is_error {
            let output = get_text_output(result, ctx.show_images);
            let mut lines: Vec<StyledLine> = output
                .split('\n')
                .map(|line| vec![StyledSpan::new(line, SpanStyle::fg(ThemeColor::Error))])
                .collect();
            trim_trailing_empty_lines(&mut lines);
            return lines;
        }

        let Some(diff) = result
            .details
            .as_ref()
            .and_then(|details| details.get("diff"))
            .and_then(serde_json::Value::as_str)
        else {
            return Vec::new();
        };

        let mut lines = render_diff(diff);
        trim_trailing_empty_lines(&mut lines);
        lines
    }
}

/// A one-span line with `color` as its foreground slot.
fn single_span_line(text: impl Into<String>, color: ThemeColor) -> StyledLine {
    vec![StyledSpan::new(text, SpanStyle::fg(color))]
}

/// Parse one `generateDiffString` line into `(prefix, line_num, content)`.
///
/// Equivalent to upstream `renderDiff`'s
/// `^([+-\s])(\s*\d*)\s(.*)$`: the middle group is the longest
/// whitespace-then-digits prefix that leaves at least one whitespace as the
/// separator, so a line-number column of any width (including the empty one
/// on a `...` collapse marker) parses the same way.
fn parse_diff_line(line: &str) -> Option<(char, &str, &str)> {
    let prefix = line.chars().next()?;
    if !matches!(prefix, '+' | '-' | ' ') {
        return None;
    }
    let rest = &line[prefix.len_utf8()..];
    let bytes = rest.as_bytes();

    let ws = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_whitespace())
        .count();
    let digits_end = ws
        + bytes[ws..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();

    // Longest-first candidate split points: the digits end, then every
    // whitespace-only prefix (the regex backtracks into the `\s*` run).
    let split = std::iter::once(digits_end)
        .chain((0..=ws).rev())
        .find(|&index| index < bytes.len() && bytes[index].is_ascii_whitespace())?;

    Some((prefix, &rest[..split], &rest[split + 1..]))
}

/// Append a non-empty run to an intra-line diff side.
/// One `(text, changed)` fragment of a diff line.
type DiffRun = (String, bool);

/// A whole diff line as an ordered list of [`DiffRun`]s.
type DiffRuns = Vec<DiffRun>;

/// Append a fragment to a run list, skipping empty text.
fn push_diff_run(runs: &mut DiffRuns, text: String, changed: bool) {
    if !text.is_empty() {
        runs.push((text, changed));
    }
}

/// Word-level diff of a modified line, each side as `(text, changed)` runs.
///
/// Ports `renderIntraLineDiff`: the leading whitespace of the first changed
/// run is emitted unhighlighted so indentation never gets inverse video.
fn render_intra_line_diff(old_content: &str, new_content: &str) -> (DiffRuns, DiffRuns) {
    let mut removed_line: DiffRuns = Vec::new();
    let mut added_line: DiffRuns = Vec::new();
    let mut is_first_removed = true;
    let mut is_first_added = true;

    for part in diff_words(old_content, new_content) {
        match part.kind {
            DiffKind::Removed => {
                let mut value = part.value;
                if is_first_removed {
                    let leading: String =
                        value.chars().take_while(|ch| ch.is_whitespace()).collect();
                    value = value[leading.len()..].to_string();
                    push_diff_run(&mut removed_line, leading, false);
                    is_first_removed = false;
                }
                push_diff_run(&mut removed_line, value, true);
            }
            DiffKind::Added => {
                let mut value = part.value;
                if is_first_added {
                    let leading: String =
                        value.chars().take_while(|ch| ch.is_whitespace()).collect();
                    value = value[leading.len()..].to_string();
                    push_diff_run(&mut added_line, leading, false);
                    is_first_added = false;
                }
                push_diff_run(&mut added_line, value, true);
            }
            DiffKind::Equal => {
                push_diff_run(&mut removed_line, part.value.clone(), false);
                push_diff_run(&mut added_line, part.value, false);
            }
        }
    }

    (removed_line, added_line)
}

/// `diff_line_spans`-style line: `-<lineNum> ` / `+<lineNum> ` prefix, then
/// the runs, inverse-video on the changed ones.
fn intra_line_spans(
    prefix: char,
    line_num: &str,
    runs: &[DiffRun],
    color: ThemeColor,
) -> StyledLine {
    let mut out = StyledLine::new();
    out.push(StyledSpan::new(
        format!("{prefix}{line_num} "),
        SpanStyle::fg(color),
    ));
    for (text, changed) in runs {
        if text.is_empty() {
            continue;
        }
        let style = if *changed {
            SpanStyle::fg(color).inverse()
        } else {
            SpanStyle::fg(color)
        };
        out.push(StyledSpan::new(text.clone(), style));
    }
    out
}

/// Render a `generateDiffString` diff with colours and intra-line highlights.
///
/// Ports `renderDiff`: a removed run immediately followed by an added run of
/// exactly one line each is rendered as a word-level diff pair, any other
/// change is rendered line by line, context lines use `toolDiffContext`, and a
/// line that does not match the diff grammar is shown verbatim.
///
/// Deviation from upstream: the `filePath` option upstream accepts is unused
/// there too, so no equivalent parameter exists here.
pub fn render_diff(diff_text: &str) -> Vec<StyledLine> {
    let lines: Vec<&str> = diff_text.split('\n').collect();
    let mut result: Vec<StyledLine> = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        let Some((prefix, line_num, content)) = parse_diff_line(lines[index]) else {
            result.push(single_span_line(lines[index], ThemeColor::ToolDiffContext));
            index += 1;
            continue;
        };

        match prefix {
            '-' => {
                let mut removed: Vec<(String, String)> = Vec::new();
                while index < lines.len() {
                    let Some(('-', num, text)) = parse_diff_line(lines[index]) else {
                        break;
                    };
                    removed.push((num.to_string(), text.to_string()));
                    index += 1;
                }
                let mut added: Vec<(String, String)> = Vec::new();
                while index < lines.len() {
                    let Some(('+', num, text)) = parse_diff_line(lines[index]) else {
                        break;
                    };
                    added.push((num.to_string(), text.to_string()));
                    index += 1;
                }

                if removed.len() == 1 && added.len() == 1 {
                    let (removed_runs, added_runs) = render_intra_line_diff(
                        &replace_tabs(&removed[0].1),
                        &replace_tabs(&added[0].1),
                    );
                    result.push(intra_line_spans(
                        '-',
                        &removed[0].0,
                        &removed_runs,
                        ThemeColor::ToolDiffRemoved,
                    ));
                    result.push(intra_line_spans(
                        '+',
                        &added[0].0,
                        &added_runs,
                        ThemeColor::ToolDiffAdded,
                    ));
                } else {
                    for (num, text) in &removed {
                        result.push(single_span_line(
                            format!("-{num} {}", replace_tabs(text)),
                            ThemeColor::ToolDiffRemoved,
                        ));
                    }
                    for (num, text) in &added {
                        result.push(single_span_line(
                            format!("+{num} {}", replace_tabs(text)),
                            ThemeColor::ToolDiffAdded,
                        ));
                    }
                }
            }
            '+' => {
                result.push(single_span_line(
                    format!("+{line_num} {}", replace_tabs(content)),
                    ThemeColor::ToolDiffAdded,
                ));
                index += 1;
            }
            _ => {
                result.push(single_span_line(
                    format!(" {line_num} {}", replace_tabs(content)),
                    ThemeColor::ToolDiffContext,
                ));
                index += 1;
            }
        }
    }

    result
}

/// Head-fold a tool's text output: the first `max_lines` lines plus a muted
/// `... (N more lines, to expand)` hint, or every line when `expanded`.
fn folded_output_lines(output: &str, expanded: bool, max_lines: usize) -> Vec<StyledLine> {
    if output.is_empty() {
        return Vec::new();
    }
    let rendered = plain_tool_output_lines(output);
    let total = rendered.len();
    let keep = if expanded { total } else { max_lines };
    let mut lines: Vec<StyledLine> = rendered.into_iter().take(keep).collect();
    if total > keep {
        lines.push(notice_line(format!(
            "... ({} more lines, to expand)",
            total - keep
        )));
    }
    lines
}

/// Build the `[Truncated: …]` warning body from a tool's hit-limit and
/// truncation details, or `None` when nothing was cut.
fn limit_warnings(
    hit_limit: Option<String>,
    truncation: Option<&TruncationResult>,
    lines_truncated: bool,
) -> Option<String> {
    let mut warnings: Vec<String> = Vec::new();
    if let Some(hit_limit) = hit_limit {
        warnings.push(hit_limit);
    }
    if let Some(truncation) = truncation.filter(|truncation| truncation.truncated) {
        warnings.push(format!("{} limit", format_size(truncation.max_bytes)));
    }
    if lines_truncated {
        warnings.push("some lines truncated".to_string());
    }
    (!warnings.is_empty()).then(|| warnings.join(", "))
}

/// Decode a truncation payload nested under `key` in a tool's `details`.
fn truncation_from_details(
    details: Option<&serde_json::Value>,
    key: &str,
) -> Option<TruncationResult> {
    let value = details?.get(key)?.clone();
    serde_json::from_value(value).ok()
}

/// A numeric `details` field (hit limits), tolerating a `null` value.
fn count_from_details(details: Option<&serde_json::Value>, key: &str) -> Option<i64> {
    details?.get(key)?.as_i64()
}

/// Upstream `str()`: a string argument, `Some("")` for a missing / `null`
/// one, and `None` for any other JSON type (the `[invalid arg]` case).
fn nullable_str_arg(args: &serde_json::Value, key: &str) -> Option<String> {
    match args.get(key) {
        None | Some(serde_json::Value::Null) => Some(String::new()),
        Some(serde_json::Value::String(value)) => Some(value.clone()),
        Some(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Session driver
// ---------------------------------------------------------------------------

/// Per-turn bookkeeping that turns `ToolCall` / `ToolResult` events into styled
/// lines.
///
/// This is the single consumer-facing entry point: the text-fallback driver
/// (and any future `--print` tool output) feeds the real agent event stream
/// through it, so the renderer state (notably the write highlight cache) is
/// kept alive across a call's start and end without the driver knowing which
/// tools have a renderer.
pub struct ToolRenderSession {
    cwd: PathBuf,
    expanded: bool,
    show_images: bool,
    renderers: HashMap<String, Box<dyn ToolRenderer>>,
}

impl ToolRenderSession {
    /// A collapsed session rooted at `cwd`.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            expanded: false,
            show_images: false,
            renderers: HashMap::new(),
        }
    }

    /// Toggle expanded rendering for every tool.
    pub fn with_expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }

    /// Toggle image rendering for every tool.
    pub fn with_show_images(mut self, show_images: bool) -> Self {
        self.show_images = show_images;
        self
    }

    /// Render a tool call started by the agent. Returns an empty vector when
    /// the tool has no renderer.
    pub fn call(&mut self, call: &ToolCall, is_error: bool) -> Vec<StyledLine> {
        let Some(mut renderer) = renderer_for(&call.name) else {
            return Vec::new();
        };
        let ctx = self.context(is_error);
        let lines = renderer.render_call(&call.arguments, &ctx);
        self.renderers.insert(call.id.clone(), renderer);
        lines
    }

    /// Render a finished tool result. Returns an empty vector when the call
    /// had no renderer.
    pub fn result(&mut self, result: &ToolResult) -> Vec<StyledLine> {
        let Some(mut renderer) = self.renderers.remove(&result.tool_call_id) else {
            return Vec::new();
        };
        let ctx = self.context(result.is_error);
        let output = output_from_result(result);
        let options = ToolRenderOptions {
            expanded: self.expanded,
        };
        renderer.render_result(&output, &options, &ctx)
    }

    /// Drop every in-flight renderer (call at a turn boundary).
    pub fn clear(&mut self) {
        self.renderers.clear();
    }

    fn context(&self, is_error: bool) -> ToolRenderContext {
        ToolRenderContext {
            cwd: self.cwd.clone(),
            expanded: self.expanded,
            is_error,
            show_images: self.show_images,
        }
    }
}

impl std::fmt::Debug for ToolRenderSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRenderSession")
            .field("cwd", &self.cwd)
            .field("expanded", &self.expanded)
            .field("show_images", &self.show_images)
            .field("in_flight", &self.renderers.len())
            .finish()
    }
}

/// Adapt a wire [`ToolResult`] into the tool-local [`ToolOutput`] shape the
/// renderers consume.
pub fn output_from_result(result: &ToolResult) -> ToolOutput {
    ToolOutput {
        content: vec![(*result.content).clone()],
        details: result.details.clone(),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Concatenate the human-readable text of every text block in `result`.
///
/// Mirrors upstream `getTextOutput`: text blocks are joined with `\n` and
/// carriage returns are stripped. Image blocks collapse to a one-line
/// indicator unless `show_images` is set (the actual image bytes are handled by
/// the image subsystem, which is not ported yet).
pub fn get_text_output(result: &ToolOutput, show_images: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    for block in &result.content {
        match block {
            Content::Text(text) => parts.push(text.text.replace('\r', "")),
            Content::Image(image) if !show_images => {
                parts.push(format!("[image: {}]", image.mime_type));
            }
            _ => {}
        }
    }
    parts.join("\n")
}

/// Paint styled lines with a theme's ANSI escapes.
pub fn render_lines_ansi(lines: &[StyledLine], theme: &Theme) -> String {
    lines
        .iter()
        .map(|line| {
            line.iter()
                .map(|span| span.style.ansi(theme, &span.text))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Flatten styled lines to plain text, dropping every style slot.
pub fn render_lines_plain(lines: &[StyledLine]) -> String {
    lines
        .iter()
        .map(|line| {
            line.iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Replace tabs with three spaces (upstream `replaceTabs`).
pub fn replace_tabs(text: &str) -> String {
    text.replace('\t', "   ")
}

/// Strip carriage returns (upstream `normalizeDisplayText`).
pub fn normalize_display_text(text: &str) -> String {
    text.replace('\r', "")
}

/// Language id for `path` only when the highlighter can actually lex it.
///
/// This is the `getLanguageFromPath` + `supportsLanguage` pair upstream's
/// `highlightCode` applies: the extension table alone must not send an
/// unsupported id to the highlighter.
pub fn supported_language_for_path(path: &str) -> Option<&'static str> {
    let language = pi_tui::get_language_from_path(path)?;
    pi_tui::supports_language(language).then_some(language)
}

/// `read` / `write` argument path, accepting upstream's `file_path` alias.
fn path_arg(args: &serde_json::Value) -> Option<String> {
    string_arg(args, "file_path").or_else(|| string_arg(args, "path"))
}

/// A string argument, or `None` when it is missing or not a string.
fn string_arg(args: &serde_json::Value, key: &str) -> Option<String> {
    args.get(key)?.as_str().map(str::to_string)
}

/// `toolTitle`-styled, bold tool name.
fn tool_title(name: &str) -> StyledLine {
    vec![StyledSpan::new(
        name,
        SpanStyle::fg(ThemeColor::ToolTitle).bold(),
    )]
}

/// Render a tool's target path: `cwd`-relative when possible, `~`-shortened
/// otherwise, and an error marker for a missing/non-string argument.
fn render_tool_path(path: Option<&str>, ctx: &ToolRenderContext) -> StyledLine {
    render_tool_path_with_fallback(path, ctx, None)
}

/// [`render_tool_path`] with an upstream-style `emptyFallback` (the `ls` tool
/// shows `.` for an empty path where `read` / `write` show `...`).
fn render_tool_path_with_fallback(
    path: Option<&str>,
    ctx: &ToolRenderContext,
    empty_fallback: Option<&str>,
) -> StyledLine {
    let Some(path) = path else {
        return vec![StyledSpan::new(
            "[invalid arg]",
            SpanStyle::fg(ThemeColor::Error),
        )];
    };
    let value = match (path.is_empty(), empty_fallback) {
        (false, _) => path,
        (true, Some(fallback)) => fallback,
        (true, None) => {
            return vec![StyledSpan::new(
                "...",
                SpanStyle::fg(ThemeColor::ToolOutput),
            )]
        }
    };
    vec![StyledSpan::new(
        display_path(value, &ctx.cwd),
        SpanStyle::fg(ThemeColor::Accent),
    )]
}

/// Display `path` relative to `cwd` when it is absolute and nested inside it,
/// `~`-shortened when it lives under `$HOME`, and verbatim otherwise.
fn display_path(path: &str, cwd: &Path) -> String {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        if let Ok(relative) = candidate.strip_prefix(cwd) {
            return relative.display().to_string();
        }
        return shorten_home(path);
    }
    path.to_string()
}

/// Replace a leading `$HOME` with `~` (upstream `shortenPath`).
fn shorten_home(path: &str) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if !home.is_empty() && path.starts_with(home.as_ref()) {
            return format!("~{}", &path[home.len()..]);
        }
    }
    path.to_string()
}

/// Plain lines where every run carries the `toolOutput` slot.
fn plain_tool_output_lines(text: &str) -> Vec<StyledLine> {
    text.split('\n')
        .map(|line| vec![StyledSpan::new(line, SpanStyle::fg(ThemeColor::ToolOutput))])
        .collect()
}

/// A muted notice line (`... (N more lines)` and friends).
fn notice_line(text: String) -> StyledLine {
    vec![StyledSpan::new(text, SpanStyle::fg(ThemeColor::Muted))]
}

/// A warning notice line (tool-level truncation notices).
fn warning_line(text: String) -> StyledLine {
    vec![StyledSpan::new(text, SpanStyle::fg(ThemeColor::Warning))]
}

/// Drop trailing lines whose visible text is empty.
fn trim_trailing_empty_lines(lines: &mut Vec<StyledLine>) {
    while lines
        .last()
        .is_some_and(|line| line.iter().all(|span| span.text.is_empty()))
    {
        lines.pop();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;

    fn ctx() -> ToolRenderContext {
        ToolRenderContext::new("/work")
    }

    fn read_output(text: &str, details: Option<serde_json::Value>) -> ToolOutput {
        let mut output = ToolOutput::text(text);
        output.details = details;
        output
    }

    fn line_text(line: &StyledLine) -> String {
        line.iter().map(|span| span.text.as_str()).collect()
    }

    fn has_slot(line: &StyledLine, slot: ThemeColor) -> bool {
        line.iter().any(|span| span.style.fg == Some(slot))
    }

    #[test]
    fn read_result_highlights_rust_and_folds_to_ten_lines() {
        let content: String = (1..=15)
            .map(|n| format!("fn line_{n}() {{}}"))
            .collect::<Vec<_>>()
            .join("\n");
        let args = json!({ "path": "src/main.rs" });
        let mut renderer = ReadRenderer::new();
        renderer.render_call(&args, &ctx());
        let lines = renderer.render_result(
            &read_output(&content, None),
            &ToolRenderOptions::collapsed(),
            &ctx(),
        );

        // 10 shown + 1 fold notice.
        assert_eq!(lines.len(), 11, "{lines:?}");
        assert_eq!(line_text(&lines[0]), "fn line_1() {}");
        assert!(
            lines[0]
                .iter()
                .any(|span| span.text == "fn" && span.style.fg == Some(ThemeColor::SyntaxKeyword)),
            "keyword span missing: {:?}",
            lines[0]
        );
        assert_eq!(line_text(&lines[10]), "... (5 more lines)");
        assert!(has_slot(&lines[10], ThemeColor::Muted));

        // Expanded shows every line and no notice.
        let expanded = renderer.render_result(
            &read_output(&content, None),
            &ToolRenderOptions::expanded(),
            &ctx(),
        );
        assert_eq!(expanded.len(), 15);
    }

    #[test]
    fn read_result_does_not_highlight_errors_and_does_not_fold_them() {
        let content: String = (1..=15)
            .map(|n| format!("error line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let args = json!({ "path": "src/main.rs" });
        let mut renderer = ReadRenderer::new();
        renderer.render_call(&args, &ctx());
        let lines = renderer.render_result(
            &read_output(&content, None),
            &ToolRenderOptions::collapsed(),
            &ctx().with_is_error(true),
        );
        assert_eq!(lines.len(), 15);
        assert!(
            !lines
                .iter()
                .any(|line| has_slot(line, ThemeColor::SyntaxKeyword)),
            "error output must not be highlighted"
        );
        assert!(lines
            .iter()
            .all(|line| has_slot(line, ThemeColor::ToolOutput)));
    }

    #[test]
    fn read_result_falls_back_to_plain_text_for_unknown_extensions() {
        for path in ["notes.txt", "README"] {
            let args = json!({ "path": path });
            let mut renderer = ReadRenderer::new();
            renderer.render_call(&args, &ctx());
            let lines = renderer.render_result(
                &read_output("fn main() {}", None),
                &ToolRenderOptions::collapsed(),
                &ctx(),
            );
            assert_eq!(lines.len(), 1);
            assert!(
                !lines[0]
                    .iter()
                    .any(|span| span.style.fg == Some(ThemeColor::SyntaxKeyword)),
                "{path} must not be highlighted"
            );
            assert_eq!(lines[0][0].style.fg, Some(ThemeColor::ToolOutput));
        }
    }

    #[test]
    fn read_result_appends_upstream_truncation_notice() {
        let details = json!({
            "truncation": {
                "content": "a\nb",
                "truncated": true,
                "truncated_by": "lines",
                "total_lines": 500,
                "total_bytes": 10,
                "output_lines": 2,
                "output_bytes": 3,
                "last_line_partial": false,
                "first_line_exceeds_limit": false,
                "max_lines": 2,
                "max_bytes": 51200
            }
        });
        let args = json!({ "path": "src/main.rs" });
        let mut renderer = ReadRenderer::new();
        renderer.render_call(&args, &ctx());
        let lines = renderer.render_result(
            &read_output("a\nb", Some(details)),
            &ToolRenderOptions::collapsed(),
            &ctx(),
        );
        let last = lines.last().expect("notice line");
        assert_eq!(
            line_text(last),
            "[Truncated: showing 2 of 500 lines (2 line limit)]"
        );
        assert!(has_slot(last, ThemeColor::Warning));
    }

    #[test]
    fn read_call_shows_path_and_line_range() {
        let mut renderer = ReadRenderer::new();
        let lines = renderer.render_call(
            &json!({ "file_path": "src/lib.rs", "offset": 3, "limit": 4 }),
            &ctx(),
        );
        assert_eq!(line_text(&lines[0]), "read src/lib.rs:3-6");
        assert_eq!(renderer.path(), Some("src/lib.rs"));
    }

    #[test]
    fn write_call_highlights_preview_and_reports_size() {
        let content = "fn main() {\n    let x = 1;\n}\n";
        let mut renderer = WriteRenderer::new();
        let lines = renderer.render_call(
            &json!({ "path": "src/main.rs", "content": content }),
            &ctx(),
        );
        // title + 3 content lines (trailing empty line trimmed).
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert_eq!(
            line_text(&lines[0]),
            format!("write src/main.rs (3 lines, {} bytes)", content.len())
        );
        assert!(
            lines[1]
                .iter()
                .any(|span| span.text == "fn" && span.style.fg == Some(ThemeColor::SyntaxKeyword)),
            "preview must be highlighted: {:?}",
            lines[1]
        );
        assert!(renderer.cache().is_some());
    }

    #[test]
    fn write_result_is_empty_on_success_and_renders_error_body() {
        let mut renderer = WriteRenderer::new();
        renderer.render_call(&json!({ "path": "a.rs", "content": "fn a() {}" }), &ctx());
        assert!(renderer
            .render_result(
                &read_output("Successfully wrote 9 bytes to a.rs", None),
                &ToolRenderOptions::collapsed(),
                &ctx(),
            )
            .is_empty());
        let error = renderer.render_result(
            &read_output("permission denied", None),
            &ToolRenderOptions::collapsed(),
            &ctx().with_is_error(true),
        );
        assert_eq!(error.len(), 1);
        assert_eq!(line_text(&error[0]), "permission denied");
        assert!(has_slot(&error[0], ThemeColor::Error));
    }

    /// Counting highlighter: records every input it is asked to lex.
    fn counting_highlighter() -> (Arc<HighlightFn>, Arc<Mutex<Vec<String>>>) {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        let highlight: Arc<HighlightFn> = Arc::new(move |code: &str, lang: &str| {
            recorder.lock().expect("record").push(code.to_string());
            pi_tui::highlight_code(code, Some(lang), SpanStyle::fg(ThemeColor::MdCodeBlock))
        });
        (highlight, seen)
    }

    fn numbered_lines(count: usize) -> String {
        (1..=count)
            .map(|n| format!("let value_{n} = {n};"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn write_cache_rebuilds_from_scratch_once() {
        let (highlight, seen) = counting_highlighter();
        let content = numbered_lines(60);
        let cache = WriteHighlightCache::with_highlighter("src/main.rs", &content, highlight)
            .expect("rust is supported");
        assert_eq!(cache.stats().full_rebuilds, 1);
        assert_eq!(cache.highlighted_lines().len(), 60);
        let calls = seen.lock().expect("calls");
        assert_eq!(calls.len(), 1, "one full highlight");
        assert_eq!(calls[0], content, "full content highlighted once");
    }

    #[test]
    fn write_cache_extends_incrementally_without_full_rebuild() {
        let (highlight, seen) = counting_highlighter();
        let content = numbered_lines(60);
        let cache = WriteHighlightCache::with_highlighter("src/main.rs", &content, highlight)
            .expect("rust is supported");
        seen.lock().expect("calls").clear();

        let appended = format!("{content}\nlet tail = 61;");
        let cache = WriteHighlightCache::updated(Some(cache), "src/main.rs", &appended)
            .expect("cache reused");

        let stats = cache.stats();
        assert_eq!(stats.full_rebuilds, 1, "no full rebuild on an append");
        assert_eq!(stats.incremental_updates, 1);
        assert_eq!(stats.prefix_refreshes, 1);
        assert_eq!(cache.highlighted_lines().len(), 61);

        let calls = seen.lock().expect("calls");
        let full_lines = appended.split('\n').count();
        assert!(
            calls.iter().all(|input| input != &appended),
            "the appended content must never be re-highlighted whole"
        );
        assert!(
            calls
                .iter()
                .all(|input| input.split('\n').count() <= WRITE_PARTIAL_FULL_HIGHLIGHT_LINES),
            "only the bounded prefix and delta segments may be re-highlighted: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|input| input.split('\n').count() == WRITE_PARTIAL_FULL_HIGHLIGHT_LINES),
            "the 50-line prefix must be refreshed as one block: {calls:?}"
        );
        assert!(full_lines > WRITE_PARTIAL_FULL_HIGHLIGHT_LINES);
    }

    #[test]
    fn write_cache_rebuilds_when_content_is_not_an_append() {
        let (highlight, _seen) = counting_highlighter();
        let cache = WriteHighlightCache::with_highlighter("src/main.rs", "let a = 1;", highlight)
            .expect("rust is supported");
        let cache = WriteHighlightCache::updated(Some(cache), "src/main.rs", "let b = 2;")
            .expect("cache rebuilt");
        assert_eq!(cache.stats().full_rebuilds, 2);
        assert_eq!(cache.stats().incremental_updates, 0);
    }

    #[test]
    fn write_cache_rebuilds_when_language_changes() {
        let (highlight, _seen) = counting_highlighter();
        let cache = WriteHighlightCache::with_highlighter("src/main.rs", "let a = 1;", highlight)
            .expect("rust is supported");
        let cache = WriteHighlightCache::updated(Some(cache), "src/main.py", "let a = 1;")
            .expect("python is supported");
        assert_eq!(cache.language(), "python");
        assert_eq!(cache.stats().full_rebuilds, 2);
    }

    #[test]
    fn write_cache_rejects_unsupported_paths() {
        assert!(WriteHighlightCache::new("notes.txt", "hello").is_none());
        assert!(WriteHighlightCache::new("README", "hello").is_none());
    }

    #[test]
    fn session_keeps_renderer_across_call_and_result() {
        let mut session = ToolRenderSession::new("/work");
        let call = ToolCall {
            id: "call-1".into(),
            name: "read".into(),
            arguments: json!({ "path": "src/main.rs" }),
        };
        let start = session.call(&call, false);
        assert_eq!(line_text(&start[0]), "read src/main.rs");

        let result = ToolResult {
            tool_call_id: "call-1".into(),
            content: Box::new(Content::text("fn main() {}")),
            is_error: false,
            details: None,
            added_tool_names: None,
        };
        let rendered = session.result(&result);
        assert!(
            rendered[0]
                .iter()
                .any(|span| span.text == "fn" && span.style.fg == Some(ThemeColor::SyntaxKeyword)),
            "session must remember the read path: {rendered:?}"
        );

        // A second result for the same id has no renderer left.
        assert!(session.result(&result).is_empty());
    }

    #[test]
    fn session_ignores_tools_without_renderers() {
        let mut session = ToolRenderSession::new("/work");
        // Extension tools (and any built-in added before it grows a
        // presentation layer) have no renderer.
        let call = ToolCall {
            id: "call-ext".into(),
            name: "extension_tool".into(),
            arguments: json!({ "path": "main.rs" }),
        };
        assert!(session.call(&call, false).is_empty());
        let result = ToolResult {
            tool_call_id: "call-ext".into(),
            content: Box::new(Content::text("ok")),
            is_error: false,
            details: None,
            added_tool_names: None,
        };
        assert!(session.result(&result).is_empty());
    }

    #[test]
    fn render_lines_ansi_paints_theme_slots() {
        let theme = pi_tui::builtin_theme("dark", pi_tui::ColorMode::TrueColor)
            .expect("builtin dark theme");
        let lines = vec![vec![StyledSpan::new(
            "fn",
            SpanStyle::fg(ThemeColor::SyntaxKeyword),
        )]];
        let ansi = render_lines_ansi(&lines, &theme);
        assert!(ansi.contains(theme.get_fg_ansi(ThemeColor::SyntaxKeyword)));
        assert_eq!(render_lines_plain(&lines), "fn");
    }

    #[test]
    fn helpers_match_upstream_semantics() {
        assert_eq!(replace_tabs("a\tb"), "a   b");
        assert_eq!(normalize_display_text("a\r\nb"), "a\nb");
        assert_eq!(supported_language_for_path("src/main.rs"), Some("rust"));
        assert_eq!(supported_language_for_path("notes.txt"), None);
        assert_eq!(
            renderer_for("read").map(|r| r.name().to_string()),
            Some("read".into())
        );
        assert_eq!(
            renderer_for("bash").map(|r| r.name().to_string()),
            Some("bash".into())
        );
        assert_eq!(
            renderer_for("edit").map(|r| r.name().to_string()),
            Some("edit".into())
        );
        assert!(renderer_for("extension_tool").is_none());
    }

    // -----------------------------------------------------------------
    // edit
    // -----------------------------------------------------------------

    #[test]
    fn parse_diff_line_matches_upstream_grammar() {
        assert_eq!(parse_diff_line("+12 content"), Some(('+', "12", "content")));
        assert_eq!(parse_diff_line("-3 x"), Some(('-', "3", "x")));
        assert_eq!(parse_diff_line(" 1 foo bar"), Some((' ', "1", "foo bar")));
        assert_eq!(parse_diff_line("  12 y"), Some((' ', " 12", "y")));
        // A `...` collapse marker has an empty line-number column; the regex
        // backtracks into the whitespace run, so a wider column leaves part of
        // the padding in the content (upstream renders it the same way).
        assert_eq!(parse_diff_line("  ..."), Some((' ', "", "...")));
        assert_eq!(parse_diff_line("    ..."), Some((' ', "  ", "...")));
        assert_eq!(parse_diff_line("@@ -1 +1 @@"), None);
        assert_eq!(parse_diff_line(""), None);
    }

    #[test]
    fn render_diff_marks_the_changed_words_within_a_modified_line() {
        let lines = render_diff(" 1 hello\n-2 world\n+2 WORLD");
        assert_eq!(line_text(&lines[0]), " 1 hello");
        assert!(has_slot(&lines[0], ThemeColor::ToolDiffContext));

        assert_eq!(line_text(&lines[1]), "-2 world");
        assert!(has_slot(&lines[1], ThemeColor::ToolDiffRemoved));
        assert!(
            lines[1]
                .iter()
                .any(|span| span.text == "world" && span.style.inverse),
            "removed word must be inverse: {lines:?}"
        );

        assert_eq!(line_text(&lines[2]), "+2 WORLD");
        assert!(has_slot(&lines[2], ThemeColor::ToolDiffAdded));
        assert!(
            lines[2]
                .iter()
                .any(|span| span.text == "WORLD" && span.style.inverse),
            "added word must be inverse: {lines:?}"
        );
    }

    #[test]
    fn render_diff_keeps_unequal_runs_line_by_line() {
        let lines = render_diff(" 1 a\n-2 b\n-3 c\n+2 z");
        assert_eq!(line_text(&lines[1]), "-2 b");
        assert_eq!(line_text(&lines[2]), "-3 c");
        assert_eq!(line_text(&lines[3]), "+2 z");
        assert!(
            !lines[1..]
                .iter()
                .any(|line| line.iter().any(|span| span.style.inverse)),
            "multi-line change must not be word-highlighted: {lines:?}"
        );
    }

    #[test]
    fn render_diff_expands_tabs_and_passes_through_unknown_lines() {
        let lines = render_diff("\x1b[31mnot a diff line\n 1 a\tb");
        assert_eq!(line_text(&lines[0]), "\x1b[31mnot a diff line");
        assert!(has_slot(&lines[0], ThemeColor::ToolDiffContext));
        assert_eq!(line_text(&lines[1]), " 1 a   b");
    }

    #[test]
    fn edit_renderer_renders_the_call_and_the_result_diff() {
        let mut renderer = EditRenderer::new();
        let call = renderer.render_call(
            &json!({
                "path": "src/main.rs",
                "edits": [{ "oldText": "world", "newText": "WORLD" }],
            }),
            &ctx(),
        );
        assert_eq!(line_text(&call[0]), "edit src/main.rs");
        assert_eq!(renderer.path(), Some("src/main.rs"));

        let result = read_output(
            "Successfully replaced 1 block(s) in src/main.rs.",
            Some(json!({
                "diff": " 1 hello\n-2 world\n+2 WORLD",
                "patch": "",
                "firstChangedLine": 2,
            })),
        );
        let lines = renderer.render_result(&result, &ToolRenderOptions::collapsed(), &ctx());
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(line_text(&lines[0]), " 1 hello");
        assert_eq!(line_text(&lines[1]), "-2 world");
        assert_eq!(line_text(&lines[2]), "+2 WORLD");
        assert!(lines[1].iter().any(|span| span.style.inverse));
    }

    #[test]
    fn edit_renderer_shows_the_error_body_and_nothing_without_a_diff() {
        let mut renderer = EditRenderer::new();
        renderer.render_call(&json!({ "path": "src/main.rs" }), &ctx());
        let error_ctx = ToolRenderContext::new("/work").with_is_error(true);
        let lines = renderer.render_result(
            &read_output("Could not find the exact text", None),
            &ToolRenderOptions::collapsed(),
            &error_ctx,
        );
        assert_eq!(line_text(&lines[0]), "Could not find the exact text");
        assert!(has_slot(&lines[0], ThemeColor::Error));

        // A successful result without `details.diff` renders nothing.
        let mut renderer = EditRenderer::new();
        renderer.render_call(&json!({ "path": "src/main.rs" }), &ctx());
        assert!(renderer
            .render_result(
                &read_output("ok", None),
                &ToolRenderOptions::collapsed(),
                &ctx()
            )
            .is_empty());
    }
}
