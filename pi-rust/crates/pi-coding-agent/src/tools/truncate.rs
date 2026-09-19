//! Shared truncation utilities for tool outputs.
//!
//! A Rust port of `packages/coding-agent/src/core/tools/truncate.ts`.
//! Truncation is driven by two independent limits — whichever is hit first
//! wins:
//!
//! * a **line** limit ([`DEFAULT_MAX_LINES`], 2000 lines)
//! * a **byte** limit ([`DEFAULT_MAX_BYTES`], 50 KB)
//!
//! Neither function ever returns a partial line, with a single deliberate
//! exception that upstream also has: [`truncate_tail`] keeps the *end* of one
//! line when that line alone already exceeds the byte limit, and reports that
//! through [`TruncationResult::last_line_partial`].
//!
//! All sizes are UTF-8 byte counts (`Buffer.byteLength(x, "utf-8")` upstream,
//! `str::len` here). Line counting matches `splitLinesForCounting`: a trailing
//! newline does **not** count as an extra line.
//!
//! ```no_run
//! use pi_coding_agent::tools::truncate::{truncate_head, TruncationOptions};
//!
//! let result = truncate_head("a\nb\nc", TruncationOptions::default());
//! assert!(!result.truncated);
//! ```

/// Default maximum number of lines kept by a truncation
/// (`truncate.ts:12`).
pub const DEFAULT_MAX_LINES: usize = 2000;

/// Default maximum number of bytes kept by a truncation — 50 KB
/// (`truncate.ts:13`).
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;

/// Maximum characters per grep match line (`truncate.ts:14`).
pub const GREP_MAX_LINE_LENGTH: usize = 500;

/// Which of the two limits stopped the truncation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TruncatedBy {
    /// The line limit was hit first.
    Lines,
    /// The byte limit was hit first.
    Bytes,
}

/// Optional overrides for the two limits. `Default` reproduces upstream's
/// `DEFAULT_MAX_LINES` / `DEFAULT_MAX_BYTES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TruncationOptions {
    /// Maximum number of lines to keep. Defaults to [`DEFAULT_MAX_LINES`].
    pub max_lines: usize,
    /// Maximum number of bytes to keep. Defaults to [`DEFAULT_MAX_BYTES`].
    pub max_bytes: usize,
}

impl Default for TruncationOptions {
    fn default() -> Self {
        Self {
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

impl TruncationOptions {
    /// Options that apply `max_lines` / `max_bytes` overrides, filling the
    /// other limit from the defaults. `None` means "use the default".
    pub fn new(max_lines: Option<usize>, max_bytes: Option<usize>) -> Self {
        Self {
            max_lines: max_lines.unwrap_or(DEFAULT_MAX_LINES),
            max_bytes: max_bytes.unwrap_or(DEFAULT_MAX_BYTES),
        }
    }

    /// Byte-limit-only variant, matching upstream's
    /// `truncateHead(raw, { maxLines: Number.MAX_SAFE_INTEGER })` call sites
    /// (`ls.ts:141`, `grep.ts:282`).
    pub fn bytes_only(max_bytes: usize) -> Self {
        Self {
            max_lines: usize::MAX,
            max_bytes,
        }
    }
}

/// Everything the caller (and the UI) needs to know about a truncation.
///
/// Field names are snake_case, unlike the upstream camelCase object: every
/// other tool `details` payload in this crate is snake_case
/// (`bash.rs` emits `exit_code` / `elapsed_ms`), so the JSON surface stays
/// internally consistent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TruncationResult {
    /// The truncated content.
    pub content: String,
    /// Whether any truncation occurred.
    pub truncated: bool,
    /// Which limit was hit, or `None` when `truncated` is false.
    pub truncated_by: Option<TruncatedBy>,
    /// Total number of lines in the original content.
    pub total_lines: usize,
    /// Total number of bytes in the original content.
    pub total_bytes: usize,
    /// Number of complete lines in the truncated output.
    pub output_lines: usize,
    /// Number of bytes in the truncated output.
    pub output_bytes: usize,
    /// Whether the last line was partially truncated. Only ever true for
    /// [`truncate_tail`]'s "single over-long line" edge case.
    pub last_line_partial: bool,
    /// Whether the first line alone exceeded the byte limit (head truncation
    /// only). When true, `content` is empty.
    pub first_line_exceeds_limit: bool,
    /// The line limit that was applied.
    pub max_lines: usize,
    /// The byte limit that was applied.
    pub max_bytes: usize,
}

/// Split `content` for line counting the way upstream `splitLinesForCounting`
/// does: empty strings have no lines, and a trailing newline does not create
/// an extra empty line.
pub fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Format a byte count the way upstream `formatSize` does
/// (`B` / `KB` / `MB`, one decimal for the latter two).
pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Truncate from the head (keep the first N lines/bytes). Suitable for file
/// reads where the beginning is what matters.
///
/// Never returns a partial line. When the first line alone exceeds the byte
/// limit the result carries empty `content` and
/// `first_line_exceeds_limit = true`, so the caller can point the model at a
/// byte-range fallback instead of silently returning nothing.
pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines;
    let max_bytes = options.max_bytes;

    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    // A single first line that cannot fit at all: report it instead of
    // returning an empty string with no explanation.
    if lines[0].len() > max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }

    let mut kept: Vec<&str> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;

    for (i, line) in lines.iter().enumerate() {
        if i >= max_lines {
            break;
        }
        // +1 for the newline that joins this line to the previous one.
        let line_bytes = line.len() + usize::from(i > 0);
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        kept.push(line);
        output_bytes_count += line_bytes;
    }

    if kept.len() >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = kept.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: kept.len(),
        output_bytes: final_output_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Truncate from the tail (keep the last N lines/bytes). Suitable for shell
/// output, where the end (errors, final results) is what matters.
///
/// If even the last line does not fit into the byte budget, its end is kept
/// and `last_line_partial` is set — the only partial line this module ever
/// returns.
pub fn truncate_tail(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines;
    let max_bytes = options.max_bytes;

    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    let mut kept: Vec<String> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;
    let mut last_line_partial = false;

    for line in lines.iter().rev() {
        if kept.len() >= max_lines {
            break;
        }
        // +1 for the newline that joins this line to the following one.
        let line_bytes = line.len() + usize::from(!kept.is_empty());
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            // Nothing fits yet and this single line is already too big: keep
            // its tail so the caller still sees the most recent bytes.
            if kept.is_empty() {
                let partial = truncate_string_to_bytes_from_end(line, max_bytes);
                output_bytes_count = partial.len();
                kept.insert(0, partial);
                last_line_partial = true;
            }
            break;
        }
        kept.insert(0, (*line).to_string());
        output_bytes_count += line_bytes;
    }

    if kept.len() >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = kept.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: kept.len(),
        output_bytes: final_output_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Truncate a single line to `max_chars`, appending `... [truncated]` when it
/// was cut. Used for grep match lines.
///
/// Counts Unicode scalar values, not UTF-16 code units like upstream's
/// `String#length`; the only lines where the two differ contain astral-plane
/// characters.
pub fn truncate_line(line: &str, max_chars: usize) -> (String, bool) {
    if line.chars().count() <= max_chars {
        (line.to_string(), false)
    } else {
        let head: String = line.chars().take(max_chars).collect();
        (format!("{}... [truncated]", head), true)
    }
}

/// Keep the last `max_bytes` bytes of `str`, moving the cut forward to the
/// next UTF-8 character boundary so the result is always valid UTF-8.
fn truncate_string_to_bytes_from_end(s: &str, max_bytes: usize) -> String {
    let buf = s.as_bytes();
    if buf.len() <= max_bytes {
        return s.to_string();
    }

    let mut start = buf.len() - max_bytes;
    // 0b10xx_xxxx is a continuation byte: skip forward to the next character.
    while start < buf.len() && (buf[start] & 0xc0) == 0x80 {
        start += 1;
    }

    String::from_utf8_lossy(&buf[start..]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build `n` lines `Line 1` … `Line n` joined by `\n` (no trailing
    /// newline), matching the upstream fixtures.
    fn numbered_lines(n: usize) -> String {
        (1..=n)
            .map(|i| format!("Line {}", i))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn split_lines_ignores_a_trailing_newline() {
        assert!(split_lines_for_counting("").is_empty());
        assert_eq!(split_lines_for_counting("a"), vec!["a"]);
        assert_eq!(split_lines_for_counting("a\n"), vec!["a"]);
        assert_eq!(split_lines_for_counting("a\nb"), vec!["a", "b"]);
        assert_eq!(split_lines_for_counting("a\nb\n"), vec!["a", "b"]);
        assert_eq!(split_lines_for_counting("a\n\n"), vec!["a", ""]);
    }

    #[test]
    fn format_size_matches_upstream_units() {
        assert_eq!(format_size(0), "0B");
        assert_eq!(format_size(512), "512B");
        assert_eq!(format_size(1024), "1.0KB");
        assert_eq!(format_size(DEFAULT_MAX_BYTES), "50.0KB");
        assert_eq!(format_size(1024 * 1024), "1.0MB");
        assert_eq!(format_size(3 * 1024 * 1024 / 2), "1.5MB");
    }

    #[test]
    fn small_content_is_left_untouched() {
        let content = "Hello, world!\nLine 2\nLine 3";
        let head = truncate_head(content, TruncationOptions::default());
        assert!(!head.truncated);
        assert_eq!(head.truncated_by, None);
        assert_eq!(head.content, content);
        assert_eq!(head.total_lines, 3);
        assert_eq!(head.output_lines, 3);
        assert_eq!(head.output_bytes, content.len());

        let tail = truncate_tail(content, TruncationOptions::default());
        assert!(!tail.truncated);
        assert_eq!(tail.content, content);
    }

    #[test]
    fn a_trailing_newline_does_not_add_a_line() {
        let content = format!("{}\n", numbered_lines(2500));
        let result = truncate_head(&content, TruncationOptions::default());
        assert_eq!(result.total_lines, 2500);
        assert_eq!(result.output_lines, 2000);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Lines));
    }

    #[test]
    fn head_truncation_stops_at_the_line_limit() {
        let result = truncate_head(&numbered_lines(2500), TruncationOptions::default());
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(result.total_lines, 2500);
        assert_eq!(result.output_lines, 2000);
        assert!(result.content.starts_with("Line 1\n"));
        assert!(result.content.ends_with("Line 2000"));
        assert!(!result.content.contains("Line 2001"));
        // 2000 lines + 1999 newlines.
        assert_eq!(result.output_bytes, result.content.len());
    }

    #[test]
    fn head_truncation_stops_at_the_byte_limit() {
        // 500 lines of ~210 bytes: fewer lines than the default, more bytes.
        let content = (1..=500)
            .map(|i| format!("Line {}: {}", i, "x".repeat(200)))
            .collect::<Vec<_>>()
            .join("\n");
        let result = truncate_head(&content, TruncationOptions::default());
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert_eq!(result.total_lines, 500);
        assert!(result.output_lines < 500);
        assert!(result.output_bytes <= DEFAULT_MAX_BYTES);
        // The bytes we dropped are the whole point of the limit: adding the
        // next line back would have blown the budget.
        let next_line_len = content
            .split('\n')
            .nth(result.output_lines)
            .expect("a line was left over")
            .len();
        assert!(result.output_bytes + 1 + next_line_len > DEFAULT_MAX_BYTES);
    }

    #[test]
    fn head_truncation_reports_a_first_line_over_the_byte_limit() {
        let content = "x".repeat(DEFAULT_MAX_BYTES + 1);
        let result = truncate_head(&content, TruncationOptions::default());
        assert!(result.truncated);
        assert!(result.first_line_exceeds_limit);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert!(result.content.is_empty());
        assert_eq!(result.output_lines, 0);
        assert_eq!(result.output_bytes, 0);
        assert_eq!(result.total_lines, 1);
    }

    #[test]
    fn tail_truncation_keeps_the_end() {
        let result = truncate_tail(&numbered_lines(2500), TruncationOptions::default());
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(result.total_lines, 2500);
        assert_eq!(result.output_lines, 2000);
        assert!(result.content.starts_with("Line 501\n"));
        assert!(result.content.ends_with("Line 2500"));
        assert!(!result.last_line_partial);
    }

    #[test]
    fn tail_truncation_stops_at_the_byte_limit() {
        let content = (1..=500)
            .map(|i| format!("Line {}: {}", i, "x".repeat(200)))
            .collect::<Vec<_>>()
            .join("\n");
        let result = truncate_tail(&content, TruncationOptions::default());
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert!(result.output_bytes <= DEFAULT_MAX_BYTES);
        assert!(result
            .content
            .ends_with(&format!("Line 500: {}", "x".repeat(200))));
    }

    #[test]
    fn tail_truncation_keeps_the_end_of_one_over_long_line() {
        // One line that is longer than the whole byte budget: the tail of the
        // line is kept and flagged as partial.
        let content = format!("prefix-{}\n{}", "p".repeat(100), "z".repeat(200));
        let result = truncate_tail(&content, TruncationOptions::new(None, Some(50)));
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert!(result.last_line_partial);
        assert_eq!(result.output_lines, 1);
        assert_eq!(result.output_bytes, 50);
        assert_eq!(result.content, "z".repeat(50));
    }

    #[test]
    fn tail_truncation_cuts_on_a_character_boundary() {
        // `é` is two bytes; requesting 3 bytes of a run of them must skip the
        // continuation byte and keep one whole character.
        let content = "é".repeat(10);
        let result = truncate_tail(&content, TruncationOptions::new(None, Some(3)));
        assert!(result.last_line_partial);
        assert_eq!(result.content, "é");
        assert_eq!(result.output_bytes, 2);
    }

    #[test]
    fn truncate_line_appends_a_marker() {
        let long = "a".repeat(600);
        let (text, was_truncated) = truncate_line(&long, GREP_MAX_LINE_LENGTH);
        assert!(was_truncated);
        assert_eq!(text.len(), GREP_MAX_LINE_LENGTH + "... [truncated]".len());
        assert!(text.ends_with("... [truncated]"));

        let (text, was_truncated) = truncate_line("short", GREP_MAX_LINE_LENGTH);
        assert!(!was_truncated);
        assert_eq!(text, "short");
    }

    #[test]
    fn bytes_only_options_disable_the_line_limit() {
        let content = numbered_lines(3000);
        let result = truncate_head(&content, TruncationOptions::bytes_only(DEFAULT_MAX_BYTES));
        // More lines than DEFAULT_MAX_LINES, but they fit in 50 KB — so only a
        // live line limit could have truncated this.
        assert!(result.max_lines > 2000);
        assert!(!result.truncated);
        assert_eq!(result.output_lines, 3000);
    }

    #[test]
    fn results_round_trip_through_json() {
        let result = truncate_head(&numbered_lines(2500), TruncationOptions::default());
        let json = serde_json::to_value(&result).expect("serialize");
        assert_eq!(json["truncated_by"], "lines");
        assert_eq!(json["total_lines"], 2500);
        assert_eq!(json["output_lines"], 2000);
        let back: TruncationResult = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, result);
    }
}
