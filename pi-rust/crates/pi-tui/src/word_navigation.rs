//! Word-boundary navigation for the single-line editor.
//!
//! Ports `packages/tui/src/word-navigation.ts`. Upstream segments text
//! with `Intl.Segmenter` (word granularity, UAX #29) and then applies a
//! small set of boundary rules on top:
//!
//! * trailing / leading whitespace is skipped, then
//! * an ASCII-punctuation run (`.`, `/`, `...`) is skipped whole,
//! * while a word-like segment that *contains* punctuation (the
//!   segmenter groups `/to` as one word on some inputs) only moves the
//!   cursor past its last / first punctuation character, preserving
//!   ASCII punctuation boundaries.
//!
//! The Rust port keeps the same rules but uses
//! [`unicode_segmentation`]'s UAX #29 word boundaries instead of ICU.
//! That is equivalent for ASCII (which the ported tests from
//! `packages/tui/test/word-navigation.test.ts` pin down); for CJK the
//! difference is that ICU applies a dictionary (so `你好` / `世界` are
//! single words) while UAX #29 splits each ideograph. Every step still
//! lands on a character boundary and reaches the buffer corner in a
//! bounded number of moves — see [`find_word_backward`] /
//! [`find_word_forward`].
//!
//! Cursors are byte offsets into the buffer, matching
//! [`Editor`](crate::Editor); upstream works in UTF-16 code units.

use unicode_segmentation::UnicodeSegmentation;

/// The ASCII punctuation set from upstream
/// `PUNCTUATION_REGEX` (`packages/tui/src/utils.ts`).
///
/// Only ASCII punctuation counts as a boundary; every other character is
/// handled by the word segmenter.
pub fn is_punctuation_char(c: char) -> bool {
    matches!(
        c,
        '(' | ')'
            | '{'
            | '}'
            | '['
            | ']'
            | '<'
            | '>'
            | '.'
            | ','
            | ';'
            | ':'
            | '\''
            | '"'
            | '!'
            | '?'
            | '+'
            | '-'
            | '='
            | '*'
            | '/'
            | '\\'
            | '|'
            | '&'
            | '%'
            | '^'
            | '$'
            | '#'
            | '@'
            | '~'
            | '`'
    )
}

/// True when `text` contains a whitespace character.
///
/// Mirrors upstream `isWhitespaceChar` (`/\s/.test(char)`) applied to a
/// whole segment. An empty string is not whitespace.
pub fn is_whitespace(text: &str) -> bool {
    text.chars().any(char::is_whitespace)
}

/// True when a segmented word is "word-like" — it contains at least one
/// alphanumeric character or underscore.
///
/// This is the Rust stand-in for `Intl.SegmentData::isWordLike`; the
/// segmenter already guarantees that a word-like segment never starts
/// with punctuation.
pub fn is_word_like(segment: &str) -> bool {
    segment.chars().any(|c| c.is_alphanumeric() || c == '_')
}

/// One segmentation unit: a byte range of the buffer, its text, and whether
/// it is a word or an atomic span (a paste marker).
///
/// The atom-aware functions treat an `atomic` unit as a single token in
/// either direction, which is what upstream's
/// `segmentWithMarkers(..., "word")` does for
/// `[paste #1 +12 lines]` (`packages/tui/src/components/editor.ts:46-98`).
#[derive(Debug, Clone, Copy)]
struct Segment<'a> {
    /// Byte offset of the unit in the buffer the slice was taken from.
    start: usize,
    /// Byte offset just past the unit.
    end: usize,
    /// The unit's text.
    text: &'a str,
    /// True when the unit is word-like ([`is_word_like`]).
    word_like: bool,
    /// True when the unit is one of the caller's atomic spans.
    atomic: bool,
}

