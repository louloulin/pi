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
//! * Width counts terminal **columns**, not characters: a frame buffer
//!   cell is one column, so a wide glyph (CJK, most emoji) is two of them
//!   and a combining mark is none. Measuring per character placed the
//!   second half of every wide glyph on top of its neighbour, let a CJK
//!   draft wrap at twice the composer's width, and made the `▍` marker
//!   land on the wrong cell — the LUM-1336 defect, measured on a real PTY.
//!   Upstream TS `wordWrapLine` and Martty's `visual_cursor` both measure
//!   columns (`wcwidth`), so this is the parity direction.
//!
//!   The rest of the crate still measures one column per character
//!   (`markdown.rs` "Width convention", `hyperlink::visible_width`,
//!   `message::display_width`, `settings.rs`, `selector.rs`); those are the
//!   *output* surfaces and are tracked separately in
//!   `docs/LUM1336_COMPOSER_WIDTH.md`. Within the composer — layout, caret,
//!   pointer mapping and paint — the column rule below is the only one.
//! * Break opportunities are whitespace runs only, upstream's first rule,
//!   plus the force-break at the row edge. Upstream's second rule (break
//!   between two CJK characters) is subsumed: under a column measure two
//!   wide glyphs are four columns, so the force-break fires exactly where
//!   upstream's CJK rule would.
//! * A row wider than the available columns still force-breaks at the
//!   row edge, so a single long word is never clipped off-screen.

/// One rendered row of the composer body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VisualRow {
    /// The row's content: a verbatim slice of the draft.
    pub text: String,
    /// Character offset in the draft where the row begins.
    pub start: usize,
    /// For each character of [`VisualRow::text`], its character offset in
    /// the draft. Sorted ascending; `source.len() == text.chars().count()`.
    pub source: Vec<usize>,
}

impl VisualRow {
    /// Number of characters the row renders.
    fn len(&self) -> usize {
        self.source.len()
    }

