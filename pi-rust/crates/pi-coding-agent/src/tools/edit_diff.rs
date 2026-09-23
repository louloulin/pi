//! Exact-text edit matching and diff generation.
//!
//! Rust port of `packages/coding-agent/src/core/tools/edit-diff.ts`. It
//! implements the matching half of the `edit` tool:
//!
//! * [`fuzzy_find_text`] — exact match first, then a fuzzy pass over
//!   NFKC-normalized content (trailing whitespace stripped, smart quotes /
//!   Unicode dashes / special spaces folded to ASCII).
//! * [`apply_edits_to_normalized_content`] — match every edit against the
//!   *original* content, reject overlaps and ambiguous matches, then apply
//!   the replacements in reverse offset order. When any edit needed the
//!   fuzzy pass the result is overlaid line-by-line onto the original
//!   content so untouched lines keep their original bytes.
//! * [`generate_diff_string`] — the display-oriented, numbered diff the TUI
//!   renders.
//! * [`generate_unified_patch`] — a standard unified patch.
//! * [`compute_edits_diff`] — the read-only preview the `edit` renderer uses
//!   before the tool runs.
//!
//! Offsets are byte offsets into the corresponding string, which keeps
//! multibyte content safe (the TypeScript original uses UTF-16 indices, but
//! every index it derives comes from `indexOf` / `slice`, so the two are
//! equivalent as long as both ends of a slice agree — which they do here).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use super::text_diff::{diff_lines, DiffKind};

/// One targeted replacement (upstream `Edit`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    /// Exact text to match against the original file.
    #[serde(rename = "oldText", alias = "old_text")]
    pub old_text: String,
    /// Replacement text.
    #[serde(rename = "newText", alias = "new_text")]
    pub new_text: String,
}

/// Result of applying a set of edits to LF-normalized content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEditsResult {
    /// The content the edits were matched against (the normalized input).
    pub base_content: String,
    /// The content after applying every replacement.
    pub new_content: String,
}

/// A display diff plus the first changed line in the new file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffResult {
    /// The rendered, line-numbered diff body.
    pub diff: String,
    /// 1-based line number of the first change in the new file, if any.
    pub first_changed_line: Option<usize>,
}

/// How flexible the match was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatchResult {
    /// Whether a match was found at all.
    pub found: bool,
    /// Byte offset of the match in [`Self::content_for_replacement`].
    pub index: usize,
    /// Byte length of the matched text.
    pub match_length: usize,
    /// `true` when the match came from the fuzzy pass.
    pub used_fuzzy_match: bool,
    /// The content replacement offsets refer to.
    pub content_for_replacement: String,
}

/// Matched edit with resolved offsets (upstream `MatchedEdit`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct MatchedEdit {
    edit_index: usize,
    match_index: usize,
    match_length: usize,
    new_text: String,
}

/// Replacement payload shaped for the low-level helpers.
#[derive(Debug, Clone, Copy)]
struct TextReplacement<'a> {
    match_index: usize,
    match_length: usize,
    new_text: &'a str,
}

#[derive(Debug, Clone, Copy)]
struct LineSpan {
    start: usize,
    end: usize,
}

/// Detect the dominant line ending (`\r\n` when the first one precedes the
/// first bare `\n`, otherwise `\n`).
pub fn detect_line_ending(content: &str) -> &'static str {
    let crlf = content.find("\r\n");
    let lf = content.find('\n');
    match (lf, crlf) {
        (None, _) => "\n",
        (Some(_), None) => "\n",
        (Some(lf), Some(crlf)) => {
            if crlf < lf {
                "\r\n"
            } else {
                "\n"
            }
        }
    }
}

/// Normalize CRLF / CR line endings to LF.
pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Restore a previously detected line ending.
pub fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

