//! Single-line editor component with prompt history.
//!
//! Mirrors the prompt-history portion of `packages/tui/components/editor.ts`:
//!
//! * Plain character keys append to the buffer at the cursor.
//! * `Backspace` deletes the character before the cursor.
//! * `Delete` deletes the character at the cursor.
//! * `Left` / `Right` move the cursor by one grapheme; `Home` / `End`
//!   jump to the start / end. `Alt+B` / `Alt+F` (also `Alt+Left` /
//!   `Alt+Right` and `Ctrl+Left` / `Ctrl+Right`) move by one word
//!   (`tui.editor.cursorWordLeft` / `cursorWordRight`) using the
//!   boundaries from [`crate::word_navigation`].
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
//! * `Ctrl+W` / `Alt+Backspace` kill the word before the cursor and
//!   `Alt+D` / `Alt+Delete` the word after it
//!   (`tui.editor.deleteWordBackward` / `deleteWordForward`). Word kills
//!   take part in the same accumulation chain as the line kills.
//! * `Ctrl+Y` yanks the most recent ring entry at the cursor and
//!   `Alt+Y` cycles through older entries (`tui.editor.yank` /
//!   `tui.editor.yankPop`).
//! * `Ctrl+-` undoes the previous edit (`tui.editor.undo`) by restoring
//!   a snapshot from the [`UndoStack`]. Snapshots are captured before
//!   destructive edits, and consecutive word characters coalesce into a
//!   single undo unit (fish-style, mirroring upstream `insertCharacter`):
//!   one undo removes a whole typed word, while every space stays
//!   separately undoable. Submitting (`clear`) drops the stack.
//!
//! History is stored in a [`VecDeque`] capped at 100 entries (matching
//! the TS implementation); consecutive duplicates are collapsed.
//!
//! [`App`]: crate::App

use std::collections::VecDeque;

use crate::input::{InputEvent, Key, KeyCode};
use crate::kill_ring::{KillDirection, KillRing};
use crate::undo_stack::UndoStack;
use crate::word_navigation::{find_word_backward, find_word_forward};

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
/// accumulates into the most recent ring entry, whether `Alt+Y` is
/// allowed to cycle a fresh yank, and whether a typed character
/// coalesces into the current undo unit. Mirrors upstream's
/// `lastAction` field (`"kill"` / `"yank"` / `"type-word"` /
/// everything else).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastAction {
    /// Anything that is not a kill, yank or word character — breaks all
    /// three chains.
    Other,
    /// The previous action killed text, so the next kill accumulates.
    Kill,
    /// The previous action yanked text, so `Alt+Y` may cycle it.
    Yank,
    /// The previous action typed a word character, so the next word
    /// character joins the same undo unit instead of opening a new one.
    TypeWord,
}

/// Editor state captured by an undo snapshot.
///
/// Upstream stores its multi-line `EditorState` plus the paste tables;
/// the Rust editor is single-line, so the buffer and cursor are the
/// whole of it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EditorSnapshot {
    /// Buffer contents at capture time.
    buffer: String,
    /// Cursor byte offset at capture time.
    cursor: usize,
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
    /// Snapshots restored by `Ctrl+-` (`tui.editor.undo`).
    undo_stack: UndoStack<EditorSnapshot>,
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
            undo_stack: UndoStack::new(),
        }
    }

    /// Borrow the current buffer.
    pub fn text(&self) -> &str {
        &self.buffer
    }

    /// Replace the buffer (cursor is clamped to the new length).
    ///
    /// Mirrors upstream `setText`: when the text actually changes, the
    /// previous state is pushed onto the undo stack (so a programmatic
    /// replacement is undoable), history browsing is exited, and any
    /// kill / yank / typing chain is broken.
    pub fn set_text(&mut self, text: impl Into<String>) {
        let text = text.into();
        if text != self.buffer {
            self.push_undo_snapshot();
        }
        self.reset_history_navigation();
        self.set_text_internal(text);
    }

    /// Set the buffer without touching the undo stack or the history
    /// navigation state. Used by [`history_prev`](Self::history_prev) /
    /// [`history_next`](Self::history_next), where resetting the history
    /// index would trap the user at the same entry on every Up arrow.
    fn set_text_internal(&mut self, text: impl Into<String>) {
        self.buffer = text.into();
        self.cursor = self.buffer.len();
        self.last_action = LastAction::Other;
    }

    /// Clear the buffer without touching history, and drop the undo
    /// stack. Upstream clears its stack the same way when a prompt is
    /// submitted, so `Ctrl+-` cannot resurrect an already-sent prompt.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.history_index = None;
        self.history_draft = None;
        self.last_action = LastAction::Other;
        self.undo_stack.clear();
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
            // Entering history browsing is undoable: `Ctrl+-` restores
            // the draft the user was typing. Mirrors upstream
            // `navigateHistory` capturing the state on first entry.
            self.push_undo_snapshot();
            self.history_draft = Some(self.buffer.clone());
        }
        self.history_index = Some(next);
        let entry = self.history[next].clone();
        self.set_text_internal(entry);
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
            self.set_text_internal(draft);
        } else {
            self.history_index = Some(current - 1);
            let entry = self.history[current - 1].clone();
            self.set_text_internal(entry);
        }
        EditorAction::Changed
    }

    /// Insert a character at the cursor.
    ///
    /// Undo coalescing (fish-style, see upstream `insertCharacter`): a
    /// snapshot is captured when the previous action was not typing a
    /// word character, or when this character is whitespace — which
    /// makes the state *before* the space the restore point, so undoing
    /// a space removes the space together with the word after it.
    pub fn insert_char(&mut self, c: char) -> EditorAction {
        if c.is_whitespace() || self.last_action != LastAction::TypeWord {
            self.push_undo_snapshot();
        }
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.reset_history_navigation();
        self.last_action = LastAction::TypeWord;
        EditorAction::Changed
    }

    /// Insert a string at the cursor.
    ///
    /// Atomic for undo — one snapshot, so a single `Ctrl+-` removes the
    /// whole string. Mirrors upstream `insertTextAtCursor`, which is the
    /// path a bracketed paste takes.
    pub fn insert_str(&mut self, s: &str) -> EditorAction {
        if s.is_empty() {
            return EditorAction::None;
        }
        self.push_undo_snapshot();
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
        self.push_undo_snapshot();
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
        self.push_undo_snapshot();
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

    /// Move the cursor one word to the left (`Alt+B`, `Alt+Left` or
    /// `Ctrl+Left`, `tui.editor.cursorWordLeft`). Trailing whitespace is
    /// skipped, then the cursor stops at the next word / punctuation
    /// boundary.
    pub fn move_word_left(&mut self) -> EditorAction {
        if self.cursor == 0 {
            return EditorAction::None;
        }
        self.cursor = find_word_backward(&self.buffer, self.cursor);
        self.last_action = LastAction::Other;
        EditorAction::Changed
    }

    /// Move the cursor one word to the right (`Alt+F`, `Alt+Right` or
    /// `Ctrl+Right`, `tui.editor.cursorWordRight`). Leading whitespace is
    /// skipped, then the cursor stops at the next word / punctuation
    /// boundary.
    pub fn move_word_right(&mut self) -> EditorAction {
        if self.cursor >= self.buffer.len() {
            return EditorAction::None;
        }
        self.cursor = find_word_forward(&self.buffer, self.cursor);
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
        self.push_undo_snapshot();
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
        self.push_undo_snapshot();
        let killed = self.buffer[self.cursor..].to_string();
        let accumulate = self.last_action == LastAction::Kill;
        self.buffer.truncate(self.cursor);
        self.kill_ring
            .push(&killed, KillDirection::Append, accumulate);
        self.last_action = LastAction::Kill;
        self.reset_history_navigation();
        EditorAction::Changed
    }

    /// Kill the word before the cursor (`Ctrl+W` / `Alt+Backspace`,
    /// `tui.editor.deleteWordBackward`). The killed text is prepended to
    /// the most recent kill ring entry when the previous action was also
    /// a kill, matching upstream's accumulation rule.
    pub fn kill_word_backward(&mut self) -> EditorAction {
        if self.cursor == 0 {
            return EditorAction::None;
        }
        let delete_from = find_word_backward(&self.buffer, self.cursor);
        if delete_from == self.cursor {
            return EditorAction::None;
        }
        self.push_undo_snapshot();
        let killed = self.buffer[delete_from..self.cursor].to_string();
        // Read the previous action *before* overwriting it: a kill that
        // follows another kill accumulates into the same ring entry
        // instead of opening a new chain (upstream `deleteWordBackwards`).
        let accumulate = self.last_action == LastAction::Kill;
        self.buffer.replace_range(delete_from..self.cursor, "");
        self.cursor = delete_from;
        self.kill_ring
            .push(&killed, KillDirection::Prepend, accumulate);
        self.last_action = LastAction::Kill;
        self.reset_history_navigation();
        EditorAction::Changed
    }

    /// Kill the word after the cursor (`Alt+D` / `Alt+Delete`,
    /// `tui.editor.deleteWordForward`). The killed text is appended to
    /// the most recent kill ring entry when the previous action was also
    /// a kill.
    pub fn kill_word_forward(&mut self) -> EditorAction {
        if self.cursor >= self.buffer.len() {
            return EditorAction::None;
        }
        let delete_to = find_word_forward(&self.buffer, self.cursor);
        if delete_to == self.cursor {
            return EditorAction::None;
        }
        self.push_undo_snapshot();
        let killed = self.buffer[self.cursor..delete_to].to_string();
        let accumulate = self.last_action == LastAction::Kill;
        self.buffer.replace_range(self.cursor..delete_to, "");
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
        self.push_undo_snapshot();
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
        self.push_undo_snapshot();
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

    /// Undo the most recent edit (`Ctrl+-`, `tui.editor.undo`).
    ///
    /// Pops the most recent snapshot and restores the buffer and cursor;
    /// history browsing is exited and the kill / yank / typing chain is
    /// broken. No-op when the stack is empty.
    pub fn undo(&mut self) -> EditorAction {
        let Some(snapshot) = self.undo_stack.pop() else {
            return EditorAction::None;
        };
        self.buffer = snapshot.buffer;
        self.cursor = snapshot.cursor.min(self.buffer.len());
        self.last_action = LastAction::Other;
        self.reset_history_navigation();
        EditorAction::Changed
    }

    /// Number of undo snapshots currently available to `Ctrl+-`.
    pub fn undo_len(&self) -> usize {
        self.undo_stack.len()
    }

    /// Capture the current buffer and cursor for `Ctrl+-`.
    fn push_undo_snapshot(&mut self) {
        self.undo_stack.push(&EditorSnapshot {
            buffer: self.buffer.clone(),
            cursor: self.cursor,
        });
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
                // `tui.editor.deleteWordBackward`.
                KeyCode::Char('w') | KeyCode::Char('W') => return self.kill_word_backward(),
                // `tui.editor.cursorWordLeft` / `cursorWordRight`.
                KeyCode::Left => return self.move_word_left(),
                KeyCode::Right => return self.move_word_right(),
                KeyCode::Char('y') | KeyCode::Char('Y') => return self.yank(),
                // `tui.editor.undo` is bound to `ctrl+-`. Kitty-protocol
                // terminals send the CSI-u sequence for `-`, which lands
                // here as `Ctrl+-`; `Ctrl+_` is accepted too because the
                // legacy byte for both is `0x1F`.
                KeyCode::Char('-') | KeyCode::Char('_') => return self.undo(),
                _ => return EditorAction::None,
            }
        }

        if key.modifiers.alt {
            // Word navigation plus the yank-pop cycle; every other Alt
            // chord this editor does not implement is a no-op.
            return match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.yank_pop(),
                // `tui.editor.cursorWordLeft` / `cursorWordRight`.
                KeyCode::Char('b') | KeyCode::Char('B') | KeyCode::Left => self.move_word_left(),
                KeyCode::Char('f') | KeyCode::Char('F') | KeyCode::Right => self.move_word_right(),
                // `tui.editor.deleteWordBackward` / `deleteWordForward`.
                KeyCode::Backspace => self.kill_word_backward(),
                KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Delete => {
                    self.kill_word_forward()
                }
                _ => EditorAction::None,
            };
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

    // -- undo --------------------------------------------------------

    fn ctrl_minus() -> Key {
        Key::new(KeyCode::Char('-'), KeyModifiers::CONTROL)
    }

    /// Type a string one character at a time, the way a terminal
    /// delivers keystrokes. This is what exercises the fish-style
    /// coalescing rules (a bulk `insert_str` is atomic instead).
    fn type_chars(ed: &mut Editor, text: &str) {
        for c in text.chars() {
            ed.insert_char(c);
        }
    }

    #[test]
    fn undo_is_noop_when_the_stack_is_empty() {
        let mut ed = Editor::new();
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::None);
        assert_eq!(ed.text(), "");
    }

    #[test]
    fn undo_coalesces_consecutive_word_chars_into_one_unit() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "hello world");
        assert_eq!(ed.text(), "hello world");

        // The space captured the state *before* itself, so one undo
        // drops the space together with the word after it.
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "hello");
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "");
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::None);
    }

    #[test]
    fn undo_removes_spaces_one_at_a_time() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "hello  ");
        assert_eq!(ed.text(), "hello  ");

        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "hello ");
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "hello");
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "");
    }

    #[test]
    fn undo_restores_backspace_and_the_cursor() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "hello");
        ed.backspace();
        assert_eq!(ed.text(), "hell");

        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "hello");
        // The snapshot carries the cursor position from before the
        // deletion.
        assert_eq!(ed.cursor(), 5);
    }

    #[test]
    fn undo_restores_a_forward_delete() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "hello");
        ed.move_home();
        ed.move_right();
        ed.delete();
        assert_eq!(ed.text(), "hllo");

        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "hello");
        assert_eq!(ed.cursor(), 1);
    }

    #[test]
    fn undo_restores_ctrl_u() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "hello world");
        ed.move_home();
        for _ in 0..6 {
            ed.move_right();
        }
        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed);
        assert_eq!(ed.text(), "world");

        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "hello world");
        assert_eq!(ed.cursor(), 6);
    }

    #[test]
    fn undo_restores_ctrl_k() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "hello world");
        ed.move_home();
        for _ in 0..6 {
            ed.move_right();
        }
        assert_eq!(ed.handle_key(ctrl('k')), EditorAction::Changed);
        assert_eq!(ed.text(), "hello ");

        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "hello world");
        assert_eq!(ed.cursor(), 6);
    }

    #[test]
    fn undo_restores_a_yank() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "hello ");
        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed);
        assert_eq!(ed.text(), "");
        assert_eq!(ed.handle_key(ctrl('y')), EditorAction::Changed);
        assert_eq!(ed.text(), "hello ");

        // One undo removes the yanked text …
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "");
        // … and the next restores what the kill removed.
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "hello ");
    }

    #[test]
    fn undo_restores_a_yank_pop() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "first");
        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed);
        type_chars(&mut ed, "second");
        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed);

        assert_eq!(ed.handle_key(ctrl('y')), EditorAction::Changed);
        assert_eq!(ed.text(), "second");
        assert_eq!(ed.handle_key(alt('y')), EditorAction::Changed);
        assert_eq!(ed.text(), "first");

        // yank-pop pushed its own snapshot, so one undo goes back to the
        // text the first yank inserted.
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "second");
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "");
    }

    #[test]
    fn undo_after_insert_str_is_atomic() {
        let mut ed = Editor::new();
        ed.insert_str("hello world");
        ed.move_home();
        for _ in 0..5 {
            ed.move_right();
        }
        // A bracketed paste arrives as one string insert.
        ed.insert_str("beep boop");
        assert_eq!(ed.text(), "hellobeep boop world");

        // A single undo restores the entire pre-insert state.
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "hello world");
        assert_eq!(ed.cursor(), 5);
    }

    #[test]
    fn undo_restores_the_draft_after_history_browsing() {
        let mut ed = Editor::new();
        ed.push_history("older");
        type_chars(&mut ed, "draft");

        assert_eq!(ed.history_prev(), EditorAction::Changed);
        assert_eq!(ed.text(), "older");
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "draft");
        assert_eq!(ed.cursor(), 5);
    }

    #[test]
    fn repeated_history_navigation_pushes_a_single_snapshot() {
        let mut ed = Editor::new();
        ed.push_history("one");
        ed.push_history("two");
        let before = ed.undo_len();

        assert_eq!(ed.history_prev(), EditorAction::Changed);
        assert_eq!(ed.undo_len(), before + 1);
        assert_eq!(ed.history_prev(), EditorAction::Changed);
        assert_eq!(ed.undo_len(), before + 1);
    }

    #[test]
    fn cursor_only_moves_do_not_push_a_snapshot() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "abc");
        let before = ed.undo_len();
        ed.move_home();
        ed.move_end();
        ed.move_left();
        assert_eq!(ed.undo_len(), before);
    }

    #[test]
    fn clear_drops_the_undo_stack() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "sent");
        assert!(ed.undo_len() > 0);

        ed.clear();
        assert_eq!(ed.undo_len(), 0);
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::None);
        assert_eq!(ed.text(), "");
    }

    #[test]
    fn set_text_pushes_a_snapshot_only_when_the_text_changes() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "abc");
        let before = ed.undo_len();

        ed.set_text("abc");
        assert_eq!(ed.undo_len(), before);

        ed.set_text("xyz");
        assert_eq!(ed.undo_len(), before + 1);
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "abc");
    }

    #[test]
    fn ctrl_underscore_is_an_undo_alias() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "ab");
        let action = ed.handle_key(Key::new(KeyCode::Char('_'), KeyModifiers::CONTROL));
        assert_eq!(action, EditorAction::Changed);
        assert_eq!(ed.text(), "");
    }

    #[test]
    fn undo_is_utf8_safe() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "héllo 世界");
        assert_eq!(ed.handle_key(ctrl('u')), EditorAction::Changed);
        assert_eq!(ed.text(), "");

        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "héllo 世界");
        assert_eq!(ed.cursor(), "héllo 世界".len());

        ed.backspace();
        assert_eq!(ed.text(), "héllo 世");
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "héllo 世界");
    }

    #[test]
    fn alt_b_and_alt_f_move_by_word() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "hello world");

        assert_eq!(ed.handle_key(alt('b')), EditorAction::Changed);
        assert_eq!(ed.cursor(), 6);
        assert_eq!(ed.handle_key(alt('b')), EditorAction::Changed);
        assert_eq!(ed.cursor(), 0);
        // At the start of the buffer the move is a no-op.
        assert_eq!(ed.handle_key(alt('b')), EditorAction::None);

        assert_eq!(ed.handle_key(alt('f')), EditorAction::Changed);
        assert_eq!(ed.cursor(), 5);
        assert_eq!(ed.handle_key(alt('f')), EditorAction::Changed);
        assert_eq!(ed.cursor(), 11);
        // At the end of the buffer the move is a no-op.
        assert_eq!(ed.handle_key(alt('f')), EditorAction::None);
    }

    #[test]
    fn alt_arrows_and_ctrl_arrows_move_by_word() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "path/to/file");

        assert_eq!(
            ed.handle_key(Key::new(KeyCode::Left, KeyModifiers::ALT)),
            EditorAction::Changed
        );
        assert_eq!(ed.cursor(), 8); // start of "file"
        assert_eq!(
            ed.handle_key(Key::new(KeyCode::Left, KeyModifiers::ALT)),
            EditorAction::Changed
        );
        assert_eq!(ed.cursor(), 7); // the "/" boundary
        assert_eq!(
            ed.handle_key(Key::new(KeyCode::Right, KeyModifiers::CONTROL)),
            EditorAction::Changed
        );
        assert_eq!(ed.cursor(), 8);
        assert_eq!(
            ed.handle_key(Key::new(KeyCode::Right, KeyModifiers::ALT)),
            EditorAction::Changed
        );
        assert_eq!(ed.cursor(), 12);
    }

    #[test]
    fn ctrl_w_kills_the_word_before_the_cursor() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "hello world");

        assert_eq!(ed.handle_key(ctrl('w')), EditorAction::Changed);
        assert_eq!(ed.text(), "hello ");
        assert_eq!(ed.cursor(), 6);
        // The killed word went onto the ring, so Ctrl+Y restores it.
        assert_eq!(ed.handle_key(ctrl('y')), EditorAction::Changed);
        assert_eq!(ed.text(), "hello world");
        assert_eq!(ed.cursor(), 11);
    }

    #[test]
    fn alt_backspace_is_a_kill_word_backward_alias() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "one two");
        assert_eq!(
            ed.handle_key(Key::new(KeyCode::Backspace, KeyModifiers::ALT)),
            EditorAction::Changed
        );
        assert_eq!(ed.text(), "one ");
        assert_eq!(ed.cursor(), 4);
    }

    #[test]
    fn alt_d_kills_the_word_after_the_cursor() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "hello world");
        ed.move_home();

        assert_eq!(ed.handle_key(alt('d')), EditorAction::Changed);
        assert_eq!(ed.text(), " world");
        assert_eq!(ed.cursor(), 0);
        // The kill is forward, so the ring entry is the whole word.
        assert_eq!(ed.handle_key(ctrl('y')), EditorAction::Changed);
        assert_eq!(ed.text(), "hello world");
    }

    #[test]
    fn alt_delete_is_a_kill_word_forward_alias() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "one two");
        ed.move_home();
        assert_eq!(
            ed.handle_key(Key::new(KeyCode::Delete, KeyModifiers::ALT)),
            EditorAction::Changed
        );
        assert_eq!(ed.text(), " two");
        assert_eq!(ed.cursor(), 0);
    }

    #[test]
    fn consecutive_word_kills_accumulate_into_one_ring_entry() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "one two three");

        assert_eq!(ed.handle_key(ctrl('w')), EditorAction::Changed);
        assert_eq!(ed.handle_key(ctrl('w')), EditorAction::Changed);
        assert_eq!(ed.text(), "one ");
        assert_eq!(ed.kill_ring_len(), 1);
        // One yank restores both words, in reading order.
        assert_eq!(ed.handle_key(ctrl('y')), EditorAction::Changed);
        assert_eq!(ed.text(), "one two three");
    }

    #[test]
    fn word_kills_are_undoable() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "hello world");

        assert_eq!(ed.handle_key(ctrl('w')), EditorAction::Changed);
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "hello world");
        assert_eq!(ed.cursor(), 11);

        ed.move_home();
        assert_eq!(ed.handle_key(alt('d')), EditorAction::Changed);
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "hello world");
        assert_eq!(ed.cursor(), 0);
    }

    #[test]
    fn word_kills_at_the_buffer_corners_are_noops() {
        let mut ed = Editor::new();
        assert_eq!(ed.handle_key(ctrl('w')), EditorAction::None);
        assert_eq!(ed.handle_key(alt('d')), EditorAction::None);
        assert_eq!(ed.kill_ring_len(), 0);

        type_chars(&mut ed, "word");
        assert_eq!(ed.handle_key(alt('d')), EditorAction::None);
        assert_eq!(ed.kill_ring_len(), 0);
        assert_eq!(ed.text(), "word");
    }

    #[test]
    fn word_kills_are_utf8_safe() {
        let mut ed = Editor::new();
        type_chars(&mut ed, "héllo wörld");

        assert_eq!(ed.handle_key(ctrl('w')), EditorAction::Changed);
        assert_eq!(ed.text(), "héllo ");
        assert_eq!(ed.cursor(), "héllo ".len());
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "héllo wörld");
        assert_eq!(ed.cursor(), "héllo wörld".len());
    }

    #[test]
    fn word_kill_lands_on_a_char_boundary_for_cjk() {
        // UAX #29 splits each ideograph while ICU's dictionary groups
        // them into words, so the exact split is engine-dependent. What
        // must hold either way: the cursor stays on a char boundary and
        // the kill is undoable.
        let mut ed = Editor::new();
        type_chars(&mut ed, "你好 世界");

        assert_eq!(ed.handle_key(ctrl('w')), EditorAction::Changed);
        assert!(ed.text().is_char_boundary(ed.cursor()));
        assert!(ed.text().starts_with("你好 "));
        assert!(ed.text().len() < "你好 世界".len());
        assert_eq!(ed.handle_key(ctrl_minus()), EditorAction::Changed);
        assert_eq!(ed.text(), "你好 世界");
        assert_eq!(ed.cursor(), "你好 世界".len());
    }
}
