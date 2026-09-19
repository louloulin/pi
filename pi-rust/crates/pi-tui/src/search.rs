//! Transcript search — the Rust port of `packages/tui/src/alt-screen-search.ts`.
//!
//! Upstream keeps a searchable *corpus* of the rendered transcript lines next
//! to a span table that maps every corpus character back to the `(row, column)`
//! cell it came from, so a match can be underlined in place while the query in
//! the search bar is still being typed
//! (`alt-screen-search.ts:22-43`, `buildSearchCorpus`). [`SearchIndex`] caches
//! both the corpus and the matches while the transcript and the query stay
//! unchanged — the exact contract of upstream's `AltScreenSearchIndex.search`
//! (`:156-184`).
//!
//! The module holds no app state: [`SearchBar`] owns the query + cursor, and
//! [`render_search_bar`] lays the bar out into [`StyledLine`]s. The [`App`]
//! owns the index, the match list and the selection
//! ([`crate::app::App::navigate_search`]).
//!
//! [`App`]: crate::app::App
//!
//! # Deliberate deviations
//!
//! * **Columns are character offsets, not display cells.** The whole port
//!   tracks transcript columns as character indices (see `app.rs`'s
//!   `# Text selection` notes and [`crate::app::App::selection_text`]);
//!   upstream's `visibleWidth` / `getGraphemeCellRange` cell arithmetic has no
//!   equivalent here. The corpus layout is otherwise identical, so a match on a
//!   wide grapheme still lands on that grapheme's starting column.
//! * **Case-insensitive matching is per-character lowercasing**, not the
//!   `regex` `iu` full case folding: each corpus character is mapped to its
//!   simple lowercase form (`char::to_lowercase`'s first scalar) so corpus
//!   offsets stay 1:1 with the source. This is what a literal, escaping-free
//!   search needs; upstream's `escapeRegExp` shows the query is never a regex
//!   anyway.
//! * **No terminal-sequence stripping.** The corpus is built from
//!   [`crate::styled::plain_text`] output, which carries no ANSI escapes by
//!   construction, so upstream's `stripTerminalSequences` pass is a no-op here.
//!   [`find_matches`] additionally skips a line that contains `ESC`, so a
//!   caller that hands over raw ANSI text cannot make the span table drift.
//! * **No input cursor cell.** Upstream's `Input` paints an inverse cursor at
//!   the query caret; a [`StyledSpan`] has no per-cell cursor, and the App's
//!   buffer path draws no hardware caret for overlays, so the caret position is
//!   modelled ([`SearchBar::cursor`]) but not painted.

use crate::input::{Key, KeyCode, KeyModifiers};
use crate::keybindings::get_keybindings;
use crate::styled::{plain_text, SpanStyle, StyledLine, StyledSpan};
use crate::theme::ThemeColor;
use ratatui::layout::Rect;

/// One highlighted run of a match on a single transcript row.
///
/// `start_col` is inclusive and `end_col` exclusive, in transcript
/// character-offset columns — the same convention as upstream's
/// `AltScreenSearchSegment` (`alt-screen-search.ts:22-27`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchSegment {
    /// Rendered transcript row (0-based, over the full log).
    pub row: usize,
    /// First matched column on that row.
    pub start_col: usize,
    /// One past the last matched column on that row.
    pub end_col: usize,
}

/// One corpus hit, possibly spanning several rows.
///
/// Upstream keeps `segments` non-empty by construction; the Rust type keeps
/// them public so callers can highlight them directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    /// The highlighted runs, in row order.
    pub segments: Vec<SearchSegment>,
}

impl SearchMatch {
    /// The upstream match key — `firstRow:firstCol:lastRow:lastCol` — used to
    /// keep the same match selected across a re-index
    /// (`getAltScreenSearchMatchKey`, `alt-screen-search.ts:191-195`).
    pub fn key(&self) -> String {
        match (self.segments.first(), self.segments.last()) {
            (Some(first), Some(last)) => {
                format!(
                    "{}:{}:{}:{}",
                    first.row, first.start_col, last.row, last.end_col
                )
            }
            _ => String::new(),
        }
    }

    /// Row of the first segment, if any.
    pub fn first_row(&self) -> Option<usize> {
        self.segments.first().map(|segment| segment.row)
    }

    /// Row of the last segment, if any.
    pub fn last_row(&self) -> Option<usize> {
        self.segments.last().map(|segment| segment.row)
    }
}

/// Normalise a query the way upstream does: collapse every whitespace run to a
/// single space and trim the ends (`normalizeQuery`, `:107-109`).
pub fn normalize_query(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    let mut pending_space = false;
    for ch in query.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() {
                pending_space = true;
            }
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
    }
    out
}

/// A single span in the search corpus (upstream `SearchSourceSpan`).
#[derive(Debug, Clone, Copy)]
struct SourceSpan {
    text_start: usize,
    text_end: usize,
    row: usize,
    start_col: usize,
    end_col: usize,
    linear_columns: bool,
}

/// The searchable transcript plus the reverse map into rendered cells.
#[derive(Debug, Clone, Default)]
struct SearchCorpus {
    text: Vec<char>,
    spans: Vec<SourceSpan>,
}

/// True when every character is a printable ASCII byte (`PRINTABLE_ASCII`).
fn is_printable_ascii(line: &str) -> bool {
    line.chars().all(|ch| ('\x20'..='\x7e').contains(&ch))
}

/// Build the corpus for `lines`.
///
/// Mirrors `buildSearchCorpus` (`alt-screen-search.ts:46-105`): printable ASCII
/// lines take the non-space-runs-at-once fast path with linear column maths,
/// every other line goes grapheme by grapheme and keeps a whole-span mapping.
fn build_corpus(lines: &[String]) -> SearchCorpus {
    let mut text: Vec<char> = Vec::new();
    let mut spans: Vec<SourceSpan> = Vec::new();
    let mut pending_separator = false;

    for (row, line) in lines.iter().enumerate() {
        // Raw ANSI would make the columns disagree with what is on screen.
        // Rendering already hands over plain text, so this is a guard only.
        let line = if line.contains('\x1b') {
            String::new()
        } else {
            line.clone()
        };
        let chars: Vec<char> = line.chars().collect();
        let mut column = 0usize;

        if is_printable_ascii(&line) {
            let mut index = 0usize;
            while index < chars.len() {
                if chars[index] == ' ' {
                    if !text.is_empty() {
                        pending_separator = true;
                    }
                    column += 1;
                    index += 1;
                    continue;
                }
                let mut end = index + 1;
                while end < chars.len() && chars[end] != ' ' {
                    end += 1;
                }
                if pending_separator {
                    text.push(' ');
                    pending_separator = false;
                }
                let text_start = text.len();
                text.extend_from_slice(&chars[index..end]);
                let text_end = text.len();
                let length = end - index;
                spans.push(SourceSpan {
                    text_start,
                    text_end,
                    row,
                    start_col: column,
                    end_col: column + length,
                    linear_columns: true,
                });
                column += length;
                index = end;
            }
        } else {
            for (offset, grapheme) in line.grapheme_indices_for_search() {
                let _ = offset;
                if grapheme.chars().all(char::is_whitespace) {
                    if !text.is_empty() {
                        pending_separator = true;
                    }
                    column += grapheme.chars().count();
                    continue;
                }
                if pending_separator {
                    text.push(' ');
                    pending_separator = false;
                }
                let text_start = text.len();
                text.extend(grapheme.chars());
                let text_end = text.len();
                spans.push(SourceSpan {
                    text_start,
                    text_end,
                    row,
                    start_col: column,
                    end_col: column + grapheme.chars().count(),
                    linear_columns: false,
                });
                column += grapheme.chars().count();
            }
        }
        if !text.is_empty() {
            pending_separator = true;
        }
    }

    SearchCorpus { text, spans }
}

/// Grapheme iteration helper.
///
/// Kept as a tiny trait so the corpus builder reads like upstream's
/// `segmenter.segment(line)` loop while still using the crate's existing
/// `unicode-segmentation` dependency.
trait GraphemeSearch {
    fn grapheme_indices_for_search(&self) -> Vec<(usize, &str)>;
}

impl GraphemeSearch for str {
    fn grapheme_indices_for_search(&self) -> Vec<(usize, &str)> {
        use unicode_segmentation::UnicodeSegmentation;
        self.grapheme_indices(true).collect()
    }
}

/// Map every character to its simple lowercase form, keeping the length.
fn lowercase_chars(text: &[char]) -> Vec<char> {
    text.iter()
        .map(|ch| ch.to_lowercase().next().unwrap_or(*ch))
        .collect()
}

