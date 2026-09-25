//! `Text` — a simple text component.
//!
//! Mirrors upstream `Text` (`packages/tui/src/components/text.ts`).

use crate::component::Component;
use crate::utils::styled::{SpanStyle, StyledLine, StyledSpan};

/// Upstream `Text` — a block of text rendered with the given span
/// style. Each line is a single styled span.
#[derive(Debug, Clone)]
pub struct Text {
    lines: Vec<String>,
    style: SpanStyle,
}

impl Text {
    /// Build a `Text` from a single line.
    pub fn new<S: Into<String>>(line: S) -> Self {
        Self {
            lines: vec![line.into()],
            style: SpanStyle::PLAIN,
        }
    }

    /// Build a `Text` from a list of plain lines.
    pub fn from_lines<I, S>(lines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            lines: lines.into_iter().map(Into::into).collect(),
            style: SpanStyle::PLAIN,
        }
    }

    /// Build a `Text` whose every span carries `style`.
    pub fn styled<S: Into<String>>(lines: impl IntoIterator<Item = S>, style: SpanStyle) -> Self {
        Self {
            lines: lines.into_iter().map(Into::into).collect(),
            style,
        }
    }

    /// Build a `Text` from a single styled line.
    pub fn styled_single<S: Into<String>>(line: S, style: SpanStyle) -> Self {
        Self {
            lines: vec![line.into()],
            style,
        }
    }

    /// Replace the content.
    pub fn set_lines<S: Into<String>>(&mut self, lines: impl IntoIterator<Item = S>) {
        self.lines = lines.into_iter().map(Into::into).collect();
    }

    /// Borrow the lines.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Current span style.
    pub fn style(&self) -> SpanStyle {
        self.style
    }
}

impl Component for Text {
    fn render(&self, _width: u16) -> Vec<StyledLine> {
        self.lines
            .iter()
            .map(|line| vec![StyledSpan::new(line.clone(), self.style)])
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeColor;

    #[test]
    fn text_renders_one_line_per_input() {
        let t = Text::from_lines(["one", "two"]);
        let lines = t.render(40);
        assert_eq!(lines.len(), 2);
        assert_eq!(crate::utils::styled::plain_text(&lines[0]), "one");
    }

    #[test]
    fn text_styled_applies_style() {
        let t = Text::styled(["hi"], SpanStyle::fg(ThemeColor::Muted));
        assert_eq!(t.style, SpanStyle::fg(ThemeColor::Muted));
    }

    #[test]
    fn text_single_line() {
        let t = Text::new("hi");
        assert_eq!(t.lines.len(), 1);
        assert_eq!(t.lines[0], "hi");
    }
}