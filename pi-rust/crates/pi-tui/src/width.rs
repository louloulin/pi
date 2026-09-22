//! Terminal column measurement — the crate's single width rule.
//!
//! Upstream measures every laid-out string with `visibleWidth`
//! (`packages/tui/src/utils.ts`): an East Asian Wide / Fullwidth code point or
//! an emoji occupies **two** terminal columns, a combining mark or a zero-width
//! joiner occupies **none**, a tab is normalised to three, and there is no
//! other exception. That rule is not cosmetic — it is what decides where a
//! paragraph wraps, how wide a select-list cell is, and how much padding a
//! composer row needs.
//!
//! Before this module the Rust port measured *characters*: every glyph was one
//! column (`message::display_width`, `markdown::styled_width`,
//! `visual_text::display_width`, `selector::display_width`, …). For ASCII that
//! is the same number, which is why the port looked correct in every test and
//! screenshot taken so far. For CJK — the very content a Chinese-language
//! session is made of — it is wrong by a factor of two: a wrapped row is then
//! twice as wide as the terminal, the terminal hard-wraps it *again*, and every
//! row below the wrap is drawn on the wrong line. This module is the one place
//! that rule lives now, so the transcript, the composer, the selectors and the
//! table layout cannot drift apart again.
//!
//! # Relationship to upstream's `visibleWidth`
//!
//! Implemented here:
//!
//! * `\t` → 3 columns (upstream normalises tabs before measuring).
//! * `\r` and every other control code → 0 columns.
//! * East Asian Wide / Fullwidth → 2 columns, East Asian Ambiguous → 1
//!   (the `unicode-width` default, matching upstream's `eastAsianWidth`
//!   default of 1 for Ambiguous).
//! * Emoji presentation → 2 columns (delegated to `unicode-width`, which
//!   implements UTS #51 presentation selection).
//! * Combining marks / zero-width joiners / variation selectors → 0 columns.
//!
//! Deliberately **not** implemented, and why:
//!
//! * **Grapheme clustering.** Measurement is per `char`, matching the editor's
//!   per-`char` cursor model (`visual_text`'s offset-accurate rows,
//!   `word_navigation`'s UAX #29 boundaries). A ZWJ emoji sequence is therefore
//!   measured as the sum of its parts (usually wider than the single cluster a
//!   terminal draws). Clustering it would produce rows the caret cannot address.
//! * **ANSI / OSC stripping.** Callers whose text may carry escape sequences
//!   (`hyperlink`, `image`) strip them first and then call this module, which
//!   is the same split upstream makes (`visibleWidth` strips, `graphemeWidth`
//!   does not).
//! * **A width cache.** Upstream memoises `visibleWidth` because it re-measures
//!   the whole frame every keystroke. Layout here already measures once per
//!   render into pre-wrapped `String`s, so a cache would be allocation without
//!   a measured win; the pure function stays trivially testable instead.

use unicode_width::UnicodeWidthChar;

/// Columns one character occupies.
///
/// This is the primitive the wrap loops call per character while they
/// accumulate a row.
pub fn char_columns(ch: char) -> usize {
    match ch {
        // Upstream normalises tabs to three spaces before measuring, so a tab
        // is three columns everywhere downstream of that normalisation.
        '\t' => 3,
        // A CRLF paste keeps its `\r` in the buffer but must not consume a
        // column (it is invisible next to the `\n` that follows it).
        '\r' => 0,
        c if c.is_control() => 0,
        c => UnicodeWidthChar::width(c).unwrap_or(0),
    }
}

/// Columns `text` occupies on screen.
///
/// `columns("你好") == 4`, `columns("😀") == 2`, `columns("a\tb") == 5`.
pub fn columns(text: &str) -> usize {
    text.chars().map(char_columns).sum()
}

/// The longest prefix of `text` whose column width does not exceed `max`.
///
/// Returns the prefix and the columns it occupies, so a caller that also
/// needs to pad the remainder does not measure twice.
pub fn prefix_columns(text: &str, max: usize) -> (&str, usize) {
    let mut used = 0usize;
    for (offset, ch) in text.char_indices() {
        let next = used + char_columns(ch);
        if next > max {
            return (&text[..offset], used);
        }
        used = next;
    }
    (text, used)
}

/// `text` truncated to at most `max` columns, dropping the tail.
///
/// A wide glyph that would straddle `max` is dropped rather than half-drawn:
/// a terminal cannot render half a glyph, and keeping it would push the row
/// past its region.
pub fn truncate_columns(text: &str, max: usize) -> &str {
    prefix_columns(text, max).0
}

