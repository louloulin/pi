//! Single-line input component with prompt history.
//!
//! Mirrors Martty's `Input` (`src/input/editor.rs`) and ports the
//! TypeScript `Input` component (`packages/tui/src/components/input.ts`).
//!
//! This is the single-line counterpart to the multi-line [`Editor`](crate::editor::Editor).
//! It is used for simple prompts, search inputs, and other single-line interactions.
//!
//! # Features
//!
//! * Plain character keys append to the buffer at the cursor.
//! * `Backspace` / `Delete` delete characters before / after the cursor.
//! * `Left` / `Right` move the cursor by one grapheme.
//! * `Ctrl+A` / `Ctrl+E` jump to the line start / end.
//! * `Ctrl+B` / `Ctrl+F` move by one grapheme (emacs aliases).
//! * `Alt+B` / `Alt+Left` / `Ctrl+Left` move one word left.
//! * `Alt+F` / `Alt+Right` / `Ctrl+Right` move one word right.
//! * `Ctrl+W` / `Alt+Backspace` kill the word before the cursor.
//! * `Alt+D` / `Alt+Delete` kill the word after the cursor.
//! * `Ctrl+U` kill to the start of the line.
//! * `Ctrl+K` kill to the end of the line.
//! * `Ctrl+Y` / `Alt+Y` yank / yank-pop from the kill ring.
//! * `Ctrl+-` undoes the previous edit (fish-style coalescing).
//! * `Up` / `Down` when the buffer is empty browse prompt history.
//! * `Up` / `Down` when the buffer is non-empty move by visual rows
//!   (soft-wrapped at the display width), preserving the sticky column.
//! * `Home` / `End` move to the visual line start / end.
//! * `Ctrl+]` / `Ctrl+Alt+]` arm jump mode: the next typed character
//!   jumps to its next / previous occurrence.
//! * `Enter` emits [`InputAction::Submit`] with the current buffer text.
//! * `Escape` emits [`InputAction::Cancel`].
//! * Bracketed paste is handled transparently.

use crate::input::Key;
use crate::kill_ring::KillRing;
use crate::undo_stack::UndoStack;
use unicode_width::UnicodeWidthChar;

/// Snapshot for undo: buffer + cursor position.
#[derive(Debug, Clone)]
struct InputSnapshot {
    value: String,
    cursor: usize,
}

/// Direction for jump mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JumpDirection {
    /// Jump forward to the next occurrence.
    Forward,
    /// Jump backward to the previous occurrence.
    Backward,
}

/// Visual caret position: (row, column) in display cells.
#[derive(Debug, Clone, Copy)]
struct VisualCaret {
    row: usize,
    col: usize,
}

/// Visual layout result: carets array + row count.
#[derive(Debug, Clone)]
struct VisualLayout {
    /// Caret position after each character boundary.
    carets: Vec<VisualCaret>,
    /// Total number of visual rows.
    rows: usize,
}

/// Visual candidate for cursor positioning: index, row, column, and wrap end affinity.
/// Tuple struct: (index, row, col, wrap_end)
#[derive(Debug, Clone, Copy)]
struct VisualCandidate(usize, usize, usize, bool);

/// Actions emitted by [`Input`] after processing a key.
#[derive(Debug, Clone)]
pub enum InputAction {
    /// User submitted the current buffer (Enter).
    Submit(String),
    /// User cancelled (Escape).
    Cancel,
}

