//! Single-line editor component with prompt history.
//!
//! Mirrors the prompt-history portion of `packages/tui/components/editor.ts`:
//!
//! * Plain character keys append to the buffer at the cursor.
//! * `Backspace` deletes the character before the cursor.
//! * `Delete` deletes the character at the cursor.
//! * `Left` / `Right` move the cursor by one grapheme; `Home` / `End`
//!   jump to the start / end.
//! * `Up` / `Down` navigate the prompt history (most recent first). The
//!   first `Up` saves the current draft so `Down` past the bottom of
//!   the history restores it.
//! * `Enter` returns [`EditorAction::Submit`] with the buffer text.
//! * `Ctrl+C` returns [`EditorAction::Interrupt`].
//! * `Ctrl+D` on an empty buffer returns [`EditorAction::Eof`]; on a
//!   non-empty buffer it forwards the control event so the [`App`]
//!   can decide what to do.
//! * `Ctrl+U` / `Ctrl+K` kill to the start / end of the buffer and push
//!   the killed text onto the [`KillRing`]. Because the Rust editor is
//!   single-line, "line start" and "line end" are the buffer corners
//!   (`tui.editor.deleteToLineStart` / `deleteToLineEnd`); consecutive
//!   kills accumulate into one ring entry, exactly like upstream.
//! * `Ctrl+Y` yanks the most recent ring entry at the cursor and
//!   `Alt+Y` cycles through older entries (`tui.editor.yank` /
//!   `tui.editor.yankPop`).
//!
//! History is stored in a [`VecDeque`] capped at 100 entries (matching
//! the TS implementation); consecutive duplicates are collapsed.
//!
//! [`App`]: crate::App

use std::collections::VecDeque;

use crate::input::{InputEvent, Key, KeyCode};
use crate::kill_ring::{KillDirection, KillRing};

#[cfg(test)]
use crate::input::KeyModifiers;

/// Maximum number of history entries kept by the editor.
pub const HISTORY_LIMIT: usize = 100;

/// Action returned from [`Editor::handle_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorAction {
    /// No state change worth redrawing.
    None,
    /// Buffer changed — caller redraws.
    Changed,
    /// User pressed Enter — buffer text is handed to the caller.
    Submit(String),
    /// User pressed Ctrl+C — caller interrupts the current operation.
    Interrupt,
    /// User pressed Ctrl+D on an empty buffer — caller exits.
    Eof,
}

/// The previous editing action, used to decide whether a kill
/// accumulates into the most recent ring entry and whether `Alt+Y` is
/// allowed to cycle a fresh yank. Mirrors upstream's `lastAction`
/// field (`"kill"` / `"yank"` / everything else).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastAction {
    /// Anything that is not a kill or a yank — breaks both chains.
    Other,
    /// The previous action killed text, so the next kill accumulates.
    Kill,
    /// The previous action yanked text, so `Alt+Y` may cycle it.
    Yank,
}

/// Single-line text editor with prompt history and an Emacs-style kill
/// ring.
#[derive(Debug, Clone)]
pub struct Editor {
    buffer: String,
    cursor: usize,
    history: VecDeque<String>,
    history_index: Option<usize>,
    /// Draft saved when the user starts navigating history. Restored
    /// when the user navigates back below index 0.
    history_draft: Option<String>,
    /// Emacs-style kill ring fed by `Ctrl+U` / `Ctrl+K` and drained by
    /// `Ctrl+Y` / `Alt+Y`.
    kill_ring: KillRing,
    /// Previous editing action (see [`LastAction`]).
    last_action: LastAction,
    /// Byte length of the text inserted by the most recent yank, so
    /// `Alt+Y` knows which range to replace.
    last_yank_len: usize,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    /// Construct an empty editor.
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            history: VecDeque::new(),
            history_index: None,
            history_draft: None,
            kill_ring: KillRing::new(),
            last_action: LastAction::Other,
            last_yank_len: 0,
        }
    }

    /// Borrow the current buffer.
    pub fn text(&self) -> &str {
        &self.buffer
    }

    /// Replace the buffer (cursor is clamped to the new length).
    /// Does **not** reset history navigation state — this method is
    /// also called from [`history_prev`](Self::history_prev) /
    /// [`history_next`](Self::history_next), where resetting would
    /// trap the user at the same entry on every Up arrow.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.buffer = text.into();
        self.cursor = self.buffer.len();
        self.last_action = LastAction::Other;
    }

    /// Clear the buffer without touching history.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.history_index = None;
        self.history_draft = None;
        self.last_action = LastAction::Other;
    }

    /// True when the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Current cursor position (byte offset, clamped to `buffer.len()`).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Number of history entries (oldest to newest).
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// Push a prompt onto the history (newest at index 0). Empty / pure
    /// whitespace entries are dropped; consecutive duplicates collapse.
    pub fn push_history(&mut self, text: impl Into<String>) {
        let trimmed = text.into();
        let trimmed_trimmed = trimmed.trim();
        if trimmed_trimmed.is_empty() {
            return;
        }
        if self.history.front().map(String::as_str) == Some(trimmed_trimmed) {
            return;
        }
        self.history.push_front(trimmed.trim().to_string());
        while self.history.len() > HISTORY_LIMIT {
            self.history.pop_back();
        }
    }

    /// Drop all history entries.
    pub fn clear_history(&mut self) {
        self.history.clear();
        self.history_index = None;
        self.history_draft = None;
    }

    /// Read-only access to history (oldest first).
    pub fn history(&self) -> impl Iterator<Item = &str> {
        self.history.iter().map(String::as_str)
    }

    /// Move the cursor to the previous history entry. Captures the
    /// current draft the first time so `history_next` can restore it.
    pub fn history_prev(&mut self) -> EditorAction {
        if self.history.is_empty() {
            return EditorAction::None;
        }
        let next = match self.history_index {
            None => 0,
            Some(idx) => idx.saturating_add(1),
        };
        if next >= self.history.len() {
            return EditorAction::None;
        }
        if self.history_index.is_none() {
            self.history_draft = Some(self.buffer.clone());
        }
        self.history_index = Some(next);
        let entry = self.history[next].clone();
        self.set_text(entry);
        EditorAction::Changed
    }

    /// Move the cursor to the next history entry, restoring the saved
    /// draft when navigating past the bottom.
    pub fn history_next(&mut self) -> EditorAction {
        let current = match self.history_index {
            Some(idx) => idx,
            None => return EditorAction::None,
        };
        if current == 0 {
            self.history_index = None;
            let draft = self.history_draft.take().unwrap_or_default();
            self.set_text(draft);
        } else {
            self.history_index = Some(current - 1);
            let entry = self.history[current - 1].clone();
            self.set_text(entry);
        }
        EditorAction::Changed
    }

    /// Insert a character at the cursor.
    pub fn insert_char(&mut self, c: char) -> EditorAction {
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.reset_history_navigation();
        self.last_action = LastAction::Other;
        EditorAction::Changed
    }

    /// Insert a string at the cursor.
    pub fn insert_str(&mut self, s: &str) -> EditorAction {
        if s.is_empty() {
            return EditorAction::None;
        }
        self.buffer.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.reset_history_navigation();
        self.last_action = LastAction::Other;
        EditorAction::Changed
    }

    /// Delete the character before the cursor (`Backspace`).
    pub fn backspace(&mut self) -> EditorAction {
        if self.cursor == 0 {
            return EditorAction::None;
        }
        // Walk back one UTF-8 character.
        let prev = self.prev_char_boundary(self.cursor);
        self.buffer.replace_range(prev..self.cursor, "");
        self.cursor = prev;
        self.reset_history_navigation();
        self.last_action = LastAction::Other;
        EditorAction::Changed
    }

    /// Delete the character at the cursor (`Delete`).
    pub fn delete(&mut self) -> EditorAction {
        if self.cursor >= self.buffer.len() {
            return EditorAction::None;
        }
        let next = self.next_char_boundary(self.cursor);
        self.buffer.replace_range(self.cursor..next, "");
        self.reset_history_navigation();
        self.last_action = LastAction::Other;
        EditorAction::Changed
    }

    /// Move the cursor one character to the left.
    pub fn move_left(&mut self) -> EditorAction {
        if self.cursor == 0 {
            return EditorAction::None;
        }
        self.cursor = self.prev_char_boundary(self.cursor);
        self.last_action = LastAction::Other;
        EditorAction::Changed
    }

    /// Move the cursor one character to the right.
    pub fn move_right(&mut self) -> EditorAction {
        if self.cursor >= self.buffer.len() {
            return EditorAction::None;
        }
        self.cursor = self.next_char_boundary(self.cursor);
        self.last_action = LastAction::Other;
        EditorAction::Changed
    }

    /// Move the cursor to the start of the buffer.
    pub fn move_home(&mut self) -> EditorAction {
        if self.cursor == 0 {
            return EditorAction::None;
        }
        self.cursor = 0;
        self.last_action = LastAction::Other;
        EditorAction::Changed
    }

    /// Move the cursor to the end of the buffer.
    pub fn move_end(&mut self) -> EditorAction {
        let end = self.buffer.len();
        if self.cursor == end {
            return EditorAction::None;
        }
        self.cursor = end;
        self.last_action = LastAction::Other;
        EditorAction::Changed
    }

    /// Kill from the start of the buffer up to the cursor (`Ctrl+U`,
    /// `tui.editor.deleteToLineStart`). The killed text is prepended to
    /// the most recent kill ring entry when the previous action was
    /// also a kill, matching upstream's accumulation rule.
    pub fn kill_to_line_start(&mut self) -> EditorAction {
        if self.cursor == 0 {
            return EditorAction::None;
        }
        let killed = self.buffer[..self.cursor].to_string();
        let accumulate = self.last_action == LastAction::Kill;
        self.buffer.replace_range(..self.cursor, "");
        self.cursor = 0;
        self.kill_ring
            .push(&killed, KillDirection::Prepend, accumulate);
        self.last_action = LastAction::Kill;
        self.reset_history_navigation();
        EditorAction::Changed
    }

    /// Kill from the cursor to the end of the buffer (`Ctrl+K`,
    /// `tui.editor.deleteToLineEnd`). The killed text is appended to the
    /// most recent kill ring entry when the previous action was also a
    /// kill.
    pub fn kill_to_line_end(&mut self) -> EditorAction {
        if self.cursor >= self.buffer.len() {
            return EditorAction::None;
        }
        let killed = self.buffer[self.cursor..].to_string();
        let accumulate = self.last_action == LastAction::Kill;
        self.buffer.truncate(self.cursor);
        self.kill_ring
            .push(&killed, KillDirection::Append, accumulate);
        self.last_action = LastAction::Kill;
        self.reset_history_navigation();
        EditorAction::Changed
    }

    /// Yank the most recent kill ring entry at the cursor (`Ctrl+Y`,
    /// `tui.editor.yank`). No-op when nothing has been killed yet.
    pub fn yank(&mut self) -> EditorAction {
        let Some(text) = self.kill_ring.peek().map(str::to_string) else {
            return EditorAction::None;
        };
        self.buffer.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.last_yank_len = text.len();
        self.last_action = LastAction::Yank;
        self.reset_history_navigation();
        EditorAction::Changed
    }

    /// Replace the text inserted by the previous [`yank`](Self::yank)
    /// with the next-older kill ring entry (`Alt+Y`,
    /// `tui.editor.yankPop`). Only valid immediately after a yank, and
    /// only when the ring holds more than one entry.
    pub fn yank_pop(&mut self) -> EditorAction {
        if self.last_action != LastAction::Yank || self.kill_ring.len() <= 1 {
            return EditorAction::None;
        }
        // Remove the text the previous yank inserted; the cursor sits at
        // its end, so the range is `cursor - last_yank_len .. cursor`.
        let start = self.cursor.saturating_sub(self.last_yank_len);
        self.buffer.replace_range(start..self.cursor, "");
        self.cursor = start;
        // Rotate first, then read: the next entry to insert is now the
        // most recent one (upstream `yankPop` order).
        self.kill_ring.rotate();
        let text = self
            .kill_ring
            .peek()
            .map(str::to_string)
            .unwrap_or_default();
        self.buffer.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.last_yank_len = text.len();
        self.last_action = LastAction::Yank;
        self.reset_history_navigation();
        EditorAction::Changed
    }

    /// Number of entries in the kill ring (oldest to newest).
    pub fn kill_ring_len(&self) -> usize {
        self.kill_ring.len()
    }

    /// Process a key event. Returns the action the [`App`](crate::App)
    /// should take.
    pub fn handle_event(&mut self, event: InputEvent) -> EditorAction {
        let InputEvent::Key(key) = event else {
            return EditorAction::None;
        };
        self.handle_key(key)
    }

    /// Process a key.
    pub fn handle_key(&mut self, key: Key) -> EditorAction {
        // Control chords first.
        if key.modifiers.control {
            match key.code {
                KeyCode::Char('c') | KeyCode::Char('C') => return EditorAction::Interrupt,
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    if self.buffer.is_empty() {
                        return EditorAction::Eof;
                    }
                    return EditorAction::None;
                }
                KeyCode::Char('u') | KeyCode::Char('U') => return self.kill_to_line_start(),
                KeyCode::Char('a') | KeyCode::Char('A') => return self.move_home(),
                KeyCode::Char('e') | KeyCode::Char('E') => return self.move_end(),
                KeyCode::Char('k') | KeyCode::Char('K') => return self.kill_to_line_end(),
                KeyCode::Char('y') | KeyCode::Char('Y') => return self.yank(),
                _ => return EditorAction::None,
            }
        }

        if key.modifiers.alt {
            // `tui.editor.yankPop` is the only Alt binding this editor
            // implements today; word-kill / word-move chords are still
            // pending (see the kill-ring follow-ups).
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                return self.yank_pop();
            }
            return EditorAction::None;
        }

        if key.modifiers.meta {
            return EditorAction::None;
        }

        match key.code {
            KeyCode::Char(c) => self.insert_char(c),
            KeyCode::Enter => {
                let submitted = self.buffer.clone();
                EditorAction::Submit(submitted)
            }
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Home => self.move_home(),
            KeyCode::End => self.move_end(),
            KeyCode::Up => self.history_prev(),
            KeyCode::Down => self.history_next(),
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Esc => EditorAction::None,
            _ => EditorAction::None,
        }
    }

    fn reset_history_navigation(&mut self) {
        self.history_index = None;
        self.history_draft = None;
    }

    /// Find the previous UTF-8 character boundary at or before `pos`.
    /// Returns the byte offset of the start of the character that ends
    /// at or before `pos`. Returns 0 when `pos == 0` or the buffer is
    /// empty.
    fn prev_char_boundary(&self, pos: usize) -> usize {
        if pos == 0 {
            return 0;
        }
        let bytes = self.buffer.as_bytes();
        if bytes.is_empty() {
            return 0;
        }
        // Clamp to the last valid boundary in the string.
        let mut p = pos.min(bytes.len());
        while p > 0 && !is_char_boundary(bytes, p) {
            p -= 1;
        }
        // Now `p` is a boundary at or after any continuation bytes we
        // stepped over; step back one full char so we land at its
        // start.
        if p == 0 {
            return 0;
        }
        p -= 1;
        while p > 0 && !is_char_boundary(bytes, p) {
            p -= 1;
        }
        p
    }

    /// Find the next UTF-8 character boundary at or after `pos`.
    /// Returns the byte offset just past the character that starts at
    /// or after `pos`. Returns the buffer length when no character
    /// follows.
    fn next_char_boundary(&self, pos: usize) -> usize {
        let bytes = self.buffer.as_bytes();
        if bytes.is_empty() {
            return 0;
        }
        let mut p = pos.min(bytes.len());
        while p < bytes.len() && !is_char_boundary(bytes, p) {
            p += 1;
        }
        if p < bytes.len() {
            p += 1;
            while p < bytes.len() && !is_char_boundary(bytes, p) {
                p += 1;
            }
        }
        p
    }
}

/// True when `bytes[pos]` is a UTF-8 character boundary. We can't use
/// `str::is_char_boundary` here because indexing `bytes[pos]` past the
/// end is undefined behaviour in safe Rust, so the helper takes a
/// pre-clamped `pos`.
fn is_char_boundary(bytes: &[u8], pos: usize) -> bool {
    debug_assert!(pos <= bytes.len());
    // `pos == bytes.len()` is always a boundary (the end of the
    // string) and is the only case where indexing would otherwise be
    // out of bounds.
    if pos == bytes.len() {
        return true;
    }
    // UTF-8 continuation bytes have the top two bits set; ASCII bytes
    // do not. A byte is a character boundary iff it is not a
    // continuation byte.
    (bytes[pos] & 0b1100_0000) != 0b1000_0000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    fn key(code: KeyCode, mods: KeyModifiers) -> InputEvent {
        InputEvent::key(code, mods)
    }

    #[test]
    fn insert_and_backspace_round_trip() {
        let mut ed = Editor::new();
        for c in "hello".chars() {
            ed.insert_char(c);
        }
        assert_eq!(ed.text(), "hello");
        assert_eq!(ed.cursor(), 5);
        ed.backspace();
        assert_eq!(ed.text(), "hell");
        assert_eq!(ed.cursor(), 4);
    }

    #[test]
    fn submit_returns_text_and_clears_via_caller() {
        let mut ed = Editor::new();
        ed.insert_str("first prompt");
        let action = ed.handle_key(Key::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            EditorAction::Submit(text) => assert_eq!(text, "first prompt"),
            other => panic!("unexpected action: {:?}", other),
        }
        // The editor does not auto-clear; the App pushes to history
        // and resets after submission.
        assert_eq!(ed.text(), "first prompt");
    }

    #[test]
    fn history_navigation_saves_and_restores_draft() {
        let mut ed = Editor::new();
        ed.push_history("first");
        ed.push_history("second");
        ed.insert_str("draft");

        // Up once — most recent (second).
        assert_eq!(ed.history_prev(), EditorAction::Changed);
        assert_eq!(ed.text(), "second");
        // Up again — older (first).
        assert_eq!(ed.history_prev(), EditorAction::Changed);
        assert_eq!(ed.text(), "first");
        // Up at the top — stays put.
        assert_eq!(ed.history_prev(), EditorAction::None);
        // Down once — back to second.
        assert_eq!(ed.history_next(), EditorAction::Changed);
        assert_eq!(ed.text(), "second");
        // Down once — restores the draft.
        assert_eq!(ed.history_next(), EditorAction::Changed);
        assert_eq!(ed.text(), "draft");
        assert_eq!(ed.cursor(), "draft".len());
    }

    #[test]
    fn history_ignores_consecutive_duplicates() {
        let mut ed = Editor::new();
        ed.push_history("same");
        ed.push_history("same");
        ed.push_history("same");
        assert_eq!(ed.history_len(), 1);
    }

    #[test]
    fn history_capped_at_limit() {
        let mut ed = Editor::new();
        for i in 0..(HISTORY_LIMIT + 50) {
            ed.push_history(format!("entry-{i}"));
        }
        assert_eq!(ed.history_len(), HISTORY_LIMIT);
    }

    #[test]
    fn ctrl_c_interrupts() {
        let mut ed = Editor::new();
        ed.insert_str("partial");
        let action = ed.handle_key(Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(action, EditorAction::Interrupt);
    }

    #[test]
    fn ctrl_d_on_empty_is_eof() {
        let mut ed = Editor::new();
        let action = ed.handle_key(Key::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(action, EditorAction::Eof);
    }

    #[test]
    fn ctrl_d_on_non_empty_is_noop() {
        let mut ed = Editor::new();
        ed.insert_char('x');
        let action = ed.handle_key(Key::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(action, EditorAction::None);
        assert_eq!(ed.text(), "x");
    }

    #[test]
    fn move_home_end_jumps_corners() {
        let mut ed = Editor::new();
        ed.insert_str("hello");
        ed.move_home();
        assert_eq!(ed.cursor(), 0);
        ed.move_end();
        assert_eq!(ed.cursor(), 5);
    }

    #[test]
    fn multibyte_insert_and_backspace() {
        let mut ed = Editor::new();
        for c in "héllo".chars() {
            ed.insert_char(c);
        }
        assert_eq!(ed.text(), "héllo");
        assert_eq!(ed.cursor(), "héllo".len());
        ed.move_home();
        // Walk past "h" and the multi-byte "é" (2 bytes) to land
        // between "é" and "l".
        ed.move_right();
        ed.move_right();
        assert_eq!(ed.cursor(), "h".len() + "é".len());
        // Backspace deletes the "é".
        ed.backspace();
        assert_eq!(ed.text(), "hllo");
        // Cursor should sit between "h" and "l".
        assert_eq!(ed.cursor(), "h".len());
    }

    #[test]
    fn handle_event_dispatches_ignored() {
        let mut ed = Editor::new();
        let action = ed.handle_event(InputEvent::Resize {
            width: 80,
            height: 24,
        });
        assert_eq!(action, EditorAction::None);
    }

    // -- kill ring / yank --------------------------------------------

    fn ctrl(c: char) -> Key {
        Key::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn alt(c: char) -> Key {
        Key::new(KeyCode::Char(c), KeyModifiers::ALT)
    }

    #[test]
    fn ctrl_u_kills_to_start_and_ctrl_y_yanks_it_back() {
        let mut ed = Editor::new();
        ed.insert_str("hello world");
        ed.move_home();
        for _ in 0..6 {
            ed.move_right();
        }
        assert_eq!(ed.cursor(), 6); // after "hello "

        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed);
        assert_eq!(ed.text(), "world");
        assert_eq!(ed.cursor(), 0);

        assert_eq!(ed.handle_key(ctrl('y')), EditorAction::Changed);
        assert_eq!(ed.text(), "hello world");
        assert_eq!(ed.cursor(), 6);
    }

    #[test]
    fn ctrl_k_kills_to_end_and_ctrl_y_yanks_it_back() {
        let mut ed = Editor::new();
        ed.insert_str("hello world");
        ed.move_home();

        assert_eq!(ed.handle_key(ctrl('k')), EditorAction::Changed);
        assert_eq!(ed.text(), "");
        assert_eq!(ed.handle_key(ctrl('y')), EditorAction::Changed);
        assert_eq!(ed.text(), "hello world");
    }

    #[test]
    fn ctrl_y_is_noop_when_kill_ring_is_empty() {
        let mut ed = Editor::new();
        ed.insert_str("test");
        assert_eq!(ed.handle_key(ctrl('y')), EditorAction::None);
        assert_eq!(ed.text(), "test");
    }

    #[test]
    fn consecutive_kills_accumulate_into_one_entry() {
        let mut ed = Editor::new();
        ed.insert_str("abcdef");
        ed.move_home();
        for _ in 0..3 {
            ed.move_right();
        }
        assert_eq!(ed.cursor(), 3);

        // Ctrl+U kills backwards ("abc"), Ctrl+K then kills forwards
        // ("def") and accumulates onto the same ring entry because the
        // previous action was also a kill.
        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed);
        assert_eq!(ed.text(), "def");
        assert_eq!(ed.handle_key(ctrl('k')), EditorAction::Changed);
        assert_eq!(ed.text(), "");
        assert_eq!(ed.kill_ring_len(), 1);

        assert_eq!(ed.handle_key(ctrl('y')), EditorAction::Changed);
        assert_eq!(ed.text(), "abcdef");
    }

    #[test]
    fn non_kill_action_breaks_accumulation() {
        let mut ed = Editor::new();
        ed.insert_str("ab");
        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed); // kill "ab"
        ed.insert_str("cd");
        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed); // new entry
        assert_eq!(ed.kill_ring_len(), 2);
    }

    #[test]
    fn alt_y_cycles_through_kill_ring_after_yank() {
        let mut ed = Editor::new();
        for entry in ["first", "second", "third"] {
            ed.insert_str(entry);
            assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed);
        }
        assert_eq!(ed.kill_ring_len(), 3);

        assert_eq!(ed.handle_key(ctrl('y')), EditorAction::Changed);
        assert_eq!(ed.text(), "third");
        assert_eq!(ed.handle_key(alt('y')), EditorAction::Changed);
        assert_eq!(ed.text(), "second");
        assert_eq!(ed.handle_key(alt('y')), EditorAction::Changed);
        assert_eq!(ed.text(), "first");
        // Rotation wraps back around to the most recent entry.
        assert_eq!(ed.handle_key(alt('y')), EditorAction::Changed);
        assert_eq!(ed.text(), "third");
    }

    #[test]
    fn alt_y_is_noop_without_a_prior_yank() {
        let mut ed = Editor::new();
        ed.insert_str("first");
        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed);
        ed.insert_str("second");
        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed);
        ed.insert_str("draft");

        assert_eq!(ed.handle_key(alt('y')), EditorAction::None);
        assert_eq!(ed.text(), "draft");
    }

    #[test]
    fn alt_y_is_noop_with_a_single_entry() {
        let mut ed = Editor::new();
        ed.insert_str("only");
        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed);
        assert_eq!(ed.handle_key(ctrl('y')), EditorAction::Changed);
        assert_eq!(ed.text(), "only");
        assert_eq!(ed.handle_key(alt('y')), EditorAction::None);
        assert_eq!(ed.text(), "only");
    }

    #[test]
    fn typing_after_yank_breaks_the_yank_pop_chain() {
        let mut ed = Editor::new();
        ed.insert_str("first");
        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed);
        ed.insert_str("second");
        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed);

        assert_eq!(ed.handle_key(ctrl('y')), EditorAction::Changed);
        assert_eq!(ed.text(), "second");
        ed.insert_char('x');
        assert_eq!(ed.handle_key(alt('y')), EditorAction::None);
        assert_eq!(ed.text(), "secondx");
    }

    #[test]
    fn yank_and_yank_pop_in_the_middle_of_text() {
        let mut ed = Editor::new();
        ed.insert_str("one");
        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed);
        ed.insert_str("two");
        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed);

        ed.insert_str("ab");
        ed.move_home();
        ed.move_right();
        assert_eq!(ed.handle_key(ctrl('y')), EditorAction::Changed);
        assert_eq!(ed.text(), "atwob");
        assert_eq!(ed.handle_key(alt('y')), EditorAction::Changed);
        assert_eq!(ed.text(), "aoneb");
        assert_eq!(ed.cursor(), 4);
    }

    #[test]
    fn kill_and_yank_are_utf8_safe() {
        let mut ed = Editor::new();
        ed.insert_str("héllo 世界");
        ed.move_home();
        // Walk to just after "héllo ".
        for _ in 0..6 {
            ed.move_right();
        }
        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed);
        assert_eq!(ed.text(), "世界");
        assert_eq!(ed.handle_key(ctrl('y')), EditorAction::Changed);
        assert_eq!(ed.text(), "héllo 世界");
        assert_eq!(ed.cursor(), "héllo ".len());
    }

    #[test]
    fn up_down_history_navigation_breaks_kill_accumulation() {
        let mut ed = Editor::new();
        ed.push_history("older");
        ed.insert_str("ab");
        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed);
        // History navigation is not a kill, so the next kill starts a
        // new ring entry.
        assert_eq!(ed.history_prev(), EditorAction::Changed);
        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed);
        assert_eq!(ed.kill_ring_len(), 2);
    }
}