/// Whether `ch` allows a line break on either side of it, by the CJK rule.
///
/// Upstream's second wrap-opportunity rule
/// (`packages/tui/src/components/editor.ts:194-203`,
/// `cjkBreakRegex` in `packages/tui/src/utils.ts:54`) allows a break between
/// any two adjacent characters when **either** side is written in a script
/// that has no inter-word spaces:
///
/// ```text
/// /[\p{Script_Extensions=Han}\p{Script_Extensions=Hiragana}\p{Script_Extensions=Katakana}
///   \p{Script_Extensions=Hangul}\p{Script_Extensions=Bopomofo}]/u
/// ```
///
/// Rust has no `Script_Extensions` table in-tree, so this is the same rule
/// expressed over the script blocks those properties resolve to. It is an
/// over-approximation at the edges (a Han character that also has a Latin
/// `Script_Extensions` entry is still a break opportunity, which is what
/// upstream wants) and deliberately **wide**: the cost of allowing one break
/// too many is a shorter row, while the cost of missing one is a paragraph of
/// CJK that cannot wrap at all until the force-break fires.
pub fn is_cjk_break(ch: char) -> bool {
    matches!(ch as u32,
        // Han (CJK Unified Ideographs and the extension blocks terminals
        // actually render).
        0x2E80..=0x2EFF   // CJK Radicals Supplement
        | 0x2F00..=0x2FDF // Kangxi Radicals
        | 0x3005          // IDEOGRAPHIC ITERATION MARK
        | 0x3007          // IDEOGRAPHIC NUMBER ZERO
        | 0x3021..=0x3029 // Hangzhou numerals
        | 0x3038..=0x303B // Hangzhou numerals, continued
        | 0x3400..=0x4DBF // CJK Extension A
        | 0x4E00..=0x9FFF // CJK Unified Ideographs
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
        | 0x20000..=0x2FA1F // Extensions B..F and compatibility supplement
        // Hiragana / Katakana, including the Katakana Phonetic Extensions.
        | 0x3040..=0x309F
        | 0x30A0..=0x30FF
        | 0x31F0..=0x31FF
        // Hangul (Jamo, compatibility Jamo, syllables).
        | 0x1100..=0x11FF
        | 0x3130..=0x318F
        | 0xAC00..=0xD7AF
        // Bopomofo, including the extended block.
        | 0x3100..=0x312F
        | 0x31A0..=0x31BF
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_one_column_per_character() {
        assert_eq!(columns("hello"), 5);
        assert_eq!(columns(""), 0);
    }

    #[test]
    fn cjk_is_two_columns_per_character() {
        assert_eq!(columns("你好"), 4);
        assert_eq!(columns("世界你好"), 8);
        assert_eq!(columns("a中b"), 4);
    }

    #[test]
    fn fullwidth_and_halfwidth_forms_measure_by_the_unicode_table() {
        // U+FF21 FULLWIDTH LATIN CAPITAL A is Wide; U+FF61 is Halfwidth.
        assert_eq!(columns("\u{ff21}"), 2);
        assert_eq!(columns("\u{ff61}"), 1);
    }

    #[test]
    fn east_asian_ambiguous_stays_narrow() {
        // Upstream's `eastAsianWidth` returns 1 for Ambiguous (it is not the
        // CJK variant), so `unicode-width`'s default `width()` — not
        // `width_cjk()` — is the matching call.
        assert_eq!(columns("±"), 1);
        assert_eq!(columns("→"), 1);
    }

    #[test]
    fn emoji_presentation_is_two_columns() {
        assert_eq!(columns("😀"), 2);
        assert_eq!(columns("🎉"), 2);
    }

    #[test]
    fn combining_marks_and_joiners_are_zero_columns() {
        // "e" + COMBINING ACUTE ACCENT.
        assert_eq!(columns("e\u{301}"), 1);
        // ZERO WIDTH JOINER, and a variation selector.
        assert_eq!(columns("\u{200d}"), 0);
        assert_eq!(columns("\u{fe0f}"), 0);
    }

    #[test]
    fn tabs_are_three_columns_and_cr_is_none() {
        assert_eq!(columns("a\tb"), 5);
        assert_eq!(columns("a\rb"), 2);
        assert_eq!(columns("a\nb"), 2, "the newline itself owns no column");
        assert_eq!(char_columns('\t'), 3);
        assert_eq!(char_columns('\r'), 0);
    }

    #[test]
    fn prefix_columns_never_straddles_a_wide_glyph() {
        assert_eq!(prefix_columns("你好", 3), ("你", 2));
        assert_eq!(prefix_columns("你好", 4), ("你好", 4));
        assert_eq!(prefix_columns("abc", 99), ("abc", 3));
        assert_eq!(prefix_columns("你好", 0), ("", 0));
        assert_eq!(prefix_columns("a中", 1), ("a", 1));
    }

    #[test]
    fn truncate_columns_reports_the_prefix() {
        assert_eq!(truncate_columns("你好世界", 5), "你好");
        assert_eq!(truncate_columns("你好世界", 4), "你好");
        assert_eq!(truncate_columns("ascii", 3), "asc");
    }

    #[test]
    fn cjk_break_covers_the_scripts_upstream_names() {
        for ch in ['你', '好', 'あ', 'ア', '한', 'ㄅ', '々', 'あ'] {
            assert!(is_cjk_break(ch), "{ch:?} must allow a break");
        }
        // ASCII letters and punctuation keep upstream's first rule only.
        for ch in ['a', 'Z', '.', '-', ' ', 'é'] {
            assert!(!is_cjk_break(ch), "{ch:?} must not allow a CJK break");
        }
    }
}