/// Find every occurrence of `normalized` in `corpus`, mapping it back to
/// rendered segments (`findSearchCorpusMatches`, `:116-148`).
fn find_corpus_matches(corpus: &SearchCorpus, normalized: &str) -> Vec<SearchMatch> {
    if normalized.is_empty() {
        return Vec::new();
    }
    let needle = lowercase_chars(&normalized.chars().collect::<Vec<_>>());
    let haystack = lowercase_chars(&corpus.text);
    let mut matches: Vec<SearchMatch> = Vec::new();
    let mut span_index = 0usize;

    let mut start = 0usize;
    while start + needle.len() <= haystack.len() {
        if haystack[start..start + needle.len()] != needle[..] {
            start += 1;
            continue;
        }
        let end = start + needle.len();

        while span_index < corpus.spans.len() && corpus.spans[span_index].text_end <= start {
            span_index += 1;
        }

        let mut segments: Vec<SearchSegment> = Vec::new();
        let mut index = span_index;
        while index < corpus.spans.len() {
            let span = corpus.spans[index];
            if span.text_start >= end {
                break;
            }
            if span.text_end <= start {
                index += 1;
                continue;
            }
            let (start_col, end_col) = if span.linear_columns {
                (
                    span.start_col + start.max(span.text_start) - span.text_start,
                    span.start_col + end.min(span.text_end) - span.text_start,
                )
            } else {
                (span.start_col, span.end_col)
            };
            match segments.last_mut() {
                Some(previous) if previous.row == span.row && start_col <= previous.end_col => {
                    previous.end_col = previous.end_col.max(end_col);
                }
                _ => segments.push(SearchSegment {
                    row: span.row,
                    start_col,
                    end_col,
                }),
            }
            index += 1;
        }

        while span_index < corpus.spans.len() && corpus.spans[span_index].text_end <= end {
            span_index += 1;
        }
        if !segments.is_empty() {
            matches.push(SearchMatch { segments });
        }
        start = end;
    }

    matches
}

/// Find every match of `query` in `lines` without caching.
///
/// The free-function form of upstream's `findAltScreenSearchMatches`
/// (`:186-189`); [`SearchIndex`] is the cached variant the App uses.
pub fn find_matches(lines: &[String], query: &str) -> Vec<SearchMatch> {
    let normalized = normalize_query(query);
    if normalized.is_empty() {
        return Vec::new();
    }
    find_corpus_matches(&build_corpus(lines), &normalized)
}

/// Outcome of [`SearchIndex::search`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    /// Every match, in row order.
    pub matches: Vec<SearchMatch>,
    /// True when the corpus or the query changed and the matches were
    /// recomputed (`AltScreenSearchResult.changed`).
    pub changed: bool,
}

/// Cached transcript search index (upstream `AltScreenSearchIndex`).
///
/// A `search` call reuses the corpus while the rendered lines are unchanged and
/// reuses the match list while the normalised query is unchanged, so the App
/// can call it every time the transcript redraws.
#[derive(Debug, Default)]
pub struct SearchIndex {
    source_lines: Option<Vec<String>>,
    corpus: Option<SearchCorpus>,
    normalized_query: Option<String>,
    matches: Vec<SearchMatch>,
}

impl SearchIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Search `lines` for `query`, reusing cached work where possible.
    pub fn search(&mut self, lines: &[String], query: &str) -> SearchResult {
        let mut source_changed = self
            .source_lines
            .as_ref()
            .map(|known| known.len() != lines.len())
            .unwrap_or(true);
        if !source_changed {
            if let Some(known) = &self.source_lines {
                source_changed = known.iter().zip(lines).any(|(a, b)| a != b);
            }
        }
        if source_changed || self.corpus.is_none() {
            self.source_lines = Some(lines.to_vec());
            self.corpus = Some(build_corpus(lines));
        }

        let normalized = normalize_query(query);
        let changed =
            source_changed || self.normalized_query.as_deref() != Some(normalized.as_str());
        if changed {
            self.normalized_query = Some(normalized.clone());
            let corpus = self.corpus.as_ref().expect("corpus is built above");
            self.matches = find_corpus_matches(corpus, &normalized);
        }
        SearchResult {
            matches: self.matches.clone(),
            changed,
        }
    }

    /// Forget the cached corpus and matches.
    pub fn clear(&mut self) {
        self.source_lines = None;
        self.corpus = None;
        self.normalized_query = None;
        self.matches.clear();
    }
}