/// A single-line text input with prompt history and Emacs-style editing.
///
/// Mirrors Martty's `Input` and the TypeScript `Input` component.
#[derive(Debug, Clone)]
pub struct Input {
    /// Buffer contents.
    buf: String,
    /// Cursor position as a char index (not bytes).
    cursor: usize,
    /// Prompt history (newest first).
    history: Vec<String>,
    /// Current history position (None = not browsing).
    hist_pos: Option<usize>,
    /// Saved draft when entering history browsing.
    stash: String,
    /// Sticky display column for vertical movement.
    preferred_visual_col: Option<usize>,
    /// Wrap end affinity: when at a soft wrap, prefer the upstream row end.
    cursor_at_wrap_end: bool,
    /// Armed jump direction.
    jump_mode: Option<JumpDirection>,
    /// Kill ring for Emacs-style kill/yank.
    kill_ring: KillRing,
    /// Last editing action (for undo coalescing).
    last_action: LastAction,
    /// Snapshots restored by `Ctrl+-`.
    undo_stack: UndoStack<InputSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastAction {
    Kill,
    Yank,
    TypeWord,
    Other,
}

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}

impl Input {
    /// Construct a new empty input.
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            cursor: 0,
            history: Vec::new(),
            hist_pos: None,
            stash: String::new(),
            preferred_visual_col: None,
            cursor_at_wrap_end: false,
            jump_mode: None,
            kill_ring: KillRing::new(),
            last_action: LastAction::Other,
            undo_stack: UndoStack::new(),
        }
    }

    /// Get the current buffer text.
    pub fn text(&self) -> &str {
        &self.buf
    }

    /// Get the cursor position as a char index.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Set the buffer text programmatically.
    pub fn set_text(&mut self, text: impl Into<String>) {
        let text = text.into();
        if text != self.buf {
            self.push_undo_snapshot();
        }
        self.buf = text;
        self.cursor = self.buf.chars().count();
        self.hist_pos = None;
        self.stash.clear();
        self.preferred_visual_col = None;
        self.cursor_at_wrap_end = false;
        self.jump_mode = None;
    }

    /// Clear the buffer.
    pub fn clear(&mut self) {
        if !self.buf.is_empty() {
            self.push_undo_snapshot();
        }
        self.buf.clear();
        self.cursor = 0;
        self.hist_pos = None;
        self.stash.clear();
        self.preferred_visual_col = None;
        self.cursor_at_wrap_end = false;
        self.jump_mode = None;
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Number of characters in the buffer.
    pub fn len_chars(&self) -> usize {
        self.buf.chars().count()
    }

    /// Char index of the byte at `char_idx`.
    fn byte_at(&self, char_idx: usize) -> usize {
        self.buf
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.buf.len())
    }

    /// Insert a character at the cursor.
    pub fn insert(&mut self, ch: char) {
        self.exit_history_browsing();
        self.push_undo_snapshot();

        let at = self.byte_at(self.cursor);
        self.buf.insert(at, ch);
        self.cursor += 1;
        self.preferred_visual_col = None;
        self.cursor_at_wrap_end = false;
        self.jump_mode = None;
        self.last_action = LastAction::TypeWord;
    }

    /// Insert a string at the cursor.
    pub fn insert_str(&mut self, s: &str) {
        self.exit_history_browsing();

        // Fish-style undo coalescing: consecutive typing is one undo unit.
        if self.last_action != LastAction::TypeWord {
            self.push_undo_snapshot();
        }

        let at = self.byte_at(self.cursor);
        self.buf.insert_str(at, s);
        self.cursor += s.chars().count();
        self.preferred_visual_col = None;
        self.cursor_at_wrap_end = false;
        self.jump_mode = None;
        self.last_action = LastAction::TypeWord;
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        self.exit_history_browsing();

        if self.cursor == 0 {
            return;
        }

        self.push_undo_snapshot();

        let start = self.byte_at(self.cursor - 1);
        let end = self.byte_at(self.cursor);
        self.buf.replace_range(start..end, "");
        self.cursor -= 1;
        self.preferred_visual_col = None;
        self.cursor_at_wrap_end = false;
        self.jump_mode = None;
        self.last_action = LastAction::Other;
    }

    /// Delete the character at the cursor.
    pub fn delete_forward(&mut self) {
        self.exit_history_browsing();

        if self.cursor >= self.len_chars() {
            return;
        }

        self.push_undo_snapshot();

        let start = self.byte_at(self.cursor);
        let end = self.byte_at(self.cursor + 1);
        self.buf.replace_range(start..end, "");
        self.preferred_visual_col = None;
        self.cursor_at_wrap_end = false;
        self.jump_mode = None;
        self.last_action = LastAction::Other;
    }

    /// Kill the word before the cursor and push to the kill ring.
    pub fn delete_word_back(&mut self) {
        self.exit_history_browsing();

        if self.cursor == 0 {
            return;
        }

        let i = self.prev_word();
        if i == self.cursor {
            return;
        }

        let killed = &self.buf[self.byte_at(i)..self.byte_at(self.cursor)];
        self.kill_ring.push(killed, crate::kill_ring::KillDirection::Prepend, self.last_action == LastAction::Kill);
        self.last_action = LastAction::Kill;

        self.push_undo_snapshot();

        let end = self.byte_at(self.cursor);
        let start = self.byte_at(i);
        self.buf.replace_range(start..end, "");
        self.cursor = i;
        self.preferred_visual_col = None;
        self.cursor_at_wrap_end = false;
        self.jump_mode = None;
    }

    /// Kill the word after the cursor and push to the kill ring.
    pub fn delete_word_forward(&mut self) {
        self.exit_history_browsing();

        if self.cursor >= self.len_chars() {
            return;
        }

        let i = self.next_word();
        if i == self.cursor {
            return;
        }

        let killed = &self.buf[self.byte_at(self.cursor)..self.byte_at(i)];
        self.kill_ring.push(killed, crate::kill_ring::KillDirection::Append, self.last_action == LastAction::Kill);
        self.last_action = LastAction::Kill;

        self.push_undo_snapshot();

        let end = self.byte_at(i);
        let start = self.byte_at(self.cursor);
        self.buf.replace_range(start..end, "");
        self.jump_mode = None;
        self.preferred_visual_col = None;
        self.cursor_at_wrap_end = false;
    }

    /// Kill to the start of the line and push to the kill ring.
    pub fn kill_to_start(&mut self) {
        self.exit_history_browsing();

        if self.cursor == 0 {
            return;
        }

        let killed = &self.buf[..self.byte_at(self.cursor)];
        self.kill_ring.push(killed, crate::kill_ring::KillDirection::Prepend, self.last_action == LastAction::Kill);
        self.last_action = LastAction::Kill;

        self.push_undo_snapshot();

        self.buf.replace_range(..self.byte_at(self.cursor), "");
        self.cursor = 0;
        self.preferred_visual_col = None;
        self.cursor_at_wrap_end = false;
        self.jump_mode = None;
    }

    /// Kill to the end of the line and push to the kill ring.
    pub fn kill_to_end(&mut self) {
        self.exit_history_browsing();

        if self.cursor >= self.len_chars() {
            return;
        }

        let killed = &self.buf[self.byte_at(self.cursor)..];
        self.kill_ring.push(killed, crate::kill_ring::KillDirection::Append, self.last_action == LastAction::Kill);
        self.last_action = LastAction::Kill;

        self.push_undo_snapshot();

        self.buf.truncate(self.byte_at(self.cursor));
        self.preferred_visual_col = None;
        self.cursor_at_wrap_end = false;
        self.jump_mode = None;
    }

    /// Yank the most recent kill ring entry at the cursor.
    pub fn yank(&mut self) {
        let text = self.kill_ring.peek().map(|s| s.to_string());
        if let Some(text) = text {
            self.exit_history_browsing();
            self.push_undo_snapshot();

            let at = self.byte_at(self.cursor);
            self.buf.insert_str(at, &text);
            self.cursor += text.chars().count();
            self.last_action = LastAction::Yank;
            self.preferred_visual_col = None;
            self.cursor_at_wrap_end = false;
            self.jump_mode = None;
        }
    }

    /// Rotate the kill ring and yank the next entry.
    pub fn yank_pop(&mut self) {
        if self.last_action != LastAction::Yank || self.kill_ring.len() <= 1 {
            return;
        }

        // Remove the previously yanked text.
        let prev_text = self.kill_ring.peek().map(|s| s.to_string());
        if let Some(prev) = prev_text {
            let prev_len = prev.chars().count();
            let new_cursor = self.cursor.saturating_sub(prev_len);
            self.buf
                .replace_range(self.byte_at(new_cursor)..self.byte_at(self.cursor), "");
            self.cursor = new_cursor;
        }

        self.kill_ring.rotate();
        let text = self.kill_ring.peek().map(|s| s.to_string());
        if let Some(text) = text {
            self.push_undo_snapshot();

            let at = self.byte_at(self.cursor);
            self.buf.insert_str(at, &text);
            self.cursor += text.chars().count();
            self.last_action = LastAction::Yank;
            self.preferred_visual_col = None;
            self.cursor_at_wrap_end = false;
        }
    }

    /// Undo the previous edit.
    pub fn undo(&mut self) {
        if let Some(snapshot) = self.undo_stack.pop() {
            self.buf = snapshot.value;
            self.cursor = snapshot.cursor;
            self.preferred_visual_col = None;
            self.cursor_at_wrap_end = false;
        }
    }

    /// Move cursor one grapheme left.
    pub fn cursor_left(&mut self) {
        self.exit_history_browsing();
        if self.cursor > 0 {
            self.cursor -= 1;
        }
        self.preferred_visual_col = None;
        self.cursor_at_wrap_end = false;
        self.jump_mode = None;
    }

    /// Move cursor one grapheme right.
    pub fn cursor_right(&mut self) {
        self.exit_history_browsing();
        if self.cursor < self.len_chars() {
            self.cursor += 1;
        }
        self.preferred_visual_col = None;
        self.cursor_at_wrap_end = false;
        self.jump_mode = None;
    }

    /// Move cursor to the start of the visual line.
    pub fn move_to_visual_line_start(&mut self, width: usize) {
        let width = width.max(1);
        let (candidates, _) = self.visual_candidates(width);
        let (current_row, _) = self.visual_cursor(width);

        if let Some(&VisualCandidate(index, _, _, wrap_end)) = candidates
            .iter()
            .filter(|&VisualCandidate(_, row, _, _)| *row == current_row)
            .min_by_key(|&VisualCandidate(_, _, col, _)| *col)
        {
            self.cursor = index;
            self.cursor_at_wrap_end = wrap_end;
        }
        self.preferred_visual_col = None;
    }

    /// Move cursor to the end of the visual line.
    pub fn move_to_visual_line_end(&mut self, width: usize) {
        let width = width.max(1);
        let (candidates, _) = self.visual_candidates(width);
        let (current_row, _) = self.visual_cursor(width);

        if let Some(&VisualCandidate(index, _, _, wrap_end)) = candidates
            .iter()
            .filter(|&VisualCandidate(_, row, _, _)| *row == current_row)
            .max_by_key(|&VisualCandidate(_, _, col, _)| *col)
        {
            self.cursor = index;
            self.cursor_at_wrap_end = wrap_end;
        }
        self.preferred_visual_col = None;
    }

    /// Move cursor one word left.
    pub fn prev_word(&self) -> usize {
        let chars: Vec<char> = self.buf.chars().collect();
        let mut i = self.cursor.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    /// Move cursor one word right.
    pub fn next_word(&self) -> usize {
        let chars: Vec<char> = self.buf.chars().collect();
        let n = chars.len();
        let mut i = self.cursor.min(n);
        while i < n && chars[i].is_whitespace() {
            i += 1;
        }
        while i < n && !chars[i].is_whitespace() {
            i += 1;
        }
        i
    }

    /// Move cursor one word left (with state update).
    pub fn move_word_back(&mut self) {
        self.exit_history_browsing();
        self.cursor = self.prev_word();
        self.preferred_visual_col = None;
        self.cursor_at_wrap_end = false;
        self.jump_mode = None;
    }

    /// Move cursor one word right (with state update).
    pub fn move_word_forward(&mut self) {
        self.exit_history_browsing();
        self.cursor = self.next_word();
        self.preferred_visual_col = None;
        self.cursor_at_wrap_end = false;
        self.jump_mode = None;
    }

    /// Move by one visual row while preserving the display column.
    /// When the buffer is empty, this browses prompt history (Martty-style).
    pub fn move_vertical(&mut self, width: usize, direction: i8) {
        let width = width.max(1);

        // History browsing: only from an empty prompt (Martty's rule).
        if self.buf.is_empty() {
            if direction < 0 {
                self.history_prev();
            } else if direction > 0 {
                self.history_next();
            }
            return;
        }

        let (candidates, rows) = self.visual_candidates(width);
        let (row, col) = self.visual_cursor(width);
        let goal = *self.preferred_visual_col.get_or_insert(col);

        let target = if direction < 0 {
            row.checked_sub(1)
        } else if direction > 0 && row + 1 < rows {
            Some(row + 1)
        } else {
            None
        };

        let Some(target) = target else {
            return;
        };

        let mut best: Option<(usize, usize, bool)> = None;
        for &VisualCandidate(index, candidate_row, candidate_col, wrap_end) in &candidates {
            if candidate_row != target {
                continue;
            }
            let distance = candidate_col.abs_diff(goal);
            if best.is_none_or(|(_, best_distance, _)| distance < best_distance) {
                best = Some((index, distance, wrap_end));
            }
        }

        if let Some((index, _, wrap_end)) = best {
            self.cursor = index;
            self.cursor_at_wrap_end = wrap_end;
        }
    }

    /// Add a string to the prompt history.
    pub fn add_to_history(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        // Don't add consecutive duplicates.
        if self.history.first().map(|s| s.as_str()) == Some(trimmed) {
            return;
        }
        self.history.insert(0, trimmed.to_string());
        if self.history.len() > 100 {
            self.history.pop();
        }
    }

    /// Navigate to the previous history entry.
    fn history_prev(&mut self) {
        if self.buf.is_empty() && self.hist_pos.is_none() {
            return; // grok: history opens from an empty prompt
        }
        if self.history.is_empty() {
            return;
        }

        let pos = match self.hist_pos {
            None => {
                self.stash = self.buf.clone();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(p) => p - 1,
        };

        self.hist_pos = Some(pos);
        self.buf = self.history[pos].clone();
        self.cursor = self.buf.chars().count();
        self.preferred_visual_col = None;
        self.cursor_at_wrap_end = false;
    }

    /// Navigate to the next history entry.
    fn history_next(&mut self) {
        if self.hist_pos.is_none() {
            return;
        }

        let pos = self.hist_pos.unwrap();
        if pos == 0 {
            // Back to current draft
            self.hist_pos = None;
            self.buf = self.stash.clone();
            self.stash.clear();
        } else {
            self.hist_pos = Some(pos - 1);
            self.buf = self.history[pos - 1].clone();
        }

        self.cursor = self.buf.chars().count();
        self.preferred_visual_col = None;
        self.cursor_at_wrap_end = false;
    }

    /// Forget the sticky display column.
    pub fn reset_vertical_goal(&mut self) {
        self.preferred_visual_col = None;
        self.cursor_at_wrap_end = false;
    }

    /// Number of visual rows at the given display width.
    pub fn visual_row_count(&self, width: usize) -> usize {
        let (_, _, rows) = self.visual_layout(width.max(1));
        rows
    }

    /// Forget history browsing state.
    fn exit_history_browsing(&mut self) {
        if self.hist_pos.is_some() {
            self.hist_pos = None;
            self.stash.clear();
        }
    }

    /// Push current state onto the undo stack.
    fn push_undo_snapshot(&mut self) {
        self.undo_stack.push(&InputSnapshot {
            value: self.buf.clone(),
            cursor: self.cursor,
        });
    }

    /// Arm jump mode.
    pub fn arm_jump(&mut self, direction: JumpDirection) {
        self.jump_mode = Some(direction);
        self.last_action = LastAction::Other;
    }

    /// Jump to the next/previous occurrence of a character.
    /// Returns true if the cursor moved.
    pub fn jump_to_char(&mut self, needle: char, direction: JumpDirection) -> bool {
        self.last_action = LastAction::Other;

        if self.buf.is_empty() {
            return false;
        }

        match direction {
            JumpDirection::Forward => {
                let search_start = self.cursor + 1;
                if let Some(pos) = self.buf[search_start..].find(needle) {
                    self.cursor = search_start + pos;
                    self.preferred_visual_col = None;
                    return true;
                }
            }
            JumpDirection::Backward => {
                let before = &self.buf[..self.cursor];
                if let Some(pos) = before.rfind(needle) {
                    self.cursor = pos;
                    self.preferred_visual_col = None;
                    return true;
                }
            }
        }
        false
    }

    /// Process a key and return the resulting action, if any.
    ///
    /// This method mirrors the TypeScript `Input.handleInput()` logic.
    pub fn handle_key(&mut self, key: &Key, width: usize) -> Option<InputAction> {
        use crate::input::KeyCode;

        // Jump mode: the next printable character jumps to its occurrence.
        if let Some(direction) = self.jump_mode.take() {
            if let KeyCode::Char(c) = key.code {
                if key.is_plain() {
                    // Plain character: attempt jump.
                    if !self.jump_to_char(c, direction) {
                        // No match: insert normally.
                        self.insert(c);
                    }
                    return None;
                }
            }
            // Any other key cancels jump mode and falls through.
        }

        let mods = &key.modifiers;

        // Ctrl+R: arm jump forward (Ctrl+R is the reverse-history-search hotkey
        // in the Editor; for this single-line Input we use it for jump mode).
        if mods.control && !mods.alt && matches!(key.code, KeyCode::Char(']')) {
            self.arm_jump(JumpDirection::Forward);
            return None;
        }
        // Ctrl+Alt+]: jump backward.
        if mods.control && mods.alt && matches!(key.code, KeyCode::Char(']')) {
            self.arm_jump(JumpDirection::Backward);
            return None;
        }

        // Submit (Enter)
        if matches!(key.code, KeyCode::Enter) {
            let text = self.buf.clone();
            self.add_to_history(&text);
            self.clear();
            return Some(InputAction::Submit(text));
        }

        // Cancel (Escape)
        if matches!(key.code, KeyCode::Esc) {
            return Some(InputAction::Cancel);
        }

        // Undo: Ctrl+-, Ctrl+_, Ctrl+Z
        if mods.control && matches!(key.code, KeyCode::Char('-') | KeyCode::Char('_') | KeyCode::Char('z')) {
            self.undo();
            return None;
        }

        // Backspace
        if matches!(key.code, KeyCode::Backspace) {
            self.backspace();
            return None;
        }

        // Delete (forward): Delete, Ctrl+D
        if matches!(key.code, KeyCode::Delete) || (mods.control && matches!(key.code, KeyCode::Char('d'))) {
            self.delete_forward();
            return None;
        }

        // Kill to start: Ctrl+U
        if mods.control && matches!(key.code, KeyCode::Char('u')) {
            self.kill_to_start();
            return None;
        }

        // Kill to end: Ctrl+K
        if mods.control && matches!(key.code, KeyCode::Char('k')) {
            self.kill_to_end();
            return None;
        }

        // Kill word back: Ctrl+W
        if mods.control && matches!(key.code, KeyCode::Char('w')) {
            self.delete_word_back();
            return None;
        }

        // Kill word forward: Alt+D
        if mods.alt && matches!(key.code, KeyCode::Char('d')) {
            self.delete_word_forward();
            return None;
        }

        // Yank: Ctrl+Y
        if mods.control && matches!(key.code, KeyCode::Char('y')) {
            self.yank();
            return None;
        }

        // Yank pop: Alt+Y
        if mods.alt && matches!(key.code, KeyCode::Char('y')) {
            self.yank_pop();
            return None;
        }

        // Cursor left: Left, Ctrl+B
        if matches!(key.code, KeyCode::Left) || (mods.control && matches!(key.code, KeyCode::Char('b'))) {
            self.cursor_left();
            return None;
        }

        // Cursor right: Right, Ctrl+F
        if matches!(key.code, KeyCode::Right) || (mods.control && matches!(key.code, KeyCode::Char('f'))) {
            self.cursor_right();
            return None;
        }

        // Line start: Home, Ctrl+A
        if matches!(key.code, KeyCode::Home) || (mods.control && matches!(key.code, KeyCode::Char('a'))) {
            self.cursor = 0;
            self.exit_history_browsing();
            self.preferred_visual_col = None;
            return None;
        }

        // Line end: End, Ctrl+E
        if matches!(key.code, KeyCode::End) || (mods.control && matches!(key.code, KeyCode::Char('e'))) {
            self.cursor = self.len_chars();
            self.exit_history_browsing();
            self.preferred_visual_col = None;
            return None;
        }

        // Word left: Alt+B, Alt+Left
        if (mods.alt && matches!(key.code, KeyCode::Char('b'))) || (mods.alt && matches!(key.code, KeyCode::Left)) {
            self.move_word_back();
            return None;
        }

        // Word right: Alt+F, Alt+Right
        if (mods.alt && matches!(key.code, KeyCode::Char('f'))) || (mods.alt && matches!(key.code, KeyCode::Right)) {
            self.move_word_forward();
            return None;
        }

        // Visual line start (Home without Ctrl)
        if matches!(key.code, KeyCode::Home) && !mods.control && !mods.alt {
            self.move_to_visual_line_start(width);
            self.exit_history_browsing();
            return None;
        }

        // Visual line end (End without Ctrl)
        if matches!(key.code, KeyCode::End) && !mods.control && !mods.alt {
            self.move_to_visual_line_end(width);
            self.exit_history_browsing();
            return None;
        }

        // Vertical movement: Up/Down
        if matches!(key.code, KeyCode::Up) {
            self.move_vertical(width, -1);
            return None;
        }
        if matches!(key.code, KeyCode::Down) {
            self.move_vertical(width, 1);
            return None;
        }

        // Plain printable character
        if let KeyCode::Char(c) = key.code {
            if key.is_plain() {
                self.insert(c);
                return None;
            }
        }

        None
    }

    /// Current visual cursor position as (row, col).
    pub fn visual_cursor(&self, width: usize) -> (usize, usize) {
        let width = width.max(1);
        let (_, carets, _) = self.visual_layout(width);
        let index = self.cursor.min(carets.len().saturating_sub(1));

        if self.cursor_at_wrap_end {
            if let Some(pos) = Self::wrap_end_position(index, &self.buf.chars().collect::<Vec<_>>(), &carets) {
                return pos;
            }
        }

        carets[index]
    }

    /// All visual candidates for cursor positioning.
    fn visual_candidates(&self, width: usize) -> (Vec<VisualCandidate>, usize) {
        let (chars, carets, rows) = self.visual_layout(width);
        let mut candidates = Vec::with_capacity(carets.len() * 2);

        for (index, &(row, col)) in carets.iter().enumerate() {
            candidates.push(VisualCandidate(index, row, col, false));

            if let Some((upstream_row, upstream_col)) =
                Self::wrap_end_position(index, &chars, &carets)
            {
                candidates.push(VisualCandidate(index, upstream_row, upstream_col, true));
            }
        }

        (candidates, rows)
    }

    /// Wrap end position for a character boundary.
    fn wrap_end_position(
        index: usize,
        chars: &[char],
        carets: &[(usize, usize)],
    ) -> Option<(usize, usize)> {
        let previous = *chars.get(index.checked_sub(1)?)?;
        if previous == '\n' {
            return None;
        }
        let (row, col) = carets[index - 1];
        let end = col + UnicodeWidthChar::width(previous).unwrap_or(0).max(1);
        (carets[index].0 > row).then_some((row, end))
    }

    /// Compute visual layout: carets + row count.
    fn visual_layout(&self, width: usize) -> (Vec<char>, Vec<(usize, usize)>, usize) {
        let chars: Vec<char> = self.buf.chars().collect();
        let mut carets = vec![(0, 0); chars.len() + 1];
        let (mut row, mut col) = (0usize, 0usize);

        for (index, ch) in chars.iter().copied().enumerate() {
            let char_width = UnicodeWidthChar::width(ch).unwrap_or(0).max(1);

            if col + char_width > width {
                row += 1;
                col = 0;
            }

            carets[index] = (row, col);
            col += char_width;
            carets[index + 1] = (row, col);
        }

        // Final row count
        let rows = if chars.is_empty() {
            1
        } else {
            row + 1
        };

        (chars, carets, rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(text: &str) -> Input {
        let mut i = Input::new();
        i.set_text(text);
        i
    }

    #[test]
    fn word_motions_hop_whitespace_delimited_words() {
        let mut i = input("hello brave world");
        assert_eq!(i.next_word(), 17, "already at end");
        i.cursor = 0;
        assert_eq!(i.next_word(), 5);
        i.cursor = 5;
        assert_eq!(i.next_word(), 11);
        i.cursor = 17;
        assert_eq!(i.prev_word(), 12);
        i.cursor = 12;
        assert_eq!(i.prev_word(), 6);
    }

    #[test]
    fn kill_commands_split_at_cursor() {
        let mut i = input("hello world");
        i.cursor = 6;
        i.kill_to_start();
        assert_eq!(i.buf, "world");
        assert_eq!(i.cursor, 0);

        let mut i = input("hello world");
        i.cursor = 5;
        i.kill_to_end();
        assert_eq!(i.buf, "hello");
    }

    #[test]
    fn delete_forward_and_multibyte_safety() {
        let mut i = input("a中b");
        i.cursor = 1;
        i.delete_forward();
        assert_eq!(i.buf, "ab");
        assert_eq!(i.cursor, 1);
        i.delete_forward();
        assert_eq!(i.buf, "a");
        i.delete_forward();
        assert_eq!(i.buf, "a", "at end: no-op");
    }

    #[test]
    fn delete_word_back_eats_trailing_whitespace_then_word() {
        let mut i = input("one two   ");
        i.delete_word_back();
        assert_eq!(i.buf, "one ");
        assert_eq!(i.cursor, 4);
    }

    #[test]
    fn vertical_motion_uses_soft_wrapped_visual_rows() {
        let mut i = input("abcdefghij");

        i.move_vertical(4, -1);
        assert_eq!(i.cursor, 6, "wrapped row 2, display column 2");
        i.move_vertical(4, -1);
        assert_eq!(i.cursor, 2, "wrapped row 1, display column 2");
    }

    #[test]
    fn history_browsing() {
        let mut i = input("");
        i.add_to_history("first");
        i.add_to_history("second");
        i.add_to_history("third");

        // Go back in history
        i.move_vertical(80, -1);
        assert_eq!(i.buf, "third");
        i.move_vertical(80, -1);
        assert_eq!(i.buf, "second");
        i.move_vertical(80, -1);
        assert_eq!(i.buf, "first");
        i.move_vertical(80, 1);
        assert_eq!(i.buf, "second");
        i.move_vertical(80, 1);
        assert_eq!(i.buf, "third");
    }

    #[test]
    fn undo_coalescing() {
        let mut i = Input::new();
        i.insert_str("hello");
        i.undo();
        assert_eq!(i.buf, "");
        i.insert_str("world");
        i.undo();
        assert_eq!(i.buf, "");
    }
}
