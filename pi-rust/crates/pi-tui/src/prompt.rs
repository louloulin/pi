//! Prompt component — wraps an [`Editor`] with the bottom-of-screen
//! prompt chrome (label, border, placeholder). Mirrors the role of
//! `packages/tui/components/editor.ts` together with the `prompt()`
//! shell in `packages/coding-agent/src/modes/interactive/interactive-mode.ts`.
//!
//! [`Editor`]: crate::Editor

use crate::editor::{Editor, EditorAction};
use crate::input::{InputEvent, Key, KeyCode};

/// Action returned from [`Prompt::handle_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptAction {
    /// No state change worth redrawing.
    None,
    /// Buffer or placeholder changed — caller redraws.
    Changed,
    /// User submitted the prompt.
    Submit(String),
    /// User pressed Ctrl+C — caller interrupts the current operation.
    Interrupt,
    /// User pressed Ctrl+D on an empty buffer — caller exits.
    Eof,
}

/// Bottom-of-screen prompt with editable buffer.
#[derive(Debug, Clone)]
pub struct Prompt {
    editor: Editor,
    placeholder: String,
    label: String,
}

impl Default for Prompt {
    fn default() -> Self {
        Self::new("> ")
    }
}

impl Prompt {
    /// Construct a prompt with the given label.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            editor: Editor::new(),
            placeholder: String::new(),
            label: label.into(),
        }
    }

    /// Set the placeholder shown when the buffer is empty.
    pub fn set_placeholder(&mut self, placeholder: impl Into<String>) {
        self.placeholder = placeholder.into();
    }

    /// Borrow the underlying editor.
    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    /// Mutable borrow of the underlying editor.
    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }

    /// The draft the prompt shows: chip sentinels expanded to their
    /// `[Image #N]` labels. Use [`Prompt::editor`] for the raw buffer.
    pub fn text(&self) -> String {
        self.editor.display_text()
    }

    /// Cursor column within [`Prompt::text`].
    pub fn cursor(&self) -> usize {
        self.editor.display_cursor()
    }

    /// The pasted image chips attached to the draft, in buffer order.
    pub fn images(&self) -> &[pi_protocol::ImageContent] {
        self.editor.image_attachments()
    }

    /// Number of image chips attached to the draft.
    pub fn image_count(&self) -> usize {
        self.editor.image_count()
    }

    /// Borrow the label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Borrow the placeholder.
    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }

    /// Clear the buffer without touching history.
    pub fn clear(&mut self) {
        self.editor.clear();
    }

    /// Push a submitted prompt onto the editor's history.
    pub fn push_history(&mut self, text: impl Into<String>) {
        self.editor.push_history(text);
    }

    /// Reset the prompt — clear buffer and history. Used by `/clear`.
    pub fn reset(&mut self) {
        self.editor.clear();
        self.editor.clear_history();
    }

    /// Render the prompt into the bottom row of the screen. The caller
    /// is responsible for selecting the area.
    pub fn render_line(&self, width: u16) -> String {
        let label = self.label.as_str();
        let label_width = label.chars().count();
        let available = (width as usize).saturating_sub(label_width);
        let text = self.editor.display_text();
        let cursor = self.editor.display_cursor();
        let (before, after) = split_at_char(&text, cursor);
        let mut line = String::new();
        line.push_str(label);
        if text.is_empty() && !self.placeholder.is_empty() {
            // Placeholder is truncated to `available` columns so we do
            // not overflow the line.
            let placeholder = char_truncate(&self.placeholder, available);
            line.push_str(&placeholder);
        } else {
            line.push_str(before);
            line.push('▍');
            line.push_str(after);
        }
        // Pad to width.
        if line.chars().count() < width as usize {
            for _ in 0..(width as usize - line.chars().count()) {
                line.push(' ');
            }
        }
        line
    }

    /// Process a key event.
    pub fn handle_key(&mut self, key: Key) -> PromptAction {
        match self.editor.handle_key(key) {
            EditorAction::None => PromptAction::None,
            EditorAction::Changed => PromptAction::Changed,
            EditorAction::Submit(text) => PromptAction::Submit(text),
            EditorAction::Interrupt => PromptAction::Interrupt,
            EditorAction::Eof => PromptAction::Eof,
        }
    }

    /// Process an input event.
    pub fn handle_event(&mut self, event: InputEvent) -> PromptAction {
        let InputEvent::Key(key) = event else {
            return PromptAction::None;
        };
        self.handle_key(key)
    }

    /// Whether the prompt currently has any buffer text.
    pub fn is_empty(&self) -> bool {
        self.editor.is_empty()
    }

    /// True when the prompt is currently showing the placeholder (no
    /// buffer text). The TUI uses this to decide whether to render the
    /// placeholder.
    pub fn shows_placeholder(&self) -> bool {
        self.editor.is_empty()
    }

    /// Detect a "submit" key (Enter) on a non-empty buffer. Returns the
    /// trimmed text if the key was Enter, otherwise `None`.
    pub fn try_submit(&mut self, key: Key) -> Option<String> {
        if key.code == KeyCode::Enter && !self.editor.is_empty() {
            Some(self.editor.display_text())
        } else {
            None
        }
    }
}

/// Split a string into `(before, after)` halves at the nth character
/// (not byte) boundary. If `idx` is out of range, the entire string is
/// returned as the first half.
fn split_at_char(text: &str, idx: usize) -> (&str, &str) {
    if idx >= text.chars().count() {
        return (text, "");
    }
    for (count, (byte_idx, _)) in text.char_indices().enumerate() {
        if count == idx {
            return (&text[..byte_idx], &text[byte_idx..]);
        }
    }
    (text, "")
}

fn char_truncate(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::KeyModifiers;

    #[test]
    fn renders_label_and_cursor() {
        let mut prompt = Prompt::new("> ");
        prompt.editor_mut().insert_str("abc");
        let line = prompt.render_line(20);
        assert!(line.starts_with("> "));
        // Cursor is at the end of the buffer, so the visual "▍"
        // appears after the text.
        assert!(line.contains("abc▍"));
    }

    #[test]
    fn renders_placeholder_when_empty() {
        let mut prompt = Prompt::new("> ");
        prompt.set_placeholder("type a prompt");
        let line = prompt.render_line(20);
        assert!(line.starts_with("> "));
        assert!(line.contains("type a prompt"));
        assert!(!line.contains('▍'));
    }

    #[test]
    fn submit_emits_text_and_caller_resets() {
        let mut prompt = Prompt::new("> ");
        prompt.editor_mut().insert_str("hello");
        let action = prompt.handle_key(Key::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            PromptAction::Submit(text) => assert_eq!(text, "hello"),
            other => panic!("unexpected action: {:?}", other),
        }
    }

    #[test]
    fn interrupt_and_eof_bubble_through() {
        let mut prompt = Prompt::new("> ");
        assert_eq!(
            prompt.handle_key(Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            PromptAction::Interrupt,
        );
        assert_eq!(
            prompt.handle_key(Key::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            PromptAction::Eof,
        );
    }

    #[test]
    fn reset_clears_buffer_and_history() {
        let mut prompt = Prompt::new("> ");
        prompt.editor_mut().insert_str("hello");
        prompt.push_history("hello");
        prompt.reset();
        assert!(prompt.is_empty());
        assert_eq!(prompt.editor().history_len(), 0);
    }
}