/// Placeholder an empty query shows, from upstream's `Input` options
/// (`packages/tui/src/alt-screen-search.ts:199`).
pub const SEARCH_PLACEHOLDER: &str = "Find in transcript";

/// How the search selection is recomputed on the next refresh
/// (upstream `SearchSelectionMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSelectionMode {
    /// The query changed: pick the first match at or after the anchor row.
    Query,
    /// Keep the current selection if it still exists.
    Retain,
    /// Move to the next match.
    Next,
    /// Move to the previous match.
    Previous,
}

/// The search bar's editable query plus its presentation state.
///
/// Upstream builds this on top of the `Input` component
/// (`AltScreenSearchComponent`, `:197-327`); the Rust port keeps the same
/// observable surface (query text, result counter, navigation-button
/// hit-testing) without dragging the editor widget in.
#[derive(Debug, Clone)]
pub struct SearchBar {
    query: String,
    cursor: usize,
    result_index: i64,
    result_count: usize,
    hovered: Option<i8>,
}

impl Default for SearchBar {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchBar {
    /// A bar with an empty query and the caret at the start.
    pub fn new() -> Self {
        Self {
            query: String::new(),
            cursor: 0,
            result_index: -1,
            result_count: 0,
            hovered: None,
        }
    }

    /// The current query text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The caret, as a character offset into [`SearchBar::query`].
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Replace the query wholesale (used by tests and by a future history).
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.cursor = self.query.chars().count();
        self.result_index = -1;
        self.result_count = 0;
    }

    /// Insert a character at the caret.
    pub fn insert_char(&mut self, ch: char) {
        let byte = byte_index(&self.query, self.cursor);
        self.query.insert(byte, ch);
        self.cursor += 1;
    }

    /// Delete the character before the caret. Returns true when text changed.
    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let end = byte_index(&self.query, self.cursor);
        let start = byte_index(&self.query, self.cursor - 1);
        self.query.replace_range(start..end, "");
        self.cursor -= 1;
        true
    }

    /// Delete the character after the caret. Returns true when text changed.
    pub fn delete_forward(&mut self) -> bool {
        if self.cursor >= self.query.chars().count() {
            return false;
        }
        let start = byte_index(&self.query, self.cursor);
        let end = byte_index(&self.query, self.cursor + 1);
        self.query.replace_range(start..end, "");
        true
    }

    /// Move the caret one character left.
    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Move the caret one character right.
    pub fn move_right(&mut self) {
        let len = self.query.chars().count();
        self.cursor = (self.cursor + 1).min(len);
    }

    /// Move the caret to the start of the query.
    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    /// Move the caret past the end of the query.
    pub fn move_end(&mut self) {
        self.cursor = self.query.chars().count();
    }

    /// Delete everything before the caret (`ctrl+u`).
    pub fn delete_to_start(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let end = byte_index(&self.query, self.cursor);
        self.query.replace_range(0..end, "");
        self.cursor = 0;
        true
    }

    /// Delete everything after the caret (`ctrl+k`).
    pub fn delete_to_end(&mut self) -> bool {
        let len = self.query.chars().count();
        if self.cursor >= len {
            return false;
        }
        let start = byte_index(&self.query, self.cursor);
        self.query.truncate(start);
        true
    }

    /// Record which match is selected and how many there are
    /// (`setResult`, `:243-246`). `index` is `-1` while nothing is selected.
    pub fn set_result(&mut self, index: i64, count: usize) {
        self.result_index = index;
        self.result_count = count;
    }

    /// The selected match index (`None` while nothing is selected).
    pub fn result_index(&self) -> Option<usize> {
        if self.result_index < 0 {
            None
        } else {
            Some(self.result_index as usize)
        }
    }

    /// How many matches the last search found.
    pub fn result_count(&self) -> usize {
        self.result_count
    }

    /// The counter text upstream paints next to the query (`:283-288`).
    pub fn result_label(&self) -> String {
        // Upstream tests the raw value (`!query`), so a whitespace-only query
        // is not the same as an empty one (`:280-288`).
        if self.query.is_empty() {
            String::new()
        } else if self.result_count == 0 {
            "No matches".to_string()
        } else {
            format!("{}/{}", self.result_index + 1, self.result_count)
        }
    }

    /// The navigation direction a bar-local cell maps to, mirroring
    /// `getNavigationDirectionAt` (`:248-255`).
    ///
    /// `width` is the width the bar was laid out at — upstream records the
    /// button spans during `render`, and the caller is the one that knows the
    /// rectangle's width.
    pub fn navigation_direction_at(&self, width: u16, row: i64, col: i64) -> Option<i8> {
        if row != 2 {
            return None;
        }
        let layout = render_search_bar(self, width);
        if let Some((start, end)) = layout.previous_span {
            if col >= start && col < end {
                return Some(-1);
            }
        }
        if let Some((start, end)) = layout.next_span {
            if col >= start && col < end {
                return Some(1);
            }
        }
        None
    }

    /// Set the hovered navigation direction; true when it changed
    /// (`setHoveredNavigationDirection`, `:257-261`).
    pub fn set_hovered(&mut self, direction: Option<i8>) -> bool {
        if self.hovered == direction {
            return false;
        }
        self.hovered = direction;
        true
    }

    /// The currently hovered navigation direction.
    pub fn hovered(&self) -> Option<i8> {
        self.hovered
    }
}

