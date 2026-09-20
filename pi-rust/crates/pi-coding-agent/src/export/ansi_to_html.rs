//! ANSI escape → HTML converter for the session export.
//!
//! Verbatim port of
//! `packages/coding-agent/src/core/export-html/ansi-to-html.ts`: the same
//! 16-colour table, 256-colour cube/grayscale maths, true-colour sequences,
//! the `bold` / `dim` / `italic` / `underline` styles, reset (`0`) and the
//! empty SGR (`ESC[m`). Every SGR sequence closes the span opened by the
//! previous one and re-opens it with the accumulated style, so the output is
//! a flat sequence of inline-styled spans rather than nested elements —
//! exactly what upstream produces.
//!
//! The template consumes this at export time only: `tool-renderer.ts` renders
//! a tool through its TUI component, paints the [`StyledLine`]s with
//! [`render_lines_ansi`](crate::tools::render_lines_ansi), and hands the ANSI
//! text to [`ansi_lines_to_html`], whose lines land in
//! `template.js`'s `.ansi-rendered` blocks.

/// Standard ANSI palette (0-15), in upstream order.
const ANSI_COLORS: [&str; 16] = [
    "#000000", // 0: black
    "#800000", // 1: red
    "#008000", // 2: green
    "#808000", // 3: yellow
    "#000080", // 4: blue
    "#800080", // 5: magenta
    "#008080", // 6: cyan
    "#c0c0c0", // 7: white
    "#808080", // 8: bright black
    "#ff0000", // 9: bright red
    "#00ff00", // 10: bright green
    "#ffff00", // 11: bright yellow
    "#0000ff", // 12: bright blue
    "#ff00ff", // 13: bright magenta
    "#00ffff", // 14: bright cyan
    "#ffffff", // 15: bright white
];

/// Accumulated SGR state while scanning one line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TextStyle {
    fg: Option<String>,
    bg: Option<String>,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
}

impl TextStyle {
    /// True when the style carries anything the converter emits a span for
    /// (upstream `hasStyle`).
    fn has_style(&self) -> bool {
        self.fg.is_some()
            || self.bg.is_some()
            || self.bold
            || self.dim
            || self.italic
            || self.underline
    }

    /// Inline CSS for the accumulated style (upstream `styleToInlineCSS`).
    ///
    /// Declaration order is significant: `template.js` compares pre-rendered
    /// HTML strings, so the upstream order (`color`, `background-color`,
    /// `font-weight`, `opacity`, `font-style`, `text-decoration`) is kept.
    fn to_inline_css(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(fg) = &self.fg {
            parts.push(format!("color:{fg}"));
        }
        if let Some(bg) = &self.bg {
            parts.push(format!("background-color:{bg}"));
        }
        if self.bold {
            parts.push("font-weight:bold".to_string());
        }
        if self.dim {
            parts.push("opacity:0.6".to_string());
        }
        if self.italic {
            parts.push("font-style:italic".to_string());
        }
        if self.underline {
            parts.push("text-decoration:underline".to_string());
        }
        parts.join(";")
    }

    /// Apply one parameter list (upstream `applySgrCode`).
    ///
    /// `i` jumps forward for the multi-parameter `38` / `48` forms, so
    /// `38;5;N` and `38;2;R;G;B` are consumed as a unit.
    fn apply_sgr(&mut self, params: &[i64]) {
        let mut i = 0;
        while i < params.len() {
            let code = params[i];
            if code == 0 {
                *self = Self::default();
            } else if code == 1 {
                self.bold = true;
            } else if code == 2 {
                self.dim = true;
            } else if code == 3 {
                self.italic = true;
            } else if code == 4 {
                self.underline = true;
            } else if code == 22 {
                self.bold = false;
                self.dim = false;
            } else if code == 23 {
                self.italic = false;
            } else if code == 24 {
                self.underline = false;
            } else if (30..=37).contains(&code) {
                self.fg = Some(ANSI_COLORS[(code - 30) as usize].to_string());
            } else if code == 38 {
                if params.get(i + 1) == Some(&5) && params.len() > i + 2 {
                    // 256-colour: 38;5;N
                    self.fg = color256_to_hex(params[i + 2]);
                    i += 2;
                } else if params.get(i + 1) == Some(&2) && params.len() > i + 4 {
                    // True colour: 38;2;R;G;B
                    self.fg = Some(format!(
                        "rgb({},{},{})",
                        params[i + 2],
                        params[i + 3],
                        params[i + 4]
                    ));
                    i += 4;
                }
            } else if code == 39 {
                self.fg = None;
            } else if (40..=47).contains(&code) {
                self.bg = Some(ANSI_COLORS[(code - 40) as usize].to_string());
            } else if code == 48 {
                if params.get(i + 1) == Some(&5) && params.len() > i + 2 {
                    // 256-colour: 48;5;N
                    self.bg = color256_to_hex(params[i + 2]);
                    i += 2;
                } else if params.get(i + 1) == Some(&2) && params.len() > i + 4 {
                    // True colour: 48;2;R;G;B
                    self.bg = Some(format!(
                        "rgb({},{},{})",
                        params[i + 2],
                        params[i + 3],
                        params[i + 4]
                    ));
                    i += 4;
                }
            } else if code == 49 {
                self.bg = None;
            } else if (90..=97).contains(&code) {
                self.fg = Some(ANSI_COLORS[(code - 90 + 8) as usize].to_string());
            } else if (100..=107).contains(&code) {
                self.bg = Some(ANSI_COLORS[(code - 100 + 8) as usize].to_string());
            }
            // Unrecognised codes are ignored.
            i += 1;
        }
    }
}

