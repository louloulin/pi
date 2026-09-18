//! Single-select list component.
//!
//! Mirrors the role of `packages/tui/components/select-list.ts` (the
//! command palette / model selector both reuse the same primitive).
//!
//! The selector is purely data + key handling; the [`App`](crate::App)
//! renders it when it is open.

use crate::input::{InputEvent, Key, KeyCode};

/// Single item in a [`Selector`].
#[derive(Debug, Clone, PartialEq)]
pub struct SelectorItem {
    /// Stable identifier — used as the return value of
    /// [`Selector::selected_value`].
    pub value: String,
    /// Human-readable label.
    pub label: String,
    /// Optional secondary text shown right of the label.
    pub description: Option<String>,
}

impl SelectorItem {
    /// Convenience constructor with just a label/value pair.
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            description: None,
        }
    }

    /// Builder-style setter for the description.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

/// Action returned from [`Selector::handle_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorAction {
    /// No state change worth redrawing.
    None,
    /// Cursor moved — caller redraws.
    Changed,
    /// User picked the highlighted item.
    Selected(String),
    /// User dismissed the selector without picking (Esc / Ctrl+C).
    Cancelled,
}

/// Modal list selector.
#[derive(Debug, Clone)]
pub struct Selector {
    title: String,
    items: Vec<SelectorItem>,
    cursor: usize,
    /// Initial cursor position used when the selector is re-opened.
    initial_cursor: usize,
}

impl Selector {
    /// Construct a selector with the given items.
    pub fn new(title: impl Into<String>, items: Vec<SelectorItem>) -> Self {
        Self {
            title: title.into(),
            items,
            cursor: 0,
            initial_cursor: 0,
        }
    }

    /// Borrow the title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Borrow the items.
    pub fn items(&self) -> &[SelectorItem] {
        &self.items
    }

    /// Number of items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when the list is empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Current cursor index.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Highlighted item, if any.
    pub fn selected(&self) -> Option<&SelectorItem> {
        self.items.get(self.cursor)
    }

    /// Value of the highlighted item.
    pub fn selected_value(&self) -> Option<&str> {
        self.selected().map(|item| item.value.as_str())
    }

    /// Move the cursor down (wraps).
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> SelectorAction {
        if self.items.is_empty() {
            return SelectorAction::None;
        }
        self.cursor = (self.cursor + 1) % self.items.len();
        SelectorAction::Changed
    }

    /// Move the cursor up (wraps).
    pub fn prev(&mut self) -> SelectorAction {
        if self.items.is_empty() {
            return SelectorAction::None;
        }
        if self.cursor == 0 {
            self.cursor = self.items.len() - 1;
        } else {
            self.cursor -= 1;
        }
        SelectorAction::Changed
    }

    /// Jump to the first item.
    pub fn first(&mut self) -> SelectorAction {
        if self.items.is_empty() {
            return SelectorAction::None;
        }
        self.cursor = 0;
        SelectorAction::Changed
    }

    /// Jump to the last item.
    pub fn last(&mut self) -> SelectorAction {
        if self.items.is_empty() {
            return SelectorAction::None;
        }
        self.cursor = self.items.len() - 1;
        SelectorAction::Changed
    }

    /// Move to the next item whose label starts with the given prefix
    /// (case-insensitive). No-op when nothing matches.
    pub fn jump_to_prefix(&mut self, prefix: &str) -> SelectorAction {
        if prefix.is_empty() || self.items.is_empty() {
            return SelectorAction::None;
        }
        let lower = prefix.to_ascii_lowercase();
        let len = self.items.len();
        for offset in 1..=len {
            let idx = (self.cursor + offset) % len;
            if self.items[idx]
                .label
                .to_ascii_lowercase()
                .starts_with(&lower)
            {
                self.cursor = idx;
                return SelectorAction::Changed;
            }
        }
        SelectorAction::None
    }

    /// Reset to the initial cursor — used when re-opening the selector.
    pub fn reset_cursor(&mut self) {
        self.cursor = self.initial_cursor.min(self.items.len().saturating_sub(1));
    }

    /// Process a key event.
    pub fn handle_key(&mut self, key: Key) -> SelectorAction {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.prev(),
            KeyCode::Down | KeyCode::Char('j') => self.next(),
            KeyCode::Home | KeyCode::Char('g') => self.first(),
            KeyCode::End | KeyCode::Char('G') => self.last(),
            KeyCode::Enter => match self.selected_value() {
                Some(value) => SelectorAction::Selected(value.to_string()),
                None => SelectorAction::None,
            },
            KeyCode::Esc => SelectorAction::Cancelled,
            _ => SelectorAction::None,
        }
    }

    /// Process an input event.
    pub fn handle_event(&mut self, event: InputEvent) -> SelectorAction {
        let InputEvent::Key(key) = event else {
            return SelectorAction::None;
        };
        self.handle_key(key)
    }

    /// Render the selector as a flat vector of lines (used by the App
    /// and by tests).
    pub fn render_lines(&self, width: u16) -> Vec<String> {
        let mut lines = Vec::new();
        let width = width as usize;
        lines.push(self.title.clone());
        lines.push("─".repeat(width.min(40)));
        if self.items.is_empty() {
            lines.push("(no items)".to_string());
            return lines;
        }
        for (idx, item) in self.items.iter().enumerate() {
            let marker = if idx == self.cursor { "❯ " } else { "  " };
            let mut line = format!("{marker}{}", item.label);
            if let Some(desc) = &item.description {
                let prefix_len = line.chars().count();
                if width > prefix_len + 2 {
                    line.push_str("  ");
                    line.push_str(desc);
                }
            }
            lines.push(line);
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::KeyModifiers;

    fn items() -> Vec<SelectorItem> {
        vec![
            SelectorItem::new("gpt", "gpt-4o").with_description("OpenAI"),
            SelectorItem::new("claude", "claude-3.5-sonnet").with_description("Anthropic"),
            SelectorItem::new("faux", "faux-model").with_description("Test"),
        ]
    }

    #[test]
    fn cursor_wraps_on_next() {
        let mut sel = Selector::new("Pick", items());
        sel.last();
        assert_eq!(sel.cursor(), 2);
        sel.next();
        assert_eq!(sel.cursor(), 0);
    }

    #[test]
    fn cursor_wraps_on_prev() {
        let mut sel = Selector::new("Pick", items());
        sel.prev();
        assert_eq!(sel.cursor(), 2);
    }

    #[test]
    fn jump_to_prefix_finds_next_match() {
        let mut sel = Selector::new("Pick", items());
        sel.cursor = 0;
        assert_eq!(sel.jump_to_prefix("cl"), SelectorAction::Changed);
        assert_eq!(sel.cursor(), 1);
    }

    #[test]
    fn enter_returns_selected_value() {
        let mut sel = Selector::new("Pick", items());
        sel.cursor = 1;
        let action = sel.handle_key(Key::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            SelectorAction::Selected(value) => assert_eq!(value, "claude"),
            other => panic!("unexpected action: {:?}", other),
        }
    }

    #[test]
    fn esc_cancels() {
        let mut sel = Selector::new("Pick", items());
        let action = sel.handle_key(Key::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, SelectorAction::Cancelled);
    }

    #[test]
    fn empty_selector_handles_keys_safely() {
        let mut sel = Selector::new("Empty", Vec::new());
        assert_eq!(sel.next(), SelectorAction::None);
        assert_eq!(sel.prev(), SelectorAction::None);
        assert_eq!(
            sel.handle_key(Key::new(KeyCode::Enter, KeyModifiers::NONE)),
            SelectorAction::None,
        );
    }
}
