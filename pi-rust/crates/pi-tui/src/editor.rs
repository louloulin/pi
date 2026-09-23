//! Composer editor component: a multi-line buffer with prompt history.
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
//! * `PageUp` / `PageDown` (`tui.editor.pageUp` / `pageDown`, also
//!   `Ctrl+PageUp` / `Ctrl+PageDown`) move the cursor one *page* of visual
//!   rows, keeping the display column, exactly like upstream
//!   `Editor.pageScroll` (`packages/tui/src/components/editor.ts:1949`):
//!   the cursor moves and the composer window then follows it, so the page
//!   key pages the draft rather than the transcript. The `App` decides
//!   which of the two the bare keys mean — see `App::composer_overflows`:
//!   while the draft fits the composer window the chord stays the
//!   transcript's, which is what upstream's fullscreen keybinding table
//!   does with its shadowing duplicate (`packages/tui/src/keybindings.ts:159`).
//! * `Up` / `Down` (`tui.editor.cursorUp` / `cursorDown`) move the cursor
//!   one *visual* row at a time while a draft occupies more than one row,
//!   keeping the display column across rows; at the first visual row they
//!   browse the prompt history (most recent first) and at the last visual
//!   row they leave the history again. This is upstream's rule
//!   (`Editor.handleInput`, `packages/tui/src/components/editor.ts:913-940`)
//!   and the one codex documents for its own composer ("Up Arrow / Ctrl+P
//!   moves up through the recalled message line by line, or through prompt
//!   history depending on cursor position", codex#21833). The first `Up`
//!   saves the current draft so `Down` past the bottom of the history
//!   restores it.
//! * `Home` / `End` (`tui.editor.cursorLineStart` / `cursorLineEnd`, also
//!   `Ctrl+A` / `Ctrl+E`) move to the start / end of the **logical line**
//!   the cursor is on, which is what upstream `moveToLineStart` /
//!   `moveToLineEnd` do and what codex documents (`Home` / `Ctrl+A` jumps
//!   to the start of the current line, not the top of the whole draft —
//!   codex#21833). On a single-line draft that is the whole buffer, which
//!   is the port's historical behaviour.
//! * `Enter` returns [`EditorAction::Submit`] with the draft text (chips
//!   expanded to their `[Image #N]` labels, paste markers expanded to the
//!   text they stand for), except when the character before the cursor is a
//!   backslash: then the backslash is deleted and a newline is inserted
//!   instead (upstream's fallback for terminals that cannot report
//!   `Shift+Enter`).
//! * A bracketed paste ([`InputEvent::Paste`], enabled by the driver) goes
//!   through [`Editor::insert_paste`]: one undo unit, line endings
//!   normalized, control bytes dropped, and nothing larger than
//!   [`PASTE_MARKER_LINE_THRESHOLD`] lines / [`PASTE_MARKER_CHAR_THRESHOLD`]
//!   characters is inserted inline — a bigger paste is stored behind a
//!   `[paste #N +L lines]` marker whose content [`Editor::expanded_text`]
//!   puts back for the model. `Backspace` / `Delete` remove a whole marker,
//!   and `Left` / `Right` step over one, so a marker can never be torn in
//!   half. Mirrors upstream `handlePaste`
//!   (`packages/tui/src/components/editor.ts:1248`).
//! * `tui.input.newLine` (`shift+enter`, `ctrl+j`) inserts a newline at the
//!   cursor, so the composer grows instead of submitting. `tui.input.submit`
//!   (`enter`) is resolved first, so rebinding it changes the submit chord.
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
//! * `tui.editor.historyPrevious` / `historyNext`: dedicated history chords,
//!   unbound by default. When bound they browse entries (and close any open
//!   autocomplete) regardless of where the cursor sits, exactly like
//!   upstream `packages/tui/src/components/editor.ts:848-856`; `Up` / `Down`
//!   remain the cursor-aware path.
//!
//! Legacy control-byte spellings that `keys.ts` normalises before matching
//! (crossterm decodes them differently) are still accepted: `Ctrl+5` /
//! `Ctrl+Alt+5` for `ctrl+]` / `ctrl+alt+]` and `Ctrl+7` / `Ctrl+_` for
//! `ctrl+-`, but only while the id's resolved chords actually contain the
//! canonical spelling.
//!
//! * `Ctrl+U` / `Ctrl+K` kill to the start / end of the **logical line** the
//!   cursor is on and push the killed text onto the [`KillRing`]; at a line
//!   boundary the newline itself is what gets killed, which merges the two
//!   lines — upstream `deleteToStartOfLine` / `deleteToEndOfLine`. Consecutive
//!   kills accumulate into one ring entry, exactly like upstream. On a
//!   single-line draft the logical line is the buffer, so the historical
//!   behaviour is unchanged.
//! * `Ctrl+W` / `Alt+Backspace` kill the word before the cursor and
//!   `Alt+D` / `Alt+Delete` the word after it
//!   (`tui.editor.deleteWordBackward` / `deleteWordForward`). Word kills are
//!   scoped to the logical line and take part in the same accumulation
//!   chain as the line kills.
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
//!   The dropdown's rows are laid out by the shared `SelectList`
//!   implementation in [`crate::selector`]
//!   ([`Editor::autocomplete_render_styled_lines`]) rather than by a second
//!   renderer here: upstream renders a real `SelectList` under the composer
//!   (`components/editor.ts:605-614`), so the label column, the description
//!   column, the width thresholds and the scroll window are the same code
//!   path the modal pickers use.
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
//! # Prompt history
//!
//! History is a [`VecDeque`] of [`HistoryEntry`], capped at
//! [`HISTORY_LIMIT`] entries with consecutive duplicates collapsed. An entry
//! keeps the draft's chips, so an `Up` recall restores `[Image #N]`
//! attachments too, not just the text.
//!
//! `Ctrl+R` opens a **reverse history search**
//! ([`Editor::begin_history_search`]): the footer carries
//! `reverse-i-search: <query>` while the composer previews the newest
//! matching entry. Typing narrows the query (each edit restarts from the
//! newest match), `Ctrl+R` / `Up` walk to older matches, `Ctrl+S` / `Down`
//! back to newer ones, `Enter` accepts the preview as an ordinary editable
//! draft, and `Esc` / `Ctrl+C` put back the exact draft that existed before
//! the search opened. Matching is a case-insensitive substring test and the
//! traversal de-duplicates by exact text, both as codex's
//! `ChatComposerHistory` does. The port's upstream (`packages/tui`) has no
//! such binding; the chord is codex's.
//!
//! When a history file is configured ([`Editor::set_history_path`]) accepted
//! prompts are appended to it and read back at startup, so recall survives a
//! restart (`crate::history_store`). Persistence is **off by default**: a
//! test or an embedder must never touch the user's real history.
//!
//! # Visual rows
//!
//! Everything that needs to know where a character sits on screen — the
//! `▍` marker, `Up` / `Down`, the scroll window — goes through
//! [`crate::visual_text::VisualLayout`], the same layout [`crate::Prompt`]
//! paints with. The App hands the editor the width it wrapped at
//! ([`Editor::set_visual_width`]) before every key press; until the first
//! frame is painted the width is unknown, and a draft then lays out one row
//! per hard line, so vertical motion still walks what the user typed.
//!
//! [`App`]: crate::App
//! [`Prompt`]: crate::Prompt
//! [`ImageContent`]: pi_protocol::ImageContent

use std::collections::{BTreeMap, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_protocol::ImageContent;

use crate::autocomplete::{AutocompleteItem, AutocompleteProvider};
use crate::input::{InputEvent, Key, KeyCode};
use crate::keybindings::{get_keybindings, KeybindingsManager};
use crate::kill_ring::{KillDirection, KillRing};
use crate::selector::{select_list_row_spans, select_list_visible_range, SelectorLayout};
use crate::styled::{plain_text, SpanStyle, StyledLine, StyledSpan};
use crate::theme::ThemeColor;
use crate::undo_stack::UndoStack;
use crate::visual_text::VisualLayout;
use crate::word_navigation::{
    find_word_backward, find_word_backward_with_atoms, find_word_forward,
    find_word_forward_with_atoms,
};

#[cfg(test)]
use crate::input::KeyModifiers;

/// Maximum number of history entries kept by the editor.
pub const HISTORY_LIMIT: usize = 100;

/// Opening of a paste marker, `[paste #`. The marker scan
/// ([`expand_paste_markers`]) and [`Editor::expanded_text`] both look for
/// this literal.
pub const PASTE_MARKER_PREFIX: &str = "[paste #";

/// A paste with more lines than this is replaced by a `[paste #N +L lines]`
/// marker instead of being inserted inline (upstream `handlePaste`,
/// `packages/tui/src/components/editor.ts:1316`).
pub const PASTE_MARKER_LINE_THRESHOLD: usize = 10;

/// A paste with more characters than this is replaced by a
/// `[paste #N C chars]` marker (same upstream rule, the single-line half).
pub const PASTE_MARKER_CHAR_THRESHOLD: usize = 1000;
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

/// Result of [`Editor::insert_paste`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteInsertOutcome {
    /// The paste carried no printable character at all; the draft is
    /// untouched.
    Ignored,
    /// The text was inserted at the cursor.
    Inserted,
    /// The paste was larger than the marker thresholds; it is kept in the
    /// registry and stands in the draft as a `[paste #N …]` marker with
    /// this id.
    Marker(u32),
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
/// One recallable prompt: its text plus the chips it was submitted with.
///
/// The text keeps [`CHIP_CHAR`] sentinels aligned with [`HistoryEntry::images`]
/// (the n-th sentinel is the n-th payload), which is what lets a recall put a
/// pasted image back into the draft instead of silently dropping it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// Raw buffer text; composer chips appear as [`CHIP_CHAR`] sentinels.
    pub text: String,
    /// Chip payloads, in sentinel order.
    pub images: Vec<ImageContent>,
}

impl HistoryEntry {
    /// A text-only entry (the persisted form, and the form every entry takes
    /// once a draft carries no chips).
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: strip_chips(&text.into()),
            images: Vec::new(),
        }
    }

    /// An entry that carries a draft's chips.
    ///
    /// Falls back to [`HistoryEntry::new`] when there is nothing to carry, so
    /// the "no images" fast path and the sentinel-free invariant both hold.
    pub fn with_images(text: impl Into<String>, images: Vec<ImageContent>) -> Self {
        let text = text.into();
        if images.is_empty() {
            return Self::new(text);
        }
        Self { text, images }
    }

    /// Number of chip sentinels present in [`HistoryEntry::text`].
    fn sentinels(&self) -> usize {
        self.text.matches(CHIP_CHAR).count()
    }

    /// The user-visible text (`[Image #N]` expanded), i.e. what the composer
    /// row shows and what a submission carries.
    pub fn display_text(&self) -> String {
        if self.images.is_empty() {
            return self.text.clone();
        }
        expand_chips(&self.text)
    }
}

/// Direction of a reverse history search step (`tui.editor.historySearch` /
/// `tui.editor.historySearchNext`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistorySearchDirection {
    /// Towards older prompts — `Ctrl+R`, `Up` (codex
    /// `HistorySearchDirection::Older`).
    Older,
    /// Back towards the newest match — `Ctrl+S`, `Down`.
    Newer,
}

/// What the reverse-search footer should say (codex
/// `HistorySearchStatus`, minus the async `Searching` phase the port has no
/// need for — the whole history is already in memory).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistorySearchStatus {
    /// A query is typed but nothing matches it; the original draft is back.
    NoMatch,
    /// A history entry is being previewed in the composer.
    Match,
}

/// State for one reverse-history-search session.
///
/// The session owns three things codex's `HistorySearchSession` also owns: the
/// footer **query** (kept apart from the preview so a transient match can never
/// destroy what the user was typing), the pre-search **draft** (restored on
/// cancel / miss), and the traversal cursor over a de-duplicated match list.
#[derive(Debug, Clone)]
struct HistorySearch {
    /// The footer query, typed while the search is open.
    query: String,
    /// Draft to restore when the search is cancelled or the query misses.
    draft: String,
    /// Chips of that draft.
    draft_images: Vec<ImageContent>,
    /// Matching history indices, newest first, de-duplicated by exact text
    /// (codex's `unique_matches`).
    matches: Vec<usize>,
    /// Index into `matches` currently previewed (`None` = nothing previewed).
    selected: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditorSnapshot {
    /// Buffer contents at capture time.
    buffer: String,
    /// Cursor byte offset at capture time.
    cursor: usize,
    /// Chip attachments at capture time, aligned with the buffer's
    /// [`CHIP_CHAR`] occurrences.
    images: Vec<ImageContent>,
    /// Paste registry at capture time. Upstream's undo snapshot carries the
    /// same map (`EditorSnapshot`, `components/editor.ts:222-226`), so
    /// undoing away a marker also puts its content back.
    pastes: BTreeMap<u32, String>,
    /// Id counter at capture time, for the same reason.
    paste_counter: u32,
}

/// Multi-line text editor with prompt history and an Emacs-style kill
/// ring.
#[derive(Debug, Clone)]
pub struct Editor {
    buffer: String,
    cursor: usize,
    /// Width the App wraps the composer body at, recorded before key
    /// dispatch. `0` until the first frame is painted.
    visual_width: usize,
    /// Rows one `PageUp` / `PageDown` covers, recorded before key dispatch
    /// (the height of the composer window the last frame painted). `0`
    /// until a frame is painted, in which case a page is a single row.
    page_rows: usize,
    /// Sticky display column kept across a run of vertical cursor moves,
    /// upstream's `preferredVisualCol`.
    preferred_col: Option<usize>,
    history: VecDeque<HistoryEntry>,
    history_index: Option<usize>,
    /// Draft saved when the user starts navigating history. Restored
    /// when the user navigates back below index 0.
    history_draft: Option<HistoryEntry>,
    /// Cross-session history file. `None` disables persistence (the
    /// default, so tests and embedders never touch the user's home).
    history_path: Option<PathBuf>,
    /// Active `Ctrl+R` reverse search, if any.
    history_search: Option<HistorySearch>,
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
    /// Content of every paste that stands in the draft as a `[paste #N …]`
    /// marker, keyed by the id the marker spells.
    ///
    /// Deliberately **not** dropped by [`Editor::clear`]: a prompt-history
    /// recall restores the marker text, and expanding it again has to find
    /// the content (upstream keeps its `pastes` map alive the same way).
    pastes: BTreeMap<u32, String>,
    /// Id handed to the next paste marker; monotonic per editor instance.
    paste_counter: u32,
    /// A recall put a marker into the draft whose content this editor never
    /// had (a marker that only ever existed in the cross-session history
    /// file). The caller can surface one hint about it.
    stale_paste_notice: bool,
    /// Whether the stale-marker hint has already been shown this session, so
    /// recalling the same marker again stays quiet.
    stale_paste_notified: bool,
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
            visual_width: 0,
            page_rows: 0,
            preferred_col: None,
            history: VecDeque::new(),
            history_index: None,
            history_draft: None,
            history_path: None,
            history_search: None,
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
            pastes: BTreeMap::new(),
            paste_counter: 0,
            stale_paste_notice: false,
            stale_paste_notified: false,
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
        self.history_search = None;
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

    /// Replace the buffer with a history entry, chips included.
    ///
    /// Unlike [`Editor::set_text_internal`] this keeps [`CHIP_CHAR`]
    /// sentinels: a recalled draft has to come back with the images that
    /// travelled with it, and the sentinels are what ties them together.
    fn set_entry_internal(&mut self, entry: &HistoryEntry) {
        self.buffer = entry.text.clone();
        self.cursor = self.buffer.len();
        self.images = entry.images.clone();
        self.last_action = LastAction::Other;
        self.cancel_autocomplete();
        // A marker restored from the cross-session history file has no
        // registry entry behind it (the file stores text only), so it is
        // ordinary text now. Raise the one-shot hint the first time it
        // happens in a session.
        if !self.stale_paste_notified && !self.unresolved_paste_marker_ids().is_empty() {
            self.stale_paste_notice = true;
        }
    }

    /// Clear the buffer without touching history, and drop the undo
    /// stack. Upstream clears its stack the same way when a prompt is
    /// submitted, so `Ctrl+-` cannot resurrect an already-sent prompt.
    /// Pasted images go with the text.
    ///
    /// The paste registry ([`Editor::pastes`]) deliberately survives: a
    /// history recall puts the marker text back, and expanding it again has
    /// to find the content. Upstream's `clear()` leaves its `pastes` map
    /// alone for the same reason.
    ///
    /// An open reverse search is dropped too: the composer it previewed into
    /// no longer exists, and leaving the mode armed would trap the next keys
    /// in a search with nothing to show.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.images.clear();
        self.history_index = None;
        self.history_draft = None;
        self.history_search = None;
        self.last_action = LastAction::Other;
        self.undo_stack.clear();
        self.jump_mode = None;
        self.preferred_col = None;
        self.cancel_autocomplete();
    }

    /// Record the width the App wrapped the composer body at.
    ///
    /// Vertical cursor motion must measure the draft exactly like the
    /// renderer does, and `Prompt::render_lines` takes `&self`, so the App
    /// hands the width over before dispatching a key. `0` means "no frame
    /// painted yet": a draft then lays out one row per hard line.
    pub fn set_visual_width(&mut self, width: usize) {
        self.visual_width = width;
    }

    /// The width recorded by [`Editor::set_visual_width`].
    pub fn visual_width(&self) -> usize {
        self.visual_width
    }

    /// Record how many rows one composer page covers.
    ///
    /// `PageUp` / `PageDown` move the cursor by this many visual rows and
    /// the window then follows it, so the page size has to be the number
    /// of rows the user can actually see — the App hands over the composer
    /// window height the last frame used. `0` means "no frame painted
    /// yet": a page is then a single row, which keeps a directly driven
    /// [`Editor`] (unit tests, embedders) well-behaved instead of making
    /// the key a no-op.
    pub fn set_page_rows(&mut self, rows: usize) {
        self.page_rows = rows;
    }

    /// Rows one page covers, never zero.
    pub fn page_rows(&self) -> usize {
        self.page_rows.max(1)
    }

    /// The rendered layout of the current draft.
    fn visual_layout(&self) -> VisualLayout {
        VisualLayout::new(&self.display_text(), self.visual_width)
    }

    /// The `(row, column)` the cursor is drawn at.
    pub fn visual_caret(&self) -> (usize, usize) {
        self.visual_layout().caret(self.display_cursor())
    }

    /// Number of visual rows the draft occupies at the recorded width.
    pub fn visual_row_count(&self) -> usize {
        self.visual_layout().len()
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
        expand_chips(&self.buffer)
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
    ///
    /// The text is also appended to the cross-session history file when one
    /// is configured ([`Editor::set_history_path`]). A write failure is
    /// deliberately ignored: persistence is best-effort, whereas losing the
    /// prompt itself would be a real loss.
    pub fn push_history(&mut self, text: impl Into<String>) {
        self.push_history_entry(HistoryEntry::new(text));
    }

    /// Push a submitted draft, chips included, so a later recall restores the
    /// whole draft and not only its text.
    pub fn push_history_entry(&mut self, entry: HistoryEntry) {
        let mut entry = entry;
        // A payload list that does not match the sentinel count would misalign
        // `[Image #N]` on recall; fall back to the text-only form instead.
        if entry.sentinels() == 0 || entry.images.len() != entry.sentinels() {
            entry = HistoryEntry::new(entry.text);
        }
        let text = entry.text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if self.history.front().map(|front| front.text.as_str()) == Some(text.as_str()) {
            return;
        }
        entry.text = text.clone();
        self.history.push_front(entry);
        while self.history.len() > HISTORY_LIMIT {
            self.history.pop_back();
        }
        if let Some(path) = self.history_path.as_deref() {
            let _ = crate::history_store::append(path, &text);
        }
    }

    /// Point the editor at a cross-session history file and load it.
    ///
    /// `None` (the default) disables persistence entirely, so a test or an
    /// embedder never reads or writes the user's real history. Passing a path
    /// both records it (for the appends that follow) and merges the file's
    /// entries into the in-memory list.
    pub fn set_history_path(&mut self, path: Option<PathBuf>) {
        self.history_path = path.clone();
        if let Some(path) = path {
            self.load_persisted_history(&path);
        }
    }

    /// The configured cross-session history file, if any.
    pub fn history_path(&self) -> Option<&Path> {
        self.history_path.as_deref()
    }

    /// Merge the persisted history into the in-memory list, newest first.
    ///
    /// Persisted entries are text-only and older than anything this session
    /// has already pushed, so they go to the back (which is the oldest end);
    /// a text the session already holds is skipped.
    fn load_persisted_history(&mut self, path: &Path) {
        for text in crate::history_store::load(path).into_iter().rev() {
            if self.history.len() >= HISTORY_LIMIT {
                break;
            }
            if self.history.iter().any(|entry| entry.text == text) {
                continue;
            }
            self.history.push_back(HistoryEntry::new(text));
        }
    }

    /// Drop all history entries.
    pub fn clear_history(&mut self) {
        self.history.clear();
        self.reset_history_navigation();
        self.history_search = None;
    }

    /// Read-only access to history (oldest first).
    pub fn history(&self) -> impl Iterator<Item = &HistoryEntry> {
        self.history.iter()
    }

    /// Read-only access to the history texts (oldest first).
    pub fn history_texts(&self) -> impl Iterator<Item = &str> {
        self.history.iter().map(|entry| entry.text.as_str())
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
            self.history_draft = Some(HistoryEntry {
                text: self.buffer.clone(),
                images: self.images.clone(),
            });
        }
        self.history_index = Some(next);
        let entry = self.history[next].clone();
        self.set_entry_internal(&entry);
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
            let draft = self
                .history_draft
                .take()
                .unwrap_or_else(|| HistoryEntry::new(String::new()));
            self.set_entry_internal(&draft);
        } else {
            self.history_index = Some(current - 1);
            let entry = self.history[current - 1].clone();
            self.set_entry_internal(&entry);
        }
        EditorAction::Changed
    }

    // -----------------------------------------------------------------
    // Reverse history search (`tui.editor.historySearch`, codex Ctrl+R)
    // -----------------------------------------------------------------

    /// True while a reverse history search owns the composer.
    pub fn history_search_active(&self) -> bool {
        self.history_search.is_some()
    }

    /// The reverse-search query, or `None` while no search is open.
    pub fn history_search_query(&self) -> Option<&str> {
        self.history_search
            .as_ref()
            .map(|search| search.query.as_str())
    }

    /// Whether the open search previews a match or has nothing to show.
    pub fn history_search_status(&self) -> Option<HistorySearchStatus> {
        self.history_search.as_ref().map(|search| {
            if search.selected.is_some() {
                HistorySearchStatus::Match
            } else {
                HistorySearchStatus::NoMatch
            }
        })
    }

    /// Number of matches the open search found (empty while closed).
    pub fn history_search_match_count(&self) -> usize {
        self.history_search
            .as_ref()
            .map(|search| search.matches.len())
            .unwrap_or(0)
    }

    /// Open a reverse history search without previewing anything yet.
    ///
    /// Mirrors codex `begin_history_search`: the draft is snapshotted so `Esc`
    /// can put it back, and an empty query previews **nothing** — opening
    /// `Ctrl+R` must not replace what the user was typing with their last
    /// prompt before they have searched for anything.
    pub fn begin_history_search(&mut self) -> EditorAction {
        if self.history_search.is_some() {
            return EditorAction::None;
        }
        self.history_search = Some(HistorySearch {
            query: String::new(),
            draft: self.buffer.clone(),
            draft_images: self.images.clone(),
            matches: Vec::new(),
            selected: None,
        });
        self.jump_mode = None;
        self.cancel_autocomplete();
        EditorAction::Changed
    }

    /// Close the search and put the pre-search draft back (`Esc` / `Ctrl+C`).
    pub fn cancel_history_search(&mut self) -> EditorAction {
        let Some(search) = self.history_search.take() else {
            return EditorAction::None;
        };
        self.buffer = search.draft;
        self.images = search.draft_images;
        self.cursor = self.buffer.len();
        self.last_action = LastAction::Other;
        EditorAction::Changed
    }

    /// Close the search, keeping the previewed entry as an editable draft.
    ///
    /// With nothing previewed this degrades to a cancel rather than leaving a
    /// half-open session (codex only accepts while `status == Match`).
    pub fn accept_history_search(&mut self) -> EditorAction {
        match self.history_search.take() {
            None => EditorAction::None,
            Some(search) => {
                if search.selected.is_none() {
                    self.buffer = search.draft;
                    self.images = search.draft_images;
                }
                self.cursor = self.buffer.len();
                self.last_action = LastAction::Other;
                EditorAction::Changed
            }
        }
    }

    /// Move the open search to an older / newer match.
    ///
    /// Boundaries do **not** close the session (codex returns `AtBoundary` and
    /// keeps the preview): the last older match stays visible, so holding
    /// `Ctrl+R` at the end of history is not a way to lose the preview.
    pub fn history_search_step(&mut self, direction: HistorySearchDirection) -> EditorAction {
        let (count, current, matches) = match self.history_search.as_ref() {
            Some(search) if !search.query.is_empty() => (
                search.matches.len(),
                search.selected,
                search.matches.clone(),
            ),
            // An empty query previews nothing, so a step only restores the
            // draft and leaves the (still open) search idle.
            Some(_) => return self.apply_history_search(Vec::new(), None),
            None => return EditorAction::None,
        };
        if count == 0 {
            return EditorAction::None;
        }
        let next = match direction {
            HistorySearchDirection::Older => current.map_or(0, |index| (index + 1).min(count - 1)),
            HistorySearchDirection::Newer => current.map_or(0, |index| index.saturating_sub(1)),
        };
        if current == Some(next) {
            // Already at that end of the match list: keep the preview and
            // report no change (codex returns `AtBoundary` and leaves the
            // session open, so holding the chord never loses the preview).
            return EditorAction::None;
        }
        self.apply_history_search(matches, Some(next))
    }

    /// Matching history indices for `query_lower`, newest first, de-duplicated
    /// by exact text (codex `search_result_is_unique`).
    fn history_search_matches(&self, query_lower: &str) -> Vec<usize> {
        let mut seen: Vec<&str> = Vec::new();
        let mut matches = Vec::new();
        for (index, entry) in self.history.iter().enumerate() {
            if !entry.text.to_lowercase().contains(query_lower) {
                continue;
            }
            if seen.contains(&entry.text.as_str()) {
                continue;
            }
            seen.push(entry.text.as_str());
            matches.push(index);
        }
        matches
    }

    /// Replace the reverse-search query and restart traversal from the newest
    /// match (codex: "query edits restart from the newest match").
    fn set_history_search_query(&mut self, query: String) -> EditorAction {
        match self.history_search.as_mut() {
            Some(search) => search.query = query,
            None => return EditorAction::None,
        }
        self.restart_history_search()
    }

    /// Recompute the matches for the current query, previewing the newest one.
    fn restart_history_search(&mut self) -> EditorAction {
        let query_lower = match self.history_search.as_ref() {
            Some(search) => search.query.to_lowercase(),
            None => return EditorAction::None,
        };
        let matches = if query_lower.is_empty() {
            Vec::new()
        } else {
            self.history_search_matches(&query_lower)
        };
        let selected = if matches.is_empty() { None } else { Some(0) };
        self.apply_history_search(matches, selected)
    }

    /// Store a traversal result and paint its preview, or restore the draft
    /// when there is nothing to preview (codex's `NoMatch`).
    fn apply_history_search(
        &mut self,
        matches: Vec<usize>,
        selected: Option<usize>,
    ) -> EditorAction {
        let preview = match self.history_search.as_mut() {
            Some(search) => {
                search.matches = matches;
                search.selected = selected;
                search
                    .selected
                    .and_then(|index| search.matches.get(index).copied())
                    .and_then(|history_index| self.history.get(history_index).cloned())
            }
            None => return EditorAction::None,
        };
        match preview {
            Some(entry) => {
                self.set_entry_internal(&entry);
                EditorAction::Changed
            }
            None => {
                let (draft, draft_images) = match self.history_search.as_ref() {
                    Some(search) => (search.draft.clone(), search.draft_images.clone()),
                    None => return EditorAction::None,
                };
                self.buffer = draft;
                self.images = draft_images;
                self.cursor = self.buffer.len();
                EditorAction::Changed
            }
        }
    }

    /// Route one key into the open search session.
    ///
    /// The search keeps the whole keyboard while it is open, exactly like
    /// codex's `handle_history_search_key`: a stray chord must not edit the
    /// preview that `Esc` exists to undo.
    fn handle_history_search_key(&mut self, kb: &KeybindingsManager, key: &Key) -> EditorAction {
        if Self::matches_binding(kb, key, "tui.select.cancel") {
            return self.cancel_history_search();
        }
        if Self::matches_binding(kb, key, "tui.input.submit") {
            return self.accept_history_search();
        }
        if Self::matches_binding(kb, key, "tui.editor.historySearch")
            || Self::matches_binding(kb, key, "tui.editor.cursorUp")
        {
            return self.history_search_step(HistorySearchDirection::Older);
        }
        if Self::matches_binding(kb, key, "tui.editor.historySearchNext")
            || Self::matches_binding(kb, key, "tui.editor.cursorDown")
        {
            return self.history_search_step(HistorySearchDirection::Newer);
        }
        if Self::matches_binding(kb, key, "tui.editor.deleteCharBackward") {
            let mut query = self
                .history_search
                .as_ref()
                .map(|search| search.query.clone())
                .unwrap_or_default();
            query.pop();
            return self.set_history_search_query(query);
        }
        // `Ctrl+U` empties the query (codex `update_history_search_query("")`).
        if Self::matches_binding(kb, key, "tui.editor.deleteToLineStart") {
            return self.set_history_search_query(String::new());
        }
        if !key.modifiers.control && !key.modifiers.alt && !key.modifiers.meta {
            if let KeyCode::Char(c) = key.code {
                let mut query = self
                    .history_search
                    .as_ref()
                    .map(|search| search.query.clone())
                    .unwrap_or_default();
                query.push(c);
                return self.set_history_search_query(query);
            }
        }
        EditorAction::None
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

    /// Insert a bracketed paste — the terminal's own paste, or the
    /// `app.clipboard.pasteImage` text fallback — the way upstream
    /// `handlePaste` does (`packages/tui/src/components/editor.ts:1248`).
    ///
    /// The whole paste is **one undo unit**, it closes the autocomplete
    /// dropdown, it leaves history browsing, and it never triggers a
    /// completion mid-text. The content is decoded (some terminals re-encode
    /// control bytes inside a paste as CSI-u), line endings are normalized
    /// (`\r\n` / `\r` → `\n`, tabs → four spaces — upstream
    /// `normalizeText`), and every control character except `\n` is dropped.
    ///
    /// A paste larger than [`PASTE_MARKER_LINE_THRESHOLD`] lines or
    /// [`PASTE_MARKER_CHAR_THRESHOLD`] characters is not inserted inline: it
    /// is kept in the registry and the draft carries a
    /// `[paste #N +L lines]` / `[paste #N C chars]` marker instead, so
    /// dropping a log file into the composer still leaves a readable draft.
    /// [`Editor::expanded_text`] puts the real content back for the model,
    /// and `Backspace` over a marker removes marker *and* content in one
    /// press.
    pub fn insert_paste(&mut self, pasted: &str) -> PasteInsertOutcome {
        if pasted.is_empty() {
            return PasteInsertOutcome::Ignored;
        }
        self.cancel_autocomplete();
        self.reset_history_navigation();
        let decoded = decode_csi_u_control(pasted);
        let normalized = normalize_pasted_text(&decoded);
        let mut text: String = normalized
            .chars()
            .filter(|c| *c == '\n' || *c >= ' ')
            .collect();
        if text.is_empty() {
            return PasteInsertOutcome::Ignored;
        }
        // A pasted path landing right after a word gets a separating space
        // (upstream's `/^[/~.]/` rule): `see/src/main.rs` is never what the
        // reader meant to say.
        if needs_path_separator(&text, self.char_before_cursor()) {
            text.insert(0, ' ');
        }
        self.push_undo_snapshot();
        self.last_action = LastAction::Other;
        let lines = text.split('\n').count();
        let chars = text.chars().count();
        if lines > PASTE_MARKER_LINE_THRESHOLD || chars > PASTE_MARKER_CHAR_THRESHOLD {
            self.paste_counter += 1;
            let id = self.paste_counter;
            self.pastes.insert(id, text);
            let marker = paste_marker_text(id, lines, chars);
            self.insert_raw_at_cursor(&marker);
            return PasteInsertOutcome::Marker(id);
        }
        self.insert_raw_at_cursor(&text);
        PasteInsertOutcome::Inserted
    }

    /// Insert `text` at the cursor without touching the undo stack, the
    /// history or the autocomplete dropdown — the inner step of a paste and
    /// of a marker insertion (upstream `insertTextAtCursorInternal`).
    fn insert_raw_at_cursor(&mut self, text: &str) {
        self.buffer.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    /// The character immediately before the cursor, `None` at offset 0.
    fn char_before_cursor(&self) -> Option<char> {
        if self.cursor == 0 {
            return None;
        }
        self.buffer[..self.cursor.min(self.buffer.len())]
            .chars()
            .next_back()
    }

    /// Content standing behind a paste marker, if it is still known.
    pub fn paste_content(&self, id: u32) -> Option<&str> {
        self.pastes.get(&id).map(String::as_str)
    }

    /// Ids of the known paste markers, ascending. Empty means the draft has
    /// no paste standing behind a marker.
    pub fn paste_marker_ids(&self) -> Vec<u32> {
        self.pastes.keys().copied().collect()
    }

    /// Ids of the markers standing in the draft whose content this editor
    /// does not have.
    ///
    /// A recall from the cross-session history file is the way this happens:
    /// the file stores the marker text only (see [`crate::history_store`]),
    /// so after a restart the `[paste #1 +12 lines]` comes back with no
    /// registry entry behind it. Such a marker is **ordinary text** — it is
    /// not atomic and [`Editor::expanded_text`] leaves it literal — and
    /// [`Editor::take_stale_paste_notice`] reports the first recall that
    /// brought one in.
    pub fn unresolved_paste_marker_ids(&self) -> Vec<u32> {
        paste_marker_spans(&self.buffer)
            .into_iter()
            .map(|span| span.id)
            .filter(|id| !self.pastes.contains_key(id))
            .collect()
    }

    /// Consume the "a recalled marker has no content" notice.
    ///
    /// Returns `true` at most once per editor instance, so a caller can
    /// flash one hint however many times the same orphan marker is recalled.
    pub fn take_stale_paste_notice(&mut self) -> bool {
        if !self.stale_paste_notice {
            return false;
        }
        self.stale_paste_notice = false;
        self.stale_paste_notified = true;
        true
    }

    /// Spans of the paste markers inside `slice` whose content is still in
    /// the registry, as byte ranges relative to `slice`.
    ///
    /// This is the Rust stand-in for upstream's `validPasteIds()` +
    /// `segmentWithMarkers`: only a marker with a live registry entry is
    /// atomic (a stale one from a restored history entry is plain text).
    fn atomic_paste_spans(&self, slice: &str) -> Vec<(usize, usize)> {
        paste_marker_spans(slice)
            .into_iter()
            .filter(|span| self.pastes.contains_key(&span.id))
            .map(|span| (span.start, span.end))
            .collect()
    }

    /// Remove the last `chars` characters before the cursor and return them.
    ///
    /// Used when a paste burst is confirmed ([`crate::input::PasteBurst`]):
    /// the characters already inserted as ordinary typing are folded into the
    /// burst so the whole paste goes through [`Editor::insert_paste`] in one
    /// piece. The cut is its own undo unit — a misclassified burst can be
    /// undone back to plain text — and it never drops anything: the returned
    /// string carries every byte that was removed.
    pub fn cut_chars_before_cursor(&mut self, chars: usize) -> String {
        if chars == 0 || self.cursor == 0 {
            return String::new();
        }
        let mut start = self.cursor;
        for _ in 0..chars {
            let prev = self.prev_char_boundary(start);
            if prev == start {
                break;
            }
            start = prev;
        }
        let taken = self.buffer[start..self.cursor].to_string();
        self.push_undo_snapshot();
        self.remove_images_in_range(start..self.cursor);
        self.buffer.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.preferred_col = None;
        self.last_action = LastAction::Other;
        self.reset_history_navigation();
        taken
    }

    /// The draft with every paste marker replaced by the text it stands
    /// for: [`Editor::display_text`] with the pastes put back.
    ///
    /// This is what a submission carries — the model never sees
    /// `[paste #1 +42 lines]` — and what the external editor is handed
    /// (upstream `getExpandedText`, `components/editor.ts:1086`).
    pub fn expanded_text(&self) -> String {
        expand_paste_markers(&self.display_text(), &self.pastes)
    }

    /// The paste marker that ends exactly at `end`, when its content is
    /// still known.
    fn paste_marker_ending_at(&self, end: usize) -> Option<PasteMarkerSpan> {
        paste_marker_spans(&self.buffer)
            .into_iter()
            .find(|span| span.end == end && self.pastes.contains_key(&span.id))
    }

    /// The paste marker that starts exactly at `start`, same condition.
    fn paste_marker_starting_at(&self, start: usize) -> Option<PasteMarkerSpan> {
        paste_marker_spans(&self.buffer)
            .into_iter()
            .find(|span| span.start == start && self.pastes.contains_key(&span.id))
    }

    /// Remove a whole marker and forget its content, keeping the cursor on
    /// the side it was already on.
    ///
    /// Deleting a marker **renumbers** the ones after it, like upstream's
    /// `higherIds` loop (`components/editor.ts:1389-1410`): the registry
    /// shifts down and every later label is rewritten, so a draft with two
    /// pastes never shows `#2` standing alone after `#1` is gone.
    ///
    /// **Deviation from upstream**: upstream also decrements `pasteCounter`
    /// on this path (`components/editor.ts:1394`), so the id it hands out
    /// next reuses the freed slot. This port keeps the counter monotone
    /// instead — an id that was already written into the buffer is never
    /// reused — which is why a paste after a deletion can take a number
    /// above the surviving labels (`#1` then `#3`). See
    /// `docs/LUM1460_PASTE_RESCUE.md` §5.
    fn remove_paste_marker(&mut self, span: PasteMarkerSpan) {
        self.remove_images_in_range(span.start..span.end);
        self.buffer.replace_range(span.start..span.end, "");
        if self.cursor > span.end {
            self.cursor -= span.end - span.start;
        } else if self.cursor > span.start {
            self.cursor = span.start;
        }
        self.pastes.remove(&span.id);
        self.renumber_paste_markers_after(span.id);
    }

    /// Shift every marker numbered above `removed_id` down by one.
    ///
    /// Upstream `higherIds` walks `this.pastes.keys()` in ascending order and
    /// both re-keys the map and rewrites the marker text it keeps in the
    /// buffer. This port does the same in two passes: the registry first
    /// (ascending, so the suffix slides into the slot the removed id left
    /// free), then the labels **back to front**, because rewriting `#10` to
    /// `#9` shortens the buffer and would invalidate the offsets of every
    /// marker after it.
    ///
    /// The caret only needs the same shift when a rewritten label sits
    /// before it (a label after the caret is never renumbered: ids ascend
    /// with buffer order); a caret that was inside the removed span was
    /// already parked at `span.start` by [`Editor::remove_paste_marker`].
    fn renumber_paste_markers_after(&mut self, removed_id: u32) {
        let higher: Vec<u32> = self
            .pastes
            .keys()
            .copied()
            .filter(|id| *id > removed_id)
            .collect();
        for id in higher {
            if let Some(content) = self.pastes.remove(&id) {
                self.pastes.insert(id - 1, content);
            }
        }

        let mut spans = paste_marker_spans(&self.buffer);
        spans.sort_by_key(|span| std::cmp::Reverse(span.start));
        for span in spans {
            if span.id <= removed_id {
                continue;
            }
            let old = self.buffer[span.start..span.end].to_string();
            let new = rewrite_marker_id(&old, span.id, span.id - 1);
            let delta = new.len() as isize - old.len() as isize;
            self.buffer.replace_range(span.start..span.end, &new);
            if delta != 0 && self.cursor >= span.end {
                self.cursor = (self.cursor as isize + delta) as usize;
            } else if self.cursor > span.start {
                self.cursor = span.start;
            }
        }
        // The draft moved under the caret, so a column cached for Up/Down no
        // longer describes it (same reason a normal edit drops it).
        self.preferred_col = None;
    }

    /// Delete the character before the cursor (`Backspace`).
    pub fn backspace(&mut self) -> EditorAction {
        if self.cursor == 0 {
            return EditorAction::None;
        }
        self.push_undo_snapshot();
        // A whole paste marker goes in one press: deleting half of
        // `[paste #1 +12 lines]` would leave text that no longer expands.
        if let Some(span) = self.paste_marker_ending_at(self.cursor) {
            self.remove_paste_marker(span);
        } else {
            // Walk back one UTF-8 character.
            let prev = self.prev_char_boundary(self.cursor);
            self.remove_images_in_range(prev..self.cursor);
            self.buffer.replace_range(prev..self.cursor, "");
            self.cursor = prev;
        }
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
        // Forward through a marker is atomic too (upstream segments graphemes
        // with marker awareness in both directions).
        if let Some(span) = self.paste_marker_starting_at(self.cursor) {
            self.remove_paste_marker(span);
        } else {
            let next = self.next_char_boundary(self.cursor);
            self.remove_images_in_range(self.cursor..next);
            self.buffer.replace_range(self.cursor..next, "");
        }
        self.reset_history_navigation();
        self.last_action = LastAction::Other;
        self.update_autocomplete_after_edit(None);
        EditorAction::Changed
    }

    /// Move the cursor one character to the left.
    ///
    /// A paste marker is one character from the reader's point of view
    /// (upstream's `isAtomicSegment`), so the cursor steps over the whole
    /// `[paste #1 +12 lines]` instead of into the middle of it.
    pub fn move_left(&mut self) -> EditorAction {
        if self.cursor == 0 {
            return EditorAction::None;
        }
        self.cursor = match self.paste_marker_ending_at(self.cursor) {
            Some(span) => span.start,
            None => self.prev_char_boundary(self.cursor),
        };
        self.last_action = LastAction::Other;
        self.refresh_autocomplete_if_open();
        EditorAction::Changed
    }

    /// Move the cursor one character to the right.
    pub fn move_right(&mut self) -> EditorAction {
        if self.cursor >= self.buffer.len() {
            return EditorAction::None;
        }
        self.cursor = match self.paste_marker_starting_at(self.cursor) {
            Some(span) => span.end,
            None => self.next_char_boundary(self.cursor),
        };
        self.last_action = LastAction::Other;
        self.refresh_autocomplete_if_open();
        EditorAction::Changed
    }

    /// Byte range of the logical line the cursor is on, with `end`
    /// exclusive of its terminating newline.
    ///
    /// A cursor sitting on a newline belongs to the line that newline
    /// terminates, so `end == cursor` there — the same place upstream's
    /// `cursorCol == line.length` puts it.
    fn line_bounds(&self) -> (usize, usize) {
        let cursor = self.cursor.min(self.buffer.len());
        let start = self.buffer[..cursor]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        let end = self
            .buffer
            .get(cursor..)
            .and_then(|rest| rest.find('\n'))
            .map(|offset| cursor + offset)
            .unwrap_or(self.buffer.len());
        (start, end)
    }

    /// Index of the logical line the cursor is on (0-based).
    fn cursor_line(&self) -> usize {
        self.buffer[..self.cursor.min(self.buffer.len())]
            .matches('\n')
            .count()
    }

    /// Byte offset of `display` (a character offset into
    /// [`Editor::display_text`]) back in the raw buffer.
    ///
    /// A chip renders as a multi-character `[Image #N]` label, so an
    /// offset landing inside one resolves to the chip's own byte: the
    /// cursor sits on the chip rather than in the middle of its label.
    fn raw_byte_for_display_offset(&self, display: usize) -> usize {
        let label_width = chip_label(1).chars().count();
        let mut seen = 0usize;
        for (byte, ch) in self.buffer.char_indices() {
            if seen >= display {
                return byte;
            }
            seen += if ch == CHIP_CHAR { label_width } else { 1 };
        }
        self.buffer.len()
    }

    /// Move the cursor to a character offset in [`Editor::display_text`].
    fn set_display_cursor(&mut self, display: usize) {
        self.cursor = self.raw_byte_for_display_offset(display);
        self.last_action = LastAction::Other;
        self.refresh_autocomplete_if_open();
    }

    /// Whether the cursor sits at display-text character offset `display`.
    pub fn is_at_display_offset(&self, display: usize) -> bool {
        self.display_cursor() == display
    }

    /// Place the caret at a character offset in [`Editor::display_text`] —
    /// the mouse-click path.
    ///
    /// Upstream's `Editor.handleMouse` click branch
    /// (`packages/tui/src/components/editor.ts:620-666`) sets the caret from
    /// the clicked visual cell and clears the sticky column / history
    /// browsing so the next `Up` / `Down` measures from the new position.
    /// This is that move expressed as an offset in the displayed draft;
    /// the caller resolves the cell to a row and a character column (see
    /// [`crate::visual_text::VisualLayout::cursor_at`]).
    pub fn place_display_cursor(&mut self, display: usize) -> EditorAction {
        self.preferred_col = None;
        self.reset_history_navigation();
        let before = self.cursor;
        self.set_display_cursor(display);
        if self.cursor == before {
            EditorAction::None
        } else {
            EditorAction::Changed
        }
    }

    /// Move the cursor to the start of the logical line it is on
    /// (`Home` / `Ctrl+A`, `tui.editor.cursorLineStart`). Upstream
    /// `moveToLineStart`.
    pub fn move_home(&mut self) -> EditorAction {
        let (start, _) = self.line_bounds();
        self.preferred_col = None;
        if self.cursor == start {
            return EditorAction::None;
        }
        self.cursor = start;
        self.last_action = LastAction::Other;
        self.refresh_autocomplete_if_open();
        EditorAction::Changed
    }

    /// Move the cursor to the end of the logical line it is on
    /// (`End` / `Ctrl+E`, `tui.editor.cursorLineEnd`). Upstream
    /// `moveToLineEnd`.
    pub fn move_end(&mut self) -> EditorAction {
        let (_, end) = self.line_bounds();
        self.preferred_col = None;
        if self.cursor == end {
            return EditorAction::None;
        }
        self.cursor = end;
        self.last_action = LastAction::Other;
        self.refresh_autocomplete_if_open();
        EditorAction::Changed
    }

    /// Insert a hard line break at the cursor (`tui.input.newLine`:
    /// `Shift+Enter` / `Ctrl+J`). Upstream `addNewLine`.
    pub fn insert_newline(&mut self) -> EditorAction {
        self.cancel_autocomplete();
        self.reset_history_navigation();
        self.push_undo_snapshot();
        self.buffer.insert(self.cursor, '\n');
        self.cursor += 1;
        self.last_action = LastAction::Other;
        self.preferred_col = None;
        EditorAction::Changed
    }

    /// Move the cursor one visual row up (`delta < 0`) or down in the
    /// draft, keeping the display column across the rows it crosses.
    ///
    /// The column is sticky for the whole run of moves (`preferred_col`),
    /// so walking down through a short row and continuing does not lose
    /// the column the run started at; it is dropped again as soon as a
    /// row can accommodate it, exactly like upstream's `preferredVisualCol`
    /// decision table (`moveToVisualLine`,
    /// `packages/tui/src/components/editor.ts:1470`).
    pub fn move_vertical(&mut self, delta: isize) -> EditorAction {
        let layout = self.visual_layout();
        let cursor = self.display_cursor();
        let (row, column) = layout.caret(cursor);
        let goal = *self.preferred_col.get_or_insert(column);
        let target = row as isize + delta;
        if target < 0 || target as usize >= layout.len() {
            return EditorAction::None;
        }
        let target = target as usize;
        let landed = goal.min(layout.row_width(target));
        let next = layout.cursor_at(target, landed);
        if landed >= goal {
            // The row could hold the column the run started at, so the run
            // is over: the next vertical move takes its column from here.
            self.preferred_col = None;
        }
        if next == cursor {
            return EditorAction::None;
        }
        self.set_display_cursor(next);
        EditorAction::Changed
    }

    /// `tui.editor.cursorUp`: one visual row up inside a draft that has
    /// one, otherwise the prompt history — and a move to the line start
    /// when the cursor is on the first row but past column 0, so a second
    /// press reaches the history. Upstream's rule
    /// (`packages/tui/src/components/editor.ts:913-926`).
    pub fn cursor_up(&mut self) -> EditorAction {
        let (row, column) = self.visual_caret();
        if row > 0 {
            return self.move_vertical(-1);
        }
        if self.is_empty() || self.history_index.is_some() || column == 0 {
            return self.history_prev();
        }
        self.move_home()
    }

    /// `tui.editor.cursorDown`: the mirror of [`Editor::cursor_up`],
    /// ending at the line end when the cursor is on the last row but not
    /// at the end of it (upstream
    /// `packages/tui/src/components/editor.ts:927-937`).
    pub fn cursor_down(&mut self) -> EditorAction {
        let layout = self.visual_layout();
        let (row, _) = layout.caret(self.display_cursor());
        if row + 1 < layout.len() {
            return self.move_vertical(1);
        }
        if self.history_index.is_some() {
            return self.history_next();
        }
        self.move_end()
    }

    /// `tui.editor.pageUp`: move the cursor one composer page up, keeping
    /// the display column. Upstream `pageScroll(-1)`
    /// (`packages/tui/src/components/editor.ts:1949`).
    pub fn page_up(&mut self) -> EditorAction {
        self.page_scroll(-1)
    }

    /// `tui.editor.pageDown`: the mirror of [`Editor::page_up`], upstream
    /// `pageScroll(1)`.
    pub fn page_down(&mut self) -> EditorAction {
        self.page_scroll(1)
    }

    /// Shared body of `PageUp` / `PageDown`: [`Editor::move_vertical`] by a
    /// page at a time, with the target row clamped to the draft's first /
    /// last row.
    ///
    /// Upstream `pageScroll` moves the *cursor* by a page and lets the
    /// renderer scroll the window to keep it visible; the port keeps that
    /// split, so the composer window follows the caret smoothly instead of
    /// the key jumping the viewport by itself. The page size is
    /// [`Editor::page_rows`] — the composer window height — where upstream
    /// uses `max(5, floor(terminalRows * 0.3))`, which is the same number
    /// as its own `maxVisibleLines`.
    fn page_scroll(&mut self, direction: isize) -> EditorAction {
        let layout = self.visual_layout();
        let cursor = self.display_cursor();
        let (row, column) = layout.caret(cursor);
        let goal = *self.preferred_col.get_or_insert(column);
        let last_row = layout.len().saturating_sub(1) as isize;
        let target =
            (row as isize + direction * self.page_rows() as isize).clamp(0, last_row) as usize;
        let landed = goal.min(layout.row_width(target));
        let next = layout.cursor_at(target, landed);
        if landed >= goal {
            // The row could hold the column the run started at, so the run
            // is over: the next vertical move takes its column from here.
            self.preferred_col = None;
        }
        if next == cursor {
            return EditorAction::None;
        }
        self.set_display_cursor(next);
        EditorAction::Changed
    }

    /// Move the cursor one word to the left (`Alt+B`, `Alt+Left` or
    /// `Ctrl+Left`, `tui.editor.cursorWordLeft`). Trailing whitespace is
    /// skipped, then the cursor stops at the next word / punctuation
    /// boundary; a `[paste #N …]` marker is one atomic word, so a single
    /// step crosses all of it (upstream `moveWordBackwards` with
    /// `isAtomicSegment`). At the start of a logical line it steps onto the
    /// end of the previous one.
    pub fn move_word_left(&mut self) -> EditorAction {
        if self.cursor == 0 {
            return EditorAction::None;
        }
        let (start, _) = self.line_bounds();
        let target = if self.cursor == start {
            // At the start of the line: step onto the previous line's end,
            // which is the newline's own offset.
            start - 1
        } else {
            let slice = &self.buffer[start..self.cursor];
            let atoms = self.atomic_paste_spans(slice);
            start + find_word_backward_with_atoms(slice, self.cursor - start, &atoms)
        };
        if target == self.cursor {
            return EditorAction::None;
        }
        self.preferred_col = None;
        self.cursor = target;
        self.last_action = LastAction::Other;
        self.refresh_autocomplete_if_open();
        EditorAction::Changed
    }

    /// Move the cursor one word to the right (`Alt+F`, `Alt+Right` or
    /// `Ctrl+Right`, `tui.editor.cursorWordRight`). Leading whitespace is
    /// skipped, then the cursor stops at the next word / punctuation
    /// boundary; a `[paste #N …]` marker is one atomic word. At the end of a
    /// logical line it steps onto the start of the next one.
    pub fn move_word_right(&mut self) -> EditorAction {
        if self.cursor >= self.buffer.len() {
            return EditorAction::None;
        }
        let (start, end) = self.line_bounds();
        let target = if self.cursor >= end {
            // At the end of the line: step past the newline onto the start
            // of the next line when there is one.
            if end < self.buffer.len() {
                end + 1
            } else {
                end
            }
        } else {
            let slice = &self.buffer[start..end];
            let atoms = self.atomic_paste_spans(slice);
            start + find_word_forward_with_atoms(slice, self.cursor - start, &atoms)
        };
        if target == self.cursor {
            return EditorAction::None;
        }
        self.preferred_col = None;
        self.cursor = target;
        self.last_action = LastAction::Other;
        self.refresh_autocomplete_if_open();
        EditorAction::Changed
    }

    /// Kill from the start of the buffer up to the cursor (`Ctrl+U`,
    /// `tui.editor.deleteToLineStart`). The killed text is prepended to
    /// the most recent kill ring entry when the previous action was
    /// also a kill, matching upstream's accumulation rule.
    pub fn kill_to_line_start(&mut self) -> EditorAction {
        let (start, _) = self.line_bounds();
        if self.cursor > start {
            return self.kill_range(start, self.cursor, KillDirection::Prepend);
        }
        if start > 0 {
            // At the start of a line: kill the newline before it, which is
            // what merges the two lines (upstream `deleteToStartOfLine`).
            return self.kill_range(start - 1, start, KillDirection::Prepend);
        }
        EditorAction::None
    }

    /// Kill `[from, to)` and push it onto the kill ring, leaving the cursor
    /// at `from`.
    fn kill_range(&mut self, from: usize, to: usize, direction: KillDirection) -> EditorAction {
        if from >= to {
            return EditorAction::None;
        }
        self.push_undo_snapshot();
        self.cancel_autocomplete();
        let killed = strip_chips(&self.buffer[from..to]);
        // Read the previous action *before* overwriting it: a kill that
        // follows another kill accumulates into the same ring entry
        // instead of opening a new chain (upstream `deleteWordBackwards`).
        let accumulate = self.last_action == LastAction::Kill;
        self.remove_images_in_range(from..to);
        self.buffer.replace_range(from..to, "");
        self.cursor = from;
        self.preferred_col = None;
        self.kill_ring.push(&killed, direction, accumulate);
        self.last_action = LastAction::Kill;
        self.reset_history_navigation();
        EditorAction::Changed
    }

    /// Kill from the cursor to the end of the buffer (`Ctrl+K`,
    /// `tui.editor.deleteToLineEnd`). The killed text is appended to the
    /// most recent kill ring entry when the previous action was also a
    /// kill.
    pub fn kill_to_line_end(&mut self) -> EditorAction {
        let (_, end) = self.line_bounds();
        if self.cursor < end {
            return self.kill_range(self.cursor, end, KillDirection::Append);
        }
        if end < self.buffer.len() {
            // At the end of a line: kill the newline after it, which is
            // what merges the next line (upstream `deleteToEndOfLine`).
            return self.kill_range(end, end + 1, KillDirection::Append);
        }
        EditorAction::None
    }

    /// Kill the word before the cursor (`Ctrl+W` / `Alt+Backspace`,
    /// `tui.editor.deleteWordBackward`). The killed text is prepended to
    /// the most recent kill ring entry when the previous action was also
    /// a kill, matching upstream's accumulation rule.
    pub fn kill_word_backward(&mut self) -> EditorAction {
        if self.cursor == 0 {
            return EditorAction::None;
        }
        let (start, _) = self.line_bounds();
        if self.cursor == start {
            // At the start of a line this behaves like a backspace at
            // column 0: the newline goes, the word does not
            // (upstream `deleteWordBackwards`).
            return self.kill_range(start - 1, start, KillDirection::Prepend);
        }
        let delete_from =
            start + find_word_backward(&self.buffer[start..self.cursor], self.cursor - start);
        self.kill_range(delete_from, self.cursor, KillDirection::Prepend)
    }

    /// Kill the word after the cursor (`Alt+D` / `Alt+Delete`,
    /// `tui.editor.deleteWordForward`). The killed text is appended to
    /// the most recent kill ring entry when the previous action was also
    /// a kill.
    pub fn kill_word_forward(&mut self) -> EditorAction {
        if self.cursor >= self.buffer.len() {
            return EditorAction::None;
        }
        let (start, end) = self.line_bounds();
        if self.cursor >= end {
            // At the end of a line this behaves like a forward delete on
            // the newline: the next line is joined onto this one
            // (upstream `deleteWordForward`).
            if end < self.buffer.len() {
                return self.kill_range(end, end + 1, KillDirection::Append);
            }
            return EditorAction::None;
        }
        let delete_to = start + find_word_forward(&self.buffer[start..end], self.cursor - start);
        self.kill_range(self.cursor, delete_to, KillDirection::Append)
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
        self.preferred_col = None;
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
        self.preferred_col = None;
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
        self.pastes = snapshot.pastes;
        self.paste_counter = snapshot.paste_counter;
        self.preferred_col = None;
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
    /// are merged into the editor defaults (`@`, `#`), replacing any trigger
    /// table installed by a previous provider — upstream
    /// `setAutocompleteProvider` → `setAutocompleteTriggerCharacters`
    /// (`packages/tui/src/components/editor.ts:407,2329-2340`), so a
    /// re-installed (re-composed) chain cannot leave a stale trigger behind.
    pub fn set_autocomplete_provider(&mut self, provider: Arc<dyn AutocompleteProvider>) {
        self.autocomplete_trigger_characters = DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS.to_vec();
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

    /// The trigger characters currently installed (the editor defaults plus
    /// the provider's own, deduplicated) — the observable form of the table
    /// `opens_autocomplete_after_insert` consults.
    pub fn autocomplete_trigger_characters(&self) -> &[char] {
        &self.autocomplete_trigger_characters
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

    /// Visible window of the dropdown as `(start, end)` candidate indices —
    /// `end` exclusive, exactly the range
    /// [`Editor::autocomplete_render_styled_lines`] paints (the trailing
    /// `(n/m)` counter row is not a candidate).
    ///
    /// The App needs it to turn a pointer row back into a candidate
    /// (upstream `SelectList.handleMouse` maps `event.y` through
    /// `getVisibleRange()`), so it must come from the same windowing call the
    /// renderer uses rather than a second copy of the arithmetic.
    pub fn autocomplete_window(&self) -> (usize, usize) {
        let len = self.autocomplete_items.len();
        if len == 0 {
            return (0, 0);
        }
        select_list_visible_range(
            len,
            self.autocomplete_selected,
            Some(self.autocomplete_max_visible),
        )
    }

    /// Highlight a candidate by index without applying it and without
    /// scrolling it into view beyond the shared windowing — the press half of
    /// a dropdown click (upstream `SelectList.handleMouse`'s `press` branch
    /// sets `selectedIndex` and notifies). Out-of-range indices are clamped to
    /// the last candidate so a stale row cannot select nothing.
    pub fn set_autocomplete_selected(&mut self, index: usize) {
        if self.autocomplete_items.is_empty() {
            return;
        }
        self.autocomplete_selected = index.min(self.autocomplete_items.len() - 1);
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
        if self.cursor_line() == 0 && trimmed.starts_with('/') && !trimmed.contains(' ') {
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
        let lines = self.buffer_lines();
        let cursor_line = self.cursor_line();
        let cursor_col = self.cursor_col();
        if force && !provider.should_trigger_file_completion(&lines, cursor_line, cursor_col) {
            return false;
        }
        let Some(suggestions) = provider.get_suggestions(&lines, cursor_line, cursor_col, force)
        else {
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
    ///
    /// The layout is the shared `SelectList` one
    /// ([`select_list_row_spans`], upstream `SelectList::renderItem`) — the
    /// label sits in the primary column, the description starts at the same
    /// offset on every row, and both are width-clamped. Upstream's editor
    /// builds a real `SelectList` and renders it under the composer
    /// (`components/editor.ts:605-614`); this is the same layout, applied to
    /// the provider's candidates.
    ///
    /// The selected row is wrapped whole (`accent` over `selectedBg`), a
    /// plain row's description is `muted` — upstream's `selectList` theme
    /// roles. A plain-text render is [`Editor::autocomplete_render_lines`].
    pub fn autocomplete_render_styled_lines(&self, width: usize) -> Vec<StyledLine> {
        let len = self.autocomplete_items.len();
        if len == 0 || width == 0 {
            return Vec::new();
        }
        // Upstream measures the primary column over the whole candidate list
        // (`getPrimaryColumnWidth`), not just the visible window, so a scroll
        // never re-flows the description column.
        let primary_column_width = self
            .autocomplete_layout()
            .primary_column_width(self.autocomplete_items.iter());
        let (start, end) = select_list_visible_range(
            len,
            self.autocomplete_selected,
            Some(self.autocomplete_max_visible),
        );
        let mut rows = Vec::with_capacity(end - start + 1);
        for index in start..end {
            rows.push(select_list_row_spans(
                &self.autocomplete_items[index],
                index == self.autocomplete_selected,
                width,
                primary_column_width,
            ));
        }
        if start > 0 || end < len {
            rows.push(vec![StyledSpan::new(
                format!("  ({}/{len})", self.autocomplete_selected + 1),
                SpanStyle::fg(ThemeColor::Muted),
            )]);
        }
        rows
    }

    /// Render the dropdown rows as plain text (the styling of
    /// [`Editor::autocomplete_render_styled_lines`] dropped, the layout
    /// kept).
    pub fn autocomplete_render_lines(&self, width: usize) -> Vec<String> {
        self.autocomplete_render_styled_lines(width)
            .iter()
            .map(|line| plain_text(line))
            .collect()
    }

    /// Primary-column bounds for the dropdown — upstream
    /// `createAutocompleteList`'s `prefix.startsWith("/") ?
    /// SLASH_COMMAND_SELECT_LIST_LAYOUT : undefined`
    /// (`components/editor.ts:2228`).
    ///
    /// The slash-command menu is the one context whose labels are short
    /// enough that a fixed 32-column primary column would push the
    /// description off a 40-80 column terminal, so it tracks the widest
    /// command name within `[12, 32]` instead.
    pub fn autocomplete_layout(&self) -> SelectorLayout {
        if self.autocomplete_prefix.starts_with('/') {
            SelectorLayout::slash_command()
        } else {
            SelectorLayout::default()
        }
    }

    /// Apply one candidate, capturing an undo snapshot first (upstream
    /// pushes the pre-completion state so `Ctrl+-` reverts it).
    fn apply_completion_item(&mut self, item: &AutocompleteItem, prefix: &str) {
        let Some(provider) = self.autocomplete_provider.clone() else {
            return;
        };
        self.push_undo_snapshot();
        let lines = self.buffer_lines();
        let cursor_line = self.cursor_line();
        let result =
            provider.apply_completion(&lines, cursor_line, self.cursor_col(), item, prefix);
        self.buffer = strip_chips(&result.lines.join("\n"));
        self.cursor = self.byte_offset_of(result.cursor_line, result.cursor_col);
        // A completion rewrites the whole line; the chip/attachment pairing
        // cannot survive that, so the chips go with it (same rule as
        // `set_text_internal`).
        self.images.clear();
        self.reset_history_navigation();
        self.last_action = LastAction::Other;
        self.preferred_col = None;
    }

    /// The buffer split into logical lines, the shape the autocomplete
    /// provider interface takes (upstream `state.lines`).
    fn buffer_lines(&self) -> Vec<String> {
        self.buffer.split('\n').map(str::to_string).collect()
    }

    /// Byte offset of the cursor inside its logical line.
    fn cursor_col(&self) -> usize {
        let (start, _) = self.line_bounds();
        self.cursor.saturating_sub(start)
    }

    /// Byte offset of `col` in logical line `line` of `text`.
    fn byte_offset_of(&self, line: usize, col: usize) -> usize {
        let mut offset = 0usize;
        for (index, chunk) in self.buffer.split('\n').enumerate() {
            if index == line {
                return clamp_to_char_boundary(&self.buffer, offset + col.min(chunk.len()));
            }
            offset += chunk.len() + 1;
        }
        self.buffer.len()
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
        // `/` at the start of the message always opens the command menu —
        // and only on the first line, which is where a command can be
        // typed (upstream `isSlashMenuAllowed`).
        if c == '/' {
            let trimmed = before.trim();
            if self.cursor_line() == 0 && (trimmed.is_empty() || trimmed == "/") {
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
        if self.cursor_line() == 0 && text.trim_start().starts_with('/') {
            return true;
        }
        trigger_pattern_matches(text, &self.autocomplete_trigger_characters)
    }

    /// The text of the logical line before the cursor.
    ///
    /// Line-scoped, like upstream's `textBeforeCursor =
    /// currentLine.slice(0, cursorCol)`: a command or a path is recognised
    /// inside the line the user is on, not across the whole draft.
    fn before_cursor_text(&self) -> &str {
        let (start, _) = self.line_bounds();
        &self.buffer[start..self.cursor.min(self.buffer.len())]
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
            pastes: self.pastes.clone(),
            paste_counter: self.paste_counter,
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

    /// Process a key against the process-wide keybindings.
    pub fn handle_key(&mut self, key: Key) -> EditorAction {
        let kb = get_keybindings();
        self.handle_key_with(&kb, key)
    }

    /// Process a key against an explicit keybindings table.
    ///
    /// The split exists so a test can exercise a *user override* without
    /// mutating the process-wide manager, which every other test in the
    /// process shares.
    pub fn handle_key_with(&mut self, kb: &KeybindingsManager, key: Key) -> EditorAction {
        // `tui.editor.historySearch` (`Ctrl+R`): reverse history search
        // (codex `ChatComposer::begin_history_search`). While a session is
        // open it owns the composer, so this check comes first — ahead of a
        // half-armed `Ctrl+]` jump target, whose next key would otherwise eat
        // the search query character.
        if self.history_search.is_some() {
            return self.handle_history_search_key(kb, &key);
        }
        if Self::matches_binding(kb, &key, "tui.editor.historySearch") {
            return self.begin_history_search();
        }

        // An armed jump consumes this key: a printable character is the
        // target, the hotkey again (or anything that is not a plain
        // printable character) cancels the mode. Cancelling keys fall
        // through to their normal handling, mirroring upstream
        // `Editor.handleInput`.
        if let Some(direction) = self.jump_mode.take() {
            if Self::is_jump_binding(kb, &key) {
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
        if Self::matches_binding(kb, &key, "tui.editor.jumpBackward") {
            self.jump_mode = Some(JumpDirection::Backward);
            return EditorAction::None;
        }
        if Self::matches_binding(kb, &key, "tui.editor.jumpForward") {
            self.jump_mode = Some(JumpDirection::Forward);
            return EditorAction::None;
        }

        // `tui.input.copy` (`Ctrl+C`). There is no selection to copy in
        // this editor, so the chord is handed back to the caller; see the
        // module docs.
        if Self::matches_binding(kb, &key, "tui.input.copy") {
            return EditorAction::Interrupt;
        }

        // `app.exit` (`Ctrl+D`): exit only while the buffer is empty,
        // otherwise fall through to `tui.editor.deleteCharForward`,
        // exactly like upstream `CustomEditor` (`custom-editor.ts:117`).
        if Self::matches_app_exit(kb, &key) && self.buffer.is_empty() {
            return EditorAction::Eof;
        }

        // Autocomplete dropdown. Checked above the plain-key handling so
        // the selection chords steer the candidate list while it is open,
        // mirroring upstream `Editor.handleInput`'s autocomplete block
        // (which runs after undo, before Tab and deletion).
        if self.is_showing_autocomplete() {
            if Self::matches_binding(kb, &key, "tui.select.cancel") {
                self.cancel_autocomplete();
                return EditorAction::None;
            }
            if Self::matches_binding(kb, &key, "tui.select.up") {
                self.move_autocomplete(-1);
                return EditorAction::Changed;
            }
            if Self::matches_binding(kb, &key, "tui.select.down") {
                self.move_autocomplete(1);
                return EditorAction::Changed;
            }
            if Self::matches_binding(kb, &key, "tui.input.tab") {
                return self.accept_autocomplete();
            }
            if Self::matches_binding(kb, &key, "tui.input.submit") {
                let prefix = self.autocomplete_prefix.clone();
                let applied = self.accept_autocomplete();
                if applied == EditorAction::None {
                    return EditorAction::None;
                }
                // A command name is still submitted: upstream falls
                // through to the normal Enter handling when the
                // prefix starts with `/`.
                if prefix.starts_with('/') {
                    return EditorAction::Submit(self.expanded_text());
                }
                return applied;
            }
        }

        // Tab with the dropdown closed forces a completion
        // (`tui.input.tab`).
        if Self::matches_binding(kb, &key, "tui.input.tab") && self.handle_tab_completion() {
            return EditorAction::Changed;
        }

        // Deletion chords.
        if Self::matches_binding(kb, &key, "tui.editor.deleteToLineStart") {
            return self.kill_to_line_start();
        }
        if Self::matches_binding(kb, &key, "tui.editor.deleteToLineEnd") {
            return self.kill_to_line_end();
        }
        if Self::matches_binding(kb, &key, "tui.editor.deleteWordBackward") {
            return self.kill_word_backward();
        }
        if Self::matches_binding(kb, &key, "tui.editor.deleteWordForward") {
            return self.kill_word_forward();
        }
        if Self::matches_binding(kb, &key, "tui.editor.deleteCharBackward") {
            return self.backspace();
        }
        // `Delete` and `Ctrl+D` (the Latter already handled above while
        // the buffer was empty).
        if Self::matches_binding(kb, &key, "tui.editor.deleteCharForward") {
            return self.delete();
        }

        // Kill-ring chords.
        if Self::matches_binding(kb, &key, "tui.editor.yank") {
            return self.yank();
        }
        if Self::matches_binding(kb, &key, "tui.editor.yankPop") {
            return self.yank_pop();
        }

        // Dedicated history chords (`tui.editor.historyPrevious` /
        // `historyNext`, unbound by default). Upstream
        // `packages/tui/src/components/editor.ts:848-856`: they always browse
        // entries instead of moving the cursor, and they close the dropdown
        // first. Checked before the cursor chords so a user binding one of
        // them to `Up`/`Down` still gets history browsing, not row motion
        // (LUM-1447: these two ids had no consumer at all before this).
        if Self::matches_binding(kb, &key, "tui.editor.historyPrevious") {
            self.cancel_autocomplete();
            return self.history_prev();
        }
        if Self::matches_binding(kb, &key, "tui.editor.historyNext") {
            self.cancel_autocomplete();
            return self.history_next();
        }

        // Cursor movement.
        if Self::matches_binding(kb, &key, "tui.editor.cursorLineStart") {
            return self.move_home();
        }
        if Self::matches_binding(kb, &key, "tui.editor.cursorLineEnd") {
            return self.move_end();
        }
        if Self::matches_binding(kb, &key, "tui.editor.cursorWordLeft") {
            return self.move_word_left();
        }
        if Self::matches_binding(kb, &key, "tui.editor.cursorWordRight") {
            return self.move_word_right();
        }
        // `Up` / `Down`: a visual row inside the draft, the prompt
        // history from its first / last row (upstream
        // `packages/tui/src/components/editor.ts:913-940`).
        if Self::matches_binding(kb, &key, "tui.editor.cursorUp") {
            return self.cursor_up();
        }
        if Self::matches_binding(kb, &key, "tui.editor.cursorDown") {
            return self.cursor_down();
        }
        if Self::matches_binding(kb, &key, "tui.editor.cursorLeft") {
            return self.move_left();
        }
        if Self::matches_binding(kb, &key, "tui.editor.cursorRight") {
            return self.move_right();
        }

        // `tui.editor.pageUp` / `pageDown`: a page of the draft. The `App`
        // only lets the bare chords reach the editor while the draft
        // overflows the composer window
        // (`App::composer_overflows`); otherwise it consumed them for the
        // transcript's own paging first. The `Ctrl+PageUp` / `Ctrl+PageDown`
        // spellings are the editor's either way, because the transcript
        // binding is the bare key only.
        if Self::matches_binding(kb, &key, "tui.editor.pageUp") {
            return self.page_up();
        }
        if Self::matches_binding(kb, &key, "tui.editor.pageDown") {
            return self.page_down();
        }

        // `tui.editor.undo`.
        if Self::matches_binding(kb, &key, "tui.editor.undo") {
            return self.undo();
        }

        // `tui.input.newLine` (`Shift+Enter` / `Ctrl+J`): a hard line
        // break, checked before the submit branch so `Shift+Enter` grows
        // the composer instead of sending the draft.
        if Self::matches_binding(kb, &key, "tui.input.newLine") {
            return self.insert_newline();
        }
        // `tui.input.submit` (`Enter`). The backslash fallback is
        // upstream's `shouldSubmitOnBackslashEnter`: in a terminal that
        // cannot report `Shift+Enter`, a trailing backslash means "I
        // wanted a newline", so it is consumed instead of submitting.
        if Self::matches_binding(kb, &key, "tui.input.submit") {
            if self.buffer[..self.cursor.min(self.buffer.len())].ends_with('\\') {
                self.backspace();
                return self.insert_newline();
            }
            return EditorAction::Submit(self.expanded_text());
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
            // `Shift+Enter` is `tui.input.newLine`; the registry matches
            // modifiers exactly, so the shifted spelling is handled here
            // for the same reason the other shifted chords below are.
            KeyCode::Enter if key.modifiers.shift => self.insert_newline(),
            KeyCode::Left if key.modifiers.shift => self.move_left(),
            KeyCode::Right if key.modifiers.shift => self.move_right(),
            KeyCode::Home if key.modifiers.shift => self.move_home(),
            KeyCode::End if key.modifiers.shift => self.move_end(),
            KeyCode::Up if key.modifiers.shift => self.cursor_up(),
            KeyCode::Down if key.modifiers.shift => self.cursor_down(),
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

/// One `[paste #N …]` marker found in a draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PasteMarkerSpan {
    /// Byte offset of the opening `[`.
    start: usize,
    /// Byte offset just past the closing `]`.
    end: usize,
    /// Id the marker spells.
    id: u32,
}

/// The marker text one large paste is replaced with (upstream
/// `handlePaste`: more than ten lines reports the line count, a long
/// single-line paste reports the character count).
fn paste_marker_text(id: u32, lines: usize, chars: usize) -> String {
    if lines > PASTE_MARKER_LINE_THRESHOLD {
        format!("{PASTE_MARKER_PREFIX}{id} +{lines} lines]")
    } else {
        format!("{PASTE_MARKER_PREFIX}{id} {chars} chars]")
    }
}

/// Every paste marker in `text`, in buffer order.
fn paste_marker_spans(text: &str) -> Vec<PasteMarkerSpan> {
    let mut spans = Vec::new();
    if !text.contains(PASTE_MARKER_PREFIX) {
        return spans;
    }
    let mut search_from = 0usize;
    while let Some(offset) = text[search_from..].find(PASTE_MARKER_PREFIX) {
        let start = search_from + offset;
        match parse_paste_marker(text, start) {
            Some((end, id)) => {
                spans.push(PasteMarkerSpan { start, end, id });
                search_from = end;
            }
            // Not a marker after all — step past the prefix and keep
            // looking, so literal text that merely starts the same way is
            // never treated as one.
            None => search_from = start + PASTE_MARKER_PREFIX.len(),
        }
    }
    spans
}

/// `[paste #<id> …]` with its id replaced by `new_id`.
///
/// Used when a deletion renumbers the markers above it: upstream rewrites
/// the same text (`higherIds`), and only the id digits differ — the summary
/// (`+12 lines` / `1234 chars`) is carried over verbatim. The marker never
/// contains a second `#`, so the first occurrence is the id.
fn rewrite_marker_id(marker: &str, id: u32, new_id: u32) -> String {
    marker.replacen(&format!("#{id}"), &format!("#{new_id}"), 1)
}

/// Parse the marker that starts at `start`, returning `(end, id)`.
///
/// Accepts the three shapes upstream's `PASTE_MARKER_REGEX` accepts:
/// `[paste #1]`, `[paste #1 +12 lines]`, `[paste #1 1234 chars]`.
fn parse_paste_marker(text: &str, start: usize) -> Option<(usize, u32)> {
    let rest = text.get(start + PASTE_MARKER_PREFIX.len()..)?;
    let digits = rest.find(|c: char| !c.is_ascii_digit())?;
    if digits == 0 {
        return None;
    }
    let id: u32 = rest[..digits].parse().ok()?;
    let after = marker_tail(&rest[digits..])?;
    Some((text.len() - after.len(), id))
}

/// The text that follows a marker body, or `None` when the body is malformed.
///
/// Returns `Some(rest)` with the bytes after the closing `]`, so the caller
/// recovers the marker's end offset as `text.len() - after.len()`.
fn marker_tail(tail: &str) -> Option<&str> {
    if let Some(r) = tail.strip_prefix(']') {
        return Some(r);
    }
    if let Some(r) = tail.strip_prefix(" +") {
        let count = r.find(|c: char| !c.is_ascii_digit())?;
        if count == 0 {
            return None;
        }
        return r[count..].strip_prefix(" lines]");
    }
    if let Some(r) = tail.strip_prefix(' ') {
        let count = r.find(|c: char| !c.is_ascii_digit())?;
        if count == 0 {
            return None;
        }
        return r[count..].strip_prefix(" chars]");
    }
    None
}

/// Replace every marker whose content is still known with that content.
///
/// An id nobody knows stays literal — the same tolerance upstream's
/// `expandPasteMarkers` has for a marker it has no entry for.
fn expand_paste_markers(text: &str, pastes: &BTreeMap<u32, String>) -> String {
    let spans = paste_marker_spans(text);
    if spans.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    for span in spans {
        if let Some(content) = pastes.get(&span.id) {
            out.push_str(&text[last..span.start]);
            out.push_str(content);
            last = span.end;
        }
    }
    out.push_str(&text[last..]);
    out
}

/// Decode the CSI-u spelling of a control byte some terminals produce
/// *inside* a bracketed paste.
///
/// A tmux popup with `extended-keys-format=csi-u` re-encodes `\x0a` as
/// `ESC [ 106 ; 5 u`, so the newline arrives as printable tail text and the
/// paste stops being multi-line. Upstream decodes the same escapes back to
/// their byte (`handlePaste`, `components/editor.ts:1257-1265`); the ASCII
/// letters are the only ones it rewrites, and so is this.
fn decode_csi_u_control(text: &str) -> String {
    if !text.contains("\u{1b}[") {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut index = 0usize;
    while index < text.len() {
        let rest = &text[index..];
        if let Some(rest) = rest.strip_prefix("\u{1b}[") {
            let digits = rest.find(|c: char| !c.is_ascii_digit());
            if let Some(width) = digits {
                if width > 0 && rest[width..].starts_with(";5u") {
                    if let Ok(code) = rest[..width].parse::<u32>() {
                        let mapped = match code {
                            97..=122 => Some(code - 96),
                            65..=90 => Some(code - 64),
                            _ => None,
                        };
                        if let Some(byte) = mapped.and_then(char::from_u32) {
                            out.push(byte);
                            index += 1 + 1 + width + 3;
                            continue;
                        }
                    }
                }
            }
        }
        let ch = rest.chars().next().expect("index is inside the string");
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

/// Upstream `normalizeText`: CRLF / CR become LF, a tab becomes four
/// spaces (the composer has no tab stops, and a literal tab is one column
/// wide in [`crate::visual_text`]).
fn normalize_pasted_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\t' => out.push_str("    "),
            _ => out.push(ch),
        }
    }
    out
}

/// True when a pasted path has to be separated from the word before the
/// cursor (upstream's `if (/^[/~.]/.test(filteredText))` rule).
fn needs_path_separator(text: &str, before: Option<char>) -> bool {
    let Some(first) = text.chars().next() else {
        return false;
    };
    if !matches!(first, '/' | '~' | '.') {
        return false;
    }
    before.is_some_and(|c| c.is_alphanumeric() || c == '_')
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

/// Expand every [`CHIP_CHAR`] sentinel into its `[Image #N]` label.
///
/// The shared body of [`Editor::display_text`] and
/// [`HistoryEntry::display_text`], so a recalled entry renders identically to
/// the draft it was submitted from.
fn expand_chips(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut index = 0usize;
    for ch in text.chars() {
        if ch == CHIP_CHAR {
            index += 1;
            out.push_str(&chip_label(index));
        } else {
            out.push(ch);
        }
    }
    out
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

    /// A `pi-tui`-only manager with user overrides on top of the TUI defaults.
    fn manager_with(entries: &[(&str, &[&str])]) -> KeybindingsManager {
        let mut config = crate::keybindings::KeybindingsConfig::new();
        for (id, keys) in entries {
            config.set(*id, keys.iter().copied());
        }
        KeybindingsManager::new(crate::keybindings::tui_default_keybindings(), config)
    }

    #[test]
    fn history_chords_are_unbound_by_default() {
        // Upstream leaves both unbound (`keybindings.ts:74-81`), so the
        // cursor-aware `Up` / `Down` stay the only history path out of the
        // box. This is what makes the two tests below about *overrides*.
        let kb = KeybindingsManager::tui_defaults();
        assert!(kb.get_keys("tui.editor.historyPrevious").is_empty());
        assert!(kb.get_keys("tui.editor.historyNext").is_empty());
    }

    #[test]
    fn history_chords_browse_entries_instead_of_moving_the_cursor() {
        // LUM-1447: both ids had no consumer, so binding them did nothing.
        // Upstream `editor.ts:848-856` treats them as dedicated history
        // chords that always browse, whatever row the cursor is on.
        let kb = manager_with(&[
            ("tui.editor.historyPrevious", &["ctrl+p"]),
            ("tui.editor.historyNext", &["ctrl+n"]),
        ]);
        let mut ed = Editor::new();
        ed.push_history("older prompt");
        ed.insert_str("draft");
        assert_eq!(ed.cursor(), "draft".len());

        assert_eq!(
            ed.handle_key_with(&kb, Key::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            EditorAction::Changed
        );
        assert_eq!(ed.text(), "older prompt");
        assert_eq!(ed.cursor(), "older prompt".len());

        assert_eq!(
            ed.handle_key_with(&kb, Key::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            EditorAction::Changed
        );
        assert_eq!(ed.text(), "draft");
        assert_eq!(ed.cursor(), "draft".len());
    }

    #[test]
    fn history_chord_closes_the_autocomplete_dropdown() {
        // Upstream calls `cancelAutocomplete()` before navigating, and its
        // early return still cancels when the history is empty. Both halves
        // matter: a stale dropdown would keep the next `Enter` for itself.
        let kb = manager_with(&[("tui.editor.historyPrevious", &["ctrl+p"])]);
        let mut ed = Editor::new();
        ed.set_autocomplete_provider(Arc::new(
            crate::autocomplete::CombinedAutocompleteProvider::new(
                vec![crate::autocomplete::SlashCommand::new("help")],
                ".",
            ),
        ));
        ed.insert_str("/he");
        assert!(ed.request_autocomplete(false, false));
        assert!(ed.is_showing_autocomplete());

        // Empty history: the chord must still close the dropdown.
        assert_eq!(
            ed.handle_key_with(&kb, Key::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            EditorAction::None
        );
        assert!(!ed.is_showing_autocomplete());
        assert_eq!(ed.text(), "/he");
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

    /// `PageUp` / `PageDown` move the caret a page of visual rows at a time
    /// and clamp at the draft's ends (upstream `Editor.pageScroll`).
    #[test]
    fn page_scroll_moves_by_page_rows_and_clamps() {
        let mut ed = Editor::new();
        ed.set_visual_width(4);
        ed.set_page_rows(3);
        ed.insert_str("a\nb\nc\nd\ne\nf\ng\nh");
        assert_eq!(ed.visual_caret().0, 7, "the caret starts on the last row");

        assert_eq!(ed.page_up(), EditorAction::Changed);
        assert_eq!(ed.visual_caret().0, 4);
        assert_eq!(ed.page_up(), EditorAction::Changed);
        assert_eq!(ed.visual_caret().0, 1);
        assert_eq!(ed.page_up(), EditorAction::Changed);
        assert_eq!(ed.visual_caret().0, 0, "the page clamps at the first row");
        // Already at the top: the key is not a redraw.
        assert_eq!(ed.page_up(), EditorAction::None);

        assert_eq!(ed.page_down(), EditorAction::Changed);
        assert_eq!(ed.visual_caret().0, 3);
        for _ in 0..3 {
            let _ = ed.page_down();
        }
        assert_eq!(ed.visual_caret().0, 7, "and at the last one");
        assert_eq!(ed.page_down(), EditorAction::None);
    }

    /// A page keeps the display column across the rows it crosses, exactly
    /// like a single-row vertical move.
    #[test]
    fn page_scroll_keeps_the_display_column() {
        let mut ed = Editor::new();
        ed.set_visual_width(10);
        ed.set_page_rows(2);
        ed.insert_str("abcdefghij\nk\nlmnopqrst");
        assert_eq!(ed.visual_caret(), (2, 9));

        assert_eq!(ed.page_up(), EditorAction::Changed);
        // Two rows up reaches row 0, which can hold column 9.
        assert_eq!(ed.visual_caret(), (0, 9));
    }

    /// Without a page size the key still moves a row instead of becoming a
    /// no-op — the fallback for an editor nobody hand a frame to.
    #[test]
    fn page_scroll_falls_back_to_one_row_without_a_page_size() {
        let mut ed = Editor::new();
        ed.set_visual_width(80);
        ed.insert_str("a\nb\nc");
        assert_eq!(ed.page_rows(), 1);
        assert_eq!(ed.page_up(), EditorAction::Changed);
        assert_eq!(ed.visual_caret().0, 1);
    }

    /// The default `tui.editor.pageUp` / `pageDown` chords reach the page
    /// scroll through [`Editor::handle_key`].
    #[test]
    fn page_keys_dispatch_to_the_page_scroll() {
        let mut ed = Editor::new();
        ed.set_visual_width(80);
        ed.set_page_rows(5);
        ed.insert_str("1\n2\n3\n4\n5\n6\n7\n8\n9\n10");
        assert_eq!(ed.visual_caret().0, 9);

        let action = ed.handle_key(Key::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(action, EditorAction::Changed);
        assert_eq!(ed.visual_caret().0, 4);
        let action = ed.handle_key(Key::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(action, EditorAction::Changed);
        assert_eq!(ed.visual_caret().0, 9);
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
    fn place_display_cursor_moves_the_caret_to_the_character_offset() {
        let mut ed = Editor::new();
        ed.insert_str("你好世界");
        assert_eq!(ed.place_display_cursor(2), EditorAction::Changed);
        assert_eq!(ed.display_cursor(), 2);
        assert_eq!(
            ed.cursor(),
            "你好".len(),
            "the cursor is a byte offset, not a character count"
        );
        // The offset the caret already sits at is a no-op.
        assert_eq!(ed.place_display_cursor(2), EditorAction::None);
    }

    #[test]
    fn place_display_cursor_snaps_out_of_a_chip_label() {
        let mut ed = Editor::new();
        ed.insert_str("ab");
        ed.insert_image(image("one"));
        let label = chip_label(1).chars().count();
        assert_eq!(ed.display_cursor(), 2 + label);

        // The chip's own display offset puts the caret in front of it.
        ed.place_display_cursor(2);
        assert_eq!(ed.cursor(), 2, "the caret sits before the chip");
        // An offset inside the `[Image #1]` label clamps out of it.
        ed.place_display_cursor(2 + label - 1);
        assert_eq!(ed.display_cursor(), 2 + label);
        // Past the end clamps to the end of the draft.
        ed.place_display_cursor(99);
        assert_eq!(ed.display_cursor(), 2 + label);
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
