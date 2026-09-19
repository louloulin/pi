//! Markdown rendering — a self-contained Rust port of the subset of
//! `packages/tui/src/components/markdown.ts` that the assistant message body
//! needs.
//!
//! The renderer is terminal-agnostic: it turns a markdown source string into
//! [`StyledLine`]s whose [`StyledSpan`]s carry the `ThemeColor::Md*` slots the
//! theme layer exposes. The App's buffer path
//! ([`write_styled_line`](crate::styled::write_styled_line)) resolves those
//! slots into `ratatui::style::Style`, so no ANSI escape ever enters a cell.
//!
//! # Covered subset
//!
//! Block level:
//!
//! * ATX headings `#` … `######` (`#` prefix shown for level ≥ 3 like upstream;
//!   H1 is `mdHeading` + bold + underline, H2+ is `mdHeading` + bold).
//! * Paragraphs; every source line inside a paragraph becomes a line break
//!   (upstream keeps marked's soft breaks the same way).
//! * Unordered (`-` / `+` / `*`) and ordered (`1.` / `1)`) lists, including
//!   nested lists (child content is dedented by the marker width, so at least
//!   one nesting level is fully covered).
//! * Fenced code blocks (```` ``` ```` and ` ~~~ `, closed by a fence of the
//!   same character and at least the opening length) with the language tag
//!   echoed on the border. An unclosed fence runs to the end of the input.
//! * Block quotes (`>`), including nested block content.
//! * Horizontal rules (`---`, `***`, `___`) capped at 80 columns.
//!
//! Inline level:
//!
//! * `**bold**` / `__bold__`, `*italic*` / `_italic_`, `` `code` ``,
//!   `~~strikethrough~~`, `[label](url)` links, backslash escapes.
//!
//! # Deliberately not covered (degrade to plain text, never panic)
//!
//! Tables, LaTeX (upstream `renderLatex`), terminal images / OSC-8 hyperlinks,
//! syntax highlighting, block HTML and the `transform` hooks are separate
//! subsystems. A link whose target is not a plain `[label](url)` is emitted as
//! its literal text, and the link URL is always rendered inline
//! (`MdLinkUrl`) because hyperlink capability detection is not ported.
//!
//! # Width convention
//!
//! Wrapping counts characters, exactly like the crate's existing
//! `display_width` (`message.rs` / `selector.rs`): a wide glyph counts as one
//! column. This is the single width convention in `pi-tui`; the markdown module
//! does not introduce a second one.
//!
//! # Examples
//!
//! ```
//! use pi_tui::markdown::render_markdown;
//! use pi_tui::styled::plain_text;
//!
//! let lines = render_markdown("# Hi\n\nbody **bold**", 40);
//! assert_eq!(plain_text(&lines[0]), "Hi");
//! assert_eq!(plain_text(&lines[2]), "body bold");
//! ```

use crate::styled::{SpanStyle, StyledLine, StyledSpan};
use crate::theme::{Theme, ThemeColor};

/// Indentation applied to the body of a fenced code block (upstream
/// `theme.codeBlockIndent ?? "  "`).
const CODE_BLOCK_INDENT: &str = "  ";

/// Horizontal rules are capped like upstream (`"─".repeat(Math.min(width, 80))`).
const MAX_RULE_WIDTH: usize = 80;

/// Render `source` as markdown at `width` columns.
///
/// This is the pure core entry point: it resolves no colours, only the
/// [`ThemeColor`] `Md*` slots carried by the returned spans. Callers that own
/// the active theme should prefer [`render_markdown_with_theme`], which
/// additionally normalises a `NO_COLOR` ([`Theme::is_plain`]) palette.
///
/// The input is normalised the way upstream does it: tabs become three spaces
/// and CRLF line endings are handled. Whitespace-only input renders nothing.
/// Malformed or half-streamed input (an unclosed fence, an unmatched `**`)
/// degrades to literal text and never panics.
pub fn render_markdown(source: &str, width: usize) -> Vec<StyledLine> {
    if source.trim().is_empty() {
        return Vec::new();
    }
    let normalized = source.replace('\t', "   ");
    let lines: Vec<String> = normalized
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect();
    let blocks = parse_blocks(&lines);
    render_blocks(&blocks, width.max(1), SpanStyle::PLAIN)
}