/// Segment `text[from..to]`, merging every span in `atoms` into one atomic
/// unit.
///
/// `atoms` are byte ranges **in `text`**, so a caller can hand over the
/// marker spans it already computed. A span the range cuts in half is not
/// merged — upstream's marker regex sees the same thing on the
/// `textBeforeCursor` / `textAfterCursor` slices it segments.
fn segments_in<'a>(
    text: &'a str,
    from: usize,
    to: usize,
    atoms: &[(usize, usize)],
) -> Vec<Segment<'a>> {
    let from = from.min(text.len());
    let to = to.min(text.len()).max(from);
    let mut atoms: Vec<(usize, usize)> = atoms
        .iter()
        .copied()
        .filter(|(start, end)| *start >= from && *end <= to && start < end)
        .map(|(start, end)| (start - from, end - from))
        .collect();
    atoms.sort_unstable();
    atoms.dedup();
    let slice = &text[from..to];
    let mut out = Vec::new();
    let mut atom_index = 0usize;
    let mut position = 0usize;
    for base in slice.split_word_bounds() {
        let start = position;
        let end = position + base.len();
        position = end;
        while atom_index < atoms.len() && atoms[atom_index].1 <= start {
            atom_index += 1;
        }
        if atom_index < atoms.len() && start >= atoms[atom_index].0 && start < atoms[atom_index].1 {
            // The first base segment of the atom is replaced by the whole
            // merged span; every later segment inside it is skipped.
            if start == atoms[atom_index].0 {
                let (atom_start, atom_end) = atoms[atom_index];
                out.push(Segment {
                    start: from + atom_start,
                    end: from + atom_end,
                    text: &slice[atom_start..atom_end],
                    word_like: false,
                    atomic: true,
                });
            }
        } else {
            out.push(Segment {
                start: from + start,
                end: from + end,
                text: base,
                word_like: is_word_like(base),
                atomic: false,
            });
        }
    }
    out
}

/// Find the cursor position after moving one word backward from
/// `cursor` in `text`, treating each span in `atoms` as a single unit.
///
/// Trailing whitespace is skipped, then the cursor stops at the next
/// word / punctuation / atomic boundary. Pure function — it does not mutate
/// the buffer. Returns `cursor` unchanged when `cursor` is `0` or past the
/// end of `text`; the byte offset is always clamped into `text`.
pub fn find_word_backward_with_atoms(text: &str, cursor: usize, atoms: &[(usize, usize)]) -> usize {
    let cursor = cursor.min(text.len());
    if cursor == 0 {
        return 0;
    }
    let mut segments = segments_in(text, 0, cursor, atoms);
    let mut new_cursor = cursor;

    // Skip trailing whitespace. An atomic span is never whitespace, even
    // when the atom's own text is (it never is: a marker starts with `[`).
    while let Some(last) = segments.last().copied() {
        if last.atomic || !is_whitespace(last.text) {
            break;
        }
        new_cursor = last.start;
        segments.pop();
    }

    let Some(last) = segments.last().copied() else {
        return new_cursor;
    };

    if last.atomic {
        // One step crosses the whole marker (upstream `isAtomicSegment`).
        new_cursor = last.start;
    } else if last.word_like {
        // Skip inside one word-like segment, preserving ASCII
        // punctuation boundaries: stop just after the last punctuation
        // character, or move the whole segment when there is none.
        match last
            .text
            .char_indices()
            .rfind(|(_, c)| is_punctuation_char(*c))
        {
            Some((idx, c)) => new_cursor -= last.text.len() - (idx + c.len_utf8()),
            None => new_cursor -= last.text.len(),
        }
    } else {
        // Skip the whole run of non-word, non-whitespace (punctuation)
        // segments; an atomic span ends the run.
        while let Some(segment) = segments.last().copied() {
            if segment.atomic || is_whitespace(segment.text) || segment.word_like {
                break;
            }
            new_cursor = segment.start;
            segments.pop();
        }
    }

    new_cursor
}

