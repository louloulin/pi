//! Offset-accurate visual layout for the composer body.
//!
//! The composer renders its draft through a word-wrap, and the editor's
//! vertical motions, its line-scoped `Home` / `End` and the `▍` cursor
//! marker all have to agree with that wrap: a cursor placed by a
//! different rule than the one that produced the rows is exactly how a
//! multi-line composer ends up with the caret on the wrong line. This
//! module is the single source of that geometry.
//!
//! It is a port of upstream `wordWrapLine`
//! (`packages/tui/src/components/editor.ts:121`), with the one thing the
//! TypeScript version carries and the Rust port did not: **every
//! rendered row remembers the source character offsets it was built
//! from**. Rows are slices of the draft, never a whitespace-normalised
//! rejoin, so `"a  b"` stays `"a  b"` on screen and a row boundary that
//! dropped a blank is still mappable back to an exact cursor position.
//!
//! Differences that remain, and why:
//!
//! * Segmentation is per `char`, not per extended grapheme cluster. The
//!   editor's own cursor model is per `char` (and per wide char for
//!   display width), so a grapheme-cluster wrap would place rows the
//!   cursor cannot address. Combining marks and ZWJ sequences therefore
//!   contribute their own `unicode-width` (usually 0 for a combining
//!   mark), exactly like [`crate::word_navigation`]'s char-level port.
//! * Width counts **terminal columns**, via [`crate::width`]: one CJK
//!   ideograph is two columns, an emoji is two, a combining mark is none.
//!   Until LUM-1418 this module counted characters (one column per char),
//!   which agreed with upstream for ASCII and doubled the real width of any
//!   CJK draft — the composer then wrapped one row too late and the terminal
//!   hard-wrapped the overflow onto a line the caret geometry did not know
//!   about. The row bookkeeping below (source offsets, `caret`,
//!   `cursor_at`, `preferred_col`) stays in **characters**, exactly like
//!   upstream's `buildVisualLineMap`, whose `startCol` / `length` are also
//!   character columns; only the wrap decision measures columns.
//! * Break opportunities are upstream's two rules: after a whitespace run
//!   followed by a non-blank, and between two adjacent characters when
//!   either is CJK ([`crate::width::is_cjk_break`]).
//! * A row wider than the available columns still force-breaks at the
//!   row edge, so a single long word is never clipped off-screen.

use crate::width::{char_columns, is_cjk_break};

/// One rendered row of the composer body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VisualRow {
    /// The row's content: a verbatim slice of the draft.
    pub text: String,
    /// Character offset in the draft where the row begins.
    pub start: usize,
    /// For each character of [`VisualRow::text`], its character offset in
    /// the draft. Sorted ascending; `source.len() == text.chars().count()`.
    /// When the draft contains chips, these are the display character indices
    /// (so a chip's sentinel is index N for all N chars of its display
    /// label), not the underlying raw buffer offsets.
    pub source: Vec<usize>,
}

impl VisualRow {
    /// Number of characters the row renders.
    fn len(&self) -> usize {
        self.source.len()
    }
}

/// The wrapped layout of a draft at a given body width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VisualLayout {
    rows: Vec<VisualRow>,
    /// Per row: is this the **last** row of its hard line (the row a
    /// hard break follows, rather than a soft wrap)?
    ///
    /// Only the pointer path needs it — see
    /// [`VisualLayout::click_offset`], where a click past the end of a
    /// soft-wrapped row snaps back onto that row instead of spilling to
    /// the next one.
    last_of_line: Vec<bool>,
}

impl VisualLayout {
    /// Wrap `text` into rows of at most `width` columns.
    ///
    /// `width == 0` means "no width known yet" (the App has not painted a
    /// frame): the draft is then laid out one row per hard line, so
    /// vertical motion still walks the lines the user typed without
    /// inventing soft wraps.
    pub(crate) fn new(text: &str, width: usize) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let mut rows: Vec<VisualRow> = Vec::new();
        let mut last_of_line: Vec<bool> = Vec::new();
        for (line_start, line_end) in hard_lines(&chars) {
            let before = rows.len();
            wrap_line(&chars, line_start, line_end, width, &mut rows);
            if rows.len() == before {
                // An empty or whitespace-only hard line is still a row:
                // otherwise `Shift+Enter` (or a blank line inside a
                // pasted draft) would leave no visible row and no cursor
                // position at all.
                rows.push(VisualRow {
                    text: String::new(),
                    start: line_start,
                    source: Vec::new(),
                });
            }
            // Every row this hard line produced is a soft wrap except the
            // last one, which the next hard break (or the end of the
            // draft) follows.
            last_of_line.resize(rows.len(), false);
            if rows.len() > before {
                last_of_line[rows.len() - 1] = true;
            }
        }
        if rows.is_empty() {
            rows.push(VisualRow {
                text: String::new(),
                start: 0,
                source: Vec::new(),
            });
            last_of_line.push(true);
        }
        Self { rows, last_of_line }
    }

    /// The rendered rows.
    pub(crate) fn rows(&self) -> &[VisualRow] {
        &self.rows
    }

    /// Number of rendered rows.
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    /// Index of the row a cursor at `cursor` (a byte offset into the draft) is drawn on.
    ///
    /// The row whose `start` is the largest one `<= cursor`. Because the
    /// rows partition the draft's characters (every character lands on
    /// exactly one row), this rule gives every cursor offset exactly one
    /// row and one column: the row break itself falls between the blank
    /// that ends the upper row and the character that opens the next one.
    /// Upstream and Martty both carry an extra "affinity" flag for the
    /// ambiguous case; the slice-based layout here has no ambiguity, so
    /// none is needed.
    pub(crate) fn row_of(&self, cursor: usize) -> usize {
        let mut picked = 0;
        for (index, row) in self.rows.iter().enumerate() {
            if row.start <= cursor {
                picked = index;
            } else {
                break;
            }
        }
        picked
    }

    /// `(row, column)` of a cursor at `cursor` — the pair the renderer
    /// draws the `▍` marker at.
    ///
    /// `cursor` is a byte offset into the draft (matching the raw buffer).
    /// `source` stores display character indices, so `take_while(|s| *s < cursor)` counts
    /// characters whose indices are less than the byte offset.
    pub(crate) fn caret(&self, cursor: usize) -> (usize, usize) {
        let row = self.row_of(cursor);
        let column = self
            .rows
            .get(row)
            .map(|row| row.source.iter().take_while(|s| **s < cursor).count())
            .unwrap_or(0);
        (row, column)
    }

    /// Character offset a cursor should take to sit at `column` of `row`.
    ///
    /// Columns past the row's content land just after its last rendered
    /// character, which is where the caret is drawn for "end of row".
    ///
    /// `source` stores the display character index for each character in the
    /// row text, so binary search correctly maps a column to the right index
    /// even when chips have a different display width than their raw chars.
    pub(crate) fn cursor_at(&self, row: usize, column: usize) -> usize {
        let Some(vrow) = self.rows.get(row) else {
            return 0;
        };
        let len = vrow.source.len();
        if column >= len {
            // Click past the last character of this row: return the position
            // after the last character.  `source.last()` is the display character
            // index of the last char; +1 moves past it.
            return vrow.start + vrow.source.last().copied().unwrap_or(0) + 1;
        }
        // `column` is a display column (terminal position).  `source[i]` is
        // the display character index of the character at column `i` in this
        // row.  Binary search: find the character that STARTS at or before
        // `column`.  `binary_search(&column)` returns `Ok(i)` when
        // `source[i] == column`, or `Err(i)` as insertion point when
        // `column` falls between character starts.  In both cases `i`
        // is the correct character index — the character at `source[i]`
        // starts at or before `column`.
        let i = vrow.source[column];
        vrow.start + i
    }

    /// Characters the row renders.
    pub(crate) fn row_len(&self, row: usize) -> usize {
        self.rows.get(row).map(VisualRow::len).unwrap_or(0)
    }

    /// Character offset a **pointer click** on `column` of `row` places the
    /// caret at.
    ///
    /// Upstream `Editor.handleMouse`
    /// (`packages/tui/src/components/editor.ts:615-670`): the caret goes to
    /// the character under the pointer, except for a click past the end of a
    /// row that is **not** the last row of its hard line, which snaps back to
    /// that row's last character (upstream's `targetIndex = lastGraphemeIndex`)
    /// instead of spilling onto the first character of the next visual row.
    /// Without that rule a click on the right half of a wrapped line looks
    /// like it jumped a line down.
    ///
    /// Unlike [`VisualLayout::cursor_at`] — shared with the keyboard's
    /// vertical motion, where "past the end of the row" must mean the row's
    /// end — this is the pointer's own rule, so the two are kept apart.
    pub(crate) fn click_offset(&self, row: usize, column: usize) -> usize {
        let Some(line) = self.rows.get(row) else {
            return 0;
        };
        let is_last_of_line = self.last_of_line.get(row).copied().unwrap_or(true);
        if !is_last_of_line && column >= line.len() && line.len() > 0 {
            return line.source.last().copied().unwrap_or(line.start);
        }
        self.cursor_at(row, column)
    }
}

/// Character-offset ranges of the hard lines in `chars`, excluding the
/// `\n` separators. Every input yields at least one range.
fn hard_lines(chars: &[char]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0usize;
    for (index, ch) in chars.iter().enumerate() {
        if *ch == '\n' {
            ranges.push((start, index));
            start = index + 1;
        }
    }
    ranges.push((start, chars.len()));
    ranges
}

/// Wrap one hard line (`chars[start..end]`) into `rows`.
///
/// Upstream's `wordWrapLine` loop: accumulate until the row would
/// overflow, then backtrack to the last break opportunity if that still
/// fits, otherwise force-break at the row edge. Widths are terminal
/// columns ([`crate::width::char_columns`]); positions stay in character
/// offsets.
fn wrap_line(chars: &[char], start: usize, end: usize, width: usize, rows: &mut Vec<VisualRow>) {
    if width == 0 {
        rows.push(row_from(chars, start, end));
        return;
    }
    if start >= end {
        return;
    }

    let line = &chars[start..end];
    let line_width: usize = line.iter().copied().map(char_columns).sum();
    if line_width <= width {
        rows.push(row_from(chars, start, end));
        return;
    }

    let mut chunk_start = 0usize; // relative to `start`
    let mut current_width = 0usize;
    // Break opportunity recorded as (relative index, width up to and
    // including the whitespace that ends the previous word).
    let mut wrap_at: Option<(usize, usize)> = None;

    for index in 0..line.len() {
        let ch = line[index];
        let glyph_width = char_columns(ch);

        if current_width + glyph_width > width {
            match wrap_at {
                Some((at, width_before)) if current_width - width_before + glyph_width <= width => {
                    push_chunk(rows, chars, start, chunk_start, at);
                    chunk_start = at;
                    current_width -= width_before;
                }
                _ if chunk_start < index => {
                    push_chunk(rows, chars, start, chunk_start, index);
                    chunk_start = index;
                    current_width = 0;
                }
                _ => {}
            }
            wrap_at = None;
        }

        current_width += glyph_width;

        // Upstream's two wrap-opportunity rules: a break is allowed after a
        // whitespace run when a non-whitespace character follows, and
        // between two adjacent characters when either side is CJK.
        let next = line.get(index + 1).copied();
        if let Some(next) = next {
            let next_is_blank = next.is_whitespace() && next != '\n';
            let after_a_blank = ch.is_whitespace();
            let cjk_boundary = is_cjk_break(ch) || is_cjk_break(next);
            if !next_is_blank && (after_a_blank || cjk_boundary) {
                wrap_at = Some((index + 1, current_width));
            }
        }
    }

    push_chunk(rows, chars, start, chunk_start, line.len());
}

/// Push `chars[start + from..start + to]` as a row.
fn push_chunk(rows: &mut Vec<VisualRow>, chars: &[char], start: usize, from: usize, to: usize) {
    if from >= to {
        return;
    }
    rows.push(row_from(chars, start + from, start + to));
}

/// Build a row from an absolute `[from, to)` character range.
fn row_from(chars: &[char], from: usize, to: usize) -> VisualRow {
    // `chars` are the display text characters (chips already expanded).
    // `source` stores the display character indices, so `cursor_at` can map a
    // terminal cell column to the corresponding character index.
    let mut text = String::with_capacity(to.saturating_sub(from));
    let mut source = Vec::with_capacity(to.saturating_sub(from));
    for i in from..to {
        text.push(chars[i]);
        source.push(i);
    }
    VisualRow {
        text,
        start: from,
        source,
    }
}

