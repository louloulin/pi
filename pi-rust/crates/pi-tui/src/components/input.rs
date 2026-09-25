//! `Input` — a single-line input component.
//!
//! Mirrors upstream `Input` (`packages/tui/src/components/input.ts`).
//! Hosts a `value`, a `placeholder`, and a cursor position.

use crate::component::Component;
use crate::utils::styled::{SpanStyle, StyledLine, StyledSpan};

/// Upstream `Input` — a single-line input component.
#[derive(Debug, Clone)]
pub struct InputComponent {
    value: String,
    placeholder: String,
    cursor: usize,
    style: SpanStyle,
}

impl InputComponent {
    /// Build an empty input.
    pub fn new() -> Self {
        Self {
            value: String::new(),
            placeholder: String::new(),
            cursor: 0,
            style: SpanStyle::PLAIN,
        }
    }

    /// Build an input with an initial value.
    pub fn with_value(value: impl Into<String>) -> Self {
        let mut s = Self::new();
        s.value = value.into();
        s.cursor = s.value.len();
        s
    }

    /// Set the placeholder.
    pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Set the span style.
    pub fn with_style(mut self, style: SpanStyle) -> Self {
        self.style = style;
        self
    }

    /// Current value.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Replace the value (cursor jumps to the end).
    pub fn set_value(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.cursor = self.value.len();
    }

    /// Insert text at the cursor.
    pub fn insert(&mut self, text: &str) {
        self.value.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.value.remove(self.cursor);
        }
    }

    /// Cursor column (0-based).
    pub fn cursor(&self) -> usize {
        self.cursor
    }
}

impl Default for InputComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for InputComponent {
    fn render(&self, _width: u16) -> Vec<StyledLine> {
        let displayed = if self.value.is_empty() {
            self.placeholder.clone()
        } else {
            self.value.clone()
        };
        vec![vec![StyledSpan::new(displayed, self.style)]]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_renders_placeholder() {
        let mut i = InputComponent::new();
        i.placeholder = "type here".into();
        let lines = i.render(40);
        assert_eq!(crate::utils::styled::plain_text(&lines[0]), "type here");
    }

    #[test]
    fn value_replaces_placeholder() {
        let i = InputComponent::with_value("hello");
        let lines = i.render(40);
        assert_eq!(crate::utils::styled::plain_text(&lines[0]), "hello");
    }

    #[test]
    fn insert_and_backspace() {
        let mut i = InputComponent::new();
        i.insert("hi");
        assert_eq!(i.value(), "hi");
        assert_eq!(i.cursor(), 2);
        i.backspace();
        assert_eq!(i.value(), "h");
        assert_eq!(i.cursor(), 1);
    }
}