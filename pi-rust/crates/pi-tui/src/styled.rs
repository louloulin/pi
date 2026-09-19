//! Styled spans — the bridge between a component's theme slots and the
//! [`App`](crate::App) buffer writer.
//!
//! A component lays its output out as [`StyledLine`]s: each [`StyledSpan`]
//! carries a run of text plus the [`SpanStyle`] slot that colours it. The App
//! resolves that slot into a [`ratatui::style::Style`] and writes it straight
//! into the render buffer, so themed cells never carry ANSI escapes in their
//! text. The legacy `*_themed` string renderers build the same spans and
//! convert them to ANSI for callers that compare strings.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};

use crate::theme::{hex_to_256, hex_to_rgb, ColorMode, ColorValue, Theme, ThemeBg, ThemeColor};

/// The theme slot(s) a [`StyledSpan`] renders with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpanStyle {
    /// Foreground slot, if any.
    pub fg: Option<ThemeColor>,
    /// Background slot, if any.
    pub bg: Option<ThemeBg>,
    /// Whether the run is rendered bold.
    pub bold: bool,
    /// Whether the run is rendered italic.
    pub italic: bool,
    /// Whether the run is rendered underlined.
    pub underline: bool,
    /// Whether the run is rendered struck through.
    pub strikethrough: bool,
}

impl SpanStyle {
    /// An unstyled run.
    pub const PLAIN: Self = Self {
        fg: None,
        bg: None,
        bold: false,
        italic: false,
        underline: false,
        strikethrough: false,
    };

    /// A run with only a foreground slot.
    pub fn fg(color: ThemeColor) -> Self {
        Self {
            fg: Some(color),
            ..Self::PLAIN
        }
    }

    /// A run with a foreground and a background slot.
    pub fn fg_bg(fg: ThemeColor, bg: ThemeBg) -> Self {
        Self {
            fg: Some(fg),
            bg: Some(bg),
            ..Self::PLAIN
        }
    }

    /// The slots with the bold modifier applied.
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// The slots with the italic modifier applied.
    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    /// The slots with the underline modifier applied.
    pub fn underline(mut self) -> Self {
        self.underline = true;
        self
    }

    /// The slots with the strikethrough modifier applied.
    pub fn strikethrough(mut self) -> Self {
        self.strikethrough = true;
        self
    }

    /// Render `text` as an ANSI string for this slot.
    ///
    /// A plain theme ([`ColorMode::None`]) returns `text` unchanged. The
    /// wrapping order matches upstream (`theme.fg("accent", theme.bold(text))`,
    /// `theme.bg("selectedBg", theme.fg("accent", text))`): the text
    /// decorations are innermost (each uses its own reset code, so their order
    /// is not observable), then the foreground, then the background.
    pub fn ansi(self, theme: &Theme, text: &str) -> String {
        if theme.is_plain() {
            return text.to_string();
        }
        let mut out = text.to_string();
        if self.bold {
            out = theme.bold(&out);
        }
        if self.italic {
            out = theme.italic(&out);
        }
        if self.underline {
            out = theme.underline(&out);
        }
        if self.strikethrough {
            out = theme.strikethrough(&out);
        }
        if let Some(fg) = self.fg {
            out = theme.fg(fg, &out);
        }
        if let Some(bg) = self.bg {
            out = theme.bg(bg, &out);
        }
        out
    }

    /// Resolve this slot into a [`ratatui::style::Style`] for the App's buffer
    /// path. A plain theme resolves to the default (unstyled) style.
    pub fn to_style(self, theme: &Theme) -> Style {
        if theme.is_plain() {
            return Style::default();
        }
        let mut style = Style::default();
        if let Some(color) = self.fg.and_then(|slot| fg_color(theme, slot)) {
            style = style.fg(color);
        }
        if let Some(color) = self.bg.and_then(|slot| bg_color(theme, slot)) {
            style = style.bg(color);
        }
        if self.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self.underline {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        if self.strikethrough {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        style
    }
}

/// One run of text with its [`SpanStyle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledSpan {
    /// The run's text.
    pub text: String,
    /// The run's theme slot.
    pub style: SpanStyle,
}

impl StyledSpan {
    /// Build a span from its text and slot.
    pub fn new(text: impl Into<String>, style: SpanStyle) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}

/// A rendered line: one or more [`StyledSpan`]s in visual order.
pub type StyledLine = Vec<StyledSpan>;

/// Concatenate a line's text, dropping all styling.
pub fn plain_text(line: &[StyledSpan]) -> String {
    line.iter().map(|span| span.text.as_str()).collect()
}

/// Render a line as an ANSI string using each span's slot.
pub fn themed_text(line: &[StyledSpan], theme: &Theme) -> String {
    line.iter()
        .map(|span| span.style.ansi(theme, &span.text))
        .collect()
}

/// Write a styled line into `buf` at row `y`, starting at column `x0` and
/// clipped to `max_width` columns.
///
/// Every cell gets both the character and the span's resolved style, so the
/// buffer is themed without any ANSI escape ever entering the cell text.
pub fn write_styled_line(
    buf: &mut Buffer,
    x0: u16,
    y: u16,
    max_width: u16,
    line: &[StyledSpan],
    theme: &Theme,
) {
    let mut col = 0u16;
    for span in line {
        let style = span.style.to_style(theme);
        for ch in span.text.chars() {
            if col >= max_width {
                return;
            }
            if let Some(cell) = buf.cell_mut((x0 + col, y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
            col += 1;
        }
    }
}

/// Resolve a foreground slot to a backend colour.
fn fg_color(theme: &Theme, slot: ThemeColor) -> Option<Color> {
    value_to_color(theme.fg_value(slot), theme.color_mode())
}

/// Resolve a background slot to a backend colour.
fn bg_color(theme: &Theme, slot: ThemeBg) -> Option<Color> {
    value_to_color(theme.bg_value(slot), theme.color_mode())
}

/// Translate a resolved [`ColorValue`] into the matching `ratatui` colour for
/// the active colour mode. `None` means "leave the terminal default".
fn value_to_color(value: Option<&ColorValue>, mode: ColorMode) -> Option<Color> {
    if matches!(mode, ColorMode::None) {
        return None;
    }
    match value? {
        ColorValue::Reset => Some(Color::Reset),
        ColorValue::Index(index) => Some(Color::Indexed(*index)),
        ColorValue::Hex(hex) => match mode {
            ColorMode::TrueColor => hex_to_rgb(hex).ok().map(|(r, g, b)| Color::Rgb(r, g, b)),
            ColorMode::Ansi256 => hex_to_256(hex).ok().map(Color::Indexed),
            ColorMode::None => None,
        },
        ColorValue::Var(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::builtin_theme;

    #[test]
    fn ansi_wrapping_order_matches_upstream() {
        let theme = builtin_theme("dark", ColorMode::TrueColor).expect("dark theme");
        let span = SpanStyle::fg_bg(ThemeColor::Accent, ThemeBg::SelectedBg);
        assert_eq!(
            span.ansi(&theme, "x"),
            "\u{1b}[48;2;58;58;74m\u{1b}[38;2;138;190;183mx\u{1b}[39m\u{1b}[49m"
        );

        let title = SpanStyle::fg(ThemeColor::Accent).bold();
        assert_eq!(
            title.ansi(&theme, "T"),
            "\u{1b}[38;2;138;190;183m\u{1b}[1mT\u{1b}[22m\u{1b}[39m"
        );
    }

    #[test]
    fn styles_resolve_to_rgb_for_truecolor() {
        let theme = builtin_theme("dark", ColorMode::TrueColor).expect("dark theme");
        let style = SpanStyle::fg_bg(ThemeColor::Accent, ThemeBg::SelectedBg).to_style(&theme);
        assert_eq!(style.fg, Some(Color::Rgb(138, 190, 183)));
        assert_eq!(style.bg, Some(Color::Rgb(58, 58, 74)));
        assert_eq!(style.add_modifier, Modifier::empty());
    }

    #[test]
    fn a_plain_theme_yields_the_default_style() {
        let theme = builtin_theme("dark", ColorMode::None).expect("plain theme");
        let style = SpanStyle::fg(ThemeColor::Accent).bold().to_style(&theme);
        assert_eq!(style, Style::default());
    }
}