/// Find the cursor position after moving one word forward from `cursor`
/// in `text`, treating each span in `atoms` as a single unit.
///
/// Leading whitespace is skipped, then the cursor stops at the next
/// word / punctuation / atomic boundary. Pure function — it does not mutate
/// the buffer. Returns `text.len()` when `cursor` is at or past the end.
pub fn find_word_forward_with_atoms(text: &str, cursor: usize, atoms: &[(usize, usize)]) -> usize {
    let cursor = cursor.min(text.len());
    if cursor >= text.len() {
        return text.len();
    }
    let segments = segments_in(text, cursor, text.len(), atoms);
    let mut new_cursor = cursor;
    let mut index = 0usize;

    // Skip leading whitespace.
    while let Some(segment) = segments.get(index).copied() {
        if segment.atomic || !is_whitespace(segment.text) {
            break;
        }
        new_cursor = segment.end;
        index += 1;
    }

    let Some(next) = segments.get(index).copied() else {
        return new_cursor;
    };

    if next.atomic {
        // One step crosses the whole marker.
        new_cursor = next.end;
    } else if next.word_like {
        // Preserve ASCII punctuation boundaries: stop at the first
        // punctuation character inside the segment, or take the whole
        // segment.
        match next
            .text
            .char_indices()
            .find(|(_, c)| is_punctuation_char(*c))
        {
            Some((idx, _)) => new_cursor = next.start + idx,
            None => new_cursor = next.end,
        }
    } else {
        // Skip the whole run of non-word, non-whitespace (punctuation)
        // segments; an atomic span ends the run.
        while let Some(segment) = segments.get(index).copied() {
            if segment.atomic || is_whitespace(segment.text) || segment.word_like {
                break;
            }
            new_cursor = segment.end;
            index += 1;
        }
    }

    new_cursor
}

/// Find the cursor position after moving one word backward from
/// `cursor` in `text`.
///
/// Trailing whitespace is skipped, then the cursor stops at the next
/// word / punctuation boundary. Pure function — it does not mutate the
/// buffer. Returns `cursor` unchanged when `cursor` is `0` or past the
/// end of `text`; the byte offset is always clamped into `text`.
pub fn find_word_backward(text: &str, cursor: usize) -> usize {
    find_word_backward_with_atoms(text, cursor, &[])
}

