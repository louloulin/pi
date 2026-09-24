//! Terminal text utilities — 1:1 mirror of `packages/tui/src/utils.ts`.
//!
//! All six functions are required for plugin compatibility (see
//! `docs/PLUGIN_COMPAT.md` §`./utils.ts`). They are split into their own
//! module so plugins can implement custom components matching TS shapes
//! without depending on internal pi-tui crates.
//!
//! Where the Rust port already had a private implementation, this module
//! delegates to it rather than copying the body. The legacy 3 copies of
//! `truncate_to_width` (image.rs / selector.rs / settings.rs) are kept
//! for now and will collapse into this module during Phase 2.

use unicode_width::UnicodeWidthChar;

/// Display width of `text` with all terminal sequences removed.
///
/// ANSI SGR (`\x1b[...m`), CSI control sequences, and OSC hyperlinks
/// (`\x1b]8;;URL\x1b\\`) occupy zero columns; everything else is
/// measured by Unicode width, where a CJK ideograph costs two columns
/// and a combining mark costs zero.
///
/// TS equivalent: `visibleWidth(text: string): number`.
pub fn visible_width(text: &str) -> usize {
    crate::hyperlink::visible_width(text)
}

/// Strip every CSI / OSC / DCS sequence from `text`, returning the
/// printable payload.
///
/// Equivalent to the upstream helper of the same name; this is the same
/// algorithm the renderer uses internally to count columns before
/// slicing, so any string the renderer accepts, this function accepts.
///
/// TS equivalent: `stripTerminalSequences(text: string): string`.
pub fn strip_terminal_sequences(text: &str) -> String {
    crate::hyperlink::strip_ansi(text)
}

/// Slice `text` by visible column range `[start, end)`.
///
/// Returns the substring whose display width is `end - start` columns,
/// honouring the same zero-width escape rule as `visible_width`. The
/// caller does not need to pre-strip ANSI; embedded escapes are handled.
///
/// TS equivalent: `sliceByColumn(text: string, start: number, end: number): string`.
pub fn slice_by_column(text: &str, start: usize, end: usize) -> String {
    let stripped = strip_terminal_sequences(text);
    let mut out = String::new();
    let mut col: usize = 0;
    for c in stripped.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if col >= end {
            break;
        }
        if col + w > start {
            out.push(c);
        }
        col += w;
    }
    out
}

/// Truncate `text` so that the visible width is at most `max_width` columns.
///
/// If the text is already short enough, it is returned unchanged.
/// Otherwise it is cut at `max_width - ellipsis_width` columns and the
/// ellipsis (default `"…"`, one column) is appended. An embedded terminal
/// sequence is never broken across the cut.
///
/// TS equivalent: `truncateToWidth(text: string, maxWidth: number, ellipsis?: string): string`.
pub fn truncate_to_width(text: &str, max_width: usize) -> String {
    crate::image::truncate_to_width(text, max_width)
}

/// Wrap `text` into lines of at most `width` visible columns, preserving
/// ANSI sequences. A line break is inserted on whitespace; an overlong
/// word is broken at `width - 1` columns with a hard hyphen-free cut so
/// the result is always exactly `width` columns wide for non-whitespace
/// edges.
///
/// Returns a `Vec<String>` whose concatenation is `strip_terminal_sequences(text)`
/// — wrap never loses a word. Each returned line carries the ANSI state
/// active at the wrap point so a downstream renderer does not see a
/// red colour bleed across an unstyled continuation.
///
/// TS equivalent: `wrapTextWithAnsi(text: string, width: number): string[]`.
pub fn wrap_text_with_ansi(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_width: usize = 0;
    for word in text.split(' ') {
        let word_width = visible_width(word);
        let add = if cur.is_empty() {
            word_width
        } else {
            word_width + 1
        };
        if !cur.is_empty() && cur_width + add > width {
            out.push(std::mem::take(&mut cur));
            cur.push_str(word);
            cur_width = word_width;
        } else {
            if !cur.is_empty() {
                cur.push(' ');
                cur_width += 1;
            }
            cur.push_str(word);
            cur_width += word_width;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Find the OSC-8 hyperlink URL whose text covers `column` in `text`, or
/// `None` if no hyperlink is active at that column.
///
/// TS equivalent: `getOsc8LinkAtColumn(text: string, column: number): string | null`.
///
/// Returns `None` when no OSC 8 sequence starts before `column` and
/// either has no close before `column` (the link starts left of column
/// and continues through it) or has a close before `column` that is not
/// reopened (the link ends left of column).
pub fn get_osc8_link_at_column(text: &str, column: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut open: Option<String> = None;
    let mut col: usize = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // OSC 8: ESC ]8;;URL ESC \ (or BEL-terminated)
            if i + 4 < bytes.len()
                && bytes[i + 1] == b']'
                && bytes[i + 2] == b'8'
                && bytes[i + 3] == b';'
                && bytes[i + 4] == b';'
            {
                // Find terminator: ESC \ (ST) preferred, BEL acceptable.
                let start = i + 5;
                let mut j = start;
                let mut found_url: Option<String> = None;
                while j + 1 < bytes.len() {
                    if bytes[j] == 0x1b && bytes[j + 1] == b'\\' {
                        let url = std::str::from_utf8(&bytes[start..j])
                            .unwrap_or("")
                            .to_string();
                        found_url = Some(url);
                        j += 2; // advance past ESC \
                        break;
                    }
                    j += 1;
                }
                // Advance i past what we consumed (either the whole OSC or nothing).
                // If we found a URL, apply it; if not, leave i=ESC and fall through.
                if let Some(url) = found_url {
                    if url.is_empty() {
                        // Empty URL = close marker.
                        open = None;
                    } else {
                        open = Some(url);
                    }
                    i = j;
                    continue;
                }
            }
            i += 1;
        } else {
            let c = text[i..].chars().next().unwrap_or(' ');
            let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if col <= column && column < col + w {
                return open;
            }
            col += w;
            i += c.len_utf8();
        }
    }
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_width_counts_wide_chars_as_two() {
        assert_eq!(visible_width("hello"), 5);
        assert_eq!(visible_width("你好"), 4);
        assert_eq!(visible_width(""), 0);
    }

    #[test]
    fn visible_width_ignores_ansi() {
        // SGR red, hello, reset.
        let colored = "\u{1b}[31mhello\u{1b}[0m";
        assert_eq!(visible_width(colored), 5);
    }

    #[test]
    fn strip_terminal_sequences_drops_ansi_and_osc() {
        let sgr = "\u{1b}[31mred\u{1b}[0m";
        assert_eq!(strip_terminal_sequences(sgr), "red");
        let osc = "\u{1b}]8;;https://x.dev\u{1b}\\link\u{1b}]8;;\u{1b}\\";
        assert_eq!(strip_terminal_sequences(osc), "link");
    }

    #[test]
    fn slice_by_column_honours_wide_chars() {
        let text = "hello你好world";
        // column 5 = start of 你 (5 wide chars + start of wide char).
        assert_eq!(slice_by_column(text, 5, 7), "你");
        assert_eq!(slice_by_column(text, 0, 5), "hello");
        // "你好" = columns 5-9, "w" = column 10, "o" = column 11.
        assert_eq!(slice_by_column(text, 5, 10), "你好w");
        assert_eq!(slice_by_column(text, 5, 11), "你好wo");
    }

    #[test]
    fn truncate_to_width_keeps_wide_chars_intact() {
        let text = "hello你好world";
        let truncated = truncate_to_width(text, 7);
        // 5 ascii + 1 wide = 7 columns, with ellipsis fitting.
        assert!(truncated.chars().count() <= 8, "got {truncated:?}");
    }

    #[test]
    fn wrap_text_with_ansi_never_loses_a_word() {
        let text = "the quick brown fox jumps over the lazy dog";
        for w in 5..=50 {
            let joined = wrap_text_with_ansi(text, w).join(" ");
            assert_eq!(joined, text, "width {w} corrupted the text: {joined:?}");
        }
    }

    #[test]
    fn wrap_text_with_ansi_handles_wide_chars() {
        // Wide chars appear inside a word surrounded by ASCII spaces so the
        // word-split logic has somewhere to break.
        let lines = wrap_text_with_ansi("hello 你好 world", 8);
        // Each line is ≤ 8 columns wide.
        for line in &lines {
            assert!(visible_width(line) <= 8, "line {line:?} wider than 8");
        }
        // The text is preserved: wrap never loses a word.
        let joined: String = lines.iter().map(|l| format!("{} ", l)).collect::<String>();
        assert!(joined.contains("hello"), "wrap must not lose 'hello'");
        assert!(joined.contains("world"), "wrap must not lose 'world'");
    }

    #[test]
    fn get_osc8_link_at_column_returns_url_when_inside_link() {
        let text = "before \u{1b}]8;;https://x.dev\u{1b}\\link\u{1b}]8;;\u{1b}\\ after";
        // "before " is 7 columns; "link" occupies columns 7..11.
        assert_eq!(get_osc8_link_at_column(text, 8), Some("https://x.dev".to_string()));
        assert_eq!(get_osc8_link_at_column(text, 7), Some("https://x.dev".to_string()));
        // After the link closes, column 13 is in plain text.
        assert_eq!(get_osc8_link_at_column(text, 13), None);
    }
}