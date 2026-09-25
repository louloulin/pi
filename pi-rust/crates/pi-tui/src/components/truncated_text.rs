//! TruncatedText — 1:1 port of
//! `packages/tui/src/components/truncated-text.ts`.
//!
//! Renders a single line of text, truncating it to fit a viewport
//! width and adding symmetric horizontal/vertical padding around it.
//! Multi-line input is collapsed to its first line (anything after the
//! first `\n` is dropped), matching upstream
//! (`packages/tui/src/components/truncated-text.ts:38-42`).

use crate::component::Component;
use crate::utils::styled::{SpanStyle, StyledLine, StyledSpan};
use crate::utils::util::{truncate_to_width, visible_width};

/// Single-line text component with horizontal / vertical padding.
///
/// Mirrors upstream `TruncatedText`
/// (`packages/tui/src/components/truncated-text.ts:7-65`).
pub struct TruncatedText {
    text: String,
    padding_x: u16,
    padding_y: u16,
}

impl TruncatedText {
    /// Build a new instance with optional horizontal (`paddingX`) and
    /// vertical (`paddingY`) padding in columns.
    pub fn new(text: impl Into<String>, padding_x: u16, padding_y: u16) -> Self {
        Self {
            text: text.into(),
            padding_x,
            padding_y,
        }
    }

    /// Update the rendered text.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }

    /// Current text content.
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Component for TruncatedText {
    fn render(&self, width: u16) -> Vec<StyledLine> {
        let width = width.max(1) as usize;
        let empty_line = vec![StyledSpan::new(" ".repeat(width), SpanStyle::PLAIN)];

        let mut lines: Vec<StyledLine> = Vec::new();
        for _ in 0..self.padding_y {
            lines.push(empty_line.clone());
        }

        let available_width = width.saturating_sub(self.padding_x as usize * 2).max(1);
        let single_line = match self.text.find('\n') {
            Some(idx) => &self.text[..idx],
            None => self.text.as_str(),
        };
        let display_text = truncate_to_width(single_line, available_width);

        let left_padding = " ".repeat(self.padding_x as usize);
        let right_padding = " ".repeat(self.padding_x as usize);
        let line_with_padding = format!("{left_padding}{display_text}{right_padding}");
        let line_visible = visible_width(&line_with_padding);
        let padding_needed = width.saturating_sub(line_visible);
        let final_line = format!("{line_with_padding}{}", " ".repeat(padding_needed));
        lines.push(vec![StyledSpan::new(final_line, SpanStyle::PLAIN)]);

        for _ in 0..self.padding_y {
            lines.push(empty_line.clone());
        }

        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::styled::plain_text;

    #[test]
    fn render_includes_horizontal_padding() {
        let t = TruncatedText::new("hi", 2, 0);
        let lines = t.render(20);
        assert_eq!(lines.len(), 1);
        let raw = plain_text(&lines[0]);
        assert!(raw.starts_with("  hi"));
        assert_eq!(raw.len(), 20);
    }

    #[test]
    fn render_adds_vertical_padding() {
        let t = TruncatedText::new("hi", 0, 2);
        let lines = t.render(20);
        assert_eq!(lines.len(), 5); // 2 + 1 + 2
        assert_eq!(plain_text(&lines[0]).len(), 20);
        assert!(plain_text(&lines[2]).contains("hi"));
    }

    #[test]
    fn render_truncates_text_to_width() {
        let t = TruncatedText::new("a very long string that exceeds width", 0, 0);
        let lines = t.render(10);
        assert_eq!(lines.len(), 1);
        let raw = plain_text(&lines[0]);
        assert!(raw.len() <= 10);
    }

    #[test]
    fn render_collapses_after_newline() {
        let t = TruncatedText::new("first\nsecond", 0, 0);
        let lines = t.render(20);
        assert_eq!(lines.len(), 1);
        let raw = plain_text(&lines[0]);
        assert!(raw.contains("first"));
        assert!(!raw.contains("second"));
    }

    #[test]
    fn set_text_updates_content() {
        let mut t = TruncatedText::new("a", 0, 0);
        t.set_text("b");
        assert_eq!(t.text(), "b");
    }
}