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
//! * GFM tables: a header row, a `---` delimiter row and any number of body
//!   rows, rendered as a box-drawing grid with width-aware cell wrapping
//!   (upstream `renderTable` in `components/markdown.ts:839-1015`).
//! * Horizontal rules (`---`, `***`, `___`) capped at 80 columns.
//! * LaTeX math blocks (`$$…$$` and `\[…\]`) rendered through
//!   [`crate::latex::render_latex_with`] in display mode, falling back to the
//!   trimmed source when the expression is unsupported.
//! * Standalone images whose target is an inline `data:image/…;base64,…` URI:
//!   the line is rendered through [`crate::image::Image`], so a terminal that
//!   advertises image support gets the kitty / iTerm2 rows and every other
//!   terminal gets the `[Image: …]` label. A payload this crate cannot size
//!   falls back to the alt text.
//!
//! Inline level:
//!
//! * `**bold**` / `__bold__`, `*italic*` / `_italic_`, `` `code` ``,
//!   `~~strikethrough~~`, `[label](url)` links, backslash escapes. An image that
//!   is *not* a standalone data-URI line (`![alt](url)`) renders as its alt
//!   text, which is what marked shows when an image cannot be drawn inline.
//! * Inline LaTeX (`$…$`, `$$…$$`, `\(…\)`, `\[…\]`) rendered through
//!   [`crate::latex::render_latex`], with the upstream pending/malformed guards
//!   (a `$` opener followed by whitespace, a trailing digit, a backtick or an
//!   empty/newline-only body stays literal text).
//!
//! # Deliberately not covered (degrade to plain text, never panic)
//!
//! Terminal images are covered only in the standalone `data:` form above: a
//! remote image URL or a `file://` target is not fetched, so it renders as the
//! alt-text link. Syntax highlighting for the outer language, block HTML and
//! the `transform` hooks are separate subsystems.
//! A link whose target is not a plain `[label](url)` is emitted as its literal
//! text. A parsed link always carries its target on the label spans
//! ([`StyledSpan::link`]) *and* the inline `MdLinkUrl` fallback; the two entry
//! points pick which of the two is visible — [`render_markdown`] keeps the
//! inline URL and drops the link, [`render_markdown_with_links`] keeps the
//! OSC 8 link and drops the inline URL.
//!
//! Table *alignment* (`:---:`) is parsed and validated but not rendered —
//! upstream's `renderTable` ignores `token.align` too, so a centred column is
//! left-aligned here exactly as it is there. A `|` inside an inline code span
//! still splits a row (marked splits it the same way). A table too narrow to
//! hold its borders replays the raw source line by line (upstream wraps
//! `token.raw` as a single string), and a body row without a pipe ends the
//! table rather than being absorbed as a row.
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

use crate::latex::{render_latex, render_latex_with};
use crate::styled::{plain_text, SpanStyle, StyledLine, StyledSpan};
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
    render_markdown_with_links(source, width, false)
}

/// Render `source` as markdown at `width` columns, choosing the link style.
///
/// With `hyperlinks` false this is exactly [`render_markdown`] (label plus an
/// inline `mdLinkUrl` suffix). With `hyperlinks` true every link label keeps
/// its OSC 8 target and the inline suffix is dropped, matching upstream's
/// `getCapabilities().hyperlinks` branch
/// (`packages/tui/src/components/markdown.ts:689-706`).
///
/// The decision is made here rather than in [`parse_link`] so the parser can
/// stay capability-agnostic and the same parse feeds both branches.
pub fn render_markdown_with_links(source: &str, width: usize, hyperlinks: bool) -> Vec<StyledLine> {
    let mut lines = render_markdown_inner(source, width);
    apply_link_capability(&mut lines, hyperlinks);
    lines
}

/// Apply the hyperlink capability to an already-rendered line set.
///
/// `mdLinkUrl` is emitted only by [`parse_link`] as the inline fallback, so
/// dropping those spans is how the OSC 8 branch hides the URL. In the other
/// direction the `link` field is cleared so [`crate::styled::themed_text`]
/// never wraps a span the terminal cannot accept.
fn apply_link_capability(lines: &mut [StyledLine], hyperlinks: bool) {
    for line in lines.iter_mut() {
        if hyperlinks {
            line.retain(|span| span.style.fg != Some(ThemeColor::MdLinkUrl));
        } else {
            for span in line.iter_mut() {
                span.link = None;
            }
        }
    }
}

fn render_markdown_inner(source: &str, width: usize) -> Vec<StyledLine> {
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
    /// A GFM table.
    Table(TableBlock),
    /// A list.
    List(ListBlock),
    /// A horizontal rule.
    Hr,
    /// A standalone `![alt](data:image/…;base64,…)` line.
    Image(ImageBlock),
    /// A LaTeX math block (`$$…$$` or `\[…\]`).
    Latex(LatexBlock),
}

/// A parsed markdown image.
///
/// Only the inline `data:` form is kept: the payload has to be in the source
/// already, because the renderer never does I/O. `alt` is what a terminal
/// without image support — or an unparsable payload — shows instead.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ImageBlock {
    /// Alt text from `![alt](…)`.
    alt: String,
    /// MIME type from the data URI, e.g. `image/png`.
    mime_type: String,
    /// Base64 payload from the data URI.
    data: String,
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

/// A parsed GFM table.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TableBlock {
    /// Header cells, one per column.
    header: Vec<String>,
    /// Body rows, normalised to `header.len()` cells (padded / truncated the
    /// way marked normalises them).
    rows: Vec<Vec<String>>,
    /// The raw source lines, replayed verbatim when the table is too narrow to
    /// lay out (upstream falls back to `token.raw` the same way).
    raw: Vec<String>,
}

/// A recognised fenced-code opener.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fence {
    marker: char,
    len: usize,
    info: String,
}

/// A LaTeX math block.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LatexBlock {
    /// Content between the delimiters: trimmed for a closed block, verbatim for
    /// a pending one (whose raw source is rendered instead).
    text: String,
    /// The raw source lines the block spans, replayed when rendering fails.
    raw: Vec<String>,
    /// True when no closing delimiter was found before the end of the input.
    pending: bool,
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

        if let Some((block, next)) = parse_latex_block(lines, i) {
            blocks.push(Block::Latex(block));
            i = next;
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

        // A standalone image is rendered as terminal rows rather than as a
        // paragraph; an image inside a paragraph stays inline (alt text).
        if let Some(image) = parse_image_block(line) {
            blocks.push(Block::Image(image));
            i += 1;
            continue;
        }

        // A table must be the first line of its block, exactly like GFM: a
        // header row sitting inside a paragraph does not start one.
        if let Some((table, next)) = parse_table(lines, i) {
            blocks.push(Block::Table(table));
            i = next;
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

/// Parse a GFM table starting at `lines[start]` (the header row).
///
/// Returns the table and the index of the first unconsumed line. The header row
/// must be followed by a delimiter row with the same number of cells, and the
/// body ends at the first blank line, block start, or line without a pipe.
fn parse_table(lines: &[String], start: usize) -> Option<(TableBlock, usize)> {
    let header_line = lines.get(start)?.as_str();
    if !header_line.contains('|') || is_block_start(header_line) {
        return None;
    }
    let delimiter = lines.get(start + 1)?;
    let columns = parse_delimiter_row(delimiter)?;
    let header = split_table_row(header_line);
    if header.is_empty() || header.len() != columns {
        return None;
    }

    let mut raw = vec![lines[start].clone(), lines[start + 1].clone()];
    let mut rows = Vec::new();
    let mut i = start + 2;
    while let Some(line) = lines.get(i) {
        if line.trim().is_empty() || !line.contains('|') || is_block_start(line) {
            break;
        }
        let mut cells = split_table_row(line);
        cells.resize(header.len(), String::new());
        cells.truncate(header.len());
        rows.push(cells);
        raw.push(line.clone());
        i += 1;
    }

    Some((TableBlock { header, rows, raw }, i))
}

/// Number of columns in a delimiter row (`| --- | :-: |`), or `None` when
/// `line` is not one.
///
/// The `:` alignment markers are accepted and validated but not reported:
/// upstream's `renderTable` never reads `token.align`, so this port renders
/// every column left-aligned like it does.
fn parse_delimiter_row(line: &str) -> Option<usize> {
    // GFM requires a pipe somewhere on the delimiter row; without it a bare
    // `---` is a horizontal rule.
    if !line.contains('|') {
        return None;
    }
    let cells = split_table_row(line);
    if cells.is_empty() || !cells.iter().all(|cell| is_delimiter_cell(cell)) {
        return None;
    }
    Some(cells.len())
}

/// True for a single delimiter cell: `-`, `--`, `:-:`, and so on.
fn is_delimiter_cell(cell: &str) -> bool {
    let body = cell.trim();
    let body = body.strip_prefix(':').unwrap_or(body);
    let body = body.strip_suffix(':').unwrap_or(body);
    !body.is_empty() && body.chars().all(|c| c == '-')
}

/// Split a table row into trimmed cells.
///
/// Splits on unescaped `|` and drops the empty cells the optional outer pipes
/// create. A `\|` is kept escaped so the inline scanner turns it back into a
/// literal `|`. Like marked, a `|` inside an inline code span still splits the
/// row — a documented divergence, never a panic.
fn split_table_row(line: &str) -> Vec<String> {
    let mut cells: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = line.trim().chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                current.push('\\');
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '|' => cells.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    cells.push(current);

    if cells.first().is_some_and(|cell| cell.trim().is_empty()) {
        cells.remove(0);
    }
    if cells.last().is_some_and(|cell| cell.trim().is_empty()) {
        cells.pop();
    }
    cells
        .into_iter()
        .map(|cell| cell.trim().to_string())
        .collect()
}

/// True when `line` opens a block construct (and therefore ends a paragraph).
fn is_block_start(line: &str) -> bool {
    starts_latex_block(line)
        || parse_fence(line).is_some()
        || parse_heading(line).is_some()
        || parse_hr(line)
        || parse_quote_line(line).is_some()
        || parse_list_marker(line).is_some()
}

/// Parse a LaTeX math block (`$$…$$` or `\[…\]`) starting at `lines[start]`.
///
/// Returns the parsed block and the index of the first unconsumed line. The
/// shape mirrors upstream `tokenizeBlockLatex` (`components/markdown.ts:101`):
/// the opener sits at up to three leading spaces, the closing delimiter must be
/// the last non-padding content of its line, and an unterminated `$$` only
/// becomes a pending block when its body looks like math (an unterminated `\[`
/// always does).
fn parse_latex_block(lines: &[String], start: usize) -> Option<(LatexBlock, usize)> {
    let indent = leading_spaces(lines[start].as_str());
    if indent > 3 {
        return None;
    }
    let source = lines[start..].join("\n");
    let rest = &source[indent..];
    let (closing, bracket) = if rest.starts_with("$$") {
        ("$$", false)
    } else if rest.starts_with("\\[") {
        ("\\]", true)
    } else {
        return None;
    };
    let content_start = skip_latex_opener(&source, indent + 2);

    let mut search = content_start;
    while let Some(index) = find_closing_delimiter(&source, closing, search) {
        // An empty capture never matches upstream's regex; keep scanning.
        if is_line_end_padding(&source, index + closing.len()) && index > content_start {
            let text = source[content_start..index].trim().to_string();
            let close_line = start + source[..index].matches('\n').count();
            return Some((
                LatexBlock {
                    text,
                    raw: lines[start..=close_line].to_vec(),
                    pending: false,
                },
                close_line + 1,
            ));
        }
        search = index + closing.len();
    }

    let text = source[content_start..].to_string();
    if !bracket && !looks_like_pending_dollar_math(&text) {
        return None;
    }
    Some((
        LatexBlock {
            text,
            raw: lines[start..].to_vec(),
            pending: true,
        },
        lines.len(),
    ))
}

/// True when `line` looks like the first line of a LaTeX math block.
fn starts_latex_block(line: &str) -> bool {
    let indent = leading_spaces(line);
    if indent > 3 {
        return false;
    }
    let rest = &line[indent..];
    let (tail, closing) = if let Some(tail) = rest.strip_prefix("$$") {
        (tail, "$$")
    } else if let Some(tail) = rest.strip_prefix("\\[") {
        (tail, "\\]")
    } else {
        return false;
    };
    if tail.bytes().all(|b| b == b' ' || b == b'\t') {
        return true;
    }
    matches!(
        find_closing_delimiter(tail, closing, 0),
        Some(index) if is_line_end_padding(tail, index + closing.len())
    )
}

/// Index after a block opener: its trailing spaces/tabs and one newline are not
/// part of the captured body (upstream `[ \t]*(?:\n)?`).
fn skip_latex_opener(source: &str, mut index: usize) -> usize {
    let bytes = source.as_bytes();
    while index < bytes.len() && (bytes[index] == b' ' || bytes[index] == b'\t') {
        index += 1;
    }
    if index < bytes.len() && bytes[index] == b'\n' {
        index += 1;
    }
    index
}

/// True when every byte from `index` to the end of its line is a space or tab.
fn is_line_end_padding(source: &str, index: usize) -> bool {
    for byte in source.as_bytes()[index..].iter().copied() {
        match byte {
            b'\n' => return true,
            b' ' | b'\t' => {}
            _ => return false,
        }
    }
    true
}

/// Index of the next `closing` occurrence at or after `from` that is not
/// backslash-escaped.
fn find_closing_delimiter(source: &str, closing: &str, from: usize) -> Option<usize> {
    let mut search = from;
    while let Some(relative) = source[search..].find(closing) {
        let index = search + relative;
        if !is_escaped_str(source, index) {
            return Some(index);
        }
        search = index + closing.len();
    }
    None
}

/// True when `index` is preceded by an odd number of backslashes.
fn is_escaped_str(source: &str, index: usize) -> bool {
    let bytes = source.as_bytes();
    let mut backslashes = 0usize;
    let mut j = index;
    while j > 0 && bytes[j - 1] == b'\\' {
        backslashes += 1;
        j -= 1;
    }
    backslashes % 2 == 1
}

/// True when an unterminated `$$` body looks like math (upstream
/// `looksLikePendingDollarMath`, `components/markdown.ts:44`).
fn looks_like_pending_dollar_math(source: &str) -> bool {
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if chars.peek().is_some_and(|next| next.is_ascii_alphabetic()) {
                return true;
            }
            continue;
        }
        if matches!(
            c,
            '_' | '^'
                | '='
                | '+'
                | '*'
                | '/'
                | '<'
                | '>'
                | '('
                | ')'
                | '['
                | ']'
                | '|'
                | '±'
                | '≤'
                | '≥'
                | '≠'
                | '≈'
                | '∈'
                | '→'
                | '⇒'
                | '∞'
                | '∫'
                | '∑'
                | '√'
                | '-'
        ) {
            return true;
        }
    }
    false
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
            Block::Image(image) => {
                out.extend(render_image_block(image, width));
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
            Block::Latex(block) => {
                let rendered = if block.pending {
                    block.raw.join("\n").trim().to_string()
                } else {
                    render_latex_with(&block.text, true)
                        .unwrap_or_else(|| block.raw.join("\n").trim().to_string())
                };
                for line in rendered.split('\n') {
                    let mut spans: StyledLine = Vec::new();
                    push_span(&mut spans, line.to_string(), base);
                    out.push(spans);
                }
                maybe_blank(&mut out, next);
            }
            Block::Code { lang, lines } => {
                out.push(vec![StyledSpan::new(
                    format!("```{lang}"),
                    SpanStyle::fg(ThemeColor::MdCodeBlockBorder),
                )]);
                let code_style = with_fg(base, ThemeColor::MdCodeBlock);
                // Upstream delegates fenced code to `theme.highlightCode`
                // (`packages/tui/src/components/markdown.ts`), which maps
                // `highlight.js` classes onto the `ThemeColor::Syntax*` slots.
                // `highlight::highlight_code` reproduces that for the subset of
                // languages it knows, keeping the base code-block color for
                // everything it does not classify.
                let source = lines.join("\n");
                let highlighted = crate::highlight::highlight_code(
                    &source,
                    if lang.is_empty() {
                        None
                    } else {
                        Some(lang.as_str())
                    },
                    code_style,
                );
                for code_line in highlighted {
                    let mut spans: StyledLine =
                        vec![StyledSpan::new(CODE_BLOCK_INDENT, SpanStyle::PLAIN)];
                    spans.extend(code_line);
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
            Block::Table(table) => {
                out.extend(render_table(table, width, base));
                maybe_blank(&mut out, next);
            }
        }
    }

    out
}

/// Push a separator blank line unless the next block is already a blank line
/// (or the document ended).
// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

/// Render a standalone image through [`crate::image::Image`].
///
/// `Image` owns both branches: with an image protocol in the terminal
/// capabilities it returns the kitty / iTerm2 rows, without one the
/// `[Image: …]` label. In both cases the label style is `mdLink`, so the
/// fallback reads as the image's alt-ish reference. A payload whose header
/// cannot be sized is a broken image: marked shows the alt text, and so does
/// this — which is also why the payload is checked before `Image` is built
/// (`Image::new` would silently assume `800x600`).
fn render_image_block(image: &ImageBlock, width: usize) -> Vec<StyledLine> {
    let style = with_fg(SpanStyle::PLAIN, ThemeColor::MdLink);
    if crate::terminal_image::get_image_dimensions(&image.data, &image.mime_type).is_none() {
        if image.alt.is_empty() {
            return Vec::new();
        }
        return vec![vec![StyledSpan::new(image.alt.clone(), style)]];
    }
    let component = crate::image::Image::new(
        image.data.clone(),
        image.mime_type.clone(),
        crate::image::ImageTheme::fallback(style),
    );
    let width = u16::try_from(width).unwrap_or(u16::MAX).max(1);
    crate::component::Component::render(&component, width)
}

/// Parse a line that is exactly `![alt](data:image/…;base64,…)`.
///
/// The whole line must be consumed, so an image sharing its line with prose
/// stays inline. Returns `None` for every other image: a remote URL, a
/// `file://` path, a vendored format without a `base64` marker or a MIME type
/// outside `image/*`.
fn parse_image_block(line: &str) -> Option<ImageBlock> {
    let trimmed = line.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.first() != Some(&'!') || chars.get(1) != Some(&'[') {
        return None;
    }
    let label_end = find_matching_bracket(&chars, 1)?;
    let open = label_end + 1;
    if chars.get(open) != Some(&'(') {
        return None;
    }
    let close = find_matching_paren(&chars, open)?;
    if close + 1 != chars.len() {
        return None;
    }

    let alt: String = chars[2..label_end].iter().collect();
    let raw: String = chars[open + 1..close].iter().collect();
    let (mime_type, data) = parse_data_image_url(&normalize_url(raw.trim()))?;
    Some(ImageBlock {
        alt,
        mime_type,
        data,
    })
}

/// Split a `data:image/<type>[;…];base64,<payload>` URI.
///
/// The `;base64` marker is required: an inline image that is percent-encoded
/// or not declared base64 is left to the inline path, which shows the alt text
/// rather than trying to decode an unknown encoding.
fn parse_data_image_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let mut params = meta.split(';');
    let mime_type = params.next()?.trim().to_ascii_lowercase();
    if !mime_type.starts_with("image/") {
        return None;
    }
    if !params.any(|param| param.eq_ignore_ascii_case("base64")) {
        return None;
    }
    let data = data.trim();
    if data.is_empty() {
        return None;
    }
    Some((mime_type, data.to_string()))
}

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
// Tables
// ---------------------------------------------------------------------------

/// Longest word (in characters) that a table cell may demand before it is
/// allowed to wrap mid-word (upstream `maxUnbrokenWordWidth`).
const MAX_UNBROKEN_WORD_WIDTH: usize = 30;

/// Column widths for one table, in the order upstream computes them.
fn table_column_widths(table: &TableBlock, available: usize, base: SpanStyle) -> Vec<usize> {
    let columns = table.header.len();
    let mut natural = vec![0usize; columns];
    let mut min_words = vec![1usize; columns];

    for (i, cell) in table.header.iter().enumerate() {
        measure_cell(&render_inline(cell, base), &mut natural, &mut min_words, i);
    }
    for row in &table.rows {
        for (i, cell) in row.iter().enumerate() {
            measure_cell(&render_inline(cell, base), &mut natural, &mut min_words, i);
        }
    }

    let mut min_widths = min_words.clone();
    let mut min_cells: usize = min_widths.iter().sum();
    if min_cells > available {
        // Not even the longest words fit: shrink every column to one column of
        // text and hand the slack back proportionally to the word widths.
        min_widths = vec![1; columns];
        let remaining = available - columns;
        if remaining > 0 {
            let total_weight: usize = min_words.iter().map(|w| w.saturating_sub(1)).sum();
            let mut allocated = 0usize;
            for i in 0..columns {
                let weight = min_words[i].saturating_sub(1);
                let growth = (weight * remaining).checked_div(total_weight).unwrap_or(0);
                min_widths[i] += growth;
                allocated += growth;
            }
            let mut leftover = remaining - allocated;
            let mut i = 0usize;
            while leftover > 0 && i < columns {
                min_widths[i] += 1;
                leftover -= 1;
                i += 1;
            }
        }
        min_cells = min_widths.iter().sum();
    }

    // "Everything fits naturally" is `sum(natural) + borderOverhead <= width`,
    // and `available` is already that width minus the border overhead.
    if natural.iter().sum::<usize>() <= available {
        return (0..columns)
            .map(|i| natural[i].max(min_widths[i]))
            .collect();
    }

    // Shrink towards the minimum widths, then hand the rounding remainder to
    // the columns that still have room to grow.
    let total_grow: usize = (0..columns)
        .map(|i| natural[i].saturating_sub(min_widths[i]))
        .sum();
    let extra = available.saturating_sub(min_cells);
    let mut widths: Vec<usize> = (0..columns)
        .map(|i| {
            let delta = natural[i].saturating_sub(min_widths[i]);
            let grow = (delta * extra).checked_div(total_grow).unwrap_or(0);
            min_widths[i] + grow
        })
        .collect();
    let mut remaining = available.saturating_sub(widths.iter().sum::<usize>());
    while remaining > 0 {
        let mut grew = false;
        for i in 0..columns {
            if remaining == 0 {
                break;
            }
            if widths[i] < natural[i] {
                widths[i] += 1;
                remaining -= 1;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    widths
}

/// Fold one rendered cell into the running natural / longest-word widths.
fn measure_cell(
    cell: &[StyledSpan],
    natural: &mut [usize],
    min_words: &mut [usize],
    column: usize,
) {
    natural[column] = natural[column].max(styled_width(cell));
    let longest = longest_word_width(&plain_text(cell));
    min_words[column] = min_words[column].max(longest.max(1));
}

/// Visible width of a styled run under this module's character-counting
/// convention.
fn styled_width(line: &[StyledSpan]) -> usize {
    line.iter().map(|span| span.text.chars().count()).sum()
}

/// Width of the longest whitespace-delimited word in `text`, capped at
/// [`MAX_UNBROKEN_WORD_WIDTH`].
fn longest_word_width(text: &str) -> usize {
    text.split_whitespace()
        .map(|word| word.chars().count())
        .max()
        .unwrap_or(0)
        .min(MAX_UNBROKEN_WORD_WIDTH)
}

/// Render a GFM table as a box-drawing grid, porting upstream `renderTable`
/// (`components/markdown.ts:839-1015`) including its width negotiation.
///
/// Falls back to replaying the raw markdown source when the available width
/// cannot hold the borders plus one column per cell, so a narrow terminal never
/// produces a mangled grid.
fn render_table(table: &TableBlock, width: usize, base: SpanStyle) -> Vec<StyledLine> {
    let columns = table.header.len();
    if columns == 0 {
        return Vec::new();
    }

    // "│ " + (n-1) * " │ " + " │" == 3n + 1 columns of border and padding.
    let border_overhead = 3 * columns + 1;
    if width < border_overhead + columns {
        let mut fallback: Vec<StyledLine> = Vec::new();
        for line in &table.raw {
            fallback.extend(wrap_line(&render_inline(line, base), width));
        }
        return fallback;
    }
    let available_for_cells = width - border_overhead;
    let widths = table_column_widths(table, available_for_cells, base);

    let mut out: Vec<StyledLine> = Vec::new();
    let border = |left: &str, middle: &str, right: &str| -> StyledLine {
        let mut spans: StyledLine = Vec::new();
        push_span(&mut spans, format!("{left}─"), base);
        for (i, cell_width) in widths.iter().enumerate() {
            if i > 0 {
                push_span(&mut spans, format!("─{middle}─"), base);
            }
            push_span(&mut spans, "─".repeat(*cell_width), base);
        }
        push_span(&mut spans, format!("─{right}"), base);
        spans
    };

    out.push(border("┌", "┬", "┐"));

    let header_cells: Vec<Vec<StyledLine>> = table
        .header
        .iter()
        .enumerate()
        .map(|(i, cell)| wrap_line(&render_inline(cell, base), widths[i].max(1)))
        .collect();
    push_table_row(&mut out, &header_cells, &widths, base, true);

    let separator = border("├", "┼", "┤");
    out.push(separator.clone());

    for (row_index, row) in table.rows.iter().enumerate() {
        let cells: Vec<Vec<StyledLine>> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| wrap_line(&render_inline(cell, base), widths[i].max(1)))
            .collect();
        push_table_row(&mut out, &cells, &widths, base, false);
        if row_index + 1 < table.rows.len() {
            out.push(separator.clone());
        }
    }

    out.push(border("└", "┴", "┘"));
    out
}

/// Push one physical table row: each cell wrapped to its column width, padded,
/// and joined with the ` │ ` separators. Header cells are bold, like upstream
/// (`theme.bold(padded)`); the padding of every cell carries the inherited
/// style so a table inside a quote stays quoted.
fn push_table_row(
    out: &mut Vec<StyledLine>,
    cells: &[Vec<StyledLine>],
    widths: &[usize],
    base: SpanStyle,
    bold: bool,
) {
    let line_count = cells.iter().map(|cell| cell.len()).max().unwrap_or(1);
    for line_index in 0..line_count {
        let mut spans: StyledLine = Vec::new();
        push_span(&mut spans, "│ ", base);
        for (column, cell_lines) in cells.iter().enumerate() {
            if column > 0 {
                push_span(&mut spans, " │ ", base);
            }
            let empty = StyledLine::new();
            let cell = cell_lines.get(line_index).unwrap_or(&empty);
            let padding = widths[column].saturating_sub(styled_width(cell));
            for span in cell {
                let style = if bold { span.style.bold() } else { span.style };
                push_span(&mut spans, span.text.clone(), style);
            }
            let pad_style = if bold { base.bold() } else { base };
            push_span(&mut spans, " ".repeat(padding), pad_style);
        }
        push_span(&mut spans, " │", base);
        out.push(spans);
    }
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
                if let Some((consumed, rendered)) = try_inline_latex(chars, i) {
                    push_span(out, rendered, base);
                    i += consumed;
                    continue;
                }
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
            '$' => {
                if let Some((consumed, rendered)) = try_inline_latex(chars, i) {
                    push_span(out, rendered, base);
                    i += consumed;
                    continue;
                }
                push_span(out, "$".to_string(), base);
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
            '!' => {
                // `![alt](url)` is an image; when it is not a standalone
                // data-URI line the block parser kept it inline, and marked
                // shows the alt text — that is exactly what the link renderer
                // does, so the `!` is dropped and the rest is parsed as a link.
                if chars.get(i + 1) == Some(&'[') {
                    if let Some(consumed) = parse_link(chars, i + 1, base, out) {
                        i += consumed + 1;
                        continue;
                    }
                }
                push_span(out, "!".to_string(), base);
                i += 1;
            }
            _ => {
                let start = i;
                // An image marker inside a run ends it, so `!` is dispatched to
                // its own arm above instead of being swallowed into the text.
                while i < chars.len() && !is_special(chars[i]) && !starts_image(chars, i) {
                    i += 1;
                }
                if i == start {
                    push_span(out, chars[i].to_string(), base);
                    i += 1;
                } else {
                    let run: String = chars[start..i].iter().collect();
                    push_span(out, run, base);
                }
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

/// Try to read an inline LaTeX run starting at `chars[start]`.
///
/// Returns the number of characters consumed and the text to emit (either the
/// rendered expression or the literal source). Mirrors upstream
/// `tokenizeInlineLatex` (`components/markdown.ts:52`) including its
/// look-alike guards for `$…$`.
fn try_inline_latex(chars: &[char], start: usize) -> Option<(usize, String)> {
    let source: String = chars[start..].iter().collect();
    let (opening_len, closing, backslash_opener) = if source.starts_with("$$") {
        (2usize, "$$", false)
    } else if source.starts_with("\\(") {
        (2, "\\)", true)
    } else if source.starts_with("\\[") {
        (2, "\\]", true)
    } else if source.starts_with('$') && !source[1..].starts_with(char::is_whitespace) {
        (1, "$", false)
    } else {
        return None;
    };

    if let Some(index) = find_closing_delimiter(&source, closing, opening_len) {
        let inner = &source[opening_len..index];
        if opening_len == 1 {
            let after = &source[index + closing.len()..];
            let trailing_whitespace = inner.ends_with(char::is_whitespace);
            let digit_after = after.starts_with(|c: char| c.is_ascii_digit());
            let name_guard = is_symbolic_dollar_name(inner) && starts_with_identifier(after);
            if trailing_whitespace || digit_after || name_guard || inner.contains('`') {
                return None;
            }
        }
        if inner.is_empty() || inner.contains('\n') {
            return None;
        }
        let rendered =
            render_latex(inner).unwrap_or_else(|| source[..index + closing.len()].to_string());
        let consumed = source[..index].chars().count() + closing.chars().count();
        return Some((consumed, rendered));
    }

    let pending_source = &source[opening_len..];
    if backslash_opener || looks_like_pending_dollar_math(pending_source) {
        return Some((source.chars().count(), source));
    }
    None
}

/// True for a `$ALL_CAPS_LOOKALIKE` body that upstream refuses to treat as math
/// (regex `^[A-Z_][A-Z0-9_]*(?:[^A-Za-z0-9_\s])?$`).
fn is_symbolic_dollar_name(inner: &str) -> bool {
    let mut chars = inner.chars().peekable();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() || c == '_' => {}
        _ => return false,
    }
    while chars
        .peek()
        .is_some_and(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
    {
        chars.next();
    }
    match chars.next() {
        None => true,
        Some(c) => {
            chars.next().is_none() && !c.is_ascii_alphanumeric() && c != '_' && !c.is_whitespace()
        }
    }
}

/// True when `text` starts with an identifier (regex `^[A-Za-z_][A-Za-z0-9_]*`).
fn starts_with_identifier(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
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
        push_linked_span(out, span.text, span.style, &url);
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
    matches!(c, '\\' | '`' | '*' | '_' | '~' | '[' | '$')
}

/// True when `chars[i]` opens a markdown image (`![`).
fn starts_image(chars: &[char], i: usize) -> bool {
    chars[i] == '!' && chars.get(i + 1) == Some(&'[')
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
        // A linked run must never absorb (or be absorbed by) unlinked text:
        // the span is the unit that carries the OSC 8 target.
        if last.style == style && last.link.is_none() {
            last.text.push_str(&text);
            return;
        }
    }
    out.push(StyledSpan::new(text, style));
}

/// Push a run that renders as an OSC 8 hyperlink, merging into the previous
/// span only when style *and* target match.
fn push_linked_span(out: &mut StyledLine, text: impl Into<String>, style: SpanStyle, url: &str) {
    let text = text.into();
    if text.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut() {
        if last.style == style && last.link.as_deref() == Some(url) {
            last.text.push_str(&text);
            return;
        }
    }
    out.push(StyledSpan::linked(text, style, url));
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
    let chars: Vec<(char, SpanStyle, Option<&str>)> = line
        .iter()
        .flat_map(|span| {
            span.text
                .chars()
                .map(move |c| (c, span.style, span.link.as_deref()))
        })
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
        let (ch, _, _) = chars[i];
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
                        for (offset, (c, _, _)) in tail.iter().enumerate() {
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

/// Build a styled line from a char/style/link run, merging equal slots.
fn build_line(chars: &[(char, SpanStyle, Option<&str>)]) -> StyledLine {
    let mut out: StyledLine = Vec::new();
    for (c, style, link) in chars {
        match link {
            Some(url) => push_linked_span(&mut out, c.to_string(), *style, url),
            None => push_span(&mut out, c.to_string(), *style),
        }
    }
    out
}
