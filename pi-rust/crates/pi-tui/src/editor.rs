//! Single-line editor component with prompt history.
//!
//! Mirrors the prompt-history portion of `packages/tui/components/editor.ts`:
//!
//! * Plain character keys append to the buffer at the cursor.
//! * `Backspace` deletes the character before the cursor.
//! * `Delete` deletes the character at the cursor.
//! * `Left` / `Right` move the cursor by one grapheme, with `Ctrl+B` /
//!   `Ctrl+F` as the emacs aliases (`tui.editor.cursorLeft` /
//!   `cursorRight` default to `["left", "ctrl+b"]` /
//!   `["right", "ctrl+f"]`); `Home` / `End` jump to the start / end.
//!   `Alt+B` / `Alt+F` (also `Alt+Left` / `Alt+Right` and `Ctrl+Left` /
//!   `Ctrl+Right`) move by one word (`tui.editor.cursorWordLeft` /
//!   `cursorWordRight`) using the boundaries from
//!   [`crate::word_navigation`].
//! * `Up` / `Down` navigate the prompt history (most recent first). The
//!   first `Up` saves the current draft so `Down` past the bottom of
//!   the history restores it.
//! * `Enter` returns [`EditorAction::Submit`] with the buffer text.
//! * `Ctrl+C` returns [`EditorAction::Interrupt`].
//! * `Ctrl+D` is `tui.editor.deleteCharForward`'s second default
//!   binding (`packages/tui/src/keybindings.ts:121`). On an empty
//!   buffer it returns [`EditorAction::Eof`] so the caller can exit; on
//!   a non-empty buffer it deletes the character at the cursor, exactly
//!   like upstream's `custom-editor.ts:117` fall-through.
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
//!   separately undoable. Submitting (`clear`) drops the stack. Legacy
//!   terminals send one `0x1F` byte for the chord, which upstream
//!   normalizes to `ctrl+-` (`keys.ts:1277`) and crossterm reports as
//!   `Ctrl+7`, so `Ctrl+7` / `Ctrl+_` are accepted as aliases of it.
//! * `Ctrl+]` / `Ctrl+Alt+]` (`tui.editor.jumpForward` /
//!   `tui.editor.jumpBackward`) arm "jump mode": the next printable
//!   character moves the cursor to its next (resp. previous) occurrence
//!   instead of inserting. Pressing either hotkey again, or any key that
//!   is not a plain printable character, cancels the mode — and a
//!   cancelling key still performs its normal action, exactly like
//!   upstream `Editor.handleInput`. A typed character with no match
//!   leaves the cursor where it was. Matching is case-sensitive and the
//!   character under the cursor is never a match. Legacy terminals send
//!   `0x1D` / `ESC 0x1D`, which crossterm decodes as `Ctrl+5` /
//!   `Ctrl+Alt+5`, so both spellings are accepted — the same legacy
//!   translation upstream performs in `packages/tui/src/keys.ts:1276`.
//! * When an [`AutocompleteProvider`] is installed
//!   ([`Editor::set_autocomplete_provider`]), typing `/` at the start of
//!   the line or `@` (and `#`) at a token boundary opens a candidate
//!   dropdown: `Up` / `Down` move the selection, `Tab` and `Enter` apply
//!   it, `Esc` dismisses it. `Tab` while the dropdown is closed forces a
//!   completion. This mirrors upstream `Editor.handleInput`'s
//!   autocomplete block; the provider is opt-in, so a caller that never
//!   installs one (the [`App`] / [`Prompt`] path) keeps the
//!   pre-autocomplete behaviour byte for byte.
//!
//! History is stored in a [`VecDeque`] capped at 100 entries (matching
//! the TS implementation); consecutive duplicates are collapsed.
//!
//! [`App`]: crate::App
//! [`Prompt`]: crate::Prompt

use std::collections::VecDeque;
use std::sync::Arc;

use crate::autocomplete::{AutocompleteItem, AutocompleteProvider};
use crate::input::{InputEvent, Key, KeyCode};
use crate::kill_ring::{KillDirection, KillRing};
use crate::undo_stack::UndoStack;
use crate::word_navigation::{find_word_backward, find_word_forward};

#[cfg(test)]
use crate::input::KeyModifiers;

/// Maximum number of history entries kept by the editor.
pub const HISTORY_LIMIT: usize = 100;

/// Characters that open the autocomplete dropdown at a token boundary
/// by default, upstream `DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS`.
pub const DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS: [char; 2] = ['@', '#'];

/// Default number of dropdown rows, upstream `autocompleteMaxVisible = 5`.
pub const DEFAULT_AUTOCOMPLETE_MAX_VISIBLE: usize = 5;

/// Lower clamp for the dropdown height, upstream's `Math.max(3, …)`.
pub const MIN_AUTOCOMPLETE_MAX_VISIBLE: usize = 3;

/// Upper clamp for the dropdown height, upstream's `Math.min(20, …)`.
pub const MAX_AUTOCOMPLETE_MAX_VISIBLE: usize = 20;

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

/// Direction of a pending editor jump (`tui.editor.jumpForward` /
/// `tui.editor.jumpBackward`).
///
/// While a jump is armed ([`Editor::jump_mode`]) the next printable key
/// is consumed as the jump target rather than inserted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JumpDirection {
    /// `Ctrl+]` — search towards the end of the buffer.
    Forward,
    /// `Ctrl+Alt+]` — search towards the start of the buffer.
    Backward,
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
    /// Armed `tui.editor.jumpForward` / `jumpBackward` target, if any.
    /// Set by `Ctrl+]` / `Ctrl+Alt+]` and consumed by the next key.
    jump_mode: Option<JumpDirection>,
    /// Optional provider driving the autocomplete dropdown. `None`
    /// disables the feature entirely, which is the default so existing
    /// callers keep their behaviour.
    autocomplete_provider: Option<Arc<dyn AutocompleteProvider>>,
    /// Characters that open the dropdown at a token boundary.
    autocomplete_trigger_characters: Vec<char>,
    /// Candidates currently shown (empty when the dropdown is closed).
    autocomplete_items: Vec<AutocompleteItem>,
    /// Index of the highlighted candidate.
    autocomplete_selected: usize,
    /// Prefix the candidates were computed for, passed back to the
    /// provider when one is applied.
    autocomplete_prefix: String,
    /// Whether the open dropdown came from an explicit Tab request
    /// (upstream's `"force"` state), so refreshing keeps forcing.
    autocomplete_force: bool,
    /// Dropdown height in rows (clamped to
    /// [`MIN_AUTOCOMPLETE_MAX_VISIBLE`]..=[`MAX_AUTOCOMPLETE_MAX_VISIBLE`]).
    autocomplete_max_visible: usize,
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
            jump_mode: None,
            autocomplete_provider: None,
            autocomplete_trigger_characters: DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS.to_vec(),
            autocomplete_items: Vec::new(),
            autocomplete_selected: 0,
            autocomplete_prefix: String::new(),
            autocomplete_force: false,
            autocomplete_max_visible: DEFAULT_AUTOCOMPLETE_MAX_VISIBLE,
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
        self.cancel_autocomplete();
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
        self.jump_mode = None;
        self.cancel_autocomplete();
    }

    /// The armed jump direction, if `Ctrl+]` / `Ctrl+Alt+]` is waiting
    /// for its target character.
    pub fn jump_mode(&self) -> Option<JumpDirection> {
        self.jump_mode
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
        self.update_autocomplete_after_edit(Some(c));
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
        self.cancel_autocomplete();
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
        self.update_autocomplete_after_edit(None);
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
        self.update_autocomplete_after_edit(None);
        EditorAction::Changed
    }

    /// Move the cursor one character to the left.
    pub fn move_left(&mut self) -> EditorAction {
        if self.cursor == 0 {
            return EditorAction::None;
        }
        self.cursor = self.prev_char_boundary(self.cursor);
        self.last_action = LastAction::Other;
        self.refresh_autocomplete_if_open();
        EditorAction::Changed
    }

    /// Move the cursor one character to the right.
    pub fn move_right(&mut self) -> EditorAction {
        if self.cursor >= self.buffer.len() {
            return EditorAction::None;
        }
        self.cursor = self.next_char_boundary(self.cursor);
        self.last_action = LastAction::Other;
        self.refresh_autocomplete_if_open();
        EditorAction::Changed
    }

    /// Move the cursor to the start of the buffer.
    pub fn move_home(&mut self) -> EditorAction {
        if self.cursor == 0 {
            return EditorAction::None;
        }
        self.cursor = 0;
        self.last_action = LastAction::Other;
        self.refresh_autocomplete_if_open();
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
        self.refresh_autocomplete_if_open();
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
        self.refresh_autocomplete_if_open();
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
        self.refresh_autocomplete_if_open();
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
        self.cancel_autocomplete();
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
        self.cancel_autocomplete();
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
        self.cancel_autocomplete();
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
        self.cancel_autocomplete();
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
        self.cancel_autocomplete();
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
        self.cancel_autocomplete();
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
        self.cancel_autocomplete();
        self.reset_history_navigation();
        EditorAction::Changed
    }

    /// Number of undo snapshots currently available to `Ctrl+-`.
    pub fn undo_len(&self) -> usize {
        self.undo_stack.len()
    }

    // -- autocomplete -------------------------------------------------

    /// Install the autocomplete provider driving the dropdown, or
    /// replace the current one. The provider's extra
    /// [`trigger_characters`](AutocompleteProvider::trigger_characters)
    /// are merged into the editor defaults (`@`, `#`).
    pub fn set_autocomplete_provider(&mut self, provider: Arc<dyn AutocompleteProvider>) {
        for trigger in provider.trigger_characters() {
            if self.is_valid_trigger_character(*trigger)
                && !self.autocomplete_trigger_characters.contains(trigger)
            {
                self.autocomplete_trigger_characters.push(*trigger);
            }
        }
        self.autocomplete_provider = Some(provider);
        self.cancel_autocomplete();
    }

    /// Remove the autocomplete provider and close any open dropdown,
    /// restoring the default trigger characters.
    pub fn clear_autocomplete_provider(&mut self) {
        self.autocomplete_provider = None;
        self.autocomplete_trigger_characters = DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS.to_vec();
        self.cancel_autocomplete();
    }

    /// True when the autocomplete dropdown currently has candidates.
    pub fn is_showing_autocomplete(&self) -> bool {
        !self.autocomplete_items.is_empty()
    }

    /// The candidates currently shown (empty when the dropdown is
    /// closed).
    pub fn autocomplete_items(&self) -> &[AutocompleteItem] {
        &self.autocomplete_items
    }

    /// Index of the highlighted candidate.
    pub fn autocomplete_selected(&self) -> usize {
        self.autocomplete_selected
    }

    /// The prefix the current candidates were computed for.
    pub fn autocomplete_prefix(&self) -> &str {
        &self.autocomplete_prefix
    }

    /// Dropdown height in rows.
    pub fn autocomplete_max_visible(&self) -> usize {
        self.autocomplete_max_visible
    }

    /// Set the dropdown height, clamped to
    /// [`MIN_AUTOCOMPLETE_MAX_VISIBLE`]..=[`MAX_AUTOCOMPLETE_MAX_VISIBLE`]
    /// like upstream's `Math.max(3, Math.min(20, …))`.
    pub fn set_autocomplete_max_visible(&mut self, rows: usize) {
        self.autocomplete_max_visible =
            rows.clamp(MIN_AUTOCOMPLETE_MAX_VISIBLE, MAX_AUTOCOMPLETE_MAX_VISIBLE);
    }

    /// Move the highlighted candidate by `delta` rows, wrapping around —
    /// upstream's ArrowUp / ArrowDown handling.
    pub fn move_autocomplete(&mut self, delta: i32) {
        let len = self.autocomplete_items.len();
        if len == 0 {
            return;
        }
        let len_i = len as i64;
        let next = (self.autocomplete_selected as i64 + i64::from(delta)).rem_euclid(len_i);
        self.autocomplete_selected = next as usize;
    }

    /// Apply the highlighted candidate and close the dropdown,
    /// returning [`EditorAction::Changed`]. A no-op returning
    /// [`EditorAction::None`] when the dropdown is closed.
    pub fn accept_autocomplete(&mut self) -> EditorAction {
        let Some(item) = self
            .autocomplete_items
            .get(self.autocomplete_selected)
            .cloned()
        else {
            self.cancel_autocomplete();
            return EditorAction::None;
        };
        let prefix = self.autocomplete_prefix.clone();
        self.apply_completion_item(&item, &prefix);
        self.cancel_autocomplete();
        EditorAction::Changed
    }

    /// Close the dropdown without touching the buffer — upstream's
    /// `cancelAutocomplete`.
    pub fn cancel_autocomplete(&mut self) {
        self.autocomplete_items.clear();
        self.autocomplete_selected = 0;
        self.autocomplete_prefix.clear();
        self.autocomplete_force = false;
    }

    /// Handle `Tab` with the dropdown closed: complete the current
    /// `/`-command when the cursor sits in a command name, otherwise
    /// force a file completion — upstream `handleTabCompletion`.
    /// Returns `true` when a completion was applied or a dropdown was
    /// opened.
    pub fn handle_tab_completion(&mut self) -> bool {
        if self.autocomplete_provider.is_none() {
            return false;
        }
        let before = self.before_cursor_text();
        let trimmed = before.trim_start();
        if trimmed.starts_with('/') && !trimmed.contains(' ') {
            self.request_autocomplete(false, true)
        } else {
            self.request_autocomplete(true, true)
        }
    }

    /// Compute candidates for the current cursor and open or refresh the
    /// dropdown. Returns `true` when the dropdown is open (or an
    /// explicit-Tab single candidate was applied directly).
    ///
    /// `force` mirrors upstream's `"force"` state (skip the textual
    /// heuristics); `explicit_tab` enables upstream's auto-apply of a
    /// lone candidate.
    pub fn request_autocomplete(&mut self, force: bool, explicit_tab: bool) -> bool {
        let Some(provider) = self.autocomplete_provider.clone() else {
            return false;
        };
        let lines = [self.buffer.clone()];
        let cursor_col = self.cursor;
        if force && !provider.should_trigger_file_completion(&lines, 0, cursor_col) {
            return false;
        }
        let Some(suggestions) = provider.get_suggestions(&lines, 0, cursor_col, force) else {
            self.cancel_autocomplete();
            return false;
        };
        if suggestions.items.is_empty() {
            self.cancel_autocomplete();
            return false;
        }
        // Upstream auto-applies a lone candidate on an explicit Tab.
        if force && explicit_tab && suggestions.items.len() == 1 {
            let item = suggestions.items[0].clone();
            self.apply_completion_item(&item, &suggestions.prefix);
            return true;
        }
        let selected = best_autocomplete_match_index(&suggestions.items, &suggestions.prefix);
        self.autocomplete_prefix = suggestions.prefix;
        self.autocomplete_items = suggestions.items;
        self.autocomplete_selected = selected;
        self.autocomplete_force = force;
        true
    }

    /// Render the dropdown rows for a widget `width`, one entry per
    /// candidate (already windowed to the configured height). The
    /// highlighted row is marked with `❯`.
    pub fn autocomplete_render_lines(&self, width: usize) -> Vec<String> {
        let len = self.autocomplete_items.len();
        if len == 0 || width == 0 {
            return Vec::new();
        }
        let visible = self.autocomplete_max_visible.min(len);
        let (start, end) = self.autocomplete_visible_range(visible);
        let mut rows = Vec::with_capacity(end - start);
        for index in start..end {
            let item = &self.autocomplete_items[index];
            let marker = if index == self.autocomplete_selected {
                "❯ "
            } else {
                "  "
            };
            let text = match &item.description {
                Some(description) if width > 44 => {
                    format!("{marker}{}  {description}", item.label)
                }
                _ => format!("{marker}{}", item.label),
            };
            rows.push(truncate_display(&text, width));
        }
        if len > visible {
            rows.push(format!("  ({}/{len})", self.autocomplete_selected + 1));
        }
        rows
    }

    /// The `(start, end)` window of candidates to render, keeping the
    /// selection centred like [`crate::Selector`].
    fn autocomplete_visible_range(&self, visible: usize) -> (usize, usize) {
        let len = self.autocomplete_items.len();
        if visible >= len {
            return (0, len);
        }
        let half = visible / 2;
        let start = self
            .autocomplete_selected
            .saturating_sub(half)
            .min(len - visible);
        (start, start + visible)
    }

    /// Apply one candidate, capturing an undo snapshot first (upstream
    /// pushes the pre-completion state so `Ctrl+-` reverts it).
    fn apply_completion_item(&mut self, item: &AutocompleteItem, prefix: &str) {
        let Some(provider) = self.autocomplete_provider.clone() else {
            return;
        };
        self.push_undo_snapshot();
        let lines = [self.buffer.clone()];
        let result = provider.apply_completion(&lines, 0, self.cursor, item, prefix);
        self.buffer = result.lines.into_iter().next().unwrap_or_default();
        self.cursor = clamp_to_char_boundary(&self.buffer, result.cursor_col);
        self.reset_history_navigation();
        self.last_action = LastAction::Other;
    }

    /// Recompute the dropdown after an edit.
    ///
    /// `inserted` is the character that was just typed (if any).
    /// Mirrors upstream `handleInput`'s rule: while the dropdown is
    /// open, every edit refreshes it; while it is closed, only an edit
    /// that lands in a command / path context opens it.
    fn update_autocomplete_after_edit(&mut self, inserted: Option<char>) {
        if self.autocomplete_provider.is_none() {
            return;
        }
        let was_open = self.is_showing_autocomplete();
        let should_request = if was_open {
            true
        } else {
            let before = self.before_cursor_text();
            match inserted {
                Some(c) => self.opens_autocomplete_after_insert(c, before),
                None => self.opens_autocomplete_for_text(before),
            }
        };
        if !should_request {
            return;
        }
        let force = was_open && self.autocomplete_force;
        self.request_autocomplete(force, false);
    }

    /// Re-query the provider while the dropdown is open so its prefix
    /// tracks the cursor, upstream's `updateAutocomplete` after a cursor
    /// move / deletion. Closes the dropdown when the new position yields
    /// no candidates.
    fn refresh_autocomplete_if_open(&mut self) {
        if !self.is_showing_autocomplete() {
            return;
        }
        let force = self.autocomplete_force;
        self.request_autocomplete(force, false);
    }

    /// Whether typing `c` (now part of `before`) should open the
    /// dropdown, upstream `handleInput`'s autocomplete trigger.
    fn opens_autocomplete_after_insert(&self, c: char, before: &str) -> bool {
        // `/` at the start of the message always opens the command menu.
        if c == '/' {
            let trimmed = before.trim();
            if trimmed.is_empty() || trimmed == "/" {
                return true;
            }
        }
        if self.autocomplete_trigger_characters.contains(&c) {
            return at_token_boundary(before);
        }
        if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
            return self.opens_autocomplete_for_text(before);
        }
        false
    }

    /// Whether `text` (before the cursor) is already inside a context
    /// the provider can complete. Upstream's
    /// `isInSlashCommandContext(text) || triggerPattern.test(text)`.
    fn opens_autocomplete_for_text(&self, text: &str) -> bool {
        if text.trim_start().starts_with('/') {
            return true;
        }
        trigger_pattern_matches(text, &self.autocomplete_trigger_characters)
    }

    /// The buffer text before the cursor.
    fn before_cursor_text(&self) -> &str {
        &self.buffer[..self.cursor.min(self.buffer.len())]
    }

    /// True for trigger characters the editor accepts (`/` is handled
    /// by the command rule, whitespace never triggers).
    fn is_valid_trigger_character(&self, c: char) -> bool {
        c != '/' && !c.is_whitespace()
    }

    /// Capture the current buffer and cursor for `Ctrl+-`.
    fn push_undo_snapshot(&mut self) {
        self.undo_stack.push(&EditorSnapshot {
            buffer: self.buffer.clone(),
            cursor: self.cursor,
        });
    }

    /// True for the jump hotkeys: `Ctrl+]` (`tui.editor.jumpForward`)
    /// and `Ctrl+Alt+]` (`tui.editor.jumpBackward`), together with the
    /// legacy `Ctrl+5` / `Ctrl+Alt+5` spellings crossterm produces for
    /// the `0x1D` / `ESC 0x1D` byte sequences.
    fn is_jump_key(key: &Key) -> bool {
        key.modifiers.control && matches!(key.code, KeyCode::Char(']') | KeyCode::Char('5'))
    }

    /// Move the cursor to the next (`Forward`) or previous (`Backward`)
    /// occurrence of `needle`, mirroring upstream `Editor.jumpToChar`.
    ///
    /// The search skips the character under the cursor, is
    /// case-sensitive, and leaves the cursor untouched when there is no
    /// match. Returns [`EditorAction::Changed`] only when the cursor
    /// actually moved; a jump is a pure cursor move, so it never
    /// touches the undo stack.
    pub fn jump_to_char(&mut self, needle: char, direction: JumpDirection) -> EditorAction {
        // Upstream clears `lastAction` before searching, so even a failed
        // jump breaks the kill / yank / typing chains.
        self.last_action = LastAction::Other;
        let target = match direction {
            JumpDirection::Forward => {
                let start = self.next_char_boundary(self.cursor);
                self.buffer[start..]
                    .find(needle)
                    .map(|offset| start + offset)
            }
            JumpDirection::Backward => {
                let end = self.cursor.min(self.buffer.len());
                self.buffer[..end].rfind(needle)
            }
        };
        match target {
            Some(pos) => {
                self.cursor = pos;
                EditorAction::Changed
            }
            None => EditorAction::None,
        }
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
        // An armed jump consumes this key: a printable character is the
        // target, the hotkey again (or anything that is not a plain
        // printable character) cancels the mode. Cancelling keys fall
        // through to their normal handling, mirroring upstream
        // `Editor.handleInput`.
        if let Some(direction) = self.jump_mode.take() {
            if Self::is_jump_key(&key) {
                return EditorAction::None;
            }
            if !key.modifiers.control && !key.modifiers.alt {
                if let KeyCode::Char(c) = key.code {
                    return self.jump_to_char(c, direction);
                }
            }
        }

        // `tui.editor.jumpForward` / `jumpBackward`: `Ctrl+]` arms a
        // forward jump, `Ctrl+Alt+]` a backward one. Checked before the
        // generic control chords so the Alt-modified spelling (which the
        // `control` branch would otherwise drop) is recognised.
        if Self::is_jump_key(&key) {
            self.jump_mode = Some(if key.modifiers.alt {
                JumpDirection::Backward
            } else {
                JumpDirection::Forward
            });
            return EditorAction::None;
        }

        // Control chords first.
        if key.modifiers.control {
            match key.code {
                KeyCode::Char('c') | KeyCode::Char('C') => return EditorAction::Interrupt,
                // `tui.editor.deleteCharForward` defaults to
                // `["delete", "ctrl+d"]` (`packages/tui/src/keybindings.ts:121`).
                // The coding agent's custom editor only treats `Ctrl+D`
                // as exit when the buffer is empty and otherwise falls
                // through to the editor's delete-char-forward handler
                // (`packages/coding-agent/src/modes/interactive/components/custom-editor.ts:117`).
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    if self.buffer.is_empty() {
                        return EditorAction::Eof;
                    }
                    return self.delete();
                }
                KeyCode::Char('u') | KeyCode::Char('U') => return self.kill_to_line_start(),
                KeyCode::Char('a') | KeyCode::Char('A') => return self.move_home(),
                // `tui.editor.cursorLeft` / `cursorRight` default to
                // `["left", "ctrl+b"]` / `["right", "ctrl+f"]`
                // (`packages/tui/src/keybindings.ts:82`), so the emacs
                // aliases move one character, not one word.
                KeyCode::Char('b') | KeyCode::Char('B') => return self.move_left(),
                KeyCode::Char('f') | KeyCode::Char('F') => return self.move_right(),
                KeyCode::Char('e') | KeyCode::Char('E') => return self.move_end(),
                KeyCode::Char('k') | KeyCode::Char('K') => return self.kill_to_line_end(),
                // `tui.editor.deleteWordBackward`.
                KeyCode::Char('w') | KeyCode::Char('W') => return self.kill_word_backward(),
                // `tui.editor.cursorWordLeft` / `cursorWordRight`.
                KeyCode::Left => return self.move_word_left(),
                KeyCode::Right => return self.move_word_right(),
                KeyCode::Char('y') | KeyCode::Char('Y') => return self.yank(),
                // `tui.editor.undo` is bound to `ctrl+-`. Upstream
                // normalizes the legacy `0x1F` control byte to `ctrl+-`
                // (`packages/tui/src/keys.ts:1277`); crossterm instead
                // decodes that byte as `Ctrl+7` (`0x1C..=0x1F` map to
                // `Ctrl+4..=Ctrl+7`, `event/sys/unix/parse.rs`), while
                // Kitty-protocol terminals deliver `Ctrl+-` directly and
                // some frontends report `Ctrl+_` (the ASCII name of the
                // same byte). Accept all three spellings so the binding
                // works with and without the Kitty keyboard protocol.
                KeyCode::Char('-') | KeyCode::Char('_') | KeyCode::Char('7') => {
                    return self.undo();
                }
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

        // Autocomplete dropdown. Checked above the plain-key handling so
        // Esc / Up / Down / Tab / Enter steer the candidate list while it
        // is open, mirroring upstream `Editor.handleInput`'s autocomplete
        // block (which runs after undo, before Tab and deletion).
        if self.is_showing_autocomplete() && key.modifiers.is_empty() {
            match key.code {
                KeyCode::Esc => {
                    self.cancel_autocomplete();
                    return EditorAction::None;
                }
                KeyCode::Up => {
                    self.move_autocomplete(-1);
                    return EditorAction::Changed;
                }
                KeyCode::Down => {
                    self.move_autocomplete(1);
                    return EditorAction::Changed;
                }
                KeyCode::Tab => return self.accept_autocomplete(),
                KeyCode::Enter => {
                    let prefix = self.autocomplete_prefix.clone();
                    let applied = self.accept_autocomplete();
                    if applied == EditorAction::None {
                        return EditorAction::None;
                    }
                    // A command name is still submitted: upstream falls
                    // through to the normal Enter handling when the
                    // prefix starts with `/`.
                    if prefix.starts_with('/') {
                        return EditorAction::Submit(self.buffer.clone());
                    }
                    return applied;
                }
                _ => {}
            }
        }

        // Tab with the dropdown closed forces a completion.
        if key.code == KeyCode::Tab && key.modifiers.is_empty() && self.handle_tab_completion() {
            return EditorAction::Changed;
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

/// Clamp a byte offset to the nearest valid UTF-8 boundary of `text`
/// (rounding down), and to the text length.
fn clamp_to_char_boundary(text: &str, pos: usize) -> usize {
    let mut pos = pos.min(text.len());
    while pos > 0 && !text.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

/// Truncate `text` to at most `max` characters.
fn truncate_display(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

/// Whether the character just before the token in `before` is a space or
/// tab — upstream's "the trigger character starts a token" test.
fn at_token_boundary(before: &str) -> bool {
    let mut chars = before.chars().rev();
    // Skip the trigger character itself.
    chars.next();
    match chars.next() {
        Some(c) => c == ' ' || c == '\t',
        // Start of the buffer: the shape of a one-character input.
        None => true,
    }
}

/// Match upstream's trigger pattern `(?:^|[\s])[@#][^\s]*$` against
/// `text`, using the editor's configured trigger characters.
fn trigger_pattern_matches(text: &str, triggers: &[char]) -> bool {
    let token_start = text
        .char_indices()
        .filter(|(_, c)| c.is_whitespace())
        .map(|(idx, c)| idx + c.len_utf8())
        .next_back()
        .unwrap_or(0);
    let token = &text[token_start..];
    let mut chars = token.chars();
    match chars.next() {
        Some(c) if triggers.contains(&c) => !chars.any(char::is_whitespace),
        _ => false,
    }
}

/// Upstream `getBestAutocompleteMatchIndex`: an exact prefix match wins,
/// otherwise the first `value.starts_with(prefix)`, otherwise the first
/// candidate.
fn best_autocomplete_match_index(items: &[AutocompleteItem], prefix: &str) -> usize {
    if prefix.is_empty() {
        return 0;
    }
    let mut first_prefix_match = None;
    for (index, item) in items.iter().enumerate() {
        if item.value == prefix {
            return index;
        }
        if first_prefix_match.is_none() && item.value.starts_with(prefix) {
            first_prefix_match = Some(index);
        }
    }
    first_prefix_match.unwrap_or(0)
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
    fn ctrl_d_on_non_empty_deletes_forward() {
        let mut ed = Editor::new();
        ed.insert_str("xy");
        ed.move_home();
        let action = ed.handle_key(Key::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(action, EditorAction::Changed);
        assert_eq!(ed.text(), "y");
        assert_eq!(ed.cursor(), 0);
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
    fn ctrl_b_and_ctrl_f_move_one_char() {
        let mut ed = Editor::new();
        ed.insert_str("hello");
        // `Ctrl+B` / `Ctrl+F` are the emacs aliases of `Left` / `Right`
        // (`tui.editor.cursorLeft` / `cursorRight`).
        assert_eq!(ed.handle_key(ctrl('b')), EditorAction::Changed);
        assert_eq!(ed.cursor(), 4);
        // A one-character move, not the word move `Alt+B` performs.
        assert_eq!(ed.handle_key(ctrl('f')), EditorAction::Changed);
        assert_eq!(ed.cursor(), 5);
    }

    #[test]
    fn ctrl_b_and_ctrl_f_are_noops_at_the_edges() {
        let mut ed = Editor::new();
        ed.insert_str("ab");
        ed.move_home();
        assert_eq!(ed.handle_key(ctrl('b')), EditorAction::None);
        assert_eq!(ed.cursor(), 0);
        ed.move_end();
        assert_eq!(ed.handle_key(ctrl('f')), EditorAction::None);
        assert_eq!(ed.cursor(), 2);
    }

    #[test]
    fn ctrl_b_and_ctrl_f_step_over_a_multibyte_char() {
        let mut ed = Editor::new();
        ed.insert_str("é");
        ed.move_end();
        assert_eq!(ed.handle_key(ctrl('b')), EditorAction::Changed);
        assert_eq!(ed.cursor(), 0);
        assert_eq!(ed.handle_key(ctrl('f')), EditorAction::Changed);
        assert_eq!(ed.cursor(), "é".len());
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
    fn ctrl_seven_is_the_legacy_undo_alias() {
        // Without the Kitty keyboard protocol a terminal sends `0x1F`
        // for `Ctrl+-`, and crossterm turns that byte into `Ctrl+7`.
        let mut ed = Editor::new();
        type_chars(&mut ed, "ab");
        let action = ed.handle_key(Key::new(KeyCode::Char('7'), KeyModifiers::CONTROL));
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
