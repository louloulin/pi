//! Single-line editor component with prompt history.
//!
//! Every chord below is resolved through the process-wide keybinding
//! registry ([`crate::keybindings::get_keybindings`]) with
//! `KeybindingsManager::matches`, so a user override installed with
//! `set_keybindings` (see `pi-coding-agent`'s `keybindings` layer) reaches
//! the editor. With nothing installed the registry serves
//! [`crate::keybindings::tui_default_keybindings`], i.e. the chords the
//! hardcoded judgements used to spell out — the default path is unchanged.
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
//! * `Enter` returns [`EditorAction::Submit`] with the draft text (chips
//!   expanded to their `[Image #N]` labels).
//! * `Ctrl+C` is `tui.input.copy`. This editor has no selection model, so
//!   there is never text to copy: the chord returns
//!   [`EditorAction::Interrupt`], handing it back to the caller (the `App`
//!   reaches `app.clear` / `app.interrupt` from there), exactly like
//!   upstream `Editor.handleInput` returning immediately for
//!   `tui.input.copy` and `CustomEditor` turning it into an app action.
//! * `Ctrl+D` is `app.exit`: on an empty buffer it returns
//!   [`EditorAction::Eof`] so the caller can exit; on a non-empty buffer it
//!   falls through to `tui.editor.deleteCharForward`'s second default
//!   binding (`packages/tui/src/keybindings.ts:121`), exactly like
//!   upstream's `custom-editor.ts:117` fall-through. `app.exit` lives in
//!   the coding-agent's `app.*` table, so a bare `pi-tui` registry does not
//!   contain the id and the built-in `Ctrl+D` chord stands in for it.
//!
//! Chords that have **no consumer** in this single-line port, listed here
//! rather than silently implemented:
//!
//! * `tui.input.newLine` (`shift+enter`, `ctrl+j`): the Rust editor is
//!   single-line, so there is nowhere to insert a newline. `Shift+Enter`
//!   therefore keeps its pre-keybinding behaviour and submits through the
//!   `Enter` branch; `Ctrl+J` stays a no-op. `tui.input.submit` (`enter`)
//!   is resolved first, so rebinding it changes the submit chord.
//! * `tui.editor.pageUp` / `tui.editor.pageDown`: the viewport belongs to
//!   the `App`, which consumes `tui.altScreen.pageUp` / `pageDown` before
//!   the prompt sees the key, so the editor never scrolls.
//! * `tui.editor.historyPrevious` / `historyNext`: unbound by default;
//!   `Up` / `Down` (`tui.editor.cursorUp` / `cursorDown`) drive the
//!   single-line history instead.
//!
//! Legacy control-byte spellings that `keys.ts` normalises before matching
//! (crossterm decodes them differently) are still accepted: `Ctrl+5` /
//! `Ctrl+Alt+5` for `ctrl+]` / `ctrl+alt+]` and `Ctrl+7` / `Ctrl+_` for
//! `ctrl+-`, but only while the id's resolved chords actually contain the
//! canonical spelling.
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
//! # Composer image chips
//!
//! A pasted image is attached to the draft as a **chip**: one [`CHIP_CHAR`]
//! sentinel in the buffer plus an [`ImageContent`] in
//! [`Editor::image_attachments`], aligned by occurrence order (the n-th
//! sentinel is the n-th attachment). Because a chip is a single character,
//! the existing single-character rules already give the semantics the
//! composer needs — `Backspace` / `Delete` remove the whole chip, `Left` /
//! `Right` step across it in one move, and text and chips interleave
//! freely. Mutation paths that remove a *range* of text (kill-to-start/end,
//! word kills, backspace/delete) drop the images whose sentinels fall inside
//! the removed bytes, and any sentinel reaching the kill ring is stripped so
//! a later yank cannot resurrect a chip without its payload. At most
//! [`MAX_IMAGE_ATTACHMENTS`] chips fit in one draft; [`Editor::insert_image`]
//! reports [`ImageInsertOutcome::AtCapacity`] beyond that rather than
//! dropping silently.
//!
//! [`Editor::display_text`] expands each sentinel to `[Image #N]` — what the
//! prompt row shows and the submission text carries — and
//! [`Editor::display_cursor`] maps the raw byte cursor onto that string.
//! Submitting never sends a bare sentinel to the model; the driver turns the
//! attachments into `UserMessage` image blocks.
//!
//! History is stored in a [`VecDeque`] capped at 100 entries (matching
//! the TS implementation); consecutive duplicates are collapsed.
//!
//! [`App`]: crate::App
//! [`Prompt`]: crate::Prompt
//! [`ImageContent`]: pi_protocol::ImageContent

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;

