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

/// Find the cursor position after moving one word backward from
/// `cursor` in `text`.
///
/// Trailing whitespace is skipped, then the cursor stops at the next
/// word / punctuation boundary. Pure function — it does not mutate the
/// buffer. Returns `cursor` unchanged when `cursor` is `0` or past the
/// end of `text`; the byte offset is always clamped into `text`.
pub fn find_word_backward(text: &str, cursor: usize) -> usize {
    let cursor = cursor.min(text.len());
    if cursor == 0 {
        return 0;
    }
    let mut segments: Vec<&str> = text[..cursor].split_word_bounds().collect();
    let mut new_cursor = cursor;

    // Skip trailing whitespace.
    while let Some(last) = segments.last() {
        if !is_whitespace(last) {
            break;
        }
        new_cursor -= last.len();
        segments.pop();
    }

    let Some(&last) = segments.last() else {
        return new_cursor;
    };

    if is_word_like(last) {
        // Skip inside one word-like segment, preserving ASCII
        // punctuation boundaries: stop just after the last punctuation
        // character, or move the whole segment when there is none.
        match last.char_indices().rfind(|(_, c)| is_punctuation_char(*c)) {
            Some((idx, c)) => new_cursor -= last.len() - (idx + c.len_utf8()),
            None => new_cursor -= last.len(),
        }
    } else {
        // Skip the whole run of non-word, non-whitespace (punctuation)
        // segments.
        while let Some(&segment) = segments.last() {
            if is_whitespace(segment) || is_word_like(segment) {
                break;
            }
            new_cursor -= segment.len();
            segments.pop();
        }
    }

    new_cursor
}

/// Find the cursor position after moving one word forward from `cursor`
/// in `text`.
///
/// Leading whitespace is skipped, then the cursor stops at the next
/// word / punctuation boundary. Pure function — it does not mutate the
/// buffer. Returns `text.len()` when `cursor` is at or past the end.
pub fn find_word_forward(text: &str, cursor: usize) -> usize {
    let cursor = cursor.min(text.len());
    if cursor >= text.len() {
        return text.len();
    }
    let mut new_cursor = cursor;
    let mut segments = text[cursor..].split_word_bounds().peekable();

    // Skip leading whitespace.
    while let Some(segment) = segments.peek() {
        if !is_whitespace(segment) {
            break;
        }
        new_cursor += segment.len();
        segments.next();
    }

    let Some(&next) = segments.peek() else {
        return new_cursor;
    };

    if is_word_like(next) {
        // Preserve ASCII punctuation boundaries: stop at the first
        // punctuation character inside the segment, or take the whole
        // segment.
        match next.char_indices().find(|(_, c)| is_punctuation_char(*c)) {
            Some((idx, _)) => new_cursor += idx,
            None => new_cursor += next.len(),
        }
    } else {
        // Skip the whole run of non-word, non-whitespace (punctuation)
        // segments.
        while let Some(&segment) = segments.peek() {
            if is_whitespace(segment) || is_word_like(segment) {
                break;
            }
            new_cursor += segment.len();
            segments.next();
        }
    }

    new_cursor
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
}