/// Theme-aware variant of [`render_markdown`].
///
/// The [`Theme`] is consulted for the `NO_COLOR` case ([`Theme::is_plain`]): a
/// plain palette defines no colours, so the renderer collapses every span to
/// [`SpanStyle::PLAIN`]. For a coloured theme the result is identical to
/// [`render_markdown`].
pub fn render_markdown_with_theme(source: &str, theme: &Theme, width: usize) -> Vec<StyledLine> {
    let mut lines = render_markdown(source, width);
    if theme.is_plain() {
        for line in &mut lines {
            let mut merged: StyledLine = Vec::new();
            for span in line.drain(..) {
                push_span(&mut merged, span.text, SpanStyle::PLAIN);
            }
            *line = merged;
        }
    }
    lines
}

/// A block-level markdown construct.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Block {
    /// A source blank line.
    Space,
    /// An ATX heading.
    Heading { level: u8, text: String },
    /// A paragraph: one entry per source line.
    Paragraph(Vec<String>),
    /// A fenced code block.
    Code { lang: String, lines: Vec<String> },
    /// A block quote, holding the recursively parsed inner blocks.
    Quote(Vec<Block>),
    /// A list.
    List(ListBlock),
    /// A horizontal rule.
    Hr,
}

/// A parsed list.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ListBlock {
    ordered: bool,
    /// First item number for ordered lists (ignored for unordered ones).
    start: usize,
    items: Vec<ListItem>,
}

/// One list item, holding its recursively parsed content.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ListItem {
    blocks: Vec<Block>,
}

/// A recognised list marker.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Marker {
    /// Leading spaces before the marker.
    indent: usize,
    ordered: bool,
    number: usize,
    /// Columns from the line start to the item content (marker + padding).
    prefix_len: usize,
    content: String,
}

/// A recognised fenced-code opener.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fence {
    marker: char,
    len: usize,
    info: String,
}

// ---------------------------------------------------------------------------
// Block parsing
// ---------------------------------------------------------------------------

/// Parse a run of source lines into blocks.
fn parse_blocks(lines: &[String]) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut i = 0usize;

    while i < lines.len() {
        let line = lines[i].as_str();

        if line.trim().is_empty() {
            blocks.push(Block::Space);
            i += 1;
            continue;
        }

        if let Some(fence) = parse_fence(line) {
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() {
                let closes = parse_fence(lines[i].as_str()).is_some_and(|close| {
                    close.marker == fence.marker && close.len >= fence.len && close.info.is_empty()
                });
                if closes {
                    i += 1;
                    break;
                }
                body.push(lines[i].clone());
                i += 1;
            }
            blocks.push(Block::Code {
                lang: fence.info.clone(),
                lines: body,
            });
            continue;
        }

        if let Some((level, text)) = parse_heading(line) {
            blocks.push(Block::Heading { level, text });
            i += 1;
            continue;
        }

        if parse_hr(line) {
            blocks.push(Block::Hr);
            i += 1;
            continue;
        }

        if parse_quote_line(line).is_some() {
            let mut inner = Vec::new();
            while i < lines.len() {
                match parse_quote_line(lines[i].as_str()) {
                    Some(rest) => {
                        inner.push(rest);
                        i += 1;
                    }
                    None => break,
                }
            }
            blocks.push(Block::Quote(parse_blocks(&inner)));
            continue;
        }

        if let Some(marker) = parse_list_marker(line) {
            let (list, next) = parse_list(lines, i, &marker);
            blocks.push(Block::List(list));
            i = next;
            continue;
        }

        // Anything else is a paragraph, ending at the next blank line or block.
        let mut paragraph = Vec::new();
        while i < lines.len() {
            let candidate = lines[i].as_str();
            if candidate.trim().is_empty() || is_block_start(candidate) {
                break;
            }
            paragraph.push(lines[i].clone());
            i += 1;
        }
        if paragraph.is_empty() {
            // Guarantee progress on an unexpected classification mismatch.
            paragraph.push(lines[i].clone());
            i += 1;
        }
        blocks.push(Block::Paragraph(paragraph));
    }

    blocks
}