/// 256-colour index → hex (upstream `color256ToHex`).
///
/// `None` mirrors upstream's `undefined` for a negative index, which cannot
/// occur here because SGR parameters are parsed from digits only.
fn color256_to_hex(index: i64) -> Option<String> {
    if index < 0 {
        return None;
    }
    if index < 16 {
        return Some(ANSI_COLORS[index as usize].to_string());
    }

    // Colour cube (16-231): 6x6x6 = 216 colours.
    if index < 232 {
        let cube_index = index - 16;
        let r = cube_index / 36;
        let g = (cube_index % 36) / 6;
        let b = cube_index % 6;
        let to_component = |n: i64| if n == 0 { 0 } else { 55 + n * 40 };
        let to_hex = |n: i64| format!("{:02x}", to_component(n));
        return Some(format!("#{}{}{}", to_hex(r), to_hex(g), to_hex(b)));
    }

    // Grayscale (232-255): 24 shades.
    let gray = 8 + (index - 232) * 10;
    Some(format!("#{gray:02x}{gray:02x}{gray:02x}"))
}

/// Escape the five HTML-significant characters (upstream `escapeHtml`).
///
/// `'` becomes `&#039;` (not `&#39;`), matching upstream byte for byte.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#039;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Parse one SGR parameter list: digits separated by `;`, empty/NaN → `0`
/// (upstream `paramStr.split(";").map((p) => parseInt(p, 10) || 0)`).
fn parse_params(param_str: &str) -> Vec<i64> {
    if param_str.is_empty() {
        return vec![0];
    }
    param_str
        .split(';')
        .map(|part| part.parse::<i64>().unwrap_or(0))
        .collect()
}

/// Convert ANSI-escaped text to HTML with inline styles
/// (upstream `ansiToHtml`).
pub fn ansi_to_html(text: &str) -> String {
    let mut style = TextStyle::default();
    let mut result = String::new();
    let mut last_index = 0;
    let mut in_span = false;

    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // `ESC [` followed by `[\d;]*` and terminated by `m`.
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                j += 1;
            }
            if bytes.get(j) == Some(&b'm') {
                let before = &text[last_index..i];
                if !before.is_empty() {
                    result.push_str(&escape_html(before));
                }

                let params = parse_params(&text[i + 2..j]);

                // Close the span opened by the previous sequence, if any.
                if in_span {
                    result.push_str("</span>");
                    in_span = false;
                }

                style.apply_sgr(&params);

                if style.has_style() {
                    result.push_str("<span style=\"");
                    result.push_str(&style.to_inline_css());
                    result.push_str("\">");
                    in_span = true;
                }

                last_index = j + 1;
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }

    let remaining = &text[last_index..];
    if !remaining.is_empty() {
        result.push_str(&escape_html(remaining));
    }
    if in_span {
        result.push_str("</span>");
    }

    result
}

/// Convert ANSI-escaped lines to HTML, one `<div class="ansi-line">` per line
/// (upstream `ansiLinesToHtml`).
///
/// A line that renders to nothing becomes `&nbsp;` so the block keeps its
/// height.
pub fn ansi_lines_to_html(lines: &[String]) -> String {
    lines
        .iter()
        .map(|line| {
            let html = ansi_to_html(line);
            if html.is_empty() {
                "<div class=\"ansi-line\">&nbsp;</div>".to_string()
            } else {
                format!("<div class=\"ansi-line\">{html}</div>")
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Remove SGR escape sequences, keeping all other bytes
/// (upstream `ANSI_ESCAPE_REGEX.replace(line, "")`).
///
/// Used to decide whether a rendered line is visually blank.
pub fn strip_ansi_sgr(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                j += 1;
            }
            if bytes.get(j) == Some(&b'm') {
                i = j + 1;
                continue;
            }
        }
        let ch = text[i..].chars().next().expect("in bounds");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Escape sequences are spelled out so the vectors mirror the upstream
    /// test inputs (which the reference implementation produced verbatim).
    const ESC: &str = "\u{1b}";

    #[test]
    fn plain_text_is_escaped() {
        assert_eq!(ansi_to_html("hello world"), "hello world");
        assert_eq!(
            ansi_to_html("<a href=\"x\">& 'y'</a>"),
            "&lt;a href=&quot;x&quot;&gt;&amp; &#039;y&#039;&lt;/a&gt;"
        );
    }

    #[test]
    fn standard_foreground_and_reset() {
        let text = format!("{ESC}[31mred{ESC}[0m");
        assert_eq!(
            ansi_to_html(&text),
            "<span style=\"color:#800000\">red</span>"
        );
    }

    #[test]
    fn consecutive_sequences_close_and_reopen_spans() {
        // Upstream emits an (empty) span for the first sequence and re-opens
        // the accumulated style with the second one.
        let text = format!("{ESC}[1m{ESC}[31mX{ESC}[0m");
        assert_eq!(
            ansi_to_html(&text),
            "<span style=\"font-weight:bold\"></span><span style=\"color:#800000;font-weight:bold\">X</span>"
        );
    }

    #[test]
    fn consecutive_sgr_parameters_apply_together() {
        let text = format!("{ESC}[1;31;4mX{ESC}[0m");
        assert_eq!(
            ansi_to_html(&text),
            "<span style=\"color:#800000;font-weight:bold;text-decoration:underline\">X</span>"
        );
    }

    #[test]
    fn unclosed_style_is_closed_at_end_of_line() {
        let text = format!("{ESC}[32mgreen");
        assert_eq!(
            ansi_to_html(&text),
            "<span style=\"color:#008000\">green</span>"
        );
    }

    #[test]
    fn truecolor_and_256_color_background_combine() {
        let text = format!("{ESC}[38;2;1;2;3;48;5;9mX{ESC}[0m");
        assert_eq!(
            ansi_to_html(&text),
            "<span style=\"color:rgb(1,2,3);background-color:#ff0000\">X</span>"
        );
    }

    #[test]
    fn color_cube_and_grayscale_indexes() {
        let text = format!("{ESC}[38;5;196;48;5;240mX{ESC}[0m");
        assert_eq!(
            ansi_to_html(&text),
            "<span style=\"color:#ff0000;background-color:#585858\">X</span>"
        );
        // Out-of-range 256-colour indexes keep upstream's unpadded hex shape.
        let text = format!("{ESC}[38;5;300mX");
        assert_eq!(
            ansi_to_html(&text),
            "<span style=\"color:#2b02b02b0\">X</span>"
        );
    }

    #[test]
    fn dim_italic_and_their_resets() {
        let text = format!("{ESC}[1;2;3;4mX{ESC}[22;23;24mY");
        assert_eq!(
            ansi_to_html(&text),
            "<span style=\"font-weight:bold;opacity:0.6;font-style:italic;text-decoration:underline\">X</span>Y"
        );
    }

    #[test]
    fn default_fg_and_bg_resets() {
        let text = format!("{ESC}[31;41mX{ESC}[39;49mY");
        assert_eq!(
            ansi_to_html(&text),
            "<span style=\"color:#800000;background-color:#800000\">X</span>Y"
        );
    }

    #[test]
    fn bright_colors_use_the_upper_palette_half() {
        let text = format!("{ESC}[91;101mX{ESC}[0m");
        assert_eq!(
            ansi_to_html(&text),
            "<span style=\"color:#ff0000;background-color:#ff0000\">X</span>"
        );
    }

    #[test]
    fn empty_sgr_resets_without_a_span() {
        let text = format!("{ESC}[mX");
        assert_eq!(ansi_to_html(&text), "X");
    }

    #[test]
    fn lines_are_wrapped_and_blank_lines_get_nbsp() {
        let lines = vec!["a".to_string(), String::new(), format!("{ESC}[31mb")];
        assert_eq!(
            ansi_lines_to_html(&lines),
            "<div class=\"ansi-line\">a</div><div class=\"ansi-line\">&nbsp;</div><div class=\"ansi-line\"><span style=\"color:#800000\">b</span></div>"
        );
    }

    #[test]
    fn strip_removes_only_sgr_sequences() {
        let text = format!("{ESC}[31;1mred{ESC}[0m {ESC}[38;2;1;2;3mblue");
        assert_eq!(strip_ansi_sgr(&text), "red blue");
        // A lone ESC that is not an SGR sequence stays.
        assert_eq!(strip_ansi_sgr("\u{1b}[2Jx"), "\u{1b}[2Jx");
        assert_eq!(strip_ansi_sgr("会话"), "会话");
    }
}