/// Normalize text for fuzzy matching.
///
/// Applies NFKC, strips trailing whitespace per line, then folds smart
/// quotes, Unicode dashes and special Unicode spaces to their ASCII
/// equivalents. The transformation is idempotent.
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    let nfkc: String = text.nfkc().collect();
    let trimmed = nfkc
        .split('\n')
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    trimmed
        .chars()
        .map(|ch| match ch {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

/// Split content into segments that keep their `\n` terminator.
fn split_lines_with_endings(content: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (idx, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            out.push(&content[start..=idx]);
            start = idx + 1;
        }
    }
    if start < content.len() {
        out.push(&content[start..]);
    }
    out
}

/// Byte spans of every line in `content`.
fn get_line_spans(content: &str) -> Vec<LineSpan> {
    let mut offset = 0;
    split_lines_with_endings(content)
        .into_iter()
        .map(|line| {
            let span = LineSpan {
                start: offset,
                end: offset + line.len(),
            };
            offset = span.end;
            span
        })
        .collect()
}

/// Which line range a replacement touches.
fn replacement_line_range(
    lines: &[LineSpan],
    replacement: &TextReplacement<'_>,
) -> Result<(usize, usize), String> {
    let replacement_start = replacement.match_index;
    let replacement_end = replacement.match_index + replacement.match_length;

    let start_line = lines
        .iter()
        .position(|line| replacement_start >= line.start && replacement_start < line.end)
        .ok_or_else(|| "Replacement range is outside the base content.".to_string())?;

    let mut end_line = start_line;
    while end_line < lines.len() && lines[end_line].end < replacement_end {
        end_line += 1;
    }
    if end_line >= lines.len() {
        return Err("Replacement range is outside the base content.".to_string());
    }

    Ok((start_line, end_line + 1))
}

/// Apply replacements to `content`, walking them in reverse so earlier
/// offsets stay valid.
fn apply_replacements(
    content: &str,
    replacements: &[TextReplacement<'_>],
    offset: usize,
) -> String {
    let mut result = content.to_string();
    for replacement in replacements.iter().rev() {
        let start = replacement.match_index - offset;
        let end = start + replacement.match_length;
        result.replace_range(start..end, replacement.new_text);
    }
    result
}

/// Apply replacements matched against `base_content` to `original_content`,
/// preserving untouched lines from the original.
///
/// See the module docs: this is what keeps CRLF / trailing-whitespace bytes
/// intact on lines the fuzzy pass never touched.
pub fn apply_replacements_preserving_unchanged_lines(
    original_content: &str,
    base_content: &str,
    replacements: &[EditMatch],
) -> Result<String, String> {
    let original_lines = split_lines_with_endings(original_content);
    let base_lines = get_line_spans(base_content);
    if original_lines.len() != base_lines.len() {
        return Err(
            "Cannot preserve unchanged lines because the base content has a different line count."
                .to_string(),
        );
    }

    let mut sorted: Vec<&EditMatch> = replacements.iter().collect();
    sorted.sort_by_key(|r| r.match_index);

    // `(start_line, end_line, replacements)` for each disjoint touched region.
    let mut groups: Vec<(usize, usize, Vec<EditMatch>)> = Vec::new();
    for replacement in sorted {
        let (start_line, end_line) = replacement_line_range(
            &base_lines,
            &TextReplacement {
                match_index: replacement.match_index,
                match_length: replacement.match_length,
                new_text: &replacement.new_text,
            },
        )?;
        if let Some(last) = groups.last_mut() {
            if start_line < last.1 {
                last.1 = last.1.max(end_line);
                last.2.push(replacement.clone());
                continue;
            }
        }
        groups.push((start_line, end_line, vec![replacement.clone()]));
    }

    let mut original_line_index = 0;
    let mut result = String::new();
    for (start_line, end_line, group_replacements) in groups {
        for line in &original_lines[original_line_index..start_line] {
            result.push_str(line);
        }

        let group_start_offset = base_lines[start_line].start;
        let group_end_offset = base_lines[end_line - 1].end;
        let slice = &base_content[group_start_offset..group_end_offset];
        let applied: Vec<TextReplacement<'_>> = group_replacements
            .iter()
            .map(|replacement| TextReplacement {
                match_index: replacement.match_index,
                match_length: replacement.match_length,
                new_text: &replacement.new_text,
            })
            .collect();
        result.push_str(&apply_replacements(slice, &applied, group_start_offset));
        original_line_index = end_line;
    }
    for line in &original_lines[original_line_index..] {
        result.push_str(line);
    }

    Ok(result)
}

/// A matched replacement with owned text (public form of the internal
/// `MatchedEdit`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditMatch {
    /// Byte offset of the match in the replacement base content.
    pub match_index: usize,
    /// Byte length of the matched text.
    pub match_length: usize,
    /// Replacement text.
    pub new_text: String,
}

/// Find `old_text` in `content`, exact match first then fuzzy.
pub fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatchResult {
    if let Some(index) = content.find(old_text) {
        return FuzzyMatchResult {
            found: true,
            index,
            match_length: old_text.len(),
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        };
    }

    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    match fuzzy_content.find(&fuzzy_old_text) {
        Some(index) => FuzzyMatchResult {
            found: true,
            index,
            match_length: fuzzy_old_text.len(),
            used_fuzzy_match: true,
            content_for_replacement: fuzzy_content,
        },
        None => FuzzyMatchResult {
            found: false,
            index: 0,
            match_length: 0,
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        },
    }
}

/// Count non-overlapping occurrences in fuzzy-normalized space.
fn count_occurrences(content: &str, old_text: &str) -> usize {
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    if fuzzy_old_text.is_empty() {
        return 0;
    }
    fuzzy_content.matches(&fuzzy_old_text).count()
}

fn not_found_error(path: &str, edit_index: usize, total_edits: usize) -> String {
    if total_edits == 1 {
        format!(
            "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
        )
    } else {
        format!(
            "Could not find edits[{edit_index}] in {path}. The oldText must match exactly including all whitespace and newlines."
        )
    }
}

fn duplicate_error(
    path: &str,
    edit_index: usize,
    total_edits: usize,
    occurrences: usize,
) -> String {
    if total_edits == 1 {
        format!(
            "Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
        )
    } else {
        format!(
            "Found {occurrences} occurrences of edits[{edit_index}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
        )
    }
}

fn empty_old_text_error(path: &str, edit_index: usize, total_edits: usize) -> String {
    if total_edits == 1 {
        format!("oldText must not be empty in {path}.")
    } else {
        format!("edits[{edit_index}].oldText must not be empty in {path}.")
    }
}

fn no_change_error(path: &str, total_edits: usize) -> String {
    if total_edits == 1 {
        format!(
            "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
        )
    } else {
        format!("No changes made to {path}. The replacements produced identical content.")
    }
}

/// Apply one or more edits to LF-normalized content.
///
/// Every edit is matched against the same original content; replacements are
/// applied in reverse offset order so earlier offsets remain valid.
pub fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[Edit],
    path: &str,
) -> Result<AppliedEditsResult, String> {
    let normalized_edits: Vec<Edit> = edits
        .iter()
        .map(|edit| Edit {
            old_text: normalize_to_lf(&edit.old_text),
            new_text: normalize_to_lf(&edit.new_text),
        })
        .collect();

    for (i, edit) in normalized_edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(empty_old_text_error(path, i, normalized_edits.len()));
        }
    }

    let initial_matches: Vec<FuzzyMatchResult> = normalized_edits
        .iter()
        .map(|edit| fuzzy_find_text(normalized_content, &edit.old_text))
        .collect();
    let used_fuzzy_match = initial_matches.iter().any(|m| m.used_fuzzy_match);
    let replacement_base_content = if used_fuzzy_match {
        normalize_for_fuzzy_match(normalized_content)
    } else {
        normalized_content.to_string()
    };

    let mut matched_edits: Vec<MatchedEdit> = Vec::new();
    for (i, edit) in normalized_edits.iter().enumerate() {
        let match_result = fuzzy_find_text(&replacement_base_content, &edit.old_text);
        if !match_result.found {
            return Err(not_found_error(path, i, normalized_edits.len()));
        }

        let occurrences = count_occurrences(&replacement_base_content, &edit.old_text);
        if occurrences > 1 {
            return Err(duplicate_error(
                path,
                i,
                normalized_edits.len(),
                occurrences,
            ));
        }

        matched_edits.push(MatchedEdit {
            edit_index: i,
            match_index: match_result.index,
            match_length: match_result.match_length,
            new_text: edit.new_text.clone(),
        });
    }

    matched_edits.sort_by_key(|m| m.match_index);
    for window in matched_edits.windows(2) {
        let previous = &window[0];
        let current = &window[1];
        if previous.match_index + previous.match_length > current.match_index {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                previous.edit_index, current.edit_index
            ));
        }
    }

    let base_content = normalized_content.to_string();
    let new_content = if used_fuzzy_match {
        let owned: Vec<EditMatch> = matched_edits
            .iter()
            .map(|m| EditMatch {
                match_index: m.match_index,
                match_length: m.match_length,
                new_text: m.new_text.clone(),
            })
            .collect();
        apply_replacements_preserving_unchanged_lines(
            normalized_content,
            &replacement_base_content,
            &owned,
        )?
    } else {
        let replacements: Vec<TextReplacement<'_>> = matched_edits
            .iter()
            .map(|m| TextReplacement {
                match_index: m.match_index,
                match_length: m.match_length,
                new_text: &m.new_text,
            })
            .collect();
        apply_replacements(&replacement_base_content, &replacements, 0)
    };

    if base_content == new_content {
        return Err(no_change_error(path, normalized_edits.len()));
    }

    Ok(AppliedEditsResult {
        base_content,
        new_content,
    })
}

fn pad_start(value: usize, width: usize) -> String {
    format!("{value:>width$}")
}

/// A run of `width` spaces, used for the collapsed-context `...` line.
fn spaces(width: usize) -> String {
    " ".repeat(width)
}

/// Generate a display-oriented diff with line numbers and collapsed context.
///
/// Returns the diff body and the 1-based line number of the first change in
/// the new file.
pub fn generate_diff_string(
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> DiffResult {
    let parts = diff_lines(old_content, new_content);
    let mut output: Vec<String> = Vec::new();

    let old_line_count = old_content.split('\n').count();
    let new_line_count = new_content.split('\n').count();
    let line_num_width = old_line_count.max(new_line_count).to_string().len();

    let mut old_line_num = 1usize;
    let mut new_line_num = 1usize;
    let mut last_was_change = false;
    let mut first_changed_line: Option<usize> = None;

    for (i, part) in parts.iter().enumerate() {
        let mut raw: Vec<&str> = part.value.split('\n').collect();
        if raw.last() == Some(&"") {
            raw.pop();
        }

        if part.kind == DiffKind::Added || part.kind == DiffKind::Removed {
            if first_changed_line.is_none() {
                first_changed_line = Some(new_line_num);
            }

            for line in &raw {
                if part.kind == DiffKind::Added {
                    output.push(format!(
                        "+{} {}",
                        pad_start(new_line_num, line_num_width),
                        line
                    ));
                    new_line_num += 1;
                } else {
                    output.push(format!(
                        "-{} {}",
                        pad_start(old_line_num, line_num_width),
                        line
                    ));
                    old_line_num += 1;
                }
            }
            last_was_change = true;
        } else {
            let next_part_is_change = parts
                .get(i + 1)
                .is_some_and(|p| p.kind == DiffKind::Added || p.kind == DiffKind::Removed);
            let has_leading_change = last_was_change;
            let has_trailing_change = next_part_is_change;

            let push_old = |out: &mut Vec<String>, line: &str, old: &mut usize, new: &mut usize| {
                out.push(format!(" {} {}", pad_start(*old, line_num_width), line));
                *old += 1;
                *new += 1;
            };

            if has_leading_change && has_trailing_change {
                if raw.len() <= context_lines * 2 {
                    for line in &raw {
                        push_old(&mut output, line, &mut old_line_num, &mut new_line_num);
                    }
                } else {
                    let leading = &raw[..context_lines];
                    let trailing = &raw[raw.len() - context_lines..];
                    let skipped = raw.len() - leading.len() - trailing.len();

                    for line in leading {
                        push_old(&mut output, line, &mut old_line_num, &mut new_line_num);
                    }
                    output.push(format!(" {} ...", spaces(line_num_width)));
                    old_line_num += skipped;
                    new_line_num += skipped;
                    for line in trailing {
                        push_old(&mut output, line, &mut old_line_num, &mut new_line_num);
                    }
                }
            } else if has_leading_change {
                let shown = raw.len().min(context_lines);
                for line in &raw[..shown] {
                    push_old(&mut output, line, &mut old_line_num, &mut new_line_num);
                }
                let skipped = raw.len() - shown;
                if skipped > 0 {
                    output.push(format!(" {} ...", spaces(line_num_width)));
                    old_line_num += skipped;
                    new_line_num += skipped;
                }
            } else if has_trailing_change {
                let skipped = raw.len().saturating_sub(context_lines);
                if skipped > 0 {
                    output.push(format!(" {} ...", spaces(line_num_width)));
                    old_line_num += skipped;
                    new_line_num += skipped;
                }
                for line in &raw[skipped..] {
                    push_old(&mut output, line, &mut old_line_num, &mut new_line_num);
                }
            } else {
                old_line_num += raw.len();
                new_line_num += raw.len();
            }

            last_was_change = false;
        }
    }

    DiffResult {
        diff: output.join("\n"),
        first_changed_line,
    }
}

/// One flattened diff line used to build a unified patch.
struct PatchLine {
    kind: DiffKind,
    text: String,
    has_newline: bool,
}

/// Generate a standard unified patch (`--- ` / `+++ ` / `@@` hunks).
///
/// Deviation from `diff`'s `createTwoFilesPatch`: jsdiff emits an
/// `Index: <path>` header when both file names match and honours
/// `headerOptions`; this port emits the separator plus `---` / `+++` only.
/// The hunk body and `\ No newline at end of file` markers follow the
/// standard format, and `applyPatch` in the npm package parses them.
pub fn generate_unified_patch(
    path: &str,
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> String {
    let parts = diff_lines(old_content, new_content);
    let mut lines: Vec<PatchLine> = Vec::new();
    for part in &parts {
        for segment in split_lines_with_endings(&part.value) {
            let (text, has_newline) = match segment.strip_suffix('\n') {
                Some(text) => (text, true),
                None => (segment, false),
            };
            lines.push(PatchLine {
                kind: part.kind,
                text: text.to_string(),
                has_newline,
            });
        }
    }

    let changed: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.kind != DiffKind::Equal)
        .map(|(i, _)| i)
        .collect();

    let mut hunks: Vec<(usize, usize)> = Vec::new();
    for &index in &changed {
        let lo = index.saturating_sub(context_lines);
        let hi = (index + context_lines).min(lines.len() - 1);
        match hunks.last_mut() {
            Some(last) if lo <= last.1 => last.1 = last.1.max(hi),
            _ => hunks.push((lo, hi)),
        }
    }

    let mut out =
        String::from("===================================================================\n");
    out.push_str(&format!("--- {path}\n"));
    out.push_str(&format!("+++ {path}\n"));

    for (lo, hi) in hunks {
        let old_start = 1 + lines[..lo]
            .iter()
            .filter(|l| l.kind != DiffKind::Added)
            .count();
        let new_start = 1 + lines[..lo]
            .iter()
            .filter(|l| l.kind != DiffKind::Removed)
            .count();
        let old_count = lines[lo..=hi]
            .iter()
            .filter(|l| l.kind != DiffKind::Added)
            .count();
        let new_count = lines[lo..=hi]
            .iter()
            .filter(|l| l.kind != DiffKind::Removed)
            .count();
        let old_start = if old_count == 0 {
            old_start - 1
        } else {
            old_start
        };
        let new_start = if new_count == 0 {
            new_start - 1
        } else {
            new_start
        };

        let old_range = if old_count == 1 {
            format!("-{old_start}")
        } else {
            format!("-{old_start},{old_count}")
        };
        let new_range = if new_count == 1 {
            format!("+{new_start}")
        } else {
            format!("+{new_start},{new_count}")
        };
        out.push_str(&format!("@@ {old_range} {new_range} @@\n"));

        for line in &lines[lo..=hi] {
            let prefix = match line.kind {
                DiffKind::Equal => ' ',
                DiffKind::Removed => '-',
                DiffKind::Added => '+',
            };
            out.push(prefix);
            out.push_str(&line.text);
            out.push('\n');
            if !line.has_newline {
                out.push_str("\\ No newline at end of file\n");
            }
        }
    }

    out
}

/// Resolve `path` against `cwd` when relative.
fn resolve_to_cwd(path: &str, cwd: &Path) -> PathBuf {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        cwd.join(candidate)
    }
}

/// Split a leading BOM off `content`, returning `(bom, text)`.
pub fn split_bom(content: &str) -> (&str, &str) {
    match content.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", content),
    }
}

/// Compute the display diff for a set of edits without applying them.
///
/// This is the preview path the `edit` renderer uses while the model is still
/// streaming arguments. Errors are returned as `Err` with a user-facing
/// message rather than an I/O type.
///
/// Deviation from upstream: jsdiff reports `Error code: ENOENT` by reading
/// the Node error's `code`; `std::io::Error` has no portable string code, so
/// the message carries the `Display` form of the error instead.
pub fn compute_edits_diff(path: &str, edits: &[Edit], cwd: &Path) -> Result<DiffResult, String> {
    let absolute_path = resolve_to_cwd(path, cwd);
    let raw_content = std::fs::read_to_string(&absolute_path)
        .map_err(|error| format!("Could not edit file: {path}. {error}."))?;
    let (_, content) = split_bom(&raw_content);
    let normalized_content = normalize_to_lf(content);
    let applied = apply_edits_to_normalized_content(&normalized_content, edits, path)?;
    Ok(generate_diff_string(
        &applied.base_content,
        &applied.new_content,
        4,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(old: &str, new: &str) -> Edit {
        Edit {
            old_text: old.to_string(),
            new_text: new.to_string(),
        }
    }

    #[test]
    fn detects_line_endings() {
        assert_eq!(detect_line_ending("a\nb"), "\n");
        assert_eq!(detect_line_ending("a\r\nb"), "\r\n");
        assert_eq!(detect_line_ending("a\r\nb\nc"), "\r\n");
        assert_eq!(detect_line_ending("a\nb\r\nc"), "\n");
        assert_eq!(detect_line_ending("a\rb"), "\n");
    }

    #[test]
    fn normalizes_and_restores_line_endings() {
        assert_eq!(normalize_to_lf("a\r\nb\rc"), "a\nb\nc");
        assert_eq!(restore_line_endings("a\nb", "\r\n"), "a\r\nb");
        assert_eq!(restore_line_endings("a\nb", "\n"), "a\nb");
    }

    #[test]
    fn fuzzy_normalization_folds_unicode() {
        assert_eq!(normalize_for_fuzzy_match("“a”"), "\"a\"");
        assert_eq!(normalize_for_fuzzy_match("a\u{2014}b"), "a-b");
        assert_eq!(normalize_for_fuzzy_match("a\u{00a0}b"), "a b");
        assert_eq!(normalize_for_fuzzy_match("a  \nb \t\n"), "a\nb\n");
        // Idempotent.
        let once = normalize_for_fuzzy_match("“x” \u{2013} y");
        assert_eq!(normalize_for_fuzzy_match(&once), once);
    }

    #[test]
    fn exact_match_wins_over_fuzzy() {
        let result = fuzzy_find_text("hello world", "world");
        assert!(result.found);
        assert!(!result.used_fuzzy_match);
        assert_eq!(result.index, 6);
        assert_eq!(result.match_length, 5);
    }

    #[test]
    fn fuzzy_match_handles_smart_quotes_and_trailing_space() {
        let content = "const x = “hi”;   \n";
        let result = fuzzy_find_text(content, "const x = \"hi\";");
        assert!(result.found);
        assert!(result.used_fuzzy_match);
    }

    #[test]
    fn applies_multiple_disjoint_edits() {
        let content = "alpha\nbeta\ngamma\ndelta\n";
        let result = apply_edits_to_normalized_content(
            content,
            &[edit("alpha\n", "ALPHA\n"), edit("gamma\n", "GAMMA\n")],
            "f.txt",
        )
        .unwrap();
        assert_eq!(result.new_content, "ALPHA\nbeta\nGAMMA\ndelta\n");
    }

    #[test]
    fn matches_against_original_not_incrementally() {
        let content = "foo\nbar\nbaz\n";
        let result = apply_edits_to_normalized_content(
            content,
            &[edit("foo\n", "foo bar\n"), edit("bar\n", "BAR\n")],
            "f.txt",
        )
        .unwrap();
        assert_eq!(result.new_content, "foo bar\nBAR\nbaz\n");
    }

    #[test]
    fn rejects_ambiguous_and_missing_matches() {
        let duplicate =
            apply_edits_to_normalized_content("foo foo foo", &[edit("foo", "bar")], "f.txt")
                .unwrap_err();
        assert!(duplicate.contains("Found 3 occurrences"), "{duplicate}");

        let missing = apply_edits_to_normalized_content("hello", &[edit("nope", "yes")], "f.txt")
            .unwrap_err();
        assert!(
            missing.contains("Could not find the exact text"),
            "{missing}"
        );
    }

    #[test]
    fn rejects_overlapping_edits() {
        let error = apply_edits_to_normalized_content(
            "one\ntwo\nthree\n",
            &[
                edit("one\ntwo\n", "ONE\nTWO\n"),
                edit("two\nthree\n", "TWO\nTHREE\n"),
            ],
            "f.txt",
        )
        .unwrap_err();
        assert!(error.contains("overlap"), "{error}");
    }

    #[test]
    fn rejects_empty_old_text() {
        let error =
            apply_edits_to_normalized_content("hello", &[edit("", "x")], "f.txt").unwrap_err();
        assert!(error.contains("must not be empty"), "{error}");
    }

    #[test]
    fn rejects_no_op_edit() {
        let error = apply_edits_to_normalized_content("hello", &[edit("hello", "hello")], "f.txt")
            .unwrap_err();
        assert!(error.contains("No changes made"), "{error}");
    }

    #[test]
    fn preserves_untouched_lines_after_fuzzy_match() {
        // The matched line uses smart quotes and trailing whitespace; an
        // untouched line keeps its CRLF-style trailing spaces.
        let original = "keep  \nconst x = “hi”;   \nkeep2\n";
        let normalized = normalize_to_lf(original);
        let result = apply_edits_to_normalized_content(
            &normalized,
            &[edit("const x = \"hi\";", "const x = \"bye\";")],
            "f.txt",
        )
        .unwrap();
        assert_eq!(result.new_content, "keep  \nconst x = \"bye\";\nkeep2\n");
    }

    #[test]
    fn diff_string_numbers_lines_and_collapses_context() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let new = "a\nb\nc\nd\nE\nf\ng\nh\ni\nj\n";
        let result = generate_diff_string(old, new, 4);
        assert!(result.diff.contains("- 5 e"), "{}", result.diff);
        assert!(result.diff.contains("+ 5 E"), "{}", result.diff);
        assert_eq!(result.first_changed_line, Some(5));
    }

    #[test]
    fn diff_string_collapses_large_gaps() {
        let lines: Vec<String> = (1..=600).map(|i| format!("line {i:03}")).collect();
        let old = format!("{}\n", lines.join("\n"));
        let new = old
            .replace("line 100", "LINE 100")
            .replace("line 300", "LINE 300")
            .replace("line 500", "LINE 500");
        let result = generate_diff_string(&old, &new, 4);
        assert!(result.diff.contains("LINE 100"));
        assert!(result.diff.contains("LINE 500"));
        assert!(result.diff.contains("..."));
        assert!(!result.diff.contains("line 250"));
        assert!(result.diff.split('\n').count() < 50, "{}", result.diff);
    }

    #[test]
    fn unified_patch_contains_hunks_and_markers() {
        let patch = generate_unified_patch("f.txt", "Hello, world!", "Hello, testing!", 4);
        assert!(patch.contains("--- f.txt"));
        assert!(patch.contains("+++ f.txt"));
        assert!(patch.contains("@@"));
        assert!(patch.contains("-Hello, world!"));
        assert!(patch.contains("+Hello, testing!"));
        assert!(patch.contains("\\ No newline at end of file"));
    }

    #[test]
    fn unified_patch_round_trips_with_newline() {
        let old = "one\ntwo\nthree\n";
        let new = "one\nTWO\nthree\nfour\n";
        let patch = generate_unified_patch("f.txt", old, new, 4);
        // Rebuild both sides from the hunk body (context + removed, context + added).
        let mut rebuilt_old = String::new();
        let mut rebuilt_new = String::new();
        for line in patch.lines() {
            if line.starts_with("@@")
                || line.starts_with("---")
                || line.starts_with("+++")
                || line.starts_with("===")
            {
                continue;
            }
            if line == "\\ No newline at end of file" {
                continue;
            }
            let (prefix, text) = line.split_at(1);
            match prefix {
                " " => {
                    rebuilt_old.push_str(text);
                    rebuilt_old.push('\n');
                    rebuilt_new.push_str(text);
                    rebuilt_new.push('\n');
                }
                "-" => {
                    rebuilt_old.push_str(text);
                    rebuilt_old.push('\n');
                }
                "+" => {
                    rebuilt_new.push_str(text);
                    rebuilt_new.push('\n');
                }
                _ => {}
            }
        }
        assert_eq!(rebuilt_old, old);
        assert_eq!(rebuilt_new, new);
    }

    #[test]
    fn computes_diff_for_existing_file() {
        let dir = std::env::temp_dir().join(format!("pi-edit-diff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("note.txt");
        std::fs::write(&file, "hello world\n").unwrap();

        let result =
            compute_edits_diff(file.to_str().unwrap(), &[edit("world", "there")], &dir).unwrap();
        assert!(result.diff.contains("+1 hello there"));

        let missing = compute_edits_diff("missing.txt", &[edit("a", "b")], &dir).unwrap_err();
        assert!(missing.starts_with("Could not edit file: missing.txt."));
    }
}