/// Find the cursor position after moving one word forward from `cursor`
/// in `text`.
///
/// Leading whitespace is skipped, then the cursor stops at the next
/// word / punctuation boundary. Pure function — it does not mutate the
/// buffer. Returns `text.len()` when `cursor` is at or past the end.
pub fn find_word_forward(text: &str, cursor: usize) -> usize {
    find_word_forward_with_atoms(text, cursor, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ported from `packages/tui/test/word-navigation.test.ts`
    /// (`findWordBackward`), with UTF-16 code-unit offsets replaced by
    /// byte offsets.
    #[test]
    fn backward_basic_words() {
        let text = "hello world";
        assert_eq!(find_word_backward(text, 11), 6);
        assert_eq!(find_word_backward(text, 6), 0);
    }

    #[test]
    fn backward_dotted() {
        let text = "foo.bar";
        assert_eq!(find_word_backward(text, 7), 4);
        assert_eq!(find_word_backward(text, 4), 3);
        assert_eq!(find_word_backward(text, 3), 0);
    }

    #[test]
    fn backward_colon() {
        let text = "foo:bar";
        assert_eq!(find_word_backward(text, 7), 4);
        assert_eq!(find_word_backward(text, 4), 3);
        assert_eq!(find_word_backward(text, 3), 0);
    }

    #[test]
    fn backward_path() {
        let text = "path/to/file";
        assert_eq!(find_word_backward(text, 12), 8);
        assert_eq!(find_word_backward(text, 8), 7);
        // "/to" is a punctuation boundary in front of a word, whichever
        // way the segmenter splits it.
        assert_eq!(find_word_backward(text, 7), 5);
        assert_eq!(find_word_backward(text, 5), 4);
        assert_eq!(find_word_backward(text, 4), 0);
    }

    #[test]
    fn backward_whitespace_at_boundaries() {
        let text = "  hello  ";
        assert_eq!(find_word_backward(text, 9), 2);
        assert_eq!(find_word_backward(text, 2), 0);
    }

    #[test]
    fn backward_punctuation_run() {
        let text = "foo...bar";
        assert_eq!(find_word_backward(text, 9), 6);
        assert_eq!(find_word_backward(text, 6), 3);
        assert_eq!(find_word_backward(text, 3), 0);
    }

    #[test]
    fn backward_at_zero_is_zero() {
        assert_eq!(find_word_backward("hello", 0), 0);
    }

    #[test]
    fn backward_reaches_start_for_cjk() {
        // ICU groups CJK dictionary words, UAX #29 splits per
        // ideograph; what must hold either way is that every step
        // lands on a char boundary and the walk terminates at 0.
        let text = "你好世界 test";
        let space = text.find(' ').expect("space");
        assert_eq!(find_word_backward(text, text.len()), space + 1);
        let mut pos = text.len();
        let mut steps = 0;
        while pos > 0 {
            let next = find_word_backward(text, pos);
            assert!(next < pos, "step {steps} did not move: {pos} -> {next}");
            assert!(
                text.is_char_boundary(next),
                "step {steps} left a char boundary: {next}"
            );
            pos = next;
            steps += 1;
            assert!(steps <= text.chars().count() + 1, "walk did not terminate");
        }
        assert_eq!(pos, 0);
    }

    /// Ported from `packages/tui/test/word-navigation.test.ts`
    /// (`findWordForward`).
    #[test]
    fn forward_basic_words() {
        let text = "hello world";
        assert_eq!(find_word_forward(text, 0), 5);
        assert_eq!(find_word_forward(text, 5), 11);
    }

    #[test]
    fn forward_dotted() {
        let text = "foo.bar";
        assert_eq!(find_word_forward(text, 0), 3);
        assert_eq!(find_word_forward(text, 3), 4);
        assert_eq!(find_word_forward(text, 4), 7);
    }

    #[test]
    fn forward_colon() {
        let text = "foo:bar";
        assert_eq!(find_word_forward(text, 0), 3);
        assert_eq!(find_word_forward(text, 3), 4);
        assert_eq!(find_word_forward(text, 4), 7);
    }

    #[test]
    fn forward_path() {
        let text = "path/to/file";
        assert_eq!(find_word_forward(text, 0), 4);
        assert_eq!(find_word_forward(text, 4), 5);
        assert_eq!(find_word_forward(text, 5), 7);
        assert_eq!(find_word_forward(text, 7), 8);
        assert_eq!(find_word_forward(text, 8), 12);
    }

    #[test]
    fn forward_whitespace_at_boundaries() {
        let text = "  hello  ";
        assert_eq!(find_word_forward(text, 0), 7);
        assert_eq!(find_word_forward(text, 7), 9);
    }

    #[test]
    fn forward_punctuation_run() {
        let text = "foo...bar";
        assert_eq!(find_word_forward(text, 0), 3);
        assert_eq!(find_word_forward(text, 3), 6);
        assert_eq!(find_word_forward(text, 6), 9);
    }

    #[test]
    fn forward_at_end_is_end() {
        assert_eq!(find_word_forward("hello", 5), 5);
    }

    #[test]
    fn forward_reaches_end_for_cjk() {
        let text = "你好世界 test";
        let mut pos = 0;
        let mut steps = 0;
        while pos < text.len() {
            let next = find_word_forward(text, pos);
            assert!(next > pos, "step {steps} did not move: {pos} -> {next}");
            assert!(
                text.is_char_boundary(next),
                "step {steps} left a char boundary: {next}"
            );
            pos = next;
            steps += 1;
            assert!(steps <= text.chars().count() + 1, "walk did not terminate");
        }
        assert_eq!(pos, text.len());
    }

    #[test]
    fn cursors_are_clamped_into_the_buffer() {
        assert_eq!(find_word_backward("abc", 99), 0);
        assert_eq!(find_word_forward("abc", 99), 3);
        assert_eq!(find_word_backward("", 5), 0);
        assert_eq!(find_word_forward("", 5), 0);
    }

    #[test]
    fn classification_helpers() {
        assert!(is_whitespace(" "));
        assert!(is_whitespace("\t"));
        assert!(!is_whitespace(""));
        assert!(!is_whitespace("a"));
        assert!(is_word_like("hello"));
        assert!(is_word_like("héllo"));
        assert!(is_word_like("snake_case"));
        assert!(is_word_like("3.14"));
        assert!(!is_word_like("..."));
        assert!(!is_word_like(" "));
        assert!(is_punctuation_char('.'));
        assert!(is_punctuation_char('/'));
        assert!(!is_punctuation_char('a'));
        assert!(!is_punctuation_char('é'));
    }

    // -- atomic paste-marker spans -------------------------------------

    /// `abc [paste #1 +12 lines] def` with the marker at bytes 4..24.
    const MARKER_TEXT: &str = "abc [paste #1 +12 lines] def";
    const MARKER_SPAN: (usize, usize) = (4, 24);

    #[test]
    fn one_backward_step_crosses_a_whole_marker() {
        let atoms = [MARKER_SPAN];
        // From the end, the first step is the ordinary word...
        assert_eq!(find_word_backward(MARKER_TEXT, MARKER_TEXT.len()), 25);
        // ...and the next one jumps over the marker and lands before it.
        assert_eq!(find_word_backward_with_atoms(MARKER_TEXT, 25, &atoms), 4);
        // Standing just after the marker, the step still leaves it whole.
        assert_eq!(find_word_backward_with_atoms(MARKER_TEXT, 24, &atoms), 4);
        // The landing point is never inside the marker (its edges are the
        // only places a step may stop).
        for cursor in [25, 24] {
            let target = find_word_backward_with_atoms(MARKER_TEXT, cursor, &atoms);
            assert!(
                !(MARKER_SPAN.0 < target && target < MARKER_SPAN.1),
                "a step landed inside the marker: {cursor} -> {target}"
            );
        }
    }

    #[test]
    fn one_forward_step_crosses_a_whole_marker() {
        let atoms = [MARKER_SPAN];
        // Before the marker, a step reaches the marker's far end...
        assert_eq!(find_word_forward_with_atoms(MARKER_TEXT, 4, &atoms), 24);
        // ...and a step from the space before it reaches it too.
        assert_eq!(find_word_forward_with_atoms(MARKER_TEXT, 3, &atoms), 24);
        // Ordinary text around it keeps its old behaviour.
        assert_eq!(find_word_forward(MARKER_TEXT, 0), 3);
        assert_eq!(find_word_forward_with_atoms(MARKER_TEXT, 24, &atoms), 28);
        for cursor in [3, 4] {
            let target = find_word_forward_with_atoms(MARKER_TEXT, cursor, &atoms);
            assert!(
                !(MARKER_SPAN.0 < target && target < MARKER_SPAN.1),
                "a step landed inside the marker: {cursor} -> {target}"
            );
        }
    }

    #[test]
    fn without_atoms_a_marker_is_walked_character_by_character() {
        // The negative control: the same text with no atom registered still
        // stops inside the marker (and the punctuation rule applies).
        let inside = find_word_backward(MARKER_TEXT, 24);
        assert!(
            MARKER_SPAN.0 < inside && inside < MARKER_SPAN.1,
            "expected a step inside the marker, got {inside}"
        );
    }

    #[test]
    fn atoms_are_only_merged_when_the_range_holds_them_whole() {
        // A marker straddling the segmented range cannot be one unit
        // (upstream's regex would not match it on that slice either).
        assert_eq!(
            find_word_backward_with_atoms(MARKER_TEXT, 10, &[MARKER_SPAN]),
            find_word_backward(MARKER_TEXT, 10)
        );
    }
}