/// Columns a string occupies; see [`crate::width::columns`].
#[cfg(test)]
pub(crate) use crate::width::columns as display_width_of;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_past_a_wrapped_row_snaps_back_onto_it() {
        // "hello world" at width 6 wraps to "hello " / "world".
        let layout = VisualLayout::new("hello world", 6);
        assert_eq!(layout.rows().len(), 2);
        assert_eq!(layout.row_len(0), 6);
        // Any column past the first row's content stops at its last
        // character (the space at offset 5) instead of landing on 'w'
        // (offset 6).
        assert_eq!(layout.click_offset(0, 6), 5);
        assert_eq!(layout.click_offset(0, 9), 5);
        // Inside the row it is the character under the pointer.
        assert_eq!(layout.click_offset(0, 1), 1);
    }

    #[test]
    fn a_click_past_the_last_row_of_a_hard_line_runs_to_its_end() {
        // The row after `hello` starts a new hard line, so a click past its
        // end is "end of line" (`cursor_at`'s trailing position), not the
        // last character.
        let layout = VisualLayout::new("hi\nthere", 6);
        assert_eq!(layout.rows().len(), 2);
        assert_eq!(layout.click_offset(0, 5), 2, "end of the first line");
        assert_eq!(layout.click_offset(1, 5), 8, "end of the draft");
    }

    fn texts(text: &str, width: usize) -> Vec<String> {
        VisualLayout::new(text, width)
            .rows()
            .iter()
            .map(|row| row.text.clone())
            .collect()
    }

    #[test]
    fn short_text_is_a_single_row() {
        assert_eq!(texts("hello", 10), vec!["hello"]);
    }

    #[test]
    fn words_wrap_on_the_blank_that_ends_the_upper_row() {
        // The blank that ends a row stays on that row, exactly like
        // upstream's `line.slice(chunkStart, wrapOppIndex)`.
        assert_eq!(texts("hello world foo", 11), vec!["hello ", "world foo"]);
        assert_eq!(texts("ab cd", 3), vec!["ab ", "cd"]);
    }

    #[test]
    fn whitespace_runs_and_tabs_survive_the_wrap() {
        // The pre-LUM-1312 port ran `split_whitespace` and rejoined with a
        // single space, so the composer silently normalised the draft.
        assert_eq!(texts("a  b\tc", 20), vec!["a  b\tc"]);
    }

    #[test]
    fn a_long_word_force_breaks_at_the_row_edge() {
        assert_eq!(texts("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn wide_glyphs_wrap_by_terminal_columns() {
        // A CJK ideograph is two terminal columns (LUM-1418). A draft of
        // eight ideographs is sixteen columns wide, so it takes two rows at
        // width 8 and four rows at width 4.
        assert_eq!(texts("你好世界你好世界", 16), vec!["你好世界你好世界"]);
        assert_eq!(texts("你好世界你好世界", 8), vec!["你好世界", "你好世界"]);
        assert_eq!(
            texts("你好世界你好世界", 4),
            vec!["你好", "世界", "你好", "世界"]
        );
    }

    #[test]
    fn a_cjk_run_breaks_between_adjacent_ideographs() {
        // Upstream's CJK rule: no spaces needed, every boundary is a break
        // opportunity, and the row is filled to the column budget before the
        // break fires. "你好世界" is 8 columns, so width 6 keeps three
        // ideographs (6 columns) and pushes the fourth to the next row.
        assert_eq!(texts("你好世界", 6), vec!["你好世", "界"]);
        // The row is filled to the column budget before the break fires, so a
        // wide glyph that exactly completes the row stays on it — this is the
        // `currentWidth - wrapOppWidth + gWidth <= maxWidth` backtrack in
        // upstream's `wordWrapLine`, not a greedy break at the first CJK
        // boundary.
        assert_eq!(texts("ab你好", 4), vec!["ab你", "好"]);
    }

    #[test]
    fn an_ascii_word_still_wraps_at_its_blank() {
        // The CJK rule must not turn a Latin word into a break opportunity.
        assert_eq!(texts("hello world foo", 11), vec!["hello ", "world foo"]);
    }

    #[test]
    fn blank_hard_lines_keep_their_row() {
        assert_eq!(texts("a\n\nb", 10), vec!["a", "", "b"]);
        assert_eq!(texts("a\n", 10), vec!["a", ""]);
        assert_eq!(texts("", 10), vec![""]);
        assert_eq!(texts("\n", 10), vec!["", ""]);
    }

    #[test]
    fn row_starts_and_sources_track_the_original_offsets() {
        // Rows are slices, so the two blanks keep their own offsets even
        // though the row break falls between them.
        let layout = VisualLayout::new("aa  bb", 3);
        assert_eq!(
            layout.rows(),
            &[
                VisualRow {
                    text: "aa ".into(),
                    start: 0,
                    source: vec![0, 1, 2],
                },
                VisualRow {
                    text: " bb".into(),
                    start: 3,
                    source: vec![3, 4, 5],
                },
            ]
        );
    }

    #[test]
    fn the_rows_partition_the_draft_characters() {
        // Every draft character except the hard breaks lands on exactly
        // one row, and each row is a verbatim slice of the draft at the
        // offsets it records. That is what makes the cursor mapping
        // exact instead of approximate.
        for text in [
            "hello world foo",
            "aa  bb",
            "a\n\nb\n",
            "你好世界你好世界",
            "a very long single word here",
            "",
        ] {
            let chars: Vec<char> = text.chars().collect();
            let expected: Vec<usize> = (0..chars.len()).filter(|i| chars[*i] != '\n').collect();
            for width in 0..12usize {
                let layout = VisualLayout::new(text, width);
                let sources: Vec<usize> = layout
                    .rows()
                    .iter()
                    .flat_map(|row| row.source.clone())
                    .collect();
                assert_eq!(sources, expected, "text={text:?} width={width}");
                for row in layout.rows() {
                    let slice: String = chars[row.start..row.start + row.source.len()]
                        .iter()
                        .collect();
                    assert_eq!(row.text, slice, "text={text:?} width={width}");
                    assert!(row.source.is_empty() || row.source[0] == row.start);
                }
            }
        }
    }

    #[test]
    fn caret_splits_a_soft_wrap_between_the_blank_and_the_next_word() {
        // "hello world foo" at width 11: row 0 is "hello " (source
        // 0..=5), row 1 starts at source 6. The blank at 5 draws at the
        // end of row 0, and the `w` after it opens row 1 — one row per
        // offset, so vertical motion needs no extra affinity flag.
        let layout = VisualLayout::new("hello world foo", 11);
        assert_eq!(layout.caret(5), (0, 5));
        assert_eq!(layout.caret(6), (1, 0));
        assert_eq!(layout.cursor_at(0, 5), 5);
        assert_eq!(layout.cursor_at(1, 0), 6);
    }

    #[test]
    fn caret_of_a_hard_break_shows_on_the_next_row() {
        let layout = VisualLayout::new("ab\ncd", 10);
        assert_eq!(layout.caret(2), (0, 2), "cursor on the newline ends row 0");
        assert_eq!(layout.caret(3), (1, 0));
    }

    #[test]
    fn caret_of_a_blank_line_lands_on_that_row() {
        let layout = VisualLayout::new("a\n\nb", 10);
        assert_eq!(layout.caret(2), (1, 0), "the empty row owns the cursor");
    }

    #[test]
    fn cursor_at_past_the_end_of_a_row_sits_after_its_last_character() {
        let layout = VisualLayout::new("abc\ndef", 10);
        assert_eq!(layout.cursor_at(0, 99), 3);
        assert_eq!(layout.cursor_at(1, 99), 7);
        assert_eq!(layout.row_len(0), 3);
    }

    #[test]
    fn unknown_width_lays_out_hard_lines_only() {
        assert_eq!(texts("hello world", 0), vec!["hello world"]);
        assert_eq!(texts("a\nb", 0), vec!["a", "b"]);
    }

    #[test]
    fn display_width_of_counts_terminal_columns() {
        assert_eq!(display_width_of("你好"), 4);
        assert_eq!(display_width_of("ab"), 2);
        assert_eq!(display_width_of("a\tb"), 5);
        assert_eq!(display_width_of("a\rb"), 2, "a CRLF paste stays invisible");
    }
}