/// Parse a list starting at `lines[start]`, whose marker is `first`.
///
/// Returns the parsed list and the index of the first unconsumed line.
fn parse_list(lines: &[String], start: usize, first: &Marker) -> (ListBlock, usize) {
    let indent = first.indent;
    let ordered = first.ordered;
    let mut items: Vec<ListItem> = Vec::new();
    let mut i = start;

    while i < lines.len() {
        let mut item_start = i;

        // A blank line only continues the list when another item follows.
        if lines[item_start].trim().is_empty() {
            let next = skip_blanks(lines, item_start);
            let sibling = next < lines.len()
                && parse_list_marker(lines[next].as_str())
                    .is_some_and(|m| m.indent == indent && m.ordered == ordered);
            if !sibling {
                break;
            }
            item_start = next;
        }

        let marker = match parse_list_marker(lines[item_start].as_str()) {
            Some(m) if m.indent == indent && m.ordered == ordered => m,
            _ => break,
        };

        let content_indent = marker.prefix_len;
        let mut item_lines: Vec<String> = vec![marker.content];

        let mut k = item_start + 1;
        while k < lines.len() {
            let candidate = lines[k].as_str();

            if candidate.trim().is_empty() {
                let next = skip_blanks(lines, k);
                if next >= lines.len() {
                    break;
                }
                let next_line = lines[next].as_str();
                let next_indent = parse_list_marker(next_line)
                    .map(|m| m.indent)
                    .unwrap_or_else(|| leading_spaces(next_line));
                let sibling = parse_list_marker(next_line)
                    .is_some_and(|m| m.indent == indent && m.ordered == ordered);
                if sibling || next_indent <= indent {
                    break;
                }
                item_lines.push(String::new());
                k += 1;
                continue;
            }

            let ls = leading_spaces(candidate);
            if ls > indent {
                item_lines.push(dedent(candidate, content_indent.min(ls)));
                k += 1;
                continue;
            }

            if is_block_start(candidate) {
                break;
            }

            // Lazy paragraph continuation.
            item_lines.push(candidate.to_string());
            k += 1;
        }

        items.push(ListItem {
            blocks: parse_blocks(&item_lines),
        });
        i = k;
    }

    (
        ListBlock {
            ordered,
            start: first.number,
            items,
        },
        i,
    )
}

/// Index of the first non-blank line at or after `from`.
fn skip_blanks(lines: &[String], from: usize) -> usize {
    let mut i = from;
    while i < lines.len() && lines[i].trim().is_empty() {
        i += 1;
    }
    i
}

/// True when `line` opens a block construct (and therefore ends a paragraph).
fn is_block_start(line: &str) -> bool {
    parse_fence(line).is_some()
        || parse_heading(line).is_some()
        || parse_hr(line)
        || parse_quote_line(line).is_some()
        || parse_list_marker(line).is_some()
}

/// Parse an ATX heading (`#` … `######`).
fn parse_heading(line: &str) -> Option<(u8, String)> {
    let trimmed = line.trim_start();
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    let text = rest.trim().to_string();
    // Strip an optional closing `#` sequence (marked's ATX behaviour).
    let without_close = text.trim_end_matches('#');
    let text = if without_close.len() != text.len()
        && (without_close.is_empty() || without_close.ends_with(' '))
    {
        without_close.trim_end().to_string()
    } else {
        text
    };
    Some((hashes as u8, text))
}

/// Parse a fenced-code opener or closer.
fn parse_fence(line: &str) -> Option<Fence> {
    let trimmed = line.trim_start();
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let marker = trimmed.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let len = trimmed.chars().take_while(|c| *c == marker).count();
    if len < 3 {
        return None;
    }
    let rest = &trimmed[len..];
    let info = rest.trim().to_string();
    if marker == '`' && info.contains('`') {
        return None;
    }
    Some(Fence { marker, len, info })
}

/// Parse a horizontal rule (`---`, `***`, `___`, optionally spaced).
fn parse_hr(line: &str) -> bool {
    let trimmed = line.trim_start();
    if line.len() - trimmed.len() > 3 {
        return false;
    }
    let trimmed = trimmed.trim_end();
    let mut chars = trimmed.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    if !matches!(first, '-' | '*' | '_') {
        return false;
    }
    let mut count = 1usize;
    for c in chars {
        if c == first {
            count += 1;
        } else if c != ' ' {
            return false;
        }
    }
    count >= 3
}

/// Strip one level of `>` quoting from `line`, if present.
fn parse_quote_line(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let rest = trimmed.strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest).to_string())
}