/// Byte offset of character index `chars` in `text`.
fn byte_index(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

/// A laid-out search bar: the rendered box plus the button spans the App
/// needs for mouse hit-testing.
#[derive(Debug, Clone)]
pub struct SearchBarLayout {
    /// The box's three rows (top rule, content, bottom rule).
    pub lines: Vec<StyledLine>,
    /// Inclusive start / exclusive end column of the previous-match button.
    pub previous_span: Option<(i64, i64)>,
    /// Inclusive start / exclusive end column of the next-match button.
    pub next_span: Option<(i64, i64)>,
}

/// The overlay rectangle the search bar occupies inside `area`.
///
/// Upstream anchors the bar `top-right`, `40%` wide, `minWidth: 32`,
/// `margin: 1` (`toggleSearch`, `tui-alt-screen.ts:505-512`). The port anchors
/// to the App's message area rather than the whole terminal because the App
/// does not own the status / prompt rows.
pub fn search_bar_rect(area: Rect) -> Option<Rect> {
    const MARGIN: u16 = 1;
    if area.width <= MARGIN * 2 || area.height == 0 {
        return None;
    }
    let pct = (u32::from(area.width) * 40 / 100) as u16;
    let max_width = area.width - MARGIN * 2;
    let width = pct.max(32).min(max_width);
    if width == 0 {
        return None;
    }
    let height = 3u16.min(area.height);
    Some(Rect {
        x: area.x + area.width - MARGIN - width,
        y: area.y + MARGIN,
        width,
        height,
    })
}

/// Prettify one chord for the button label (`formatKey`, `:270-282`).
fn format_key(key: Option<&String>) -> String {
    let Some(key) = key else {
        return "Unbound".to_string();
    };
    key.split('+')
        .map(|part| {
            if cfg!(target_os = "macos") && part.eq_ignore_ascii_case("alt") {
                return "Option".to_string();
            }
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// The first configured chord of `keybinding`, prettified.
fn first_key_label(keybinding: &str) -> String {
    format_key(get_keybindings().get_keys(keybinding).first())
}

/// Lay the search bar out into `width` columns.
///
/// A faithful port of `AltScreenSearchComponent.render`
/// (`alt-screen-search.ts:263-326`): the box is `┌─┐ / │…│ / └─┘`, the top rule
/// spans the inner width, and the bottom rule carries the previous / next
/// buttons right-aligned when they fit, collapsing to bare arrows when they do
/// not. The recorded button spans are what [`SearchBar::navigation_direction_at`]
/// hit-tests against.
pub fn render_search_bar(bar: &SearchBar, width: u16) -> SearchBarLayout {
    let safe_width = width.max(1);
    let inner_width = safe_width.saturating_sub(2);
    let inner = inner_width as usize;

    let previous_key = first_key_label("tui.altScreen.searchPrevious");
    let next_key = first_key_label("tui.altScreen.searchNext");
    let mut previous_button = format!("↑ {previous_key}");
    let mut next_button = format!("↓ {next_key}");
    let mut separator = " · ";
    let outer_gap_width = 1usize;
    let available_controls_width = inner.saturating_sub(outer_gap_width * 2 + 1);
    let mut controls_width =
        previous_button.chars().count() + separator.chars().count() + next_button.chars().count();
    if controls_width > available_controls_width {
        previous_button = "↑".to_string();
        next_button = "↓".to_string();
        separator = " ";
        controls_width = previous_button.chars().count()
            + separator.chars().count()
            + next_button.chars().count();
    }
    let show_buttons = controls_width <= available_controls_width;
    let outer_gaps_width = if show_buttons { outer_gap_width * 2 } else { 0 };
    let right_rule_width = if show_buttons && inner > controls_width + outer_gaps_width {
        1
    } else {
        0
    };
    let left_rule_width = inner
        .saturating_sub(if show_buttons { controls_width } else { 0 })
        .saturating_sub(outer_gaps_width)
        .saturating_sub(right_rule_width);
    let previous_start = 1 + left_rule_width + outer_gap_width;

    let result_label = bar.result_label();
    let result_space = inner.saturating_sub(3);
    let visible_result: String = result_label.chars().take(result_space).collect();
    let result_text_width = if visible_result.is_empty() {
        0
    } else {
        visible_result.chars().count() + 2
    };
    let input_width = inner.saturating_sub(result_text_width);
    // Upstream renders the query through an `Input` whose prompt is a single
    // space and whose placeholder is `Find in transcript`, then truncates the
    // whole line to `inputWidth` (`:274-278`).
    let text_budget = input_width.saturating_sub(1);
    let (text, text_style) = if bar.query().is_empty() {
        (
            SEARCH_PLACEHOLDER
                .chars()
                .take(text_budget)
                .collect::<String>(),
            SpanStyle::fg(ThemeColor::Dim),
        )
    } else {
        (
            bar.query().chars().take(text_budget).collect::<String>(),
            SpanStyle::fg(ThemeColor::Text),
        )
    };
    let padding = " ".repeat(input_width.saturating_sub(1 + text.chars().count()));

    let border_style = SpanStyle::fg(ThemeColor::BorderMuted);
    let mut lines: Vec<StyledLine> = Vec::with_capacity(3);
    if safe_width == 1 {
        lines.push(vec![StyledSpan::new("┌", border_style)]);
        lines.push(vec![StyledSpan::new("│", border_style)]);
        lines.push(vec![StyledSpan::new("└", border_style)]);
        return SearchBarLayout {
            lines,
            previous_span: None,
            next_span: None,
        };
    }

    lines.push(vec![StyledSpan::new(
        format!("┌{}┐", "─".repeat(inner)),
        border_style,
    )]);

    let mut content: Vec<StyledSpan> = vec![StyledSpan::new("│", border_style)];
    if input_width > 0 {
        content.push(StyledSpan::new(" ", SpanStyle::PLAIN));
        if !text.is_empty() {
            content.push(StyledSpan::new(text, text_style));
        }
    }
    if !padding.is_empty() {
        content.push(StyledSpan::new(padding, SpanStyle::PLAIN));
    }
    if !visible_result.is_empty() {
        content.push(StyledSpan::new(
            format!(" {visible_result} "),
            SpanStyle::fg(ThemeColor::Dim),
        ));
    }
    content.push(StyledSpan::new("│", border_style));
    lines.push(content);

    let mut bottom: Vec<StyledSpan> = vec![StyledSpan::new("└", border_style)];
    if left_rule_width > 0 {
        bottom.push(StyledSpan::new("─".repeat(left_rule_width), border_style));
    }
    let (previous_span, next_span) = if show_buttons {
        let previous_start = previous_start as i64;
        let previous_end = previous_start + previous_button.chars().count() as i64;
        let next_start = previous_end + separator.chars().count() as i64;
        let next_end = next_start + next_button.chars().count() as i64;
        bottom.push(StyledSpan::new(" ", SpanStyle::PLAIN));
        bottom.push(StyledSpan::new(
            previous_button.clone(),
            SpanStyle::fg(if bar.hovered == Some(-1) {
                ThemeColor::BorderAccent
            } else {
                ThemeColor::Muted
            }),
        ));
        bottom.push(StyledSpan::new(separator, SpanStyle::fg(ThemeColor::Muted)));
        bottom.push(StyledSpan::new(
            next_button.clone(),
            SpanStyle::fg(if bar.hovered == Some(1) {
                ThemeColor::BorderAccent
            } else {
                ThemeColor::Muted
            }),
        ));
        bottom.push(StyledSpan::new(" ", SpanStyle::PLAIN));
        (
            Some((previous_start, previous_end)),
            Some((next_start, next_end)),
        )
    } else {
        (None, None)
    };
    if right_rule_width > 0 {
        bottom.push(StyledSpan::new("─".repeat(right_rule_width), border_style));
    }
    bottom.push(StyledSpan::new("┘", border_style));
    lines.push(bottom);

    SearchBarLayout {
        lines,
        previous_span,
        next_span,
    }
}

/// Apply the chars a search-bar keystroke should insert, if any.
///
/// Returns true when the query changed.
pub fn apply_query_key(bar: &mut SearchBar, key: Key) -> bool {
    match key.code {
        KeyCode::Char(ch) => {
            if key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT {
                bar.insert_char(ch);
                return true;
            }
            match ch {
                'a' => bar.move_home(),
                'e' => bar.move_end(),
                'u' => {
                    return bar.delete_to_start();
                }
                'k' => {
                    return bar.delete_to_end();
                }
                _ => {}
            }
            false
        }
        KeyCode::Backspace => bar.backspace(),
        KeyCode::Delete => bar.delete_forward(),
        KeyCode::Left => {
            bar.move_left();
            false
        }
        KeyCode::Right => {
            bar.move_right();
            false
        }
        _ => false,
    }
}

/// Render a [`SearchBar`] into plain text lines (test helper).
pub fn search_bar_text(lines: &[StyledLine]) -> Vec<String> {
    lines.iter().map(|line| plain_text(line)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn normalize_query_collapses_and_trims() {
        assert_eq!(normalize_query("  a \t b\n c  "), "a b c");
        assert_eq!(normalize_query("   "), "");
        assert_eq!(normalize_query("x"), "x");
    }

    #[test]
    fn find_matches_splits_on_the_corpus_separators() {
        // The corpus joins non-space runs with a single separator character
        // that belongs to no span, so a multi-word query highlights one
        // segment per run and leaves the space between them alone — exactly
        // what upstream's span table produces.
        let matches = find_matches(
            &lines(&["[user] hello world", "[user] world"]),
            "hello world",
        );
        assert_eq!(matches.len(), 1);
        assert_eq!(
            matches[0].segments,
            vec![
                SearchSegment {
                    row: 0,
                    start_col: 7,
                    end_col: 12,
                },
                SearchSegment {
                    row: 0,
                    start_col: 13,
                    end_col: 18,
                },
            ]
        );
    }

    #[test]
    fn find_matches_is_case_insensitive_and_literal() {
        let matches = find_matches(&lines(&["Hello .* World"]), "hello .* world");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].segments.len(), 3);
        assert_eq!(matches[0].segments[0].start_col, 0);
        assert_eq!(matches[0].segments[0].end_col, 5);
        assert_eq!(matches[0].segments[2].start_col, 9);
        assert_eq!(matches[0].segments[2].end_col, 14);
    }

    #[test]
    fn find_matches_merges_adjacent_segments_on_a_row() {
        // "ab" is a single non-space run, so both hit characters collapse into
        // one segment rather than two abutting ones.
        let matches = find_matches(&lines(&["xxabab"]), "aba");
        assert_eq!(matches.len(), 1);
        assert_eq!(
            matches[0].segments,
            vec![SearchSegment {
                row: 0,
                start_col: 2,
                end_col: 5
            }]
        );
    }

    #[test]
    fn match_key_round_trips() {
        let matches = find_matches(&lines(&["alpha beta"]), "beta");
        assert_eq!(matches[0].key(), "0:6:0:10");
    }

    #[test]
    fn empty_query_matches_nothing() {
        assert!(find_matches(&lines(&["alpha"]), "").is_empty());
        assert!(find_matches(&lines(&["alpha"]), "   ").is_empty());
    }

    #[test]
    fn non_ascii_lines_use_the_grapheme_path() {
        let matches = find_matches(&lines(&["你好 世界"]), "你好");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].segments[0].row, 0);
        assert_eq!(matches[0].segments[0].start_col, 0);
        assert_eq!(matches[0].segments[0].end_col, 2);
    }

    #[test]
    fn index_reuses_the_corpus_and_reports_changes() {
        let source = lines(&["alpha beta", "gamma"]);
        let mut index = SearchIndex::new();
        let first = index.search(&source, "beta");
        assert!(first.changed);
        assert_eq!(first.matches.len(), 1);

        let same = index.search(&source, "beta");
        assert!(!same.changed);
        assert_eq!(same.matches.len(), 1);

        let changed = index.search(&source, "gamma");
        assert!(changed.changed);
        assert_eq!(changed.matches.len(), 1);

        let resized = index.search(&lines(&["alpha beta"]), "gamma");
        assert!(resized.changed);
        assert!(resized.matches.is_empty());
    }

    #[test]
    fn bar_editing_and_result_label() {
        let mut bar = SearchBar::new();
        assert_eq!(bar.query(), "");
        for ch in "abc".chars() {
            bar.insert_char(ch);
        }
        assert_eq!(bar.query(), "abc");
        assert_eq!(bar.cursor(), 3);
        assert_eq!(bar.result_label(), "No matches");
        bar.set_result(1, 3);
        assert_eq!(bar.result_label(), "2/3");
        bar.set_result(-1, 0);
        assert_eq!(bar.result_label(), "No matches");
        assert!(bar.backspace());
        assert_eq!(bar.query(), "ab");
        bar.move_home();
        assert!(bar.delete_forward());
        assert_eq!(bar.query(), "b");
        bar.move_end();
        assert!(!bar.delete_forward());
        assert!(bar.backspace());
        assert_eq!(bar.query(), "");
        assert_eq!(bar.result_label(), "");
        assert!(!bar.backspace());
    }

    #[test]
    fn bar_layout_records_button_spans() {
        let bar = SearchBar::new();
        let layout = render_search_bar(&bar, 40);
        assert_eq!(layout.lines.len(), 3);
        let previous = layout.previous_span.expect("previous button");
        let next = layout.next_span.expect("next button");
        // "↑ Shift+Enter" / " · " / "↓ Enter" right-aligned in the rule:
        // `tui.altScreen.searchPrevious` is `shift+enter`, `searchNext` is
        // `enter`, so the previous button is the wider one.
        assert_eq!(previous.0, 14);
        assert_eq!(previous.1, previous.0 + 13);
        assert_eq!(next.0, previous.1 + 3);
        assert_eq!(next.1, next.0 + 7);
        for line in &layout.lines {
            assert_eq!(
                search_bar_text(std::slice::from_ref(line))[0]
                    .chars()
                    .count(),
                40
            );
        }
        // Hit-testing routes each span to its direction.
        assert_eq!(bar.navigation_direction_at(40, 2, previous.0), Some(-1));
        assert_eq!(bar.navigation_direction_at(40, 2, next.0), Some(1));
        assert_eq!(bar.navigation_direction_at(40, 2, 0), None);
        assert_eq!(bar.navigation_direction_at(40, 1, previous.0), None);
    }

    #[test]
    fn bar_layout_collapses_on_narrow_widths() {
        let bar = SearchBar::new();
        let layout = render_search_bar(&bar, 24);
        assert_eq!(layout.lines.len(), 3);
        assert_eq!(search_bar_text(&layout.lines)[0].chars().count(), 24);
        let tiny = render_search_bar(&bar, 1);
        assert_eq!(search_bar_text(&tiny.lines), vec!["┌", "│", "└"]);
    }

    #[test]
    fn bar_rect_anchors_top_right_with_margin() {
        let rect = search_bar_rect(Rect::new(0, 0, 100, 20)).expect("rect");
        assert_eq!(rect.width, 40);
        assert_eq!(rect.x, 59);
        assert_eq!(rect.y, 1);

        let narrow = search_bar_rect(Rect::new(0, 0, 20, 5)).expect("rect");
        assert_eq!(narrow.width, 18);
        assert_eq!(narrow.x, 1);
        assert!(search_bar_rect(Rect::new(0, 0, 2, 5)).is_none());
    }

    #[test]
    fn apply_query_key_handles_printing_and_control() {
        let mut bar = SearchBar::new();
        assert!(apply_query_key(&mut bar, Key::char('a')));
        assert!(apply_query_key(&mut bar, Key::char('b')));
        assert!(!apply_query_key(
            &mut bar,
            Key::new(KeyCode::Char('a'), KeyModifiers::CONTROL)
        ));
        assert_eq!(bar.cursor(), 0);
        assert!(apply_query_key(
            &mut bar,
            Key::new(KeyCode::Char('k'), KeyModifiers::CONTROL)
        ));
        assert_eq!(bar.query(), "");
    }
}