    /// Columns the row occupies on screen (see [`cell_width`]).
    pub(crate) fn width(&self) -> usize {
        self.text.chars().map(cell_width).sum()
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

    /// Index of the row a cursor at `cursor` (a character offset into the
    /// draft) is drawn on.
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
    /// The column counts the **columns** of the row's rendered characters
    /// that start before the cursor, so a wide glyph moves the marker by
    /// two cells and a combining mark by none. Under an all-ASCII draft
    /// this is the same number as the character count.
    pub(crate) fn caret(&self, cursor: usize) -> (usize, usize) {
        let row = self.row_of(cursor);
        let column = self
            .rows
            .get(row)
            .map(|row| {
                row.text
                    .chars()
                    .zip(&row.source)
                    .take_while(|(_, source)| **source < cursor)
                    .map(|(ch, _)| cell_width(ch))
                    .sum()
            })
            .unwrap_or(0);
        (row, column)
    }

    /// Character offset a cursor should take to sit at `column` — a
    /// **column** of `row` per [`VisualLayout::caret`] — of that row.
    ///
    /// The cursor sits before the first character whose cell range starts
    /// past `column`, which is the largest character index whose prefix
    /// width is still `<= column`. Columns past the row's content land just
    /// after its last rendered character, which is where the caret is drawn
    /// for "end of row".
    pub(crate) fn cursor_at(&self, row: usize, column: usize) -> usize {
        let Some(row) = self.rows.get(row) else {
            return 0;
        };
        let index = char_index_at_column(row, column);
        if let Some(offset) = row.source.get(index) {
            return *offset;
        }
        row.source
            .last()
            .map(|offset| offset + 1)
            .unwrap_or(row.start)
    }

    /// Columns the row occupies on screen.
    pub(crate) fn row_width(&self, row: usize) -> usize {
        self.rows.get(row).map(VisualRow::width).unwrap_or(0)
    }

    /// Character offset a **pointer click** on `column` of `row` places the
    /// caret at. `column` is a terminal column, like the click's `x`.
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
    /// end — this is the pointer's own rule, so the two are kept apart. It is
    /// [`VisualLayout::click_char`] where a character exists and the row's end
    /// boundary where none does, which is the split a *selection* needs.
    pub(crate) fn click_offset(&self, row: usize, column: usize) -> usize {
        self.click_char(row, column)
            .unwrap_or_else(|| self.cursor_at(row, column))
    }

    /// The character a **pointer** cell covers, or `None` when the pointer
    /// landed past the end of the row's content.
    ///
    /// The selection counterpart of [`VisualLayout::click_offset`]: a
    /// selection takes the character under its *start* and one past the
    /// character under its *end*, so "is there a character under this cell?"
    /// has to be answerable on its own. A column inside a wide glyph's cell
    /// range covers that glyph — the same cell range `cursor_at` walks — and
    /// past the end of a soft-wrapped row the snap-back rule above puts the
    /// caret on the row's last character, which is a character too; past the
    /// end of a row that ends its hard line the caret offset is a boundary
    /// (the row's end, or the offset of the `\n` that separates it from the
    /// next one), which no cell covers and therefore answers `None`.
    pub(crate) fn click_char(&self, row: usize, column: usize) -> Option<usize> {
        let line = self.rows.get(row)?;
        let index = char_index_at_column(line, column);
        if let Some(offset) = line.source.get(index) {
            return Some(*offset);
        }
        let is_last_of_line = self.last_of_line.get(row).copied().unwrap_or(true);
        if !is_last_of_line && line.len() > 0 {
            return line.source.last().copied();
        }
        None
    }
}

/// Character index of the cursor that sits at cell `column` of `row`:
/// the largest index whose prefix width is `<= column`.
fn char_index_at_column(row: &VisualRow, column: usize) -> usize {
    let mut index = 0usize;
    let mut acc = 0usize;
    for ch in row.text.chars() {
        let width = cell_width(ch);
        if acc + width > column {
            break;
        }
        acc += width;
        index += 1;
    }
    index
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
/// fits, otherwise force-break at the row edge.
fn wrap_line(chars: &[char], start: usize, end: usize, width: usize, rows: &mut Vec<VisualRow>) {
    if width == 0 {
        rows.push(row_from(chars, start, end));
        return;
    }
    if start >= end {
        return;
    }

    let line = &chars[start..end];
    let line_width: usize = line.iter().copied().map(cell_width).sum();
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
        let glyph_width = cell_width(ch);

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

        // A break is allowed after a whitespace run when a non-whitespace
        // character follows (upstream's first wrap-opportunity rule).
        let next = line.get(index + 1).copied();
        if let Some(next) = next {
            let next_is_blank = next.is_whitespace() && next != '\n';
            if ch.is_whitespace() && !next_is_blank {
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
    let mut text = String::with_capacity(to.saturating_sub(from));
    let mut source = Vec::with_capacity(to.saturating_sub(from));
    for (offset, ch) in chars[from..to].iter().enumerate() {
        text.push(*ch);
        source.push(from + offset);
    }
    VisualRow {
        text,
        start: from,
        source,
    }
}

/// Columns one character occupies on screen.
///
/// A frame buffer cell is one terminal column, and [`ratatui`] measures a
/// cell's symbol with `unicode-width` when it diffs the frame, so the
/// composer has to lay its draft out with the same measure or the two
/// disagree by one cell per wide glyph (LUM-1336). Concretely:
///
/// * a wide glyph (CJK, most emoji) is 2 columns,
/// * a combining mark is 0,
/// * a tab counts as 1 column, the pre-LUM-1336 convention — a paste is
///   never re-entered as a tab and the composer's own insert paths have no
///   tab key, so this only keeps a pasted tab from collapsing to nothing,
/// * any other control character (`\r` in a CRLF paste, for example) is 0:
///   it is invisible and must not consume a cell.
pub(crate) fn cell_width(ch: char) -> usize {
    if ch == '\t' {
        return 1;
    }
    if ch.is_control() {
        return 0;
    }
    unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// Columns a string occupies on screen (see [`cell_width`]).
pub(crate) fn cells(text: &str) -> usize {
    text.chars().map(cell_width).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_past_a_wrapped_row_snaps_back_onto_it() {
        // "hello world" at width 6 wraps to "hello " / "world".
        let layout = VisualLayout::new("hello world", 6);
        assert_eq!(layout.rows().len(), 2);
        assert_eq!(layout.row_width(0), 6);
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

    #[test]
    fn a_pointer_cell_reports_the_character_it_covers() {
        // "hello world" at width 6 wraps to "hello " / "world".
        let layout = VisualLayout::new("hello world", 6);
        assert_eq!(layout.click_char(0, 1), Some(1), "the character under it");
        assert_eq!(layout.click_char(1, 3), Some(9), "and on the second row");
        // Past the end of the *soft-wrapped* row the snap-back rule puts the
        // caret on the row's last character, which a selection must include.
        assert_eq!(layout.click_char(0, 6), Some(5));
        assert_eq!(layout.click_char(0, 99), Some(5));
    }

    #[test]
    fn a_pointer_past_a_hard_line_boundary_covers_no_character() {
        // `click_offset` reports a *boundary* there (the end of `hi`, or the
        // offset of the `\n`), which a selection end must not widen past.
        let layout = VisualLayout::new("hi\nthere", 6);
        assert_eq!(layout.click_offset(0, 5), 2);
        assert_eq!(layout.click_char(0, 5), None);
        assert_eq!(layout.click_offset(1, 5), 8);
        assert_eq!(layout.click_char(1, 5), None);
        // An empty row covers no character either.
        let blank = VisualLayout::new("a\n\nb", 6);
        assert_eq!(blank.click_char(1, 0), None);
    }

    #[test]
    fn a_wide_character_covers_both_of_its_cells() {
        // "你好a": '你' owns columns 0-1, '好' 2-3, 'a' 4 (LUM-1336's column
        // measure), so a pointer on either half of a glyph covers that glyph.
        let layout = VisualLayout::new("你好a", 10);
        assert_eq!(layout.click_char(0, 0), Some(0));
        assert_eq!(layout.click_char(0, 1), Some(0), "the second half of 你");
        assert_eq!(layout.click_char(0, 2), Some(1));
        assert_eq!(layout.click_char(0, 3), Some(1), "the second half of 好");
        assert_eq!(layout.click_char(0, 4), Some(2));
        assert_eq!(layout.click_char(0, 5), None, "one past the row's content");
        // The caret rule agrees with it: a click on either half of a glyph
        // puts the caret *before* that glyph.
        assert_eq!(layout.click_offset(0, 1), 0);
        assert_eq!(layout.click_offset(0, 3), 1);
        assert_eq!(layout.click_offset(0, 4), 2);
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
    fn wide_glyphs_wrap_by_columns_not_by_characters() {
        // LUM-1336: four CJK characters are eight terminal columns, so they
        // fill a width-8 row exactly and do *not* wrap at four of them like
        // the pre-fix char measure did.
        assert_eq!(texts("你好世界", 8), vec!["你好世界"]);
        assert_eq!(texts("你好世界你好世界", 8), vec!["你好世界", "你好世界"]);
        // 6 columns cannot hold 世(2)+界(2) after 你好(4): the force break
        // lands between two wide glyphs, which is upstream's CJK rule.
        assert_eq!(texts("你好世界", 6), vec!["你好世", "界"]);
        // Mixed widths fill the row by columns: 3 ASCII (3) + 一 (2) = 5,
        // 好 (2) would be 7 > 6.
        assert_eq!(texts("abc你好世界", 6), vec!["abc你", "好世界"]);
    }

    #[test]
    fn caret_and_pointer_map_columns_not_characters() {
        let layout = VisualLayout::new("你好世界", 8);
        assert_eq!(layout.rows()[0].text.chars().count(), 4, "four characters");
        assert_eq!(layout.row_width(0), 8, "eight columns");
        // Caret before each character, in columns: 2 per wide glyph.
        assert_eq!(layout.caret(0), (0, 0));
        assert_eq!(layout.caret(1), (0, 2));
        assert_eq!(layout.caret(2), (0, 4));
        assert_eq!(layout.caret(4), (0, 8));
        // A pointer on column 5 — the second cell of 世, whose two cells are
        // 4 and 5 — still lands before 世, i.e. at character offset 2;
        // column 4 is the boundary where 世 starts.
        assert_eq!(layout.cursor_at(0, 4), 2);
        assert_eq!(layout.click_offset(0, 5), 2);
        assert_eq!(layout.click_offset(0, 3), 1, "the second cell of 好");
        assert_eq!(layout.click_offset(0, 0), 0);
        // Past the row end: the trailing position of a single hard line.
        assert_eq!(layout.click_offset(0, 9), 4);
    }

    #[test]
    fn caret_columns_agree_with_the_rows_a_wrapped_cjk_draft_produces() {
        // "你好世界你好世界" at width 8 wraps into two 4-character rows; a
        // cursor in the middle of the *second* row is 12 columns in, which
        // the renderer turns back into 4 columns on that row.
        let layout = VisualLayout::new("你好世界你好世界", 8);
        assert_eq!(layout.rows().len(), 2);
        assert_eq!(layout.caret(4), (1, 0));
        assert_eq!(layout.caret(6), (1, 4));
        assert_eq!(layout.caret(8), (1, 8));
        // The two halves are addressable as draft rows, so vertical motion
        // keeps a column the row can hold.
        assert_eq!(layout.cursor_at(1, 4), 6);
        assert_eq!(layout.row_width(1), 8);
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
                    source: vec![0, 1, 2]
                },
                VisualRow {
                    text: " bb".into(),
                    start: 3,
                    source: vec![3, 4, 5]
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
        assert_eq!(layout.row_width(0), 3);
    }

    #[test]
    fn unknown_width_lays_out_hard_lines_only() {
        assert_eq!(texts("hello world", 0), vec!["hello world"]);
        assert_eq!(texts("a\nb", 0), vec!["a", "b"]);
    }

    #[test]
    fn cells_count_columns_like_the_terminal_does() {
        assert_eq!(cells("你好"), 4, "each CJK glyph is two columns");
        assert_eq!(cells("ab"), 2);
        assert_eq!(cells("a	b"), 3, "a tab keeps the pre-fix one-column rule");
        assert_eq!(cells("a\rb"), 2, "a CRLF paste stays invisible");
        assert_eq!(cells("e\u{301}"), 1, "a combining accent occupies none");
        assert_eq!(cells("界"), 2);
    }
}