/// Parse a list marker (`-` / `+` / `*` or `1.` / `1)`).
fn parse_list_marker(line: &str) -> Option<Marker> {
    let indent = leading_spaces(line);
    let rest = &line[indent..];
    let first = rest.chars().next()?;

    if matches!(first, '-' | '+' | '*') {
        let after = &rest[1..];
        let padding = after.chars().take_while(|c| *c == ' ').count();
        if padding == 0 && !after.is_empty() {
            return None;
        }
        return Some(Marker {
            indent,
            ordered: false,
            number: 1,
            prefix_len: 1 + padding,
            content: after[padding..].to_string(),
        });
    }

    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 9 {
        return None;
    }
    let after_digits = &rest[digits.len()..];
    let delim = after_digits.chars().next()?;
    if delim != '.' && delim != ')' {
        return None;
    }
    let after = &after_digits[1..];
    let padding = after.chars().take_while(|c| *c == ' ').count();
    if padding == 0 || padding > 4 {
        return None;
    }
    Some(Marker {
        indent,
        ordered: true,
        number: digits.parse().unwrap_or(1),
        prefix_len: digits.len() + 1 + padding,
        content: after[padding..].to_string(),
    })
}

/// Count leading ASCII spaces. Tabs were already normalised to spaces.
fn leading_spaces(line: &str) -> usize {
    line.bytes().take_while(|b| *b == b' ').count()
}

/// Drop up to `count` leading spaces from `line`.
fn dedent(line: &str, count: usize) -> String {
    let spaces = leading_spaces(line).min(count);
    line[spaces..].to_string()
}

// ---------------------------------------------------------------------------
// Block rendering
// ---------------------------------------------------------------------------

/// Render blocks into styled lines, using `base` as the inherited style for
/// unstyled body text (plain for a top-level paragraph, quote/`mdQuote` inside
/// a block quote, and so on).
fn render_blocks(blocks: &[Block], width: usize, base: SpanStyle) -> Vec<StyledLine> {
    let width = width.max(1);
    let mut out: Vec<StyledLine> = Vec::new();

    for (idx, block) in blocks.iter().enumerate() {
        let next = blocks.get(idx + 1);
        match block {
            Block::Space => out.push(Vec::new()),
            Block::Hr => {
                out.push(vec![StyledSpan::new(
                    "─".repeat(width.min(MAX_RULE_WIDTH)),
                    SpanStyle::fg(ThemeColor::MdHr),
                )]);
                maybe_blank(&mut out, next);
            }
            Block::Heading { level, text } => {
                let mut style = with_fg(base, ThemeColor::MdHeading).bold();
                if *level == 1 {
                    style = style.underline();
                }
                let mut line: StyledLine = Vec::new();
                if *level >= 3 {
                    push_span(
                        &mut line,
                        format!("{} ", "#".repeat(*level as usize)),
                        style,
                    );
                }
                for span in render_inline(text, style) {
                    push_span(&mut line, span.text, span.style);
                }
                out.extend(wrap_line(&line, width));
                maybe_blank(&mut out, next);
            }
            Block::Paragraph(lines) => {
                for line in lines {
                    out.extend(wrap_line(&render_inline(line, base), width));
                }
                // Upstream suppresses the trailing blank before a list (the
                // list supplies its own separation) or another blank line.
                if next.is_some_and(|b| !matches!(b, Block::Space | Block::List(_))) {
                    out.push(Vec::new());
                }
            }
            Block::Code { lang, lines } => {
                out.push(vec![StyledSpan::new(
                    format!("```{lang}"),
                    SpanStyle::fg(ThemeColor::MdCodeBlockBorder),
                )]);
                let code_style = with_fg(base, ThemeColor::MdCodeBlock);
                for code_line in lines {
                    let mut spans: StyledLine =
                        vec![StyledSpan::new(CODE_BLOCK_INDENT, SpanStyle::PLAIN)];
                    push_span(&mut spans, code_line.clone(), code_style);
                    out.push(spans);
                }
                out.push(vec![StyledSpan::new(
                    "```",
                    SpanStyle::fg(ThemeColor::MdCodeBlockBorder),
                )]);
                maybe_blank(&mut out, next);
            }
            Block::Quote(inner) => {
                let inner_width = width.saturating_sub(2).max(1);
                let quote_base = with_fg(base, ThemeColor::MdQuote).italic();
                let mut inner_lines = render_blocks(inner, inner_width, quote_base);
                while inner_lines.last().is_some_and(|line| line.is_empty()) {
                    inner_lines.pop();
                }
                for line in inner_lines {
                    for wrapped in wrap_line(&line, inner_width) {
                        let mut spans: StyledLine = vec![StyledSpan::new(
                            "│ ",
                            SpanStyle::fg(ThemeColor::MdQuoteBorder),
                        )];
                        spans.extend(wrapped);
                        out.push(spans);
                    }
                }
                maybe_blank(&mut out, next);
            }
            Block::List(list) => out.extend(render_list(list, width)),
        }
    }

    out
}

/// Push a separator blank line unless the next block is already a blank line
/// (or the document ended).
fn maybe_blank(out: &mut Vec<StyledLine>, next: Option<&Block>) {
    if next.is_some_and(|b| !matches!(b, Block::Space)) {
        out.push(Vec::new());
    }
}

/// Render a list, prefixing each item with an `MdListBullet` marker and
/// indenting continuation / nested lines by the marker width.
fn render_list(list: &ListBlock, width: usize) -> Vec<StyledLine> {
    let mut out = Vec::new();
    for (idx, item) in list.items.iter().enumerate() {
        let marker = if list.ordered {
            format!("{}. ", list.start + idx)
        } else {
            "- ".to_string()
        };
        let marker_w = marker.chars().count();
        let content_width = width.saturating_sub(marker_w).max(1);
        let mut item_lines = render_blocks(&item.blocks, content_width, SpanStyle::PLAIN);
        if item_lines.is_empty() {
            item_lines.push(Vec::new());
        }

        for (line_idx, line) in item_lines.into_iter().enumerate() {
            let mut spans: StyledLine = Vec::new();
            if line_idx == 0 {
                push_span(
                    &mut spans,
                    marker.clone(),
                    SpanStyle::fg(ThemeColor::MdListBullet),
                );
            } else {
                push_span(&mut spans, " ".repeat(marker_w), SpanStyle::PLAIN);
            }
            spans.extend(line);
            out.push(spans);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Inline parsing
// ---------------------------------------------------------------------------

/// Parse inline markup into styled spans, inheriting `base`.
fn render_inline(text: &str, base: SpanStyle) -> StyledLine {
    let chars: Vec<char> = text.chars().collect();
    let mut out: StyledLine = Vec::new();
    parse_inline(&chars, base, &mut out);
    out
}

/// Recursive inline scanner over `chars`.
fn parse_inline(chars: &[char], base: SpanStyle, out: &mut StyledLine) {
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                if i + 1 < chars.len() && is_escapable(chars[i + 1]) {
                    push_span(out, chars[i + 1].to_string(), base);
                    i += 2;
                } else {
                    push_span(out, "\\".to_string(), base);
                    i += 1;
                }
            }
            '`' => {
                if let Some(end) = find_char(chars, i + 1, '`') {
                    if end > i + 1 {
                        let code: String = chars[i + 1..end].iter().collect();
                        push_span(out, code, with_fg(base, ThemeColor::MdCode));
                        i = end + 1;
                        continue;
                    }
                }
                push_span(out, "`".to_string(), base);
                i += 1;
            }
            '*' | '_' => {
                if let Some(consumed) = parse_emphasis(chars, i, base, out) {
                    i += consumed;
                    continue;
                }
                push_span(out, c.to_string(), base);
                i += 1;
            }
            '~' => {
                if let Some(consumed) = parse_strikethrough(chars, i, base, out) {
                    i += consumed;
                    continue;
                }
                push_span(out, "~".to_string(), base);
                i += 1;
            }
            '[' => {
                if let Some(consumed) = parse_link(chars, i, base, out) {
                    i += consumed;
                    continue;
                }
                push_span(out, "[".to_string(), base);
                i += 1;
            }
            _ => {
                let start = i;
                while i < chars.len() && !is_special(chars[i]) {
                    i += 1;
                }
                let run: String = chars[start..i].iter().collect();
                push_span(out, run, base);
            }
        }
    }
}

/// Parse `**` / `__` (strong) or `*` / `_` (emphasis) at `chars[i]`.
///
/// Returns the number of characters consumed, or `None` when the delimiter has
/// no valid closing run (the caller then emits it literally).
fn parse_emphasis(
    chars: &[char],
    i: usize,
    base: SpanStyle,
    out: &mut StyledLine,
) -> Option<usize> {
    let c = chars[i];
    if i + 1 < chars.len() && chars[i + 1] == c {
        if let Some(end) = find_seq(chars, i + 2, &[c, c]) {
            if end > i + 2 {
                parse_inline(&chars[i + 2..end], base.bold(), out);
                return Some(end + 2 - i);
            }
        }
    }
    if !emphasis_opens(chars, i, c) {
        return None;
    }
    let end = find_single_close(chars, i + 1, c)?;
    if end <= i + 1 {
        return None;
    }
    parse_inline(&chars[i + 1..end], base.italic(), out);
    Some(end + 1 - i)
}

/// Parse a `~~strikethrough~~` run at `chars[i]`.
fn parse_strikethrough(
    chars: &[char],
    i: usize,
    base: SpanStyle,
    out: &mut StyledLine,
) -> Option<usize> {
    if i + 1 >= chars.len() || chars[i + 1] != '~' {
        return None;
    }
    let end = find_seq(chars, i + 2, &['~', '~'])?;
    if end <= i + 2 {
        return None;
    }
    parse_inline(&chars[i + 2..end], base.strikethrough(), out);
    Some(end + 2 - i)
}

/// Parse an inline `[label](url)` link at `chars[start]`.
///
/// The label is rendered with `mdLink` + underline; the URL is appended in
/// parentheses with `mdLinkUrl` unless the label already *is* the URL. Returns
/// the number of characters consumed, or `None` when this is not a well-formed
/// link.
fn parse_link(
    chars: &[char],
    start: usize,
    base: SpanStyle,
    out: &mut StyledLine,
) -> Option<usize> {
    let label_end = find_matching_bracket(chars, start)?;
    let open = label_end + 1;
    if open >= chars.len() || chars[open] != '(' {
        return None;
    }
    let close = find_matching_paren(chars, open)?;
    let label: String = chars[start + 1..label_end].iter().collect();
    let raw: String = chars[open + 1..close].iter().collect();
    let url = normalize_url(raw.trim());
    if url.is_empty() {
        return None;
    }

    let label_style = with_fg(base, ThemeColor::MdLink).underline();
    for span in render_inline(&label, label_style) {
        push_span(out, span.text, span.style);
    }
    let compare = url.strip_prefix("mailto:").unwrap_or(&url);
    if label.trim() != url.as_str() && label.trim() != compare {
        push_span(
            out,
            format!(" ({url})"),
            with_fg(base, ThemeColor::MdLinkUrl),
        );
    }
    Some(close + 1 - start)
}

/// Strip an optional `<...>` wrapper and any `"title"` suffix from a link
/// target.
fn normalize_url(raw: &str) -> String {
    if let Some(rest) = raw.strip_prefix('<') {
        if let Some(end) = rest.find('>') {
            return rest[..end].to_string();
        }
    }
    raw.split_whitespace().next().unwrap_or("").to_string()
}

/// Index of the `]` matching the `[` at `start`.
fn find_matching_bracket(chars: &[char], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut j = start;
    while j < chars.len() {
        match chars[j] {
            '[' if !is_escaped(chars, j) => depth += 1,
            ']' if !is_escaped(chars, j) => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// Index of the `)` matching the `(` at `open`.
fn find_matching_paren(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut j = open;
    while j < chars.len() {
        match chars[j] {
            '(' if !is_escaped(chars, j) => depth += 1,
            ')' if !is_escaped(chars, j) => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// Index of the first unescaped `target` at or after `from`.
fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    let mut j = from;
    while j < chars.len() {
        if chars[j] == target && !is_escaped(chars, j) {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// Index of the first unescaped occurrence of `seq` at or after `from`.
fn find_seq(chars: &[char], from: usize, seq: &[char]) -> Option<usize> {
    if seq.is_empty() {
        return None;
    }
    let mut j = from;
    while j + seq.len() <= chars.len() {
        if chars[j..j + seq.len()].iter().eq(seq.iter()) && !is_escaped(chars, j) {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// True when the delimiter at `i` may open an emphasis run.
fn emphasis_opens(chars: &[char], i: usize, c: char) -> bool {
    if i + 1 >= chars.len() || chars[i + 1].is_whitespace() {
        return false;
    }
    if c == '_' && i > 0 && chars[i - 1].is_alphanumeric() {
        return false;
    }
    true
}

/// Index of the single-char emphasis closer for `chars[i]`.
fn find_single_close(chars: &[char], from: usize, c: char) -> Option<usize> {
    let mut j = from;
    while j < chars.len() {
        if chars[j] == c && !is_escaped(chars, j) {
            let double_start = j + 1 < chars.len() && chars[j + 1] == c;
            let word_boundary_violation =
                c == '_' && j + 1 < chars.len() && chars[j + 1].is_alphanumeric();
            if !double_start && !word_boundary_violation && j > from {
                return Some(j);
            }
        }
        j += 1;
    }
    None
}

/// True when `idx` is preceded by an odd number of backslashes.
fn is_escaped(chars: &[char], idx: usize) -> bool {
    let mut backslashes = 0usize;
    let mut j = idx;
    while j > 0 && chars[j - 1] == '\\' {
        backslashes += 1;
        j -= 1;
    }
    backslashes % 2 == 1
}

/// Characters with dedicated inline handling; the plain-text scanner stops at
/// every one of them.
fn is_special(c: char) -> bool {
    matches!(c, '\\' | '`' | '*' | '_' | '~' | '[')
}

/// Characters a backslash escapes.
fn is_escapable(c: char) -> bool {
    matches!(
        c,
        '\\' | '`'
            | '*'
            | '_'
            | '{'
            | '}'
            | '['
            | ']'
            | '('
            | ')'
            | '#'
            | '+'
            | '-'
            | '.'
            | '!'
            | '>'
            | '~'
            | '|'
    )
}

/// `base` with its foreground replaced by `fg`.
fn with_fg(base: SpanStyle, fg: ThemeColor) -> SpanStyle {
    SpanStyle {
        fg: Some(fg),
        ..base
    }
}

/// Append `text` to `out`, merging with the previous span when the slots match.
fn push_span(out: &mut StyledLine, text: impl Into<String>, style: SpanStyle) {
    let text = text.into();
    if text.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut() {
        if last.style == style {
            last.text.push_str(&text);
            return;
        }
    }
    out.push(StyledSpan::new(text, style));
}

// ---------------------------------------------------------------------------
// Wrapping
// ---------------------------------------------------------------------------

/// Wrap a styled line at `width` columns.
///
/// Breaks at the last space that fits, hard-breaks a run that is wider than the
/// whole line, and honours embedded `\n` as a hard break. Character counting
/// matches the crate's `display_width` convention (see the module docs).
fn wrap_line(line: &[StyledSpan], width: usize) -> Vec<StyledLine> {
    if width == 0 {
        return vec![line.to_vec()];
    }
    let chars: Vec<(char, SpanStyle)> = line
        .iter()
        .flat_map(|span| span.text.chars().map(move |c| (c, span.style)))
        .collect();
    if chars.is_empty() {
        return vec![Vec::new()];
    }

    let mut out: Vec<StyledLine> = Vec::new();
    let mut start = 0usize;
    let mut last_space: Option<usize> = None;
    let mut col = 0usize;
    let mut i = 0usize;

    while i < chars.len() {
        let (ch, _) = chars[i];
        if ch == '\n' {
            out.push(build_line(&chars[start..i]));
            start = i + 1;
            col = 0;
            last_space = None;
            i += 1;
            continue;
        }
        if ch == ' ' && i >= start {
            last_space = Some(i);
        }
        if col >= width {
            match last_space.filter(|b| *b > start) {
                Some(b) => {
                    out.push(build_line(&chars[start..b]));
                    start = b + 1;
                    last_space = None;
                    if let Some(tail) = chars.get(start..i) {
                        for (offset, (c, _)) in tail.iter().enumerate() {
                            if *c == ' ' {
                                last_space = Some(start + offset);
                            }
                        }
                    }
                    col = i.saturating_sub(start);
                    continue;
                }
                None => {
                    out.push(build_line(&chars[start..i]));
                    start = i;
                    col = 0;
                    last_space = None;
                    continue;
                }
            }
        }
        col += 1;
        i += 1;
    }

    out.push(build_line(&chars[start..]));
    out
}

/// Build a styled line from a char/style run, merging equal slots.
fn build_line(chars: &[(char, SpanStyle)]) -> StyledLine {
    let mut out: StyledLine = Vec::new();
    for (c, style) in chars {
        push_span(&mut out, c.to_string(), *style);
    }
    out
}
