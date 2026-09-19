//! Message view — renders the conversation log (user / assistant /
//! tool) on screen and folds incremental assistant updates into the
//! last in-flight assistant block.
//!
//! The component is terminal-agnostic: the [`render_lines`](MessageView::render_lines)
//! method produces a sequence of pre-wrapped text lines for a given
//! width, and the [`MessageView::render_to_buffer`] method writes the
//! lines into a `ratatui::buffer::Buffer` for snapshot tests.

use std::fmt::Write as _;

/// Logical role — drives the visual prefix and the message-view
/// rendering. Mirrors the `user` / `assistant` / `tool` distinction the
/// TS message components make.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// User-typed prompt.
    User,
    /// Assistant response.
    Assistant,
    /// Tool call + result block.
    Tool,
}

/// One entry in the rendered message log.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageItem {
    /// Role this entry represents.
    pub role: Role,
    /// Plain text body. For `Role::Tool` the body is the formatted
    /// call / result pair.
    pub text: String,
    /// True while an assistant message is still streaming (the TUI
    /// shows a caret indicator).
    pub streaming: bool,
}

impl MessageItem {
    /// Convenience constructor for a user prompt.
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            text: text.into(),
            streaming: false,
        }
    }

    /// Convenience constructor for a finalized assistant message.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            text: text.into(),
            streaming: false,
        }
    }

    /// Convenience constructor for an in-flight assistant block (used
    /// when a `MessageStart` arrives and before `MessageEnd`).
    pub fn assistant_streaming() -> Self {
        Self {
            role: Role::Assistant,
            text: String::new(),
            streaming: true,
        }
    }

    /// Convenience constructor for a tool execution block.
    pub fn tool(text: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            text: text.into(),
            streaming: false,
        }
    }
}

/// Conversation log rendered by the TUI. Holds an ordered list of
/// [`MessageItem`] entries and supports incremental updates so the
/// TUI redraws only the tail while the assistant streams.
#[derive(Debug, Clone, Default)]
pub struct MessageView {
    items: Vec<MessageItem>,
    /// Scroll offset — lines from the bottom. `0` means pinned to the
    /// tail (latest message visible).
    scroll_from_bottom: usize,
}

impl MessageView {
    /// Construct an empty view.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of items in the log.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when the log has no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Borrow the items.
    pub fn items(&self) -> &[MessageItem] {
        &self.items
    }

    /// Append a finalized message to the log.
    pub fn push(&mut self, item: MessageItem) {
        self.items.push(item);
        self.scroll_from_bottom = 0;
    }

    /// Drop every item from the log. `/clear` uses this.
    pub fn clear(&mut self) {
        self.items.clear();
        self.scroll_from_bottom = 0;
    }

    /// Append a text delta to the trailing assistant block. If the
    /// last item is not an in-flight assistant, a new streaming
    /// assistant block is created and the delta is appended to it.
    pub fn append_assistant_delta(&mut self, delta: &str) {
        match self.items.last_mut() {
            Some(item) if item.role == Role::Assistant && item.streaming => {
                item.text.push_str(delta);
            }
            _ => {
                let mut item = MessageItem::assistant_streaming();
                item.text.push_str(delta);
                self.items.push(item);
            }
        }
        self.scroll_from_bottom = 0;
    }

    /// Start a new streaming assistant block — used when the TUI sees
    /// a `MessageStart` event for a new turn.
    pub fn begin_assistant_stream(&mut self, model: &str) {
        let mut item = MessageItem::assistant_streaming();
        let _ = write!(item.text, "[{model}]");
        self.items.push(item);
        self.scroll_from_bottom = 0;
    }

    /// Finalize the trailing streaming assistant block — converts the
    /// placeholder into a non-streaming assistant item. If the last
    /// item is not streaming, the call is a no-op.
    pub fn end_assistant_stream(&mut self) {
        if let Some(item) = self.items.last_mut() {
            if item.role == Role::Assistant && item.streaming {
                item.streaming = false;
            }
        }
    }

    /// Append a tool execution block.
    pub fn push_tool(&mut self, name: &str, args: &str, result: &str, is_error: bool) {
        let mut text = String::new();
        if is_error {
            let _ = write!(text, "[tool:{name}] error");
            if !result.is_empty() {
                let _ = write!(text, ": {result}");
            }
        } else {
            let _ = write!(text, "[tool:{name}]");
            if !args.is_empty() {
                let _ = write!(text, " {args}");
            }
            if !result.is_empty() {
                let _ = write!(text, " → {result}");
            }
        }
        self.push(MessageItem::tool(text));
    }

    /// Push a free-form info message — used by `/help`, slash
    /// command output, and TUI-side notices.
    pub fn push_info(&mut self, text: impl Into<String>) {
        self.push(MessageItem {
            role: Role::User,
            text: text.into(),
            streaming: false,
        });
    }

    /// True when the log is currently pinned to the tail. The TUI
    /// re-pins whenever it appends a new item or a delta.
    pub fn is_pinned_to_bottom(&self) -> bool {
        self.scroll_from_bottom == 0
    }

    /// Scroll up by one line (away from the tail).
    pub fn scroll_up(&mut self) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(1);
    }

    /// Scroll down by one line (toward the tail).
    pub fn scroll_down(&mut self) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(1);
    }

    /// Pin to the tail.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_from_bottom = 0;
    }

    /// Pin to the head.
    pub fn scroll_to_top(&mut self) {
        self.scroll_from_bottom = usize::MAX;
    }

    /// Current scroll offset (lines from the bottom). `0` means
    /// pinned.
    pub fn scroll_offset(&self) -> usize {
        self.scroll_from_bottom
    }

    /// Render the log into a flat vector of pre-wrapped lines,
    /// prepending a one-character role prefix to each line. The TUI
    /// slices the returned vector into the visible viewport.
    pub fn render_lines(&self, width: u16) -> Vec<String> {
        let prefix_width = 2usize; // "> " or "* "
        let text_width = (width as usize).saturating_sub(prefix_width).max(1);

        let mut out: Vec<String> = Vec::new();
        for item in &self.items {
            let (prefix, body) = match item.role {
                Role::User => ("> ", item.text.clone()),
                Role::Assistant => ("  ", item.text.clone()),
                Role::Tool => ("* ", item.text.clone()),
            };
            let wrapped = wrap_text(&body, text_width);
            if wrapped.is_empty() {
                out.push(prefix.to_string());
                continue;
            }
            for (idx, line) in wrapped.iter().enumerate() {
                if idx == 0 {
                    let tail = if item.role == Role::Assistant && item.streaming {
                        " ▍"
                    } else {
                        ""
                    };
                    out.push(format!("{prefix}{line}{tail}"));
                } else {
                    out.push(format!("{prefix}{line}"));
                }
            }
        }

        if out.is_empty() {
            out.push(String::new());
        }
        out
    }

    /// Render the trailing `height` lines of the log into a
    /// `ratatui::buffer::Buffer`. Used by the [`App`](crate::App) and
    /// by the snapshot tests in `tests/snapshot.rs`.
    pub fn render_to_buffer(&self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        let lines = self.render_lines(area.width);
        let height = area.height as usize;
        let total = lines.len();
        let skip = total.saturating_sub(height + self.scroll_from_bottom);
        let visible_start = skip.min(total);
        let visible_end = (visible_start + height).min(total);

        for (row, line) in lines[visible_start..visible_end].iter().enumerate() {
            let y = area.y + row as u16;
            if y >= area.y + area.height {
                break;
            }
            for (col, ch) in line.chars().enumerate() {
                let x = area.x + col as u16;
                if x >= area.x + area.width {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(ch);
                }
            }
        }
    }
}

/// Word-aware wrap that prefers to break at word boundaries and
/// falls back to hard-wrapping at `width` when a single word is longer
/// than the available space.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for word in split_words(text) {
        let word_width = display_width(word);
        if word_width > width {
            // Flush whatever we have, then hard-wrap the long word.
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_width = 0;
            }
            for chunk in hard_wrap(word, width) {
                lines.push(chunk);
            }
            continue;
        }
        let sep = if current.is_empty() { 0 } else { 1 };
        if current_width + sep + word_width > width {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
            current_width = word_width;
        } else {
            if sep == 1 {
                current.push(' ');
                current_width += 1;
            }
            current.push_str(word);
            current_width += word_width;
        }
    }

    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Split text into non-whitespace runs. The wrap function joins the
/// runs with a single ASCII space, so collapsing whitespace runs to
/// their own words produces the wrong output (e.g. `"a   b"` becomes
/// `"a   b"` instead of `"a b"`).
fn split_words(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut in_ws = bytes
        .first()
        .map(|b| b.is_ascii_whitespace())
        .unwrap_or(false);

    for (idx, byte) in bytes.iter().enumerate() {
        let ws = byte.is_ascii_whitespace();
        if ws != in_ws {
            if !in_ws {
                out.push(&text[start..idx]);
            }
            start = idx;
            in_ws = ws;
        }
    }
    if !in_ws && start < text.len() {
        out.push(&text[start..]);
    }
    out
}

fn display_width(s: &str) -> usize {
    s.chars().count()
}

fn hard_wrap(word: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for ch in word.chars() {
        if current_width + 1 > width {
            out.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(ch);
        current_width += 1;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_lines_includes_role_prefix() {
        let mut view = MessageView::new();
        view.push(MessageItem::user("hi"));
        view.push(MessageItem::assistant("hello"));
        let lines = view.render_lines(40);
        assert!(lines[0].starts_with("> "));
        assert!(lines[1].starts_with("  "));
    }

    #[test]
    fn streaming_assistant_appends_deltas_in_place() {
        let mut view = MessageView::new();
        view.begin_assistant_stream("gpt");
        view.append_assistant_delta("hel");
        view.append_assistant_delta("lo");
        view.end_assistant_stream();
        assert_eq!(view.len(), 1);
        let item = &view.items()[0];
        assert_eq!(item.text, "[gpt]hello");
        assert!(!item.streaming);
    }

    #[test]
    fn clear_resets_state() {
        let mut view = MessageView::new();
        view.push(MessageItem::user("a"));
        view.push(MessageItem::assistant("b"));
        view.clear();
        assert!(view.is_empty());
    }

    #[test]
    fn push_tool_formats_call_and_result() {
        let mut view = MessageView::new();
        view.push_tool("read", "{\"path\":\"x\"}", "ok", false);
        assert!(view.items()[0].text.contains("[tool:read]"));
        assert!(view.items()[0].text.contains("{\"path\":\"x\"}"));
        assert!(view.items()[0].text.contains("→ ok"));
    }

    #[test]
    fn push_tool_error_uses_killed_message() {
        let mut view = MessageView::new();
        view.push_tool("read", "", "boom", true);
        assert!(view.items()[0].text.contains("error"));
        assert!(!view.items()[0].text.contains("→ "));
    }

    #[test]
    fn wrap_preserves_blank_lines() {
        let view = MessageView::new();
        let lines = view.render_lines(20);
        // Empty view yields one empty line.
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "");
    }

    #[test]
    fn hard_wraps_long_word() {
        let mut view = MessageView::new();
        view.push(MessageItem::user("x".repeat(50)));
        let lines = view.render_lines(10);
        // The first line fits at most 8 chars ("x" repeated), so the
        // body is broken across multiple lines.
        assert!(lines.iter().all(|l| l.starts_with("> ")));
        assert!(lines.len() >= 5);
    }
}