use pi_protocol::ImageContent;

use crate::autocomplete::{AutocompleteItem, AutocompleteProvider};
use crate::input::{InputEvent, Key, KeyCode};
use crate::keybindings::{get_keybindings, KeybindingsManager};
use crate::kill_ring::{KillDirection, KillRing};
use crate::undo_stack::UndoStack;
use crate::word_navigation::{find_word_backward, find_word_forward};

#[cfg(test)]
use crate::input::KeyModifiers;

/// Maximum number of history entries kept by the editor.
pub const HISTORY_LIMIT: usize = 100;

/// Sentinel standing in for one composer image chip in the buffer.
///
/// U+FFFC OBJECT REPLACEMENT CHARACTER is Unicode's "object embedded in
/// text" marker; it is one character wide from the editor's point of view,
/// which is what makes backspace and cursor movement treat a chip
/// atomically. It is never rendered literally —[`Editor::display_text`]
/// replaces it with `[Image #N]`.
pub const CHIP_CHAR: char = '\u{FFFC}';

/// Maximum number of image chips a single draft can hold (the issue's
/// "≤ 8 张" bound). [`Editor::insert_image`] refuses past this instead of
/// silently discarding the paste.
pub const MAX_IMAGE_ATTACHMENTS: usize = 8;

/// Characters that open the autocomplete dropdown at a token boundary
/// by default, upstream `DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS`.
pub const DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS: [char; 2] = ['@', '#'];

/// Default number of dropdown rows, upstream `autocompleteMaxVisible = 5`.
pub const DEFAULT_AUTOCOMPLETE_MAX_VISIBLE: usize = 5;

/// Lower clamp for the dropdown height, upstream's `Math.max(3, …)`.
pub const MIN_AUTOCOMPLETE_MAX_VISIBLE: usize = 3;

/// Upper clamp for the dropdown height, upstream's `Math.min(20, …)`.
pub const MAX_AUTOCOMPLETE_MAX_VISIBLE: usize = 20;

/// A local shell submission typed into the editor (`!cmd` / `!!cmd`).
///
/// Upstream runs these against the local shell without asking the model
/// (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:3106-3118`);
/// `!!` additionally keeps the result out of the model context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashCommand {
    /// The command with the `!` / `!!` prefix and surrounding whitespace
    /// removed. Never empty — an empty command falls back to the normal
    /// prompt path.
    pub command: String,
    /// `true` for `!!` — the transcript shows the output but it must not be
    /// added to the agent's message log.
    pub excluded: bool,
}

/// True while the editor buffer reads as a bash submission — upstream's
/// `onChange` bash-mode test (`text.trimStart().startsWith("!")`,
/// `interactive-mode.ts:2907`), which drives the editor border colour.
///
/// This ignores leading whitespace, unlike [`parse_bash_command`], mirroring
/// upstream where the border test trims but the submit branch does not.
pub fn is_bash_mode(text: &str) -> bool {
    text.trim_start().starts_with('!')
}

/// Parse a submitted buffer as `!cmd` / `!!cmd`.
///
/// Mirrors the upstream submit branch (`interactive-mode.ts:3106-3111`):
/// only a leading `!` counts, `!!` excludes the result from context, and an
/// empty command (`!`, `!!`, or `!   `) yields `None` so the caller falls
/// back to the normal prompt path.
pub fn parse_bash_command(text: &str) -> Option<BashCommand> {
    let rest = text.strip_prefix('!')?;
    let (excluded, command) = match rest.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, rest),
    };
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    Some(BashCommand {
        command: command.to_string(),
        excluded,
    })
}

/// Result of [`Editor::insert_image`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageInsertOutcome {
    /// The chip was inserted at the cursor.
    Inserted,
    /// [`MAX_IMAGE_ATTACHMENTS`] chips are already attached; nothing
    /// changed. Callers surface a status message.
    AtCapacity,
}

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
/// the Rust editor is single-line, so the buffer, cursor and the chip
/// attachments are the whole of it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EditorSnapshot {
    /// Buffer contents at capture time.
    buffer: String,
    /// Cursor byte offset at capture time.
    cursor: usize,
    /// Chip attachments at capture time, aligned with the buffer's
    /// [`CHIP_CHAR`] occurrences.
    images: Vec<ImageContent>,
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
    /// Pasted image chips, aligned with the buffer's [`CHIP_CHAR`]
    /// occurrences: the n-th sentinel carries `images[n]`.
    images: Vec<ImageContent>,
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
            images: Vec::new(),
        }
    }

    /// Borrow the raw buffer.
    ///
    /// A composer chip appears here as [`CHIP_CHAR`]; use
    /// [`Editor::display_text`] for anything the user reads or the model
    /// receives.
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
        self.buffer = strip_chips(&text.into());
        self.cursor = self.buffer.len();
        self.images.clear();
        self.last_action = LastAction::Other;
        self.cancel_autocomplete();
    }

    /// Clear the buffer without touching history, and drop the undo
    /// stack. Upstream clears its stack the same way when a prompt is
    /// submitted, so `Ctrl+-` cannot resurrect an already-sent prompt.
    /// Pasted images go with the text.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.images.clear();
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

    /// The buffer with each chip sentinel rendered as `[Image #N]`.
    ///
    /// This is the user-visible draft and the text a submission carries;
    /// [`Editor::text`] is the raw form that keeps the [`CHIP_CHAR`]
    /// placeholders.
    pub fn display_text(&self) -> String {
        if self.images.is_empty() {
            return self.buffer.clone();
        }
        let mut out = String::with_capacity(self.buffer.len() + self.images.len() * 12);
        let mut index = 0usize;
        for ch in self.buffer.chars() {
            if ch == CHIP_CHAR {
                index += 1;
                out.push_str(&chip_label(index));
            } else {
                out.push(ch);
            }
        }
        out
    }

    /// Cursor column within [`Editor::display_text`] (character count,
    /// with each chip counted as its full `[Image #N]` label).
    pub fn display_cursor(&self) -> usize {
        let label_width = chip_label(1).chars().count();
        self.buffer
            .char_indices()
            .take_while(|(byte, _)| *byte < self.cursor)
            .map(|(_, ch)| if ch == CHIP_CHAR { label_width } else { 1 })
            .sum()
    }

    /// Attached image chips, in buffer order.
    pub fn image_attachments(&self) -> &[ImageContent] {
        &self.images
    }

    /// Number of attached image chips.
    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    /// Attach `image` as a chip at the cursor.
    ///
    /// The chip is one [`CHIP_CHAR`] in the buffer plus an entry in
    /// [`Editor::image_attachments`]; the attachment is inserted at the
    /// index matching the sentinel's position, so the two stay aligned.
    /// Refuses once [`MAX_IMAGE_ATTACHMENTS`] chips are attached.
    pub fn insert_image(&mut self, image: ImageContent) -> ImageInsertOutcome {
        if self.images.len() >= MAX_IMAGE_ATTACHMENTS {
            return ImageInsertOutcome::AtCapacity;
        }
        self.push_undo_snapshot();
        let index = self.buffer[..self.cursor.min(self.buffer.len())]
            .matches(CHIP_CHAR)
            .count();
        self.buffer.insert(self.cursor, CHIP_CHAR);
        self.images.insert(index, image);
        self.cursor += CHIP_CHAR.len_utf8();
        self.reset_history_navigation();
        self.last_action = LastAction::Other;
        self.cancel_autocomplete();
        ImageInsertOutcome::Inserted
    }

    /// Drop every chip, removing their sentinels from the buffer as well.
    pub fn clear_images(&mut self) {
        if self.images.is_empty() && !self.buffer.contains(CHIP_CHAR) {
            return;
        }
        self.images.clear();
        self.buffer = strip_chips(&self.buffer);
        self.cursor = self.cursor.min(self.buffer.len());
    }

    /// Drop the images whose sentinels fall inside `range` (a buffer byte
    /// range that is about to be deleted), keeping chip and attachment
    /// indices aligned.
    fn remove_images_in_range(&mut self, range: Range<usize>) {
        let start = range.start.min(self.buffer.len());
        let end = range.end.min(self.buffer.len());
        if start >= end {
            return;
        }
        let base = self.buffer[..start].matches(CHIP_CHAR).count();
        let removed = self.buffer[start..end].matches(CHIP_CHAR).count();
        if removed > 0 {
            self.images.drain(base..base + removed);
        }
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
        if c == CHIP_CHAR {
            // A literal sentinel would desync the buffer from
            // `image_attachments`; only `insert_image` may place one.
            return EditorAction::None;
        }
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
        let s = strip_chips(s);
        if s.is_empty() {
            // The paste was nothing but chip sentinels; there is no text to
            // insert and the images themselves are not part of the paste.
            return EditorAction::None;
        }
        self.push_undo_snapshot();
        self.buffer.insert_str(self.cursor, &s);
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
        self.remove_images_in_range(prev..self.cursor);
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
        self.remove_images_in_range(self.cursor..next);
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
        let killed = strip_chips(&self.buffer[..self.cursor]);
        let accumulate = self.last_action == LastAction::Kill;
        self.remove_images_in_range(0..self.cursor);
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
        let killed = strip_chips(&self.buffer[self.cursor..]);
        let accumulate = self.last_action == LastAction::Kill;
        self.remove_images_in_range(self.cursor..self.buffer.len());
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
        let killed = strip_chips(&self.buffer[delete_from..self.cursor]);
        // Read the previous action *before* overwriting it: a kill that
        // follows another kill accumulates into the same ring entry
        // instead of opening a new chain (upstream `deleteWordBackwards`).
        let accumulate = self.last_action == LastAction::Kill;
        self.remove_images_in_range(delete_from..self.cursor);
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
        let killed = strip_chips(&self.buffer[self.cursor..delete_to]);
        let accumulate = self.last_action == LastAction::Kill;
        self.remove_images_in_range(self.cursor..delete_to);
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
        let text = strip_chips(&text);
        if text.is_empty() {
            return EditorAction::None;
        }
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
        self.remove_images_in_range(start..self.cursor);
        self.buffer.replace_range(start..self.cursor, "");
        self.cursor = start;
        // Rotate first, then read: the next entry to insert is now the
        // most recent one (upstream `yankPop` order).
        self.kill_ring.rotate();
        let text = strip_chips(
            &self
                .kill_ring
                .peek()
                .map(str::to_string)
                .unwrap_or_default(),
        );
        if text.is_empty() {
            self.last_action = LastAction::Yank;
            self.reset_history_navigation();
            return EditorAction::Changed;
        }
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
        self.images = snapshot.images;
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
        self.buffer = strip_chips(&result.lines.into_iter().next().unwrap_or_default());
        self.cursor = clamp_to_char_boundary(&self.buffer, result.cursor_col);
        // A completion rewrites the whole line; the chip/attachment pairing
        // cannot survive that, so the chips go with it (same rule as
        // `set_text_internal`).
        self.images.clear();
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
            images: self.images.clone(),
        });
    }

    /// True for the jump hotkeys: `Ctrl+]` (`tui.editor.jumpForward`)
    /// and `Ctrl+Alt+]` (`tui.editor.jumpBackward`), together with the
    /// legacy `Ctrl+5` / `Ctrl+Alt+5` spellings crossterm produces for
    /// the `0x1D` / `ESC 0x1D` byte sequences.
    fn is_jump_binding(kb: &KeybindingsManager, key: &Key) -> bool {
        Self::matches_binding(kb, key, "tui.editor.jumpForward")
            || Self::matches_binding(kb, key, "tui.editor.jumpBackward")
    }

    /// True when `key` triggers `keybinding` under the installed table.
    ///
    /// The registry's [`KeybindingsManager::matches`] is the primary
    /// judgement; the legacy control-byte spellings crossterm derives are
    /// layered on top by [`legacy_key_spelling`].
    fn matches_binding(kb: &KeybindingsManager, key: &Key, keybinding: &str) -> bool {
        kb.matches(&InputEvent::Key(*key), keybinding) || legacy_key_spelling(kb, key, keybinding)
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

    /// True when `key` triggers `app.exit` (`Ctrl+D`).
    ///
    /// `app.exit` belongs to the coding-agent's `app.*` table, so a bare
    /// `pi-tui` registry does not contain it; the port's built-in chord then
    /// stands in, keeping the standalone editor's behaviour (and its tests)
    /// intact.
    fn matches_app_exit(kb: &KeybindingsManager, key: &Key) -> bool {
        crate::keybindings::matches_with_fallback(
            kb,
            &InputEvent::Key(*key),
            "app.exit",
            &["ctrl+d"],
        )
    }

    /// Process a key.
    pub fn handle_key(&mut self, key: Key) -> EditorAction {
        let kb = get_keybindings();

        // An armed jump consumes this key: a printable character is the
        // target, the hotkey again (or anything that is not a plain
        // printable character) cancels the mode. Cancelling keys fall
        // through to their normal handling, mirroring upstream
        // `Editor.handleInput`.
        if let Some(direction) = self.jump_mode.take() {
            if Self::is_jump_binding(&kb, &key) {
                return EditorAction::None;
            }
            if !key.modifiers.control && !key.modifiers.alt {
                if let KeyCode::Char(c) = key.code {
                    return self.jump_to_char(c, direction);
                }
            }
        }

        // `tui.editor.jumpBackward` / `jumpForward`: arm a one-shot
        // character jump. Checked before every other chord so the
        // Alt-modified backward spelling (which the other branches would
        // otherwise not recognise) is handled first.
        if Self::matches_binding(&kb, &key, "tui.editor.jumpBackward") {
            self.jump_mode = Some(JumpDirection::Backward);
            return EditorAction::None;
        }
        if Self::matches_binding(&kb, &key, "tui.editor.jumpForward") {
            self.jump_mode = Some(JumpDirection::Forward);
            return EditorAction::None;
        }

        // `tui.input.copy` (`Ctrl+C`). There is no selection to copy in
        // this editor, so the chord is handed back to the caller; see the
        // module docs.
        if Self::matches_binding(&kb, &key, "tui.input.copy") {
            return EditorAction::Interrupt;
        }

        // `app.exit` (`Ctrl+D`): exit only while the buffer is empty,
        // otherwise fall through to `tui.editor.deleteCharForward`,
        // exactly like upstream `CustomEditor` (`custom-editor.ts:117`).
        if Self::matches_app_exit(&kb, &key) && self.buffer.is_empty() {
            return EditorAction::Eof;
        }

        // Autocomplete dropdown. Checked above the plain-key handling so
        // the selection chords steer the candidate list while it is open,
        // mirroring upstream `Editor.handleInput`'s autocomplete block
        // (which runs after undo, before Tab and deletion).
        if self.is_showing_autocomplete() {
            if Self::matches_binding(&kb, &key, "tui.select.cancel") {
                self.cancel_autocomplete();
                return EditorAction::None;
            }
            if Self::matches_binding(&kb, &key, "tui.select.up") {
                self.move_autocomplete(-1);
                return EditorAction::Changed;
            }
            if Self::matches_binding(&kb, &key, "tui.select.down") {
                self.move_autocomplete(1);
                return EditorAction::Changed;
            }
            if Self::matches_binding(&kb, &key, "tui.input.tab") {
                return self.accept_autocomplete();
            }
            if Self::matches_binding(&kb, &key, "tui.input.submit") {
                let prefix = self.autocomplete_prefix.clone();
                let applied = self.accept_autocomplete();
                if applied == EditorAction::None {
                    return EditorAction::None;
                }
                // A command name is still submitted: upstream falls
                // through to the normal Enter handling when the
                // prefix starts with `/`.
                if prefix.starts_with('/') {
                    return EditorAction::Submit(self.display_text());
                }
                return applied;
            }
        }

        // Tab with the dropdown closed forces a completion
        // (`tui.input.tab`).
        if Self::matches_binding(&kb, &key, "tui.input.tab") && self.handle_tab_completion() {
            return EditorAction::Changed;
        }

        // Deletion chords.
        if Self::matches_binding(&kb, &key, "tui.editor.deleteToLineStart") {
            return self.kill_to_line_start();
        }
        if Self::matches_binding(&kb, &key, "tui.editor.deleteToLineEnd") {
            return self.kill_to_line_end();
        }
        if Self::matches_binding(&kb, &key, "tui.editor.deleteWordBackward") {
            return self.kill_word_backward();
        }
        if Self::matches_binding(&kb, &key, "tui.editor.deleteWordForward") {
            return self.kill_word_forward();
        }
        if Self::matches_binding(&kb, &key, "tui.editor.deleteCharBackward") {
            return self.backspace();
        }
        // `Delete` and `Ctrl+D` (the Latter already handled above while
        // the buffer was empty).
        if Self::matches_binding(&kb, &key, "tui.editor.deleteCharForward") {
            return self.delete();
        }

        // Kill-ring chords.
        if Self::matches_binding(&kb, &key, "tui.editor.yank") {
            return self.yank();
        }
        if Self::matches_binding(&kb, &key, "tui.editor.yankPop") {
            return self.yank_pop();
        }

        // Cursor movement.
        if Self::matches_binding(&kb, &key, "tui.editor.cursorLineStart") {
            return self.move_home();
        }
        if Self::matches_binding(&kb, &key, "tui.editor.cursorLineEnd") {
            return self.move_end();
        }
        if Self::matches_binding(&kb, &key, "tui.editor.cursorWordLeft") {
            return self.move_word_left();
        }
        if Self::matches_binding(&kb, &key, "tui.editor.cursorWordRight") {
            return self.move_word_right();
        }
        // `Up` / `Down` browse the single-line prompt history, the
        // roles the pre-keybinding editor gave them.
        if Self::matches_binding(&kb, &key, "tui.editor.cursorUp") {
            return self.history_prev();
        }
        if Self::matches_binding(&kb, &key, "tui.editor.cursorDown") {
            return self.history_next();
        }
        if Self::matches_binding(&kb, &key, "tui.editor.cursorLeft") {
            return self.move_left();
        }
        if Self::matches_binding(&kb, &key, "tui.editor.cursorRight") {
            return self.move_right();
        }

        // `tui.editor.undo`.
        if Self::matches_binding(&kb, &key, "tui.editor.undo") {
            return self.undo();
        }

        // `tui.input.submit` (`Enter`).
        if Self::matches_binding(&kb, &key, "tui.input.submit") {
            return EditorAction::Submit(self.display_text());
        }

        // Meta chords are not part of the default table; ignore them
        // rather than inserting their character (the plain-key vocabulary
        // below rejects control / Alt / meta input).
        if key.modifiers.meta {
            return EditorAction::None;
        }

        // A control or Alt chord that matched no binding never inserts
        // its character.
        if key.modifiers.control || key.modifiers.alt {
            return EditorAction::None;
        }

        // Shift-modified spellings of the chords above keep their
        // pre-keybinding behaviour. The registry matches modifiers exactly,
        // so `Shift+Up` is not `tui.editor.cursorUp`, yet the old plain
        // match-arm fallback sent it to history navigation (`Shift+Home` to
        // the line start, and so on). The bare keys — `Up`, `Backspace`,
        // `Home`, ... — are the ids' own defaults and no longer reach here
        // once the id is rebound or unbound, so they are deliberately
        // absent.
        match key.code {
            KeyCode::Char(c) => self.insert_char(c),
            // `Shift+Enter` is `tui.input.newLine`, which the single-line
            // editor cannot honour; the pre-keybinding editor submitted on
            // it, so it keeps submitting (see the module docs).
            KeyCode::Enter if key.modifiers.shift => {
                let submitted = self.display_text();
                EditorAction::Submit(submitted)
            }
            KeyCode::Left if key.modifiers.shift => self.move_left(),
            KeyCode::Right if key.modifiers.shift => self.move_right(),
            KeyCode::Home if key.modifiers.shift => self.move_home(),
            KeyCode::End if key.modifiers.shift => self.move_end(),
            KeyCode::Up if key.modifiers.shift => self.history_prev(),
            KeyCode::Down if key.modifiers.shift => self.history_next(),
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

/// The visible label for the n-th composer image chip (1-based).
fn chip_label(index: usize) -> String {
    format!("[Image #{index}]")
}

/// Remove every chip sentinel from `text`.
///
/// Used wherever raw text enters the buffer from outside the chip model
/// (programmatic `set_text`, the kill ring, completion insertions), so a
/// stray sentinel can never desync the buffer from
/// [`Editor::image_attachments`].
fn strip_chips(text: &str) -> String {
    if text.contains(CHIP_CHAR) {
        text.replace(CHIP_CHAR, "")
    } else {
        text.to_string()
    }
}

/// Accept the legacy control-byte spelling of a resolved chord.
///
/// `keys.ts` normalises the legacy bytes before matching: `0x1D` / `ESC
/// 0x1D` become `ctrl+]` / `ctrl+alt+]` and `0x1F` becomes `ctrl+-`. The
/// Rust port hands decoding to crossterm, which instead reports those bytes
/// as `Ctrl+5` / `Ctrl+Alt+5` (`0x1C..=0x1F` map to `Ctrl+4..=Ctrl+7`,
/// `event/sys/unix/parse.rs`) and `Ctrl+7` / `Ctrl+_`. Treat the two
/// spellings as the same chord, but only while the id actually resolves to
/// the canonical one — rebinding `tui.editor.undo` to `ctrl+z` must stop
/// `Ctrl+7` from undoing.
fn legacy_key_spelling(kb: &KeybindingsManager, key: &Key, keybinding: &str) -> bool {
    let keys = kb.get_keys(keybinding);
    let bound = |chord: &str| keys.iter().any(|candidate| candidate == chord);

    if key.modifiers.control && !key.modifiers.alt {
        if bound("ctrl+-") {
            return matches!(key.code, KeyCode::Char('7') | KeyCode::Char('_'));
        }
        if bound("ctrl+]") {
            return matches!(key.code, KeyCode::Char('5'));
        }
    }
    if key.modifiers.control && key.modifiers.alt && bound("ctrl+alt+]") {
        return matches!(key.code, KeyCode::Char('5'));
    }
    false
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

    // -------------------------------------------------------------------
    // `!` / `!!` local bash prefix detection (LUM-1223)
    // -------------------------------------------------------------------

    #[test]
    fn bash_mode_ignores_leading_whitespace() {
        assert!(is_bash_mode("!ls"));
        assert!(is_bash_mode("  !!ls"));
        assert!(is_bash_mode("!"));
        assert!(!is_bash_mode("ls"));
        assert!(!is_bash_mode(""));
    }

    #[test]
    fn parses_bang_and_double_bang_commands() {
        let one = parse_bash_command("!echo hi").expect("! parses");
        assert_eq!(one.command, "echo hi");
        assert!(!one.excluded);

        let two = parse_bash_command("!!echo hi").expect("!! parses");
        assert_eq!(two.command, "echo hi");
        assert!(two.excluded);

        // The submit branch only recognises a leading `!`; leading whitespace
        // is not trimmed there (upstream `text.startsWith("!")`).
        assert!(parse_bash_command("  !echo hi").is_none());
        assert_eq!(parse_bash_command("!  spaced  ").unwrap().command, "spaced");
    }

    #[test]
    fn empty_bash_commands_fall_back_to_the_prompt() {
        assert!(parse_bash_command("!").is_none());
        assert!(parse_bash_command("!!").is_none());
        assert!(parse_bash_command("!   ").is_none());
        assert!(parse_bash_command("!!   ").is_none());
    }

    // -------------------------------------------------------------------
    // Composer image chips (LUM-1224)
    // -------------------------------------------------------------------

    fn image(data: &str) -> ImageContent {
        ImageContent {
            mime_type: "image/png".to_string(),
            data: data.to_string(),
        }
    }

    #[test]
    fn chip_renders_as_a_label_and_submits_its_display_text() {
        let mut ed = Editor::new();
        ed.insert_str("look");
        assert_eq!(ed.insert_image(image("aaa")), ImageInsertOutcome::Inserted);
        // Raw buffer keeps the sentinel; the user-visible draft expands it.
        assert_eq!(ed.text(), format!("look{CHIP_CHAR}"));
        assert_eq!(ed.display_text(), "look[Image #1]");
        assert_eq!(ed.image_count(), 1);
        assert_eq!(ed.display_cursor(), "look[Image #1]".chars().count());

        match ed.handle_key(Key::new(KeyCode::Enter, KeyModifiers::NONE)) {
            EditorAction::Submit(text) => assert_eq!(text, "look[Image #1]"),
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn backspace_deletes_the_whole_chip() {
        let mut ed = Editor::new();
        ed.insert_str("a");
        ed.insert_image(image("one"));
        ed.insert_str("b");
        assert_eq!(ed.display_text(), "a[Image #1]b");

        // Cursor sits after `b`; one Backspace removes `b` only.
        ed.backspace();
        assert_eq!(ed.display_text(), "a[Image #1]");
        assert_eq!(ed.image_count(), 1);
        // The next Backspace removes the chip in a single stroke.
        ed.backspace();
        assert_eq!(ed.display_text(), "a");
        assert_eq!(ed.image_count(), 0);
        assert_eq!(ed.text(), "a");
    }

    #[test]
    fn delete_removes_the_chip_at_the_cursor() {
        let mut ed = Editor::new();
        ed.insert_image(image("one"));
        ed.insert_str("tail");
        ed.move_home();
        ed.delete();
        assert_eq!(ed.display_text(), "tail");
        assert_eq!(ed.image_count(), 0);
    }

    #[test]
    fn cursor_steps_across_a_chip_as_one_character() {
        let mut ed = Editor::new();
        ed.insert_str("ab");
        ed.insert_image(image("one"));
        ed.insert_str("cd");
        assert_eq!(ed.cursor(), 2 + CHIP_CHAR.len_utf8() + 2);

        // Left crosses `d`, then `c`, then the whole chip in one move, then
        // `b` — the chip is a single character to the cursor.
        ed.move_left();
        assert_eq!(ed.cursor(), 6);
        ed.move_left();
        assert_eq!(ed.cursor(), 5);
        ed.move_left();
        assert_eq!(ed.cursor(), 2);
        ed.move_left();
        assert_eq!(ed.cursor(), 1);
        // Right steps back across the whole chip.
        ed.move_right();
        assert_eq!(ed.cursor(), 2);
        ed.move_right();
        assert_eq!(ed.cursor(), 2 + CHIP_CHAR.len_utf8());
    }

    #[test]
    fn draft_split_keeps_text_and_images_interleaved() {
        // Mirrors Martty's `draft_split_keeps_text_and_images_interleaved`:
        // text typed before, between and after chips must stay in order and
        // the attachment list must line up with the sentinels.
        let mut ed = Editor::new();
        ed.insert_str("before ");
        ed.insert_image(image("one"));
        ed.insert_str(" middle ");
        ed.insert_image(image("two"));
        ed.insert_str(" after");

        assert_eq!(
            ed.display_text(),
            "before [Image #1] middle [Image #2] after"
        );
        assert_eq!(ed.image_attachments(), &[image("one"), image("two")]);

        // Inserting at the start shifts the attachment indices with the
        // sentinel, so the pairing never desyncs.
        ed.move_home();
        ed.insert_image(image("zero"));
        assert_eq!(
            ed.image_attachments(),
            &[image("zero"), image("one"), image("two")]
        );
        assert!(ed.display_text().starts_with("[Image #1]before "));
    }

    #[test]
    fn insert_image_refuses_past_the_capacity() {
        let mut ed = Editor::new();
        for index in 0..MAX_IMAGE_ATTACHMENTS {
            assert_eq!(
                ed.insert_image(image(&format!("img-{index}"))),
                ImageInsertOutcome::Inserted
            );
        }
        assert_eq!(
            ed.insert_image(image("overflow")),
            ImageInsertOutcome::AtCapacity
        );
        assert_eq!(ed.image_count(), MAX_IMAGE_ATTACHMENTS);
        // The refused paste did not disturb the buffer.
        assert_eq!(ed.text().matches(CHIP_CHAR).count(), MAX_IMAGE_ATTACHMENTS);
    }

    #[test]
    fn kill_ring_never_resurrects_a_chip_without_its_payload() {
        let mut ed = Editor::new();
        ed.insert_str("keep ");
        ed.insert_image(image("one"));
        ed.insert_str(" tail");
        ed.move_home();
        // Kill the whole line: the chip is inside the removed range, and the
        // sentinel is stripped from the kill-ring text.
        ed.kill_to_line_end();
        assert_eq!(ed.image_count(), 0);
        assert!(!ed.text().contains(CHIP_CHAR));
        // Yanking the killed text back must not insert a sentinel.
        ed.yank();
        assert!(!ed.text().contains(CHIP_CHAR));
    }

    #[test]
    fn killing_a_chip_drops_its_attachment() {
        let mut ed = Editor::new();
        ed.insert_str("a");
        ed.insert_image(image("one"));
        ed.insert_str("b");
        ed.insert_image(image("two"));
        // Cursor after the second chip; kill to line start removes both
        // chips (and the text) in one range.
        ed.kill_to_line_start();
        assert_eq!(ed.image_count(), 0);
        assert!(ed.text().is_empty());
    }

    #[test]
    fn programmatic_set_text_drops_chips() {
        let mut ed = Editor::new();
        ed.insert_image(image("one"));
        ed.set_text("replacement");
        assert_eq!(ed.image_count(), 0);
        assert_eq!(ed.text(), "replacement");
        // A stray sentinel in incoming text is stripped, not adopted.
        ed.set_text(format!("x{CHIP_CHAR}y"));
        assert_eq!(ed.image_count(), 0);
        assert_eq!(ed.text(), "xy");
    }

    #[test]
    fn clear_drops_chips() {
        let mut ed = Editor::new();
        ed.insert_str("a");
        ed.insert_image(image("one"));
        ed.clear();
        assert_eq!(ed.image_count(), 0);
        assert!(ed.is_empty());
    }

    #[test]
    fn undo_restores_a_deleted_chip() {
        let mut ed = Editor::new();
        ed.insert_str("a");
        ed.insert_image(image("one"));
        ed.backspace();
        assert_eq!(ed.image_count(), 0);
        ed.undo();
        assert_eq!(ed.image_count(), 1);
        assert_eq!(ed.display_text(), "a[Image #1]");
    }
}
