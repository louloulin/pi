//! Prompt component — wraps an [`Editor`] with the bottom-of-screen
//! prompt chrome (label, border, placeholder). Mirrors the role of
//! `packages/tui/components/editor.ts` together with the `prompt()`
//! shell in `packages/coding-agent/src/modes/interactive/interactive-mode.ts`.
//!
//! [`Editor`]: crate::Editor

use crate::editor::{Editor, EditorAction};
use crate::input::{InputEvent, Key, KeyCode};
use crate::visual_text::VisualLayout;

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

    /// How many visual rows the prompt needs to render its current buffer
    /// at `width`, clamped to `max_rows`. Always at least 1 — an empty
    /// prompt still occupies one row.
    ///
    /// The buffer is word-wrapped the same way [`Prompt::render_lines`]
    /// does, so callers can reserve the row count ahead of time and the
    /// resulting layout does not jump when the buffer is typed into.
    pub fn body_width(&self, width: u16) -> usize {
        wrap_available(width as usize, self.label.chars().count()).max(1)
    }

    /// Columns of `width` the draft itself may use (the label takes the
    /// rest, and at least one column always remains).
    ///
    /// Exposed because the composer's word wrap is not only a rendering
    /// concern: [`crate::Editor`]'s vertical cursor motion has to measure
    /// the draft with the same number, or the caret lands on a row the
    /// renderer did not draw it on. The App hands this value to the editor
    /// before every key press.
    pub fn line_count(&self, width: u16, max_rows: usize) -> usize {
        let width = width as usize;
        if width == 0 || max_rows == 0 {
            return 1;
        }
        let text = self.editor.display_text();
        if text.is_empty() {
            return 1;
        }
        let rows = VisualLayout::new(&text, self.body_width(width as u16)).len();
        rows.clamp(1, max_rows.max(1))
    }

    /// Render the prompt into 1..=`max_rows` lines at the given width.
    ///
    /// The first row carries the label (e.g. `"> "`) and the placeholder
    /// (when the buffer is empty); subsequent rows are indented to keep
    /// the visual column of the buffer aligned across the wrap.
    /// Word-wrapping matches upstream's `wordWrapLine`
    /// (`packages/tui/src/components/editor.ts:121`) through
    /// [`VisualLayout`], which also supplies the cursor's row and column, so
    /// the `▍` marker cannot disagree with the rows it is drawn on.
    /// A hard break (`\n`) always starts a new row, including a blank one,
    /// so `Shift+Enter` grows the composer immediately.
    ///
    /// When the draft needs more rows than `max_rows`, the window follows
    /// the cursor (see [`scroll_window_start`]) instead of showing the tail:
    /// a cursor above the tail used to be scrolled off the composer
    /// entirely.
    ///
    /// Callers that size the composer by content pass the value returned
    /// by [`Prompt::line_count`]; callers that want a fixed-height
    /// composer pass `1` and get the existing single-row behaviour.
    pub fn render_lines(&self, width: u16, max_rows: usize) -> Vec<String> {
        let width = width as usize;
        let max_rows = max_rows.max(1);
        if width == 0 {
            return vec![String::new()];
        }
        let label_width = self.label.chars().count();
        let available = self.body_width(width as u16);
        let text = self.editor.display_text();

        if text.is_empty() {
            // Empty buffer — placeholder on the first row, blank padded
            // continuation rows so the row count matches `max_rows`.
            let mut out = Vec::with_capacity(max_rows);
            out.push(self.empty_row(width, available));
            let indent = " ".repeat(label_width);
            while out.len() < max_rows {
                let row_index = out.len();
                let prefix = if row_index == 0 {
                    self.label.as_str()
                } else {
                    indent.as_str()
                };
                out.push(blank_row(prefix, width));
            }
            return out;
        }

        let layout = VisualLayout::new(&text, available);
        let (cursor_row, cursor_col) = layout.caret(self.editor.display_cursor());
        let total_rows = layout.len();
        let show_rows = total_rows.min(max_rows);
        let skip = scroll_window_start(cursor_row, total_rows, show_rows);
        let indent = " ".repeat(label_width);

        let mut out: Vec<String> = Vec::with_capacity(show_rows);
        for (offset, row) in layout.rows()[skip..skip + show_rows].iter().enumerate() {
            let index = skip + offset;
            // The label belongs to the draft's own first row; a scrolled
            // window indents every row it shows.
            let prefix = if index == 0 {
                self.label.as_str()
            } else {
                indent.as_str()
            };
            let draw_cursor = index == cursor_row;
            out.push(build_prompt_row(
                prefix,
                &row.text,
                if draw_cursor { cursor_col } else { usize::MAX },
                width,
                draw_cursor,
            ));
        }
        // Pad with blank rows if the cap exceeds the natural row count.
        // Continuation rows (anything past the first) get the indent so
        // the visual column of the buffer stays aligned; the first row
        // gets the label so the user still sees what mode they are in.
        let first_prefix = self.label.as_str();
        let cont_prefix = indent.as_str();
        while out.len() < max_rows {
            let row_index = out.len();
            let prefix = if row_index == 0 {
                first_prefix
            } else {
                cont_prefix
            };
            out.push(blank_row(prefix, width));
        }
        out
    }

    /// Empty single-row representation (placeholder + padding).
    fn empty_row(&self, width: usize, available: usize) -> String {
        let mut line = String::with_capacity(width);
        line.push_str(&self.label);
        if !self.placeholder.is_empty() {
            line.push_str(&char_truncate(&self.placeholder, available));
        }
        while line.chars().count() < width {
            line.push(' ');
        }
        line
    }
}

/// How many columns of the prompt row are available to the buffer
/// (after subtracting the label).
fn wrap_available(width: usize, label_width: usize) -> usize {
    width.saturating_sub(label_width)
}

/// Build a padded blank row with the given prefix.
fn blank_row(prefix: &str, width: usize) -> String {
    let mut line = String::with_capacity(width);
    line.push_str(prefix);
    while line.chars().count() < width {
        line.push(' ');
    }
    line
}

/// Build a single prompt row (label + body + cursor + padding) padded to
/// `width` columns. `cursor_in_row == usize::MAX` means "no cursor on
/// this row"; any smaller value is the absolute column inside the body
/// where the `▍` marker should appear.
fn build_prompt_row(
    prefix: &str,
    body: &str,
    cursor_in_row: usize,
    width: usize,
    draw_cursor: bool,
) -> String {
    let mut line = String::with_capacity(width + 1);
    line.push_str(prefix);
    let body_chars: Vec<char> = body.chars().collect();
    if draw_cursor {
        let col = cursor_in_row.min(body_chars.len());
        for ch in &body_chars[..col] {
            line.push(*ch);
        }
        line.push('▍');
        for ch in &body_chars[col..] {
            line.push(*ch);
        }
    } else {
        for ch in &body_chars {
            line.push(*ch);
        }
    }
    while line.chars().count() < width {
        line.push(' ');
    }
    line
}

/// First row of the composer window that keeps the cursor visible.
///
/// The window is page-aligned on the cursor row instead of centred on it:
/// `render_lines` takes `&self`, so the renderer cannot remember a scroll
/// offset, and re-anchoring on every keystroke would make the composer jump
/// while the cursor moves inside one page. The last page is clamped, so the
/// tail of a long draft is always fully visible.
fn scroll_window_start(cursor_row: usize, total_rows: usize, show_rows: usize) -> usize {
    let show_rows = show_rows.max(1);
    if show_rows >= total_rows {
        return 0;
    }
    let page = (cursor_row / show_rows) * show_rows;
    page.min(total_rows - show_rows)
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

    /// An empty prompt still claims one row so callers always get a
    /// paintable region; a single-row buffer on a width that fits it
    /// is exactly one row.
    #[test]
    fn line_count_is_at_least_one_for_empty_and_short_buffers() {
        let mut prompt = Prompt::new("> ");
        assert_eq!(prompt.line_count(20, 8), 1);
        prompt.editor_mut().insert_str("hello");
        assert_eq!(prompt.line_count(20, 8), 1);
    }

    /// Word-wrap: at width 10 (label "> " = 2, body width 8) the buffer
    /// "hello world foo bar" splits across three rows.
    #[test]
    fn line_count_wraps_long_buffers() {
        let mut prompt = Prompt::new("> ");
        prompt.editor_mut().insert_str("hello world foo bar");
        assert_eq!(prompt.line_count(10, 8), 3);
    }

    /// Hard line breaks (`\n`) become row boundaries even when the
    /// individual pieces are short.
    #[test]
    fn line_count_respects_hard_breaks() {
        let mut prompt = Prompt::new("> ");
        prompt.editor_mut().insert_str("first\nsecond");
        assert_eq!(prompt.line_count(20, 8), 2);
    }

    /// The row count is clamped to `max_rows`, so callers that pass a
    /// small cap never see more rows than the cap.
    #[test]
    fn line_count_clamps_to_max_rows() {
        let mut prompt = Prompt::new("> ");
        prompt.editor_mut().insert_str("a b c d e f g h i j k l m");
        // Width 10 (body 8) → at least 5 rows; cap to 3.
        assert_eq!(prompt.line_count(10, 3), 3);
    }

    /// `render_lines` with `max_rows == 1` and a buffer that needs more
    /// rows shows only the cursor row (the scroll behaviour). The label
    /// only appears on the very first row of the rendered slice, so the
    /// cursor row here uses the indent, not the label.
    #[test]
    fn render_lines_with_one_row_keeps_the_legacy_compat() {
        let mut prompt = Prompt::new("> ");
        prompt.editor_mut().insert_str("abcdefghij");
        let lines = prompt.render_lines(8, 1);
        assert_eq!(lines.len(), 1);
        // Body width is 8 - 2 (label) = 6; only the trailing 4 chars fit.
        assert!(!lines[0].starts_with("> "));
        assert!(lines[0].starts_with("  "));
        assert!(!lines[0].contains("abcdef"));
        assert!(lines[0].contains("ghij"));
        assert!(lines[0].contains('▍'));
    }

    /// Multi-row render: label only on the first row, continuation rows
    /// are indented to keep the buffer column aligned.
    #[test]
    fn render_lines_multi_row_indents_continuation_rows() {
        let mut prompt = Prompt::new("> ");
        prompt.editor_mut().insert_str("hello world foo bar");
        let lines = prompt.render_lines(10, 8);
        assert!(lines.len() >= 3, "got {} lines", lines.len());
        assert!(lines[0].starts_with("> "));
        for cont in &lines[1..] {
            assert!(
                cont.starts_with("  "),
                "continuation row must align with the label: {:?}",
                cont
            );
        }
    }

    /// The cursor marker shows on the visual row that contains the
    /// cursor, not on rows past it.
    #[test]
    fn render_lines_places_the_cursor_on_the_right_row() {
        let mut prompt = Prompt::new("> ");
        // 14 chars; width 6 (label "> ") → body width 4.
        prompt.editor_mut().insert_str("abcdefghij klm");
        let lines = prompt.render_lines(6, 8);
        let cursors: Vec<_> = lines.iter().map(|line| line.contains('▍')).collect();
        // Exactly one row carries the cursor.
        let cursor_rows = cursors.iter().filter(|c| **c).count();
        assert_eq!(cursor_rows, 1, "cursors on rows: {cursors:?}");
    }

    /// When the buffer needs more rows than `max_rows`, the head is
    /// dropped so the cursor (always near the tail) stays visible —
    /// the upstream editor's scroll behaviour.
    #[test]
    fn render_lines_caps_to_max_rows_keeping_the_cursor() {
        let mut prompt = Prompt::new("> ");
        prompt
            .editor_mut()
            .insert_str("a b c d e f g h i j k l m n o p");
        // Width 6 (body 4) → 8 rows of natural content; cap to 3.
        let lines = prompt.render_lines(6, 3);
        assert_eq!(lines.len(), 3);
        // The cursor row must still be in the rendered slice.
        assert!(lines.iter().any(|line| line.contains('▍')));
    }

    /// An empty prompt renders the placeholder + padding across the
    /// requested row count.
    #[test]
    fn render_lines_empty_buffer_uses_placeholder() {
        let mut prompt = Prompt::new("> ");
        prompt.set_placeholder("type a prompt");
        let lines = prompt.render_lines(20, 3);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("type a prompt"));
        for row in &lines[1..] {
            // No label / no body / padded with spaces — the cursor only
            // appears on rows that own the buffer, which is none here.
            assert!(!row.contains('▍'));
        }
    }
}
