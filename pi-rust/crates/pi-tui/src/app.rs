//! Top-level [`App`] — owns the editor / message view / status bar and
//! drives the render loop.
//!
//! The App is intentionally framework-agnostic: it operates on the
//! [`InputEvent`] enum and a [`ratatui::buffer::Buffer`] view, so unit
//! tests can drive the full loop with synthetic input and assert on
//! the rendered buffer without ever touching a real terminal.
//!
//! Concrete binaries embed the App in a `crossterm`-backed
//! [`ratatui::Terminal`] and feed it events from the terminal input.
//! See `crates/pi-coding-agent/src/interactive.rs` for the wiring.
//!
//! # Text selection
//!
//! The chat log owns its own selection because mouse capture takes the
//! terminal's native text selection away. Upstream supports three
//! granularities (double-click selects a word, triple-click a line —
//! `SelectionGranularity`, `packages/tui/src/tui-alt-screen.ts:104`) and
//! keeps scrolling while the drag pointer rests on a viewport edge
//! (`updateSelectionAutoScroll`, `tui-alt-screen.ts:1264-1297`). This port
//! mirrors both.
//!
//! Deliberate deviations, documented here rather than silently omitted:
//!
//! * **Click-count source.** Upstream reads `TuiMouseEvent.clickCount`,
//!   which the engine synthesises; `crossterm` exposes no such field and
//!   [`MouseGesture`] deliberately carries none, so the App reconstructs
//!   the count itself from the same key upstream uses: a press within
//!   [`DOUBLE_CLICK_INTERVAL`] of the previous one, on the same rendered
//!   log line and the same word column range. The rule is upstream's
//!   `getClickCount` (`tui-alt-screen.ts:1220-1245`); only the time source
//!   differs (`std::time::Instant` instead of `Date.now()`).
//! * **Word segmentation.** Word selection is built from
//!   [`crate::word_navigation`]'s UAX #29 boundaries (`unicode_segmentation`,
//!   already a dependency), not `Intl.Segmenter`, with `/` and `-` treated
//!   as joiners exactly like upstream's `TERMINAL_WORD_SELECTION_JOINERS`
//!   (`tui-alt-screen.ts:82`). The segmentation difference is the CJK one
//!   `word_navigation` already documents: ICU's dictionary keeps `你好`
//!   together, UAX #29 splits each ideograph, so double-click selects one
//!   ideograph per hop.
//! * **Columns are characters, not display cells.** The whole port tracks
//!   selection columns as character offsets (see [`App::selection_text`]),
//!   so word / line ranges are measured with `chars().count()` rather than
//!   upstream's `visibleWidth`. Wide glyphs keep the existing behaviour of
//!   the character-granularity path.
//! * **Autoscroll beat.** There is no `setInterval` in Rust and this crate
//!   must not spawn a timer thread, so the drag autoscroll advances one
//!   line **per draw**. The driver redraws on a 50 ms interval
//!   (`pi-coding-agent`'s render loop), which reproduces upstream's 50 ms
//!   `setInterval`; on a stationary pointer, redraws are what keep the
//!   viewport moving. [`App::advance_selection_autoscroll`] is public so
//!   tests can step the beat deterministically.
//!
//! # Transcript search
//!
//! `Ctrl+Shift+F` opens an in-transcript search overlay — the port of
//! upstream's `AltScreenSearch*` (`packages/tui/src/alt-screen-search.ts`,
//! wired up in `packages/tui/src/tui-alt-screen.ts:496-660,705-720`). The bar
//! is anchored to the top-right of the message viewport, the query is matched
//! against the **rendered** transcript, the selection starts at the first
//! match at or after the viewport's top row, `Enter` / `Ctrl+G` and
//! `Shift+Enter` / `Ctrl+Shift+G` step through matches with wraparound and
//! scroll them into view, hits are highlighted in place (current = bold +
//! reversed, others = underlined), and the bar answers mouse hover / clicks
//! inside its own rectangle. [`crate::search`] holds the query bar, the corpus
//! and the geometry; the App owns the state machine.
//!
//! Deliberate deviations, documented here rather than silently omitted:
//!
//! * **Columns are characters, not display cells.** [`crate::search`] returns
//!   character-offset spans, matching the selection path above, so wide glyphs
//!   are not widened to the display cell.
//! * **Case folding is per-character `to_lowercase()`**, not the Unicode case
//!   folding of upstream's `regex` `iu` flags: `pi-tui` has no `regex`
//!   dependency and the query is matched literally.
//! * **The corpus is already plain text.** Upstream strips terminal sequences
//!   while building it; the lines this port indexes come from
//!   [`MessageView::visible_lines`] and [`plain_text`], which are sequence-free
//!   by construction.
//! * **No cursor cell.** The bar keeps the query cursor offset (editing is
//!   fully supported) but does not paint a reversed cell for it, and pads its
//!   own borders, so every rendered bar line is exactly as wide as the bar.
//! * **The overlay is anchored to the message viewport**, not the whole
//!   terminal: the App does not own the status / prompt rows. It is painted
//!   *last*, so an extension dialog cannot cover it.

use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{
    Event as CtEvent, KeyModifiers as CtModifiers, MouseEventKind as CtMouseEventKind,
};
use parking_lot::Mutex;
use pi_agent_core::{Agent, AgentEvent, AssistantMessageUpdate};
use pi_protocol::{Content, Message, Usage};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;
use unicode_segmentation::UnicodeSegmentation;

use crate::dialog::{Dialog, DialogAction, DialogKind};
use crate::editor::EditorAction;
use crate::input::{
    InputEvent, Key, KeyCode, KeyModifiers, MouseButton, MouseGesture, MouseGestureKind,
};
use crate::keybindings::get_keybindings;
use crate::message::{MessageItem, MessageView};
use crate::mouse_region::{MouseRegion, MouseRegionPoint};
use crate::prompt::{Prompt, PromptAction};
use crate::search::{
    apply_query_key, render_search_bar, search_bar_rect, SearchBar, SearchIndex, SearchMatch,
    SearchSelectionMode,
};
use crate::selector::{Selector, SelectorAction, SelectorItem};
use crate::settings::{SettingsAction, SettingsList};
use crate::status::{StatusBar, StatusData};
use crate::styled::{plain_text, write_styled_line};
use crate::theme::{builtin_theme, load_theme, ColorMode, Theme, ThemeError};

/// Lines scrolled per wheel notch. Mirrors the upstream `wheelScrollLines`
/// option's default (`packages/tui/src/tui-alt-screen.ts:166,264`).
const WHEEL_SCROLL_LINES: usize = 1;

/// Alt+wheel multiplies the per-notch step by this factor, matching
/// upstream's `ALT_WHEEL_SCROLL_MULTIPLIER`
/// (`packages/tui/src/tui-alt-screen.ts:75,968-971`).
const ALT_WHEEL_SCROLL_MULTIPLIER: usize = 5;

/// Window in which two presses on the same word count as a double click
/// (and three as a triple click). Upstream's `DOUBLE_CLICK_INTERVAL_MS`
/// (`packages/tui/src/tui-alt-screen.ts:79`).
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(500);

/// Line-column segments that stay attached to a word when double-clicking,
/// so paths and kebab-case tokens select whole. Upstream's
/// `TERMINAL_WORD_SELECTION_JOINERS` (`packages/tui/src/tui-alt-screen.ts:82`).
const TERMINAL_WORD_SELECTION_JOINERS: [&str; 2] = ["/", "-"];

/// Lines a single autoscroll beat moves the viewport. Upstream scrolls one
/// line per 50 ms `setInterval` tick (`tui-alt-screen.ts:1264-1297`).
const SELECTION_AUTOSCROLL_LINES: usize = 1;

/// Configuration knobs for the App.
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// Placeholder shown in the prompt when it is empty.
    pub prompt_placeholder: String,
    /// Session identifier shown in the status bar.
    pub session_id: String,
    /// Polling interval for `crossterm::event::poll` (microseconds). The
    /// TUI uses this to throttle the render loop when there is no
    /// input.
    pub event_poll_interval: Duration,
    /// Render assistant bodies as markdown (headings, lists, fenced code,
    /// emphasis, …) instead of plain text. On by default, matching
    /// upstream's `Markdown` component in the assistant message
    /// (`packages/coding-agent/src/modes/interactive/components/assistant-message.ts`).
    ///
    /// The switch can be flipped at runtime with [`App::set_markdown`];
    /// `/clear` and other transcript operations leave it untouched.
    pub markdown: bool,
    /// Copy a finished chat-log text selection to the clipboard as soon as
    /// the mouse button is released. On by default, matching upstream's
    /// `copyOnSelect ?? true` (`packages/tui/src/tui-alt-screen.ts:272`).
    ///
    /// "Copy" here means "hand the selection text to the driver" (see
    /// [`App::take_clipboard_request`]); the App never touches the
    /// terminal or the system clipboard itself.
    pub copy_on_select: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            prompt_placeholder: "type a prompt — /help for commands".to_string(),
            session_id: "local".to_string(),
            event_poll_interval: Duration::from_millis(50),
            markdown: true,
            copy_on_select: true,
        }
    }
}

/// Provider usage reported by the most recently finished agent turn.
///
/// Rust [`Message`] rows carry no provider usage, and `pi-tui` cannot
/// depend on `pi-coding-agent` (the dependency runs the other way), so
/// the driver cannot recover the last turn's usage from the agent's
/// message log. [`App::drain_agent_events`] records it here instead and
/// drivers consume it with [`App::take_turn_usage`] — `pi-coding-agent`
/// uses it to decide whether automatic compaction should run.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnUsage {
    /// Usage reported by the turn's final assistant message.
    pub usage: Usage,
    /// Messages that followed that assistant message in the turn (the
    /// tool results it produced), for `context_tokens_with_trailing`.
    pub trailing: Vec<Message>,
}

/// Granularity of the active selection. Upstream's
/// `SelectionGranularity` (`packages/tui/src/tui-alt-screen.ts:104`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionGranularity {
    /// Cell-by-cell selection (a plain drag).
    Character,
    /// Whole-word selection (a double click, or a drag started by one).
    Word,
    /// Whole-line selection (a triple click, or a drag started by one).
    Line,
}

/// One end of a selection, in absolute rendered-log coordinates.
///
/// [`SelectionPoint::boundary`] mirrors upstream's `SelectionPoint.boundary`
/// (`packages/tui/src/tui-alt-screen.ts:104-110`): a word / line range ends
/// *between* cells, so the end column is exclusive, while a character
/// focus column is inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SelectionPoint {
    /// Index of a line in [`MessageView::render_styled_lines`].
    line: usize,
    /// Character column within that line.
    col: usize,
    /// True when `col` is an exclusive end (a word / line range edge).
    boundary: bool,
}

impl SelectionPoint {
    /// An inclusive cell (a character-granularity endpoint).
    const fn cell(line: usize, col: usize) -> Self {
        Self {
            line,
            col,
            boundary: false,
        }
    }

    /// An exclusive range edge (the end of a word / line range).
    const fn boundary(line: usize, col: usize) -> Self {
        Self {
            line,
            col,
            boundary: true,
        }
    }

    /// Reading-order key: line first, then column (the `boundary` flag does
    /// not affect order, matching upstream's row/col comparison).
    const fn order(&self) -> (usize, usize) {
        (self.line, self.col)
    }
}

/// A text selection in the chat log.
///
/// Both ends are absolute coordinates in the *rendered log*: the `line` of a
/// line as produced by [`MessageView::render_styled_lines`], and a character
/// column within it. Absolute line indices (rather than screen rows) are what
/// makes a selection survive scrolling and trailing output: the same text
/// stays selected while the viewport moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Selection {
    /// Where the drag started (or the start of the press range).
    anchor: SelectionPoint,
    /// Where the pointer currently is (or the end of the press range).
    focus: SelectionPoint,
    /// Granularity the gesture started at; a drag recomputes the focus
    /// range the same way (upstream's `selectionGranularity`).
    granularity: SelectionGranularity,
    /// The range captured on press for word / line granularity. Drags
    /// recompute the focus range from the pointer and anchor against this
    /// (upstream's `selectionInitialRange`,
    /// `packages/tui/src/tui-alt-screen.ts:1193-1218`). `None` for
    /// character selections.
    initial: Option<(SelectionPoint, SelectionPoint)>,
}

impl Selection {
    /// Construct a zero-length character selection at `point` (a mouse
    /// press).
    const fn at(point: (usize, usize)) -> Self {
        let point = SelectionPoint::cell(point.0, point.1);
        Self {
            anchor: point,
            focus: point,
            granularity: SelectionGranularity::Character,
            initial: None,
        }
    }

    /// Construct a word / line selection from the range captured on press.
    const fn range(
        start: SelectionPoint,
        end: SelectionPoint,
        granularity: SelectionGranularity,
    ) -> Self {
        Self {
            anchor: start,
            focus: end,
            granularity,
            initial: Some((start, end)),
        }
    }

    /// Start / end in reading order, or `None` when the selection is
    /// empty — upstream's `getSelectionBounds`
    /// (`packages/tui/src/tui-alt-screen.ts:1381-1397`) treats an
    /// anchor equal to the focus as "no selection".
    fn bounds(&self) -> Option<(SelectionPoint, SelectionPoint)> {
        if self.anchor.order() == self.focus.order() {
            return None;
        }
        Some(if self.anchor.order() < self.focus.order() {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        })
    }
}

/// A press recorded for double / triple click detection (upstream's
/// `ClickTarget`, `packages/tui/src/tui-alt-screen.ts:114-121`).
#[derive(Debug, Clone, Copy)]
struct ClickTarget {
    /// When the press happened.
    at: Instant,
    /// Consecutive clicks so far (1, 2, 3, then back to 1).
    count: usize,
    /// Rendered-log line the press landed on.
    row: usize,
    /// First column of the word range the press resolved to.
    word_start: usize,
    /// Exclusive end column of that word range.
    word_end: usize,
}

/// One UAX #29 segment of a rendered line, in character columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WordSegment {
    /// First column of the segment (inclusive).
    start: usize,
    /// One past the last column of the segment (exclusive).
    end: usize,
    /// Whether the segment is pickable (word-like, or a joiner).
    selectable: bool,
    /// Whether the segment is one of [`TERMINAL_WORD_SELECTION_JOINERS`].
    joiner: bool,
}

/// Split a rendered line into the segments upstream's `getWordSelection`
/// walks (`packages/tui/src/tui-alt-screen.ts:1156-1197`).
///
/// Columns are character offsets, not terminal cells — see the module docs.
fn word_segments(line: &str) -> Vec<WordSegment> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    for segment in line.split_word_bounds() {
        let end = start + segment.chars().count();
        let joiner = TERMINAL_WORD_SELECTION_JOINERS.contains(&segment);
        let selectable = crate::word_navigation::is_word_like(segment) || joiner;
        segments.push(WordSegment {
            start,
            end,
            selectable,
            joiner,
        });
        start = end;
    }
    segments
}

/// Upstream's `canJoin` (`packages/tui/src/tui-alt-screen.ts:1174-1177`):
/// two pickable segments join when at least one of them is a joiner.
fn word_segments_can_join(left: &WordSegment, right: &WordSegment) -> bool {
    left.selectable && right.selectable && (left.joiner || right.joiner)
}

/// Exclusive end column of a selection end, clamped to `len` — upstream's
/// `getSelectionColumns` end half (`packages/tui/src/tui-alt-screen.ts:1424-1432`):
/// a boundary column is already one past the last selected cell, while an
/// inclusive focus column selects the cell under it.
fn selection_end_column(end: &SelectionPoint, len: usize) -> usize {
    if end.boundary {
        end.col.min(len)
    } else {
        end.col.saturating_add(1).min(len)
    }
}

/// Open transcript search state (upstream `ActiveSearch`,
/// `packages/tui/src/tui-alt-screen.ts:500-511`).
///
/// The [`SearchIndex`] caches the corpus + matches, [`SearchBar`] owns the
/// query and its caret, and the remaining fields are the selection contract
/// `refreshSearch` implements: which match is selected, the key used to keep
/// that match across a re-index, the row the next "first match at or after"
/// lookup is anchored to, and how the *next* refresh should recompute the
/// selection.
#[derive(Debug)]
struct SearchState {
    /// Cached corpus + matches for the rendered transcript.
    index: SearchIndex,
    /// The query bar and its result counter.
    bar: SearchBar,
    /// Matches from the last refresh.
    matches: Vec<SearchMatch>,
    /// Index into [`SearchState::matches`] of the selected match.
    selected_index: Option<usize>,
    /// [`SearchMatch::key`] of the selected match, so a re-index keeps it.
    selected_key: Option<String>,
    /// Row the `Query` selection mode anchors to.
    anchor_row: usize,
    /// How the next refresh recomputes the selection.
    selection_mode: SearchSelectionMode,
}

impl SearchState {
    /// A fresh, empty search anchored to `anchor_row`.
    fn new(anchor_row: usize) -> Self {
        Self {
            index: SearchIndex::new(),
            bar: SearchBar::new(),
            matches: Vec::new(),
            selected_index: None,
            selected_key: None,
            anchor_row,
            selection_mode: SearchSelectionMode::Query,
        }
    }
}

/// Result of routing a key to the open search overlay.
///
/// The overlay owns the keyboard while it is open, *except* for the viewport
/// scroll chords and the process-global Ctrl+C / Ctrl+L: upstream lets those
/// through because `shouldDeferViewportInputToOverlay` is false once the search
/// overlay itself holds focus
/// (`packages/tui/src/tui-alt-screen.ts:644-645,1806-1811`).
#[derive(Debug, Clone)]
enum SearchKeyOutcome {
    /// The overlay consumed the key.
    Handled(StepOutcome),
    /// `App::step_key`'s global handling must still see the key.
    PassThrough,
}

/// Outcome returned by [`App::step`] after each key event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// Step made no meaningful state change.
    Idle,
    /// Step mutated the rendered state.
    Redraw,
    /// User submitted a prompt. The caller is responsible for handing
    /// the text to the agent.
    Submitted(String),
    /// User pressed Ctrl+C / Ctrl+D — the caller should shut the App
    /// down and (optionally) fall back to print mode.
    Exit,
}

/// Snapshot of the rendered App for tests.
#[derive(Debug, Clone)]
pub struct RenderSnapshot {
    /// Width the snapshot was rendered at.
    pub width: u16,
    /// Height the snapshot was rendered at.
    pub height: u16,
    /// Pre-rendered content lines, top to bottom.
    pub lines: Vec<String>,
    /// Currently displayed placeholder text (empty if the prompt has
    /// a buffer).
    pub prompt_placeholder: String,
    /// Buffer text the editor currently shows.
    pub prompt_buffer: String,
    /// Whether the settings modal is currently visible.
    pub settings_open: bool,
    /// Rendered settings lines (when open).
    pub settings_lines: Vec<String>,
    /// Whether the selector modal is currently visible.
    pub selector_open: bool,
    /// Selector title (when open).
    pub selector_title: Option<String>,
    /// Selector items (when open).
    pub selector_items: Vec<SelectorItem>,
    /// Selector cursor index (when open).
    pub selector_cursor: Option<usize>,
    /// Whether a modal extension dialog is currently visible.
    pub dialog_open: bool,
    /// Flavour of the open dialog (when open).
    pub dialog_kind: Option<DialogKind>,
    /// Title of the open dialog (when open).
    pub dialog_title: Option<String>,
    /// Rendered dialog lines (when open).
    pub dialog_lines: Vec<String>,
    /// Whether the transcript search overlay is visible.
    pub search_open: bool,
    /// Current search query (empty while the overlay is closed).
    pub search_query: String,
    /// Rendered search-bar lines (when open).
    pub search_lines: Vec<String>,
    /// Status bar snapshot.
    pub status: StatusData,
}

impl RenderSnapshot {
    /// Convert the lines into a `Buffer` so callers can compare against
    /// `Buffer::with_lines` results.
    pub fn to_buffer(&self) -> Buffer {
        Buffer::with_lines(self.lines.iter().map(String::as_str))
    }
}

/// Top-level App.
pub struct App {
    config: AppConfig,
    prompt: Prompt,
    messages: MessageView,
    status_bar: StatusBar,
    status_data: StatusData,
    /// The active palette the buffer render path consumes. Swapping it with
    /// [`App::set_theme`] takes effect on the next [`App::render_to_buffer`]
    /// call — no rebuild, no restart.
    theme: Theme,
    selector: Option<Selector>,
    /// Modal settings list opened by `/settings` (upstream
    /// `SettingsSelectorComponent`).
    settings: Option<SettingsList>,
    /// Value the driver must persist — the last
    /// [`SettingsAction::ValueChanged`] the list reported. Consumed with
    /// [`App::take_pending_setting_change`].
    pending_setting_change: Option<(String, String)>,
    /// Item id the driver must handle — the last
    /// [`SettingsAction::Activated`] the list reported (upstream opens the
    /// item's submenu). Consumed with
    /// [`App::take_pending_setting_activation`].
    pending_setting_activation: Option<String>,
    /// Modal requested by a JS extension (`ctx.ui.confirm` / `input` /
    /// `select`) that is waiting for a key press.
    dialog: Option<Dialog>,
    /// Channel carrying dialogs from the extension host. Only set in
    /// interactive mode; see `pi-coding-agent`'s extension UI bridge.
    ui_dialogs: Option<mpsc::UnboundedReceiver<Dialog>>,
    /// Live subscription to agent events. Constructed in
    /// [`App::new`] from `agent.subscribe()` so the App receives every
    /// event the agent emits across all turns.
    event_rx: Option<mpsc::UnboundedReceiver<AgentEvent>>,
    /// Cancellation token for the in-flight prompt, if any.
    cancel_token: Option<CancellationToken>,
    /// Last user-facing error surfaced by the agent loop. Rendered
    /// into the message view on the next step.
    pending_error: Option<String>,
    /// Usage of the most recently finished turn, recorded by
    /// [`App::drain_agent_events`] and consumed by
    /// [`App::take_turn_usage`].
    last_turn_usage: Option<TurnUsage>,
    /// Shared liveness flag for the in-flight `submit` task. The task
    /// clears it when `Agent::prompt` returns, so [`App::is_busy`]
    /// answers "a turn is running" instead of "a turn was ever
    /// started".
    turn_busy: Arc<AtomicBool>,
    /// Set when the App should exit at the next opportunity. The TUI
    /// exit path checks this between key events.
    exit_requested: bool,
    /// Width of the message viewport as of the last render. Scroll keys use
    /// it to wrap the log exactly like the renderer does, so a "page" is a
    /// real screenful.
    viewport_width: AtomicU16,
    /// Height of the message viewport as of the last render; the page size
    /// for `PageUp` / `PageDown`.
    viewport_height: AtomicU16,
    /// Top-left cell of the message viewport as of the last render. Pointer
    /// coordinates are absolute, so selection has to map them back into the
    /// viewport the reader was actually looking at.
    viewport_origin: (AtomicU16, AtomicU16),
    /// Active chat-log text selection, if any.
    selection: Option<Selection>,
    /// Region-local cell of a left press that landed inside a modal overlay,
    /// kept until its release so only a click that starts and ends on the
    /// same cell commits (upstream's `isClick`,
    /// `packages/tui/src/tui-alt-screen.ts:1312-1315`). `None` whenever no
    /// modal is on screen.
    modal_mouse_press: Option<MouseRegionPoint>,
    /// True between a left-button press and its release, so drags extend
    /// the selection without requiring the terminal to report the button.
    selection_dragging: bool,
    /// Open transcript search overlay, if any (upstream `activeSearch`,
    /// `packages/tui/src/tui-alt-screen.ts:496-521`).
    search: Option<SearchState>,
    /// Last press, kept for double / triple click detection (upstream's
    /// `lastClick`, `packages/tui/src/tui-alt-screen.ts:114-121`).
    last_click: Option<ClickTarget>,
    /// Where the drag autoscroll is pulling: `-1` towards older output,
    /// `1` towards the tail, `0` when it is stopped (`selectionAutoScrollDirection`).
    selection_autoscroll_direction: i8,
    /// The pointer cell a drag autoscroll is anchored to — absolute
    /// terminal coordinates, `None` while stopped
    /// (`selectionDragPointer`).
    selection_autoscroll_pointer: Option<(u16, u16)>,
    /// Text waiting to be copied by the driver. Filled by copy-on-select;
    /// consumed with [`App::take_clipboard_request`].
    pending_clipboard: Option<String>,
}

impl App {
    /// Construct an App over an [`Agent`] handle. The App subscribes
    /// to the agent's event stream immediately and drains events
    /// during [`App::drain_agent_events`] (called by the render
    /// loop).
    pub fn new(agent: &Agent, config: AppConfig) -> Self {
        let mut status_data = StatusData::new(
            agent
                .model()
                .label
                .clone()
                .unwrap_or_else(|| agent.model().id.clone()),
            config.session_id.clone(),
        );
        status_data.hint = Some("? for help".to_string());
        let mut prompt = Prompt::new("> ");
        prompt.set_placeholder(config.prompt_placeholder.clone());
        let event_rx = agent.subscribe();
        let markdown = config.markdown;
        Self {
            config,
            prompt,
            messages: MessageView::new().with_markdown(markdown),
            status_bar: StatusBar::new(),
            status_data,
            theme: builtin_theme("dark", ColorMode::TrueColor)
                .expect("built-in dark theme is valid"),
            selector: None,
            settings: None,
            pending_setting_change: None,
            pending_setting_activation: None,
            dialog: None,
            ui_dialogs: None,
            event_rx: Some(event_rx),
            cancel_token: None,
            pending_error: None,
            last_turn_usage: None,
            turn_busy: Arc::new(AtomicBool::new(false)),
            exit_requested: false,
            viewport_width: AtomicU16::new(0),
            viewport_height: AtomicU16::new(0),
            viewport_origin: (AtomicU16::new(0), AtomicU16::new(0)),
            selection: None,
            search: None,
            modal_mouse_press: None,
            selection_dragging: false,
            last_click: None,
            selection_autoscroll_direction: 0,
            selection_autoscroll_pointer: None,
            pending_clipboard: None,
        }
    }

    /// Whether assistant bodies are currently rendered as markdown.
    pub fn markdown(&self) -> bool {
        self.messages.markdown()
    }

    /// Turn markdown rendering of assistant bodies on or off.
    ///
    /// Takes effect on the next render; the transcript items themselves are
    /// untouched, so flipping the switch back re-renders the same bodies.
    pub fn set_markdown(&mut self, enabled: bool) {
        self.messages.set_markdown(enabled);
    }

    /// Borrow the message view (for tests and snapshots).
    pub fn messages(&self) -> &MessageView {
        &self.messages
    }

    /// Mutable borrow of the message view.
    pub fn messages_mut(&mut self) -> &mut MessageView {
        &mut self.messages
    }

    /// Borrow the prompt.
    pub fn prompt(&self) -> &Prompt {
        &self.prompt
    }

    /// Mutable borrow of the prompt.
    pub fn prompt_mut(&mut self) -> &mut Prompt {
        &mut self.prompt
    }

    /// Borrow the status data.
    pub fn status_data(&self) -> &StatusData {
        &self.status_data
    }

    /// Mutable borrow of the status data.
    pub fn status_data_mut(&mut self) -> &mut StatusData {
        &mut self.status_data
    }

    /// The palette the buffer render path currently consumes.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Install a theme. The next [`App::render_to_buffer`] (or
    /// [`App::render_snapshot`]) reflects it immediately — hot-swapping does
    /// not require rebuilding the App.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    /// Whether copy-on-select is on for the chat log.
    pub fn copy_on_select(&self) -> bool {
        self.config.copy_on_select
    }

    /// Turn copy-on-select on or off for the running session.
    ///
    /// The next selection release respects the new value; selections that
    /// were already captured stay queued.
    pub fn set_copy_on_select(&mut self, enabled: bool) {
        self.config.copy_on_select = enabled;
    }

    /// Load and install a theme by name, keeping the current colour mode.
    ///
    /// Built-in names (`dark`, `light`) always resolve; custom themes are not
    /// searched because the App does not own a custom-themes directory. On
    /// failure the previous theme is left untouched.
    pub fn set_theme_by_name(&mut self, name: &str) -> Result<(), ThemeError> {
        let theme = load_theme(name, self.theme.color_mode(), None)?;
        self.theme = theme;
        Ok(())
    }

    /// Whether the user has requested an exit.
    pub fn is_exit_requested(&self) -> bool {
        self.exit_requested
    }

    /// Whether a background agent turn is currently in flight.
    pub fn is_busy(&self) -> bool {
        self.turn_busy.load(Ordering::SeqCst)
    }

    /// Usage of the most recently finished turn.
    ///
    /// Read-only peek at what [`App::take_turn_usage`] would return;
    /// it does not consume the value, so a driver that peeks during a
    /// render pass can still take it later.
    pub fn last_turn_usage(&self) -> Option<&TurnUsage> {
        self.last_turn_usage.as_ref()
    }

    /// Take the usage of the most recently finished turn.
    ///
    /// Returns `None` when no turn has finished since the last call.
    /// The value is cleared so a driver reacts once per turn; a later
    /// `TurnEnd` replaces it, so an intermediate turn is never acted on
    /// while the prompt that produced it is still running.
    pub fn take_turn_usage(&mut self) -> Option<TurnUsage> {
        self.last_turn_usage.take()
    }

    /// Set a model override that takes effect on the next
    /// `Agent::prompt` call.
    pub fn queue_model_switch(&mut self, agent: &mut Agent, model: pi_protocol::Model) {
        self.status_data.model = model.label.clone().unwrap_or_else(|| model.id.clone());
        agent.set_model(model);
    }

    /// Drain any pending agent events into the message view. The
    /// caller calls this on every render tick — the App drains
    /// synchronously, so the TUI never blocks on the agent.
    pub fn drain_agent_events(&mut self) -> bool {
        let mut changed = false;
        loop {
            let event = match self.event_rx.as_mut() {
                None => break,
                Some(rx) => match rx.try_recv() {
                    Ok(event) => event,
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        self.cancel_token = None;
                        self.event_rx = None;
                        changed = true;
                        break;
                    }
                },
            };
            changed = true;
            self.apply_event(event);
        }
        changed
    }

    /// Apply a single [`AgentEvent`] to the message view + status bar.
    fn apply_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStart => {}
            AgentEvent::MessageStart { model } => {
                self.messages.begin_assistant_stream(&model);
            }
            AgentEvent::MessageUpdate(update) => match update {
                AssistantMessageUpdate::TextDelta { delta } => {
                    self.messages.append_assistant_delta(&delta);
                }
                AssistantMessageUpdate::ThinkingDelta { .. } => {
                    // Collapsed thinking — not rendered by Stage 4.
                }
                AssistantMessageUpdate::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments_delta: _,
                } => {
                    let label = name.unwrap_or_else(|| format!("tool-{index}"));
                    let _ = id; // placeholder — Stage 4 collapses into a single block
                    self.messages.push_tool(&label, "(streaming)", "", false);
                }
            },
            AgentEvent::MessageEnd { message } => {
                self.messages.end_assistant_stream();
                self.status_data
                    .add_tokens(message.usage.input, message.usage.output);
            }
            AgentEvent::ToolExecutionStart { call } => {
                self.messages.push_tool(&call.name, "(running)", "", false);
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id: _,
                delta,
            } => {
                self.messages.append_assistant_delta(&delta);
            }
            AgentEvent::ToolExecutionEnd {
                result,
                duration_ms,
            } => {
                let name = String::new();
                let is_error = result.is_error;
                let body = match result.content.as_ref() {
                    Content::Text(t) => t.text.clone(),
                    _ => "(binary result)".to_string(),
                };
                self.messages
                    .push_tool(&name, &format!("{duration_ms}ms"), &body, is_error);
            }
            AgentEvent::TurnEnd {
                message,
                tool_results,
            } => {
                if message.stop_reason == pi_protocol::StopReason::Error {
                    self.pending_error = Some("provider returned an error".into());
                }
                self.last_turn_usage = Some(TurnUsage {
                    usage: message.usage,
                    trailing: tool_results,
                });
            }
            AgentEvent::UserMessage(_) => {}
            AgentEvent::Error(message) => {
                self.pending_error = Some(message);
            }
        }
    }

    /// Pop the last pending error (if any). The TUI prints this on
    /// the next render so the user sees the agent surface error.
    pub fn take_error(&mut self) -> Option<String> {
        self.pending_error.take()
    }

    /// Begin an agent turn asynchronously. The App spawns a tokio
    /// task that calls `Agent::prompt`; events flow through the
    /// subscriber channel established in [`App::new`] and are drained
    /// by [`App::drain_agent_events`].
    pub fn submit(&mut self, agent: Arc<AsyncMutex<Agent>>, text: String) {
        if self.turn_busy.load(Ordering::SeqCst) {
            return; // already busy
        }
        if self.event_rx.is_none() {
            // App constructed without a subscription — re-establish one.
            if let Ok(guard) = agent.try_lock() {
                self.event_rx = Some(guard.subscribe());
            }
        }
        self.messages.push(MessageItem::user(&text));
        self.prompt.push_history(&text);
        let cancel = CancellationToken::new();
        self.cancel_token = Some(cancel.clone());
        self.turn_busy.store(true, Ordering::SeqCst);

        let busy = self.turn_busy.clone();
        let cancel_for_task = cancel.clone();
        let agent_clone = agent.clone();
        let text_clone = text.clone();
        tokio::spawn(async move {
            let mut guard = agent_clone.lock().await;
            let result = guard.prompt(&text_clone).await;
            drop(guard);
            if let Err(err) = result {
                let _ = cancel_for_task; // keep the cancellation alive until drop
                let guard = agent_clone.lock().await;
                guard.emit(AgentEvent::Error(err.to_string()));
            }
            busy.store(false, Ordering::SeqCst);
        });
    }

    /// Cancel the in-flight turn (if any).
    pub fn cancel(&mut self) {
        if let Some(token) = self.cancel_token.take() {
            token.cancel();
        }
    }

    /// Open the model selector. The selector lists the supplied
    /// candidates; the App does not interpret `value` — the caller
    /// closes the selector with [`App::close_selector`] and applies
    /// the picked value.
    pub fn open_selector(&mut self, selector: Selector) {
        self.selector = Some(selector);
    }

    /// Close the selector and return the picked value if any.
    pub fn close_selector(&mut self) -> Option<Selector> {
        self.selector.take()
    }

    /// Replace the active selector — used by the binary entry point
    /// when it wants to mutate the selector state in response to a key
    /// event without taking it out of the App.
    pub fn replace_selector(&mut self, selector: Selector) {
        self.selector = Some(selector);
    }

    /// Whether the selector modal is currently visible.
    pub fn selector_open(&self) -> bool {
        self.selector.is_some()
    }

    /// Attach the channel that carries extension dialogs.
    ///
    /// The interactive entry point calls this with the receiver half of
    /// the extension UI bridge; [`App::poll_ui_dialogs`] then turns each
    /// envelope into a modal.
    pub fn attach_ui_dialogs(&mut self, rx: mpsc::UnboundedReceiver<Dialog>) {
        self.ui_dialogs = Some(rx);
    }

    /// Show `dialog` and wait for the user.
    ///
    /// Returns `false` without showing it when another modal is already
    /// on screen — the dialog is answered with its cancel default so the
    /// extension never waits on a prompt nobody can see.
    pub fn open_dialog(&mut self, mut dialog: Dialog) -> bool {
        if self.dialog.is_some() {
            dialog.cancel();
            return false;
        }
        self.dialog = Some(dialog);
        true
    }

    /// Whether a modal extension dialog is on screen.
    pub fn dialog_open(&self) -> bool {
        self.dialog.is_some()
    }

    /// Borrow the open dialog.
    pub fn dialog(&self) -> Option<&Dialog> {
        self.dialog.as_ref()
    }

    /// Take the open dialog out of the App — used by the entry point
    /// after it resolved the answer itself.
    pub fn take_dialog(&mut self) -> Option<Dialog> {
        self.dialog.take()
    }

    /// Drain queued extension UI requests.
    ///
    /// `notify` requests become message-view lines (they are
    /// fire-and-forget), everything else becomes a modal. A modal whose
    /// host stopped waiting is closed. Returns whether the App changed
    /// and the caller should redraw.
    pub fn poll_ui_dialogs(&mut self) -> bool {
        let mut changed = false;
        if let Some(dialog) = &self.dialog {
            if dialog.is_abandoned() {
                self.dialog = None;
                changed = true;
            }
        }
        loop {
            let dialog = match self.ui_dialogs.as_mut() {
                None => break,
                Some(rx) => match rx.try_recv() {
                    Ok(dialog) => dialog,
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        self.ui_dialogs = None;
                        changed = true;
                        break;
                    }
                },
            };
            changed = true;
            if dialog.kind() == DialogKind::Notify {
                let mut dialog = dialog;
                if let Some((message, level)) = dialog.notify_text() {
                    self.messages.push_info(format!("[{level:?}] {message}"));
                }
                dialog.resolve(Some(pi_protocol::UiResponse::NotifyAck));
                continue;
            }
            // A rejected dialog is answered with its cancel default by
            // `open_dialog`; the sender dropping would do the same, but
            // sending is explicit.
            let _ = self.open_dialog(dialog);
        }
        changed
    }

    /// Borrow the active selector (if any).
    pub fn selector(&self) -> Option<&Selector> {
        self.selector.as_ref()
    }

    /// Open the settings modal (`/settings`).
    ///
    /// The list owns the keyboard until it is cancelled or the driver calls
    /// [`App::close_settings`]. Any queued setup work from a previous
    /// session is dropped so a stale change cannot be persisted against the
    /// new list.
    pub fn open_settings(&mut self, list: SettingsList) {
        self.settings = Some(list);
        self.pending_setting_change = None;
        self.pending_setting_activation = None;
    }

    /// Close the settings modal and hand the list back to the driver.
    pub fn close_settings(&mut self) -> Option<SettingsList> {
        self.settings.take()
    }

    /// Whether the settings modal is on screen.
    pub fn settings_open(&self) -> bool {
        self.settings.is_some()
    }

    /// Borrow the open settings list.
    pub fn settings(&self) -> Option<&SettingsList> {
        self.settings.as_ref()
    }

    /// Mutably borrow the open settings list — the driver uses it to
    /// refresh a value it changed outside the modal.
    pub fn settings_mut(&mut self) -> Option<&mut SettingsList> {
        self.settings.as_mut()
    }

    /// Take the value the list asked to persist, if any.
    ///
    /// Returns `(id, value)` — upstream's `onChange(id, newValue)`.
    pub fn take_pending_setting_change(&mut self) -> Option<(String, String)> {
        self.pending_setting_change.take()
    }

    /// Take the item id the list asked the driver to open, if any.
    ///
    /// Returns the id of an item without `values` that was confirmed —
    /// upstream's `submenu` hook.
    pub fn take_pending_setting_activation(&mut self) -> Option<String> {
        self.pending_setting_activation.take()
    }

    /// Route one key while the settings modal is open.
    fn step_settings(&mut self, key: Key) -> StepOutcome {
        let Some(list) = self.settings.as_mut() else {
            return StepOutcome::Idle;
        };
        match list.handle_key(key) {
            SettingsAction::None => StepOutcome::Idle,
            SettingsAction::Changed => StepOutcome::Redraw,
            SettingsAction::ValueChanged { id, value } => {
                self.pending_setting_change = Some((id, value));
                StepOutcome::Redraw
            }
            SettingsAction::Activated(id) => {
                self.pending_setting_activation = Some(id);
                StepOutcome::Redraw
            }
            SettingsAction::Cancelled => {
                self.settings = None;
                StepOutcome::Redraw
            }
        }
    }

    /// Route a wheel event to the open settings list. Returns `None` when
    /// no settings modal is open, so the caller can fall through to the
    /// transcript.
    fn step_settings_wheel(&mut self, up: bool, alt: bool) -> Option<StepOutcome> {
        let list = self.settings.as_mut()?;
        let lines = WHEEL_SCROLL_LINES * if alt { ALT_WHEEL_SCROLL_MULTIPLIER } else { 1 };
        let delta = if up { -(lines as i32) } else { lines as i32 };
        Some(match list.scroll_by(delta) {
            SettingsAction::Changed => StepOutcome::Redraw,
            _ => StepOutcome::Idle,
        })
    }

    /// Process a single [`InputEvent`]. Returns the outcome so the
    /// caller can decide whether to redraw.
    pub fn step(&mut self, event: InputEvent) -> StepOutcome {
        if self.exit_requested {
            return StepOutcome::Exit;
        }
        // Mouse gestures are routed by rectangle rather than through the
        // keyboard's modal guard: `step_mouse_gesture` hit-tests the open
        // modal overlays first and only reaches the chat log with no modal up.
        if let InputEvent::MouseGesture(gesture) = event {
            return self.step_mouse_gesture(gesture);
        }
        // An open modal owns the keyboard: the selector and prompt stay
        // frozen underneath it.
        if self.dialog.is_some() {
            let InputEvent::Key(key) = event else {
                return StepOutcome::Idle;
            };
            return self.step_dialog(key);
        }
        // A settings modal owns both the keyboard and the wheel: it is the
        // only interactive surface on screen while it is open.
        if self.settings.is_some() {
            if let InputEvent::Mouse { up, alt } = event {
                if let Some(outcome) = self.step_settings_wheel(up, alt) {
                    return outcome;
                }
            }
            let InputEvent::Key(key) = event else {
                return StepOutcome::Idle;
            };
            return self.step_settings(key);
        }
        // Selector gets first dibs on keys when it is open.
        if let Some(selector) = self.selector.as_mut() {
            let InputEvent::Key(key) = event else {
                return StepOutcome::Idle;
            };
            match selector.handle_key(key) {
                SelectorAction::None => return StepOutcome::Idle,
                SelectorAction::Changed => return StepOutcome::Redraw,
                SelectorAction::Selected(_) | SelectorAction::Cancelled => {
                    return StepOutcome::Redraw;
                }
            }
        }
        let InputEvent::Key(key) = event else {
            if let InputEvent::Mouse { up, alt } = event {
                let lines = WHEEL_SCROLL_LINES * if alt { ALT_WHEEL_SCROLL_MULTIPLIER } else { 1 };
                let changed = if up {
                    self.scroll_viewport_up(lines)
                } else {
                    self.scroll_viewport_down(lines)
                };
                return if changed {
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                };
            }
            return StepOutcome::Idle;
        };
        self.step_key(key)
    }

    /// Process a single [`Key`]. Public so tests can step the App
    /// with explicit keys.
    pub fn step_key(&mut self, key: Key) -> StepOutcome {
        // A modal dialog swallows every key — including Ctrl+C / Esc,
        // which cancel the dialog instead of the turn or the App.
        if self.dialog.is_some() {
            return self.step_dialog(key);
        }
        // The settings modal is the next-outermost layer.
        if self.settings.is_some() {
            return self.step_settings(key);
        }
        // The transcript search overlay owns the keyboard while it is open,
        // except for the chords the viewport keeps for itself
        // (`shouldDeferViewportInputToOverlay`,
        // `packages/tui/src/tui-alt-screen.ts:644-645`).
        if self.search.is_some() {
            match self.step_search_key(key) {
                SearchKeyOutcome::Handled(outcome) => return outcome,
                SearchKeyOutcome::PassThrough => {}
            }
        }
        // `tui.altScreen.search` opens the overlay; while it is open the
        // overlay itself consumes the chord above (upstream checks the chord
        // before it checks whether the overlay holds focus,
        // `packages/tui/src/tui-alt-screen.ts:705-708`).
        if get_keybindings().matches(&InputEvent::Key(key), "tui.altScreen.search") {
            return if self.open_search() {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            };
        }
        // Global keys first.
        match key {
            // Esc cancels the in-flight turn, otherwise dismisses
            // selectors (handled above) or is a no-op.
            Key {
                code: KeyCode::Esc,
                modifiers,
            } if modifiers.is_empty() && self.is_busy() => {
                self.cancel();
                return StepOutcome::Redraw;
            }
            // Global Ctrl+C.
            Key {
                code: KeyCode::Char('c'),
                modifiers,
            } if modifiers == KeyModifiers::CONTROL => {
                if self.is_busy() {
                    self.cancel();
                    return StepOutcome::Redraw;
                }
                self.exit_requested = true;
                return StepOutcome::Exit;
            }
            // Global Ctrl+L clears the screen.
            Key {
                code: KeyCode::Char('l'),
                modifiers,
            } if modifiers == KeyModifiers::CONTROL => {
                self.messages.clear();
                // The selected line indices point into the transcript that
                // just disappeared.
                self.clear_selection();
                return StepOutcome::Redraw;
            }
            // Fullscreen chat-log scrolling. Upstream deliberately shadows
            // the bare editor bindings for these chords in fullscreen mode
            // (`packages/tui/src/keybindings.ts:159-165,208-209`: "These
            // intentionally shadow the unmodified editor bindings in
            // fullscreen mode"); `Ctrl+A` / `Ctrl+E` still reach the editor
            // for start / end of line.
            Key {
                code: KeyCode::PageUp,
                modifiers,
            } if modifiers.is_empty() => {
                let page = self.message_page();
                return if self.scroll_viewport_up(page) {
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                };
            }
            Key {
                code: KeyCode::PageDown,
                modifiers,
            } if modifiers.is_empty() => {
                let page = self.message_page();
                return if self.scroll_viewport_down(page) {
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                };
            }
            // `tui.altScreen.top` / `tui.altScreen.bottom`.
            Key {
                code: KeyCode::Home,
                modifiers,
            } if modifiers.is_empty() => {
                return if self.scroll_viewport_to_top() {
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                };
            }
            Key {
                code: KeyCode::End,
                modifiers,
            } if modifiers.is_empty() => {
                return if self.scroll_viewport_to_bottom() {
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                };
            }
            _ => {}
        }

        match self.prompt.handle_key(key) {
            PromptAction::None => StepOutcome::Idle,
            PromptAction::Changed => StepOutcome::Redraw,
            PromptAction::Submit(text) => {
                // Caller is responsible for invoking `submit` with an
                // `Arc<AsyncMutex<Agent>>` — we just announce the
                // submitted text and clear the buffer.
                let submitted = text.clone();
                self.prompt.clear();
                StepOutcome::Submitted(submitted)
            }
            PromptAction::Interrupt => {
                self.cancel();
                StepOutcome::Redraw
            }
            PromptAction::Eof => {
                self.exit_requested = true;
                StepOutcome::Exit
            }
        }
    }

    /// Route a key to the open dialog.
    fn step_dialog(&mut self, key: Key) -> StepOutcome {
        let Some(dialog) = self.dialog.as_mut() else {
            return StepOutcome::Idle;
        };
        match dialog.handle_key(key) {
            DialogAction::None => StepOutcome::Idle,
            DialogAction::Changed => StepOutcome::Redraw,
            // The answer already travelled to the host over the
            // dialog's reply channel; the modal just closes.
            DialogAction::Resolved(_) => {
                self.dialog = None;
                StepOutcome::Redraw
            }
        }
    }

    /// Mark the App for exit (e.g. after `/exit`).
    pub fn request_exit(&mut self) {
        self.exit_requested = true;
    }

    // -----------------------------------------------------------------
    // Transcript search (upstream `AltScreenSearch*`,
    // `packages/tui/src/tui-alt-screen.ts:496-660`)
    // -----------------------------------------------------------------

    /// Whether the search overlay is open.
    pub fn search_open(&self) -> bool {
        self.search.is_some()
    }

    /// The current search query, or `None` while the overlay is closed.
    pub fn search_query(&self) -> Option<&str> {
        self.search.as_ref().map(|state| state.bar.query())
    }

    /// The matches from the last refresh (empty while closed).
    pub fn search_matches(&self) -> &[SearchMatch] {
        self.search
            .as_ref()
            .map(|state| state.matches.as_slice())
            .unwrap_or(&[])
    }

    /// Index of the selected match, or `None`.
    pub fn search_match_index(&self) -> Option<usize> {
        self.search.as_ref().and_then(|state| state.selected_index)
    }

    /// Borrow the query bar (for tests).
    pub fn search_bar(&self) -> Option<&SearchBar> {
        self.search.as_ref().map(|state| &state.bar)
    }

    /// Open the search overlay, anchored to the current viewport row.
    ///
    /// Returns true when the state changed (i.e. it was closed). Mirrors
    /// upstream's `toggleSearch` first half
    /// (`packages/tui/src/tui-alt-screen.ts:496-521`).
    pub fn open_search(&mut self) -> bool {
        if self.search.is_some() {
            return false;
        }
        let anchor = self.viewport_skip();
        self.search = Some(SearchState::new(anchor));
        true
    }

    /// Close the search overlay. Returns true when it was open
    /// (`closeSearch`, `:523-529`).
    pub fn close_search(&mut self) -> bool {
        self.search.take().is_some()
    }

    /// Toggle the search overlay
    /// (`Ctrl+Shift+F`, `tui.altScreen.search`).
    pub fn toggle_search(&mut self) -> bool {
        if self.search.is_some() {
            self.close_search()
        } else {
            self.open_search()
        }
    }

    /// Replace the query as if the reader had typed it, refreshing the
    /// matches. Opens the overlay when it is closed.
    pub fn set_search_query(&mut self, query: impl Into<String>) -> bool {
        if self.search.is_none() {
            self.open_search();
        }
        let query = query.into();
        let Some(state) = self.search.as_mut() else {
            return false;
        };
        if state.bar.query() == query {
            return false;
        }
        state.bar.set_query(query);
        self.search_query_changed();
        true
    }

    /// Select the next (`1`) or previous (`-1`) match
    /// (`navigateSearch`, `:535-540`).
    pub fn navigate_search(&mut self, direction: i8) -> bool {
        let Some(state) = self.search.as_mut() else {
            return false;
        };
        // Upstream ignores navigation while the query is empty.
        if state.bar.query().trim().is_empty() || state.matches.is_empty() {
            return false;
        }
        state.selection_mode = if direction < 0 {
            SearchSelectionMode::Previous
        } else {
            SearchSelectionMode::Next
        };
        let before = self.search_match_index();
        let revealed = self.refresh_search();
        // A step that lands on a match already on screen still changed the
        // selection, so it still needs a redraw.
        revealed || self.search_match_index() != before
    }

    /// Route a key to the open search overlay.
    fn step_search_key(&mut self, key: Key) -> SearchKeyOutcome {
        let bindings = get_keybindings();
        let event = InputEvent::Key(key);
        if bindings.matches(&event, "tui.altScreen.searchClose")
            || bindings.matches(&event, "tui.altScreen.search")
        {
            return SearchKeyOutcome::Handled(if self.close_search() {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            });
        }
        if bindings.matches(&event, "tui.altScreen.searchNext") {
            return SearchKeyOutcome::Handled(if self.navigate_search(1) {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            });
        }
        if bindings.matches(&event, "tui.altScreen.searchPrevious") {
            return SearchKeyOutcome::Handled(if self.navigate_search(-1) {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            });
        }

        // The viewport scroll chords and the process-global keys keep
        // working while the bar has focus.
        match key {
            Key {
                code: KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End,
                modifiers,
            } if modifiers.is_empty() => return SearchKeyOutcome::PassThrough,
            Key {
                code: KeyCode::Char('c' | 'l'),
                modifiers,
            } if modifiers == KeyModifiers::CONTROL => return SearchKeyOutcome::PassThrough,
            _ => {}
        }

        let Some(state) = self.search.as_mut() else {
            return SearchKeyOutcome::Handled(StepOutcome::Idle);
        };
        if apply_query_key(&mut state.bar, key) {
            self.search_query_changed();
            SearchKeyOutcome::Handled(StepOutcome::Redraw)
        } else {
            SearchKeyOutcome::Handled(StepOutcome::Idle)
        }
    }

    /// Re-run the search against the current transcript and re-select the
    /// match, revealing it when the selection mode asked for a jump.
    ///
    /// The Rust port of `refreshSearch`
    /// (`packages/tui/src/tui-alt-screen.ts:570-641`), including its choice to
    /// do nothing while the query is blank and to keep the selection stable
    /// across a re-index by [`SearchMatch::key`].
    pub fn refresh_search(&mut self) -> bool {
        let (width, height) = self.viewport();
        let Some(state) = self.search.as_mut() else {
            return false;
        };
        let query = state.bar.query().to_string();
        let lines: Vec<String> = if width == 0 || height == 0 {
            Vec::new()
        } else {
            self.messages
                .render_styled_lines(width)
                .iter()
                .map(|line| plain_text(line))
                .collect()
        };

        if query.trim().is_empty() || lines.is_empty() {
            state.matches.clear();
            state.selected_index = None;
            state.selected_key = None;
            state.selection_mode = SearchSelectionMode::Retain;
            state.bar.set_result(-1, 0);
            return false;
        }

        let should_reveal = state.selection_mode != SearchSelectionMode::Retain;
        let result = state.index.search(&lines, &query);
        let changed = result.changed;
        let mode = state.selection_mode;
        let anchor_row = state.anchor_row;
        let previous_index = state.selected_index;
        let previous_key = state.selected_key.clone();
        state.matches = result.matches;

        if !changed && mode == SearchSelectionMode::Retain {
            return false;
        }

        let len = state.matches.len();
        let exact = if changed {
            previous_key
                .as_ref()
                .and_then(|key| state.matches.iter().position(|m| &m.key() == key))
        } else {
            previous_index.filter(|index| *index < len)
        };
        let selected = if len == 0 {
            None
        } else {
            Some(match mode {
                SearchSelectionMode::Query => state
                    .matches
                    .iter()
                    .position(|m| m.first_row().is_some_and(|row| row >= anchor_row))
                    .unwrap_or(0),
                SearchSelectionMode::Next => {
                    let base = search_base_index(exact, previous_index, len);
                    if base < 0 {
                        0
                    } else {
                        ((base + 1) % len as i64) as usize
                    }
                }
                SearchSelectionMode::Previous => {
                    let base = search_base_index(exact, previous_index, len);
                    if base < 0 {
                        len - 1
                    } else {
                        ((base - 1 + len as i64) % len as i64) as usize
                    }
                }
                SearchSelectionMode::Retain => exact
                    .or_else(|| previous_index.map(|index| index.min(len - 1)))
                    .unwrap_or(0),
            })
        };

        state.selected_index = selected;
        state.selected_key = selected.and_then(|index| state.matches.get(index).map(|m| m.key()));
        state.selection_mode = SearchSelectionMode::Retain;
        state
            .bar
            .set_result(selected.map(|i| i as i64).unwrap_or(-1), len);

        if !should_reveal {
            return false;
        }
        self.search_reveal()
    }

    /// The query changed: re-anchor the next selection under the current one
    /// and switch to the `Query` mode (`updateSearchQuery`, `:531-543`).
    fn search_query_changed(&mut self) {
        let anchor = self.search_anchor_row();
        let Some(state) = self.search.as_mut() else {
            return;
        };
        state.anchor_row = anchor;
        state.selection_mode = SearchSelectionMode::Query;
        state.bar.set_result(-1, 0);
        let _ = self.refresh_search();
    }

    /// Row the `Query` selection mode anchors to: the selected match's first
    /// row, else the viewport top.
    fn search_anchor_row(&self) -> usize {
        if let Some(state) = self.search.as_ref() {
            if let Some(row) = state
                .selected_index
                .and_then(|index| state.matches.get(index))
                .and_then(|m| m.first_row())
            {
                return row;
            }
        }
        self.viewport_skip()
    }

    /// First rendered row of the viewport the App last drew.
    fn viewport_skip(&self) -> usize {
        let (width, height) = self.viewport();
        if width == 0 || height == 0 {
            return 0;
        }
        self.messages.visible_lines(width, height).0
    }

    /// Scroll the selected match into view, a third of a page below the top
    /// edge (`refreshSearch`'s reveal half, `:627-641`).
    fn search_reveal(&mut self) -> bool {
        let (width, height) = self.viewport();
        if width == 0 || height == 0 {
            return false;
        }
        let Some(state) = self.search.as_ref() else {
            return false;
        };
        let (Some(first), Some(last)) = (
            state
                .selected_index
                .and_then(|index| state.matches.get(index))
                .and_then(|m| m.first_row()),
            state
                .selected_index
                .and_then(|index| state.matches.get(index))
                .and_then(|m| m.last_row()),
        ) else {
            return false;
        };
        let (visible_start, lines) = self.messages.visible_lines(width, height);
        let visible_end = visible_start + lines.len();
        if first >= visible_start && last < visible_end {
            return false;
        }
        let page = height as usize;
        let max = self.messages.line_count(width).saturating_sub(page);
        let target = first.saturating_sub(page / 3).min(max);
        self.messages.set_scroll_from_bottom(max - target);
        true
    }

    /// Paint the search matches into the already-rendered message area.
    ///
    /// Non-current matches get an underline, the current one bold + reverse —
    /// upstream's `searchMatchStyle` / `searchCurrentMatchStyle`
    /// (`packages/tui/src/tui-alt-screen.ts:118-121`). The port keeps the
    /// themed foreground already in the cell instead of replacing it, so a
    /// highlighted token stays readable in any theme.
    fn apply_search_highlight(&self, area: Rect, buf: &mut Buffer) {
        let Some(state) = self.search.as_ref() else {
            return;
        };
        if state.matches.is_empty() || area.width == 0 || area.height == 0 {
            return;
        }
        let (visible_start, lines) = self.messages.visible_lines(area.width, area.height);
        let visible_end = visible_start + lines.len();
        for (index, search_match) in state.matches.iter().enumerate() {
            let modifier = if Some(index) == state.selected_index {
                Modifier::BOLD | Modifier::REVERSED
            } else {
                Modifier::UNDERLINED
            };
            for segment in &search_match.segments {
                if segment.row < visible_start || segment.row >= visible_end {
                    continue;
                }
                let row = segment.row - visible_start;
                let text = plain_text(&lines[row]);
                let len = text.chars().count();
                let to = segment.end_col.min(len);
                let from = segment.start_col.min(to);
                let y = area.y + row as u16;
                for col in from..to {
                    let x = area.x + col as u16;
                    if x >= area.x + area.width {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.modifier |= modifier;
                    }
                }
            }
        }
    }

    /// Give an open search overlay the mouse first, exactly like the modal
    /// overlays: a gesture inside the bar's rectangle is consumed by the bar
    /// (hover clears, a press on a navigation button navigates) and never
    /// falls through to the chat-log selection underneath
    /// (`getSearchNavigationDirectionAt` / `handleSearchMouseEvent`,
    /// `packages/tui/src/tui-alt-screen.ts:541-568`).
    ///
    /// Returns `None` when the overlay is closed, has no rectangle yet, or the
    /// gesture landed outside it, so the caller keeps routing normally.
    fn step_search_mouse_gesture(&mut self, gesture: &MouseGesture) -> Option<StepOutcome> {
        self.search.as_ref()?;
        let (width, height) = self.viewport();
        let (origin_x, origin_y) = self.viewport_origin();
        if width == 0 || height == 0 {
            return None;
        }
        let rect = search_bar_rect(Rect::new(origin_x, origin_y, width, height))?;
        let inside = gesture.x >= rect.x
            && gesture.x < rect.x + rect.width
            && gesture.y >= rect.y
            && gesture.y < rect.y + rect.height;
        let (row, col) = (
            gesture.y as i64 - rect.y as i64,
            gesture.x as i64 - rect.x as i64,
        );
        let direction = if inside {
            self.search
                .as_ref()
                .and_then(|state| state.bar.navigation_direction_at(rect.width, row, col))
        } else {
            None
        };
        let changed = self
            .search
            .as_mut()
            .map(|state| state.bar.set_hovered(direction))
            .unwrap_or(false);
        if !inside {
            // The pointer left the bar: clear any stale hover and let the
            // gesture reach the chat log.
            return if changed {
                Some(StepOutcome::Redraw)
            } else {
                None
            };
        }
        if let (Some(direction), true) = (
            direction,
            matches!(gesture.kind, MouseGestureKind::Press(MouseButton::Left)),
        ) {
            if self.navigate_search(direction) {
                return Some(StepOutcome::Redraw);
            }
        }
        Some(if changed {
            StepOutcome::Redraw
        } else {
            StepOutcome::Idle
        })
    }

    /// Append a free-form info line to the message view (used by
    /// slash commands to print help / errors).
    pub fn info(&mut self, text: impl Into<String>) {
        self.messages.push_info(text);
    }

    /// Size of the message viewport as of the last render — `(0, 0)`
    /// before the first one.
    pub fn viewport(&self) -> (u16, u16) {
        (
            self.viewport_width.load(Ordering::Relaxed),
            self.viewport_height.load(Ordering::Relaxed),
        )
    }

    /// Top-left cell of the message viewport as of the last render. Pointer
    /// coordinates are absolute, so this is what maps them back in.
    pub fn viewport_origin(&self) -> (u16, u16) {
        (
            self.viewport_origin.0.load(Ordering::Relaxed),
            self.viewport_origin.1.load(Ordering::Relaxed),
        )
    }

    /// Feed a non-wheel mouse gesture to the App: start, extend, finish or
    /// clear the chat-log text selection.
    ///
    /// Mirrors upstream's `handleSelectionMouseEvent`
    /// (`packages/tui/src/tui-alt-screen.ts:1300-1380`): a press anchors a
    /// selection whose granularity comes from the click count (single →
    /// character, double → word, triple → line), a drag extends it under
    /// that same granularity, and a release copies it when
    /// [copy-on-select](AppConfig::copy_on_select) is on. A press and
    /// release on the same cell leaves an empty selection, so a plain
    /// click clears whatever was selected before.
    ///
    /// While a drag is in flight and the pointer rests on the viewport's
    /// top or bottom row, the gesture also arms the drag autoscroll; see
    /// [`App::advance_selection_autoscroll`] for the beat.
    ///
    /// With a modal on screen the gesture is hit-tested against the modal
    /// overlays instead — see [`App::mouse_regions`] and
    /// [`App::step_modal_mouse_gesture`].
    ///
    /// Deviations (deliberate, documented in the module docs): the click
    /// count is reconstructed from press timing because `crossterm` has no
    /// `clickCount` field; gestures that press outside the message viewport
    /// (the prompt and status rows) are ignored, though an in-flight drag is
    /// clamped back into the viewport so edge autoscroll keeps tracking.
    pub fn step_mouse_gesture(&mut self, gesture: MouseGesture) -> StepOutcome {
        // A modal owns the mouse exactly as it owns the keyboard
        // (`step_key`): gestures are hit-tested against the open overlays'
        // rectangles and never reach the chat log underneath.
        if self.dialog.is_some() || self.settings.is_some() || self.selector.is_some() {
            return self.step_modal_mouse_gesture(gesture);
        }
        // The search bar is an overlay too, but it lives alongside the chat
        // log instead of over a modal, so it gets the same first pass.
        if let Some(outcome) = self.step_search_mouse_gesture(&gesture) {
            return outcome;
        }
        // No modal is up, so a modal click cannot still be pending.
        self.modal_mouse_press = None;
        match gesture.kind {
            MouseGestureKind::Press(MouseButton::Left) => {
                self.stop_selection_autoscroll();
                let Some(point) = self.selection_point(gesture.x, gesture.y) else {
                    return StepOutcome::Idle;
                };
                // The click key is always the word under the pointer, even
                // when the count resolves to a line selection (upstream
                // `getClickCount(anchor, word)`,
                // `packages/tui/src/tui-alt-screen.ts:1364-1367`).
                let word = self.word_selection(point);
                let click_count = self.next_click_count(point, word);
                let next = match click_count {
                    2 => word.map(|(start, end)| {
                        Selection::range(start, end, SelectionGranularity::Word)
                    }),
                    3 => self.line_selection(point).map(|(start, end)| {
                        Selection::range(start, end, SelectionGranularity::Line)
                    }),
                    _ => Some(Selection::at(point)),
                };
                let Some(next) = next else {
                    return StepOutcome::Idle;
                };
                self.selection_dragging = true;
                if self.selection == Some(next) {
                    return StepOutcome::Idle;
                }
                self.selection = Some(next);
                StepOutcome::Redraw
            }
            MouseGestureKind::Drag(MouseButton::Left) | MouseGestureKind::Move
                if self.selection_dragging =>
            {
                // A drag is not a click: upstream clears `lastClick`
                // (`tui-alt-screen.ts:1341`), so a click-drag-click is not a
                // double click.
                self.last_click = None;
                // A drag can leave the viewport (onto the prompt / status
                // rows or past the top); it is clamped back in so the focus
                // still tracks, exactly like upstream's
                // `getScrollSelectionPoint`.
                let changed = self
                    .selection_point_clamped(gesture.x, gesture.y)
                    .map(|point| self.extend_selection(point) == StepOutcome::Redraw)
                    .unwrap_or(false);
                let armed = self.update_selection_autoscroll(gesture.x, gesture.y);
                if changed || armed {
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                }
            }
            MouseGestureKind::Release(MouseButton::Left) => {
                self.selection_dragging = false;
                self.stop_selection_autoscroll();
                let changed = self
                    .selection_point(gesture.x, gesture.y)
                    .map(|point| self.extend_selection(point) == StepOutcome::Redraw)
                    .unwrap_or(false);
                if self.config.copy_on_select {
                    if let Some(text) = self.selection_text() {
                        self.pending_clipboard = Some(text);
                    }
                }
                if changed {
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                }
            }
            // Middle / right buttons and bare moves with no button held
            // are not part of the selection gesture (upstream gives the
            // right button to paste on Windows only).
            _ => StepOutcome::Idle,
        }
    }

    /// Advance the pending edge autoscroll by one beat, returning whether
    /// the viewport moved.
    ///
    /// Upstream runs this from a 50 ms `setInterval`
    /// (`packages/tui/src/tui-alt-screen.ts:1292-1297`); this port has no
    /// timer thread, so the beat is the draw itself —
    /// [`App::render_to_buffer`] calls this once per frame, and the driver's
    /// render loop is already throttled to 50 ms. A drag parked on an edge
    /// therefore scrolls one line per redraw, and a stationary pointer keeps
    /// moving because the driver keeps drawing.
    ///
    /// Stops when the viewport cannot move any further in the requested
    /// direction (upstream's `remaining === direction`), which also means a
    /// drag at the tail's bottom edge does not spin.
    pub fn advance_selection_autoscroll(&mut self) -> bool {
        let Some((x, y)) = self.selection_autoscroll_pointer else {
            return false;
        };
        let direction = self.selection_autoscroll_direction;
        if direction == 0 {
            return false;
        }
        let moved = if direction < 0 {
            self.scroll_viewport_up(SELECTION_AUTOSCROLL_LINES)
        } else {
            self.scroll_viewport_down(SELECTION_AUTOSCROLL_LINES)
        };
        if !moved {
            self.stop_selection_autoscroll();
            return false;
        }
        // The viewport moved, so the focus under the stationary pointer is a
        // different log line now — recompute it like upstream's
        // `autoScrollSelection`.
        if let Some(point) = self.selection_point_clamped(x, y) {
            let _ = self.extend_selection(point);
        }
        true
    }

    /// On-screen rectangles of the open modal overlays, topmost first.
    ///
    /// The Rust counterpart of upstream's `renderedOverlayLayouts`
    /// (`packages/tui/src/tui.ts:492,824-847`): each region is the box the
    /// modal occupied in the last render — the dialog starts at the top of
    /// the message area, the settings list and the selector one row below it
    /// (`App::render_to_buffer`) — in absolute terminal cells. The order is
    /// the keyboard focus order in `step_key` (dialog → settings → selector),
    /// so the topmost modal owns a click when overlays overlap. Empty before
    /// the first render, when the geometry is not known yet.
    pub fn mouse_regions(&self) -> Vec<MouseRegion> {
        let (width, height) = self.viewport();
        if width == 0 || height == 0 {
            return Vec::new();
        }
        let (origin_x, origin_y) = self.viewport_origin();
        let mut overlays: Vec<(u16, usize)> = Vec::new();
        if let Some(dialog) = &self.dialog {
            overlays.push((0, dialog.render_lines(width).len()));
        }
        if let Some(settings) = &self.settings {
            overlays.push((1, settings.render_lines(width).len()));
        }
        if let Some(selector) = &self.selector {
            overlays.push((1, selector.render_styled_lines(width).len()));
        }
        let mut regions = Vec::new();
        for (top, lines) in overlays {
            // The renderer clips an overlay to the message viewport, so the
            // status / prompt rows can never be part of its rectangle.
            let rows = u16::try_from(lines).unwrap_or(u16::MAX);
            let rows = rows.min(height.saturating_sub(top));
            if rows > 0 {
                regions.push(MouseRegion::new(Rect::new(
                    origin_x,
                    origin_y + top,
                    width,
                    rows,
                )));
            }
        }
        regions
    }

    /// Route a gesture to the open modal overlays.
    ///
    /// Upstream dispatches to the topmost overlay under the pointer and only
    /// falls back to the layout underneath when no overlay rectangle contains
    /// the event (`packages/tui/src/tui.ts:824-847`,
    /// `tui-alt-screen.ts:912-922`). This port swallows the gesture either
    /// way: LUM-1124 already made an open modal own the mouse
    /// (`tests/mouse_selection.rs::an_open_modal_swallows_gestures`) and the
    /// chat log is not a layout component here, but a hit on a modal's
    /// rectangle is a click *on* that modal.
    ///
    /// A click only counts when the left press and the left release land on
    /// the same cell — upstream's `isClick`
    /// (`packages/tui/src/tui-alt-screen.ts:1312-1315`) — and committing one
    /// drops the chat-log selection, like upstream's click path
    /// (`packages/tui/src/tui-alt-screen.ts:1330-1339`). A press on its own
    /// does *not* clear: upstream clears once a handled press starts a
    /// component gesture (`tui-alt-screen.ts:925-930`), but this port has no
    /// press-capture target yet, and clearing on press would throw the
    /// selection away even when the pointer is dragged off the modal before
    /// releasing. A hit click never queues a clipboard request — copy-on-select
    /// belongs to the chat log, not to a modal.
    fn step_modal_mouse_gesture(&mut self, gesture: MouseGesture) -> StepOutcome {
        let hit = self
            .mouse_regions()
            .into_iter()
            .find_map(|region| region.capture(gesture));
        let Some(point) = hit else {
            // Outside every overlay rectangle the modal still swallows the
            // gesture; a press that missed cannot become a click.
            if matches!(gesture.kind, MouseGestureKind::Release(MouseButton::Left)) {
                self.modal_mouse_press = None;
            }
            return StepOutcome::Idle;
        };
        match gesture.kind {
            MouseGestureKind::Press(MouseButton::Left) => {
                self.modal_mouse_press = Some(point);
                StepOutcome::Idle
            }
            MouseGestureKind::Release(MouseButton::Left) => {
                let clicked = self.modal_mouse_press.take() == Some(point);
                if clicked && self.has_selection() {
                    self.clear_selection();
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                }
            }
            // Drags inside a modal, bare moves and the other buttons are
            // consumed without changing anything.
            _ => StepOutcome::Idle,
        }
    }

    /// Move the selection focus, returning whether the rendered highlight
    /// actually changed.
    ///
    /// For character selections the focus is the pointer cell. For word /
    /// line selections the pointer is first resolved to the same granularity
    /// range, then the selection is rebuilt against the range captured on
    /// press — upstream's `updateSelectionFocus`
    /// (`packages/tui/src/tui-alt-screen.ts:1200-1218`).
    fn extend_selection(&mut self, point: (usize, usize)) -> StepOutcome {
        let Some(selection) = self.selection else {
            return StepOutcome::Idle;
        };
        let Some(updated) = self.updated_selection(selection, point) else {
            return StepOutcome::Idle;
        };
        if updated == selection {
            return StepOutcome::Idle;
        }
        self.selection = Some(updated);
        StepOutcome::Redraw
    }

    /// Recompute `selection` with its focus dragged to `point`, or `None`
    /// when the granularity range under the pointer cannot be resolved
    /// (upstream leaves the focus untouched in that case).
    fn updated_selection(
        &self,
        mut selection: Selection,
        point: (usize, usize),
    ) -> Option<Selection> {
        let Some(initial) = selection.initial else {
            selection.focus = SelectionPoint::cell(point.0, point.1);
            return Some(selection);
        };
        let (start, end) = match selection.granularity {
            SelectionGranularity::Word => self.word_selection(point)?,
            SelectionGranularity::Line => self.line_selection(point)?,
            SelectionGranularity::Character => {
                selection.focus = SelectionPoint::cell(point.0, point.1);
                return Some(selection);
            }
        };
        // A target before the initial range swaps the ends, so dragging up
        // past the anchor still selects from the new range to the anchor.
        if start.order() < initial.0.order() {
            selection.anchor = initial.1;
            selection.focus = start;
        } else {
            selection.anchor = initial.0;
            selection.focus = end;
        }
        Some(selection)
    }

    /// The rendered text of one log line, with styles stripped — upstream's
    /// `getSelectionSourceLine` (`packages/tui/src/tui-alt-screen.ts:1145-1151`).
    fn selection_line(&self, line: usize) -> Option<String> {
        let (width, _) = self.viewport();
        if width == 0 {
            return None;
        }
        let lines = self.messages.render_styled_lines(width);
        lines.get(line).map(|line| crate::styled::plain_text(line))
    }

    /// The word range under `point`, or `None` when the line has no
    /// segment there (upstream's `getWordSelection`,
    /// `packages/tui/src/tui-alt-screen.ts:1156-1197`).
    ///
    /// A pickable segment (word-like, or one of
    /// [`TERMINAL_WORD_SELECTION_JOINERS`]) grows across adjacent pickable
    /// segments whenever a joiner sits on one side, which is what keeps
    /// `path/to/file` and `kebab-case` whole. The returned end carries an
    /// exclusive boundary column.
    fn word_selection(&self, point: (usize, usize)) -> Option<(SelectionPoint, SelectionPoint)> {
        let line = self.selection_line(point.0)?;
        let segments = word_segments(&line);
        let clicked = segments
            .iter()
            .position(|segment| point.1 >= segment.start && point.1 < segment.end)?;
        let mut start = segments[clicked].start;
        let mut end = segments[clicked].end;
        let mut index = clicked;
        while index > 0 && word_segments_can_join(&segments[index - 1], &segments[index]) {
            start = segments[index - 1].start;
            index -= 1;
        }
        let mut index = clicked;
        while index + 1 < segments.len()
            && word_segments_can_join(&segments[index], &segments[index + 1])
        {
            end = segments[index + 1].end;
            index += 1;
        }
        Some((
            SelectionPoint::cell(point.0, start),
            SelectionPoint::boundary(point.0, end),
        ))
    }

    /// The whole visible line under `point` — upstream's `getLineSelection`
    /// (`packages/tui/src/tui-alt-screen.ts:1193-1203`).
    fn line_selection(&self, point: (usize, usize)) -> Option<(SelectionPoint, SelectionPoint)> {
        let line = self.selection_line(point.0)?;
        let width = line.chars().count();
        Some((
            SelectionPoint::cell(point.0, 0),
            SelectionPoint::boundary(point.0, width),
        ))
    }

    /// Consume the previous press and return this press's click count.
    ///
    /// Upstream's `getClickCount` (`packages/tui/src/tui-alt-screen.ts:1220-1245`):
    /// a press repeats the count when it lands within
    /// [`DOUBLE_CLICK_INTERVAL`] of the previous one, on the same line and
    /// the same word column range; otherwise it restarts at `1`. The count
    /// cycles `1 → 2 → 3 → 1`. The only deviation is the time source —
    /// `std::time::Instant` rather than `Date.now()` — because `crossterm`
    /// does not report `clickCount`.
    fn next_click_count(
        &mut self,
        point: (usize, usize),
        word: Option<(SelectionPoint, SelectionPoint)>,
    ) -> usize {
        let now = Instant::now();
        let count = match (word, self.last_click) {
            (Some((start, end)), Some(previous))
                if now.duration_since(previous.at) <= DOUBLE_CLICK_INTERVAL
                    && previous.row == point.0
                    && previous.word_start == start.col
                    && previous.word_end == end.col =>
            {
                (previous.count % 3) + 1
            }
            _ => 1,
        };
        self.last_click = word.map(|(start, end)| ClickTarget {
            at: now,
            count,
            row: point.0,
            word_start: start.col,
            word_end: end.col,
        });
        count
    }

    /// Arm or disarm the edge autoscroll for a drag at `(x, y)`, returning
    /// whether the autoscroll is now pulling.
    ///
    /// Upstream's `updateSelectionAutoScroll`
    /// (`packages/tui/src/tui-alt-screen.ts:1247-1284`): a pointer at or
    /// above the viewport top pulls towards older output, at or below the
    /// bottom pulls towards the tail, and anywhere in between stops it.
    fn update_selection_autoscroll(&mut self, x: u16, y: u16) -> bool {
        let (width, height) = self.viewport();
        if width == 0 || height == 0 {
            self.stop_selection_autoscroll();
            return false;
        }
        let (_, origin_y) = self.viewport_origin();
        let top = origin_y;
        let bottom = origin_y.saturating_add(height).saturating_sub(1);
        let direction = if y <= top {
            -1
        } else if y >= bottom {
            1
        } else {
            0
        };
        if direction == 0 {
            self.stop_selection_autoscroll();
            return false;
        }
        self.selection_autoscroll_direction = direction;
        self.selection_autoscroll_pointer = Some((x, y));
        true
    }

    /// Stop the edge autoscroll, if one is pending.
    fn stop_selection_autoscroll(&mut self) {
        self.selection_autoscroll_direction = 0;
        self.selection_autoscroll_pointer = None;
    }

    /// Map an absolute terminal cell onto a rendered-log coordinate, or
    /// `None` when it falls outside the message viewport or before the
    /// first render.
    fn selection_point(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        let (width, height) = self.viewport();
        if width == 0 || height == 0 {
            return None;
        }
        let (_, origin_y) = self.viewport_origin();
        if y < origin_y || y >= origin_y.saturating_add(height) {
            return None;
        }
        self.selection_point_clamped(x, y)
    }

    /// Like [`App::selection_point`], but clamps a pointer outside the
    /// viewport back onto its nearest row.
    ///
    /// Upstream keeps tracking a drag that left the viewport and maps the
    /// pointer through `getScrollSelectionPoint`, so the focus follows a
    /// pointer resting on the status / prompt rows while the autoscroll
    /// runs.
    fn selection_point_clamped(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        let (width, height) = self.viewport();
        if width == 0 || height == 0 {
            return None;
        }
        let (origin_x, origin_y) = self.viewport_origin();
        let row = if y < origin_y {
            0
        } else {
            ((y - origin_y) as usize).min(height as usize - 1)
        };
        let (start, lines) = self.messages.visible_lines(width, height);
        // Below the last rendered line (a short log): clamp to the last
        // line so a drag past the end still selects to the end of the text.
        let row = row.min(lines.len().saturating_sub(1));
        let col = x.saturating_sub(origin_x) as usize;
        Some((start + row, col))
    }

    /// Start / end of the active selection in rendered-log coordinates, or
    /// `None` when nothing is selected.
    pub fn selection_bounds(&self) -> Option<((usize, usize), (usize, usize))> {
        let (start, end) = self.selection?.bounds()?;
        Some(((start.line, start.col), (end.line, end.col)))
    }

    /// True when there is a non-empty text selection.
    pub fn has_selection(&self) -> bool {
        self.selection_bounds().is_some()
    }

    /// The selected text, exactly as copy-on-select would hand it to the
    /// driver: one entry per rendered line, joined by `\n`, with trailing
    /// whitespace trimmed per line and the character under the focus cell
    /// included. A word / line range ends on an exclusive boundary column,
    /// so its end column is *not* widened (upstream `getActiveSelectionText`,
    /// `packages/tui/src/tui-alt-screen.ts:1412-1429`).
    pub fn selection_text(&self) -> Option<String> {
        let (start, end) = self.selection?.bounds()?;
        let (width, _) = self.viewport();
        if width == 0 {
            return None;
        }
        let lines = self.messages.render_styled_lines(width);
        if lines.is_empty() {
            return None;
        }
        let last = lines.len() - 1;
        let end_line = end.line.min(last);
        let mut out: Vec<String> = Vec::new();
        for (idx, line) in lines.iter().enumerate().take(end_line + 1).skip(start.line) {
            let text = crate::styled::plain_text(line);
            let len = text.chars().count();
            let from = if idx == start.line {
                start.col.min(len)
            } else {
                0
            };
            let to = if idx == end.line {
                selection_end_column(&end, len)
            } else {
                len
            };
            let segment: String = text
                .chars()
                .skip(from)
                .take(to.saturating_sub(from))
                .collect();
            out.push(segment.trim_end().to_string());
        }
        let text = out.join("\n");
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    /// Drop the active selection (and any highlight it produced).
    ///
    /// Mirrors upstream's `clearTextSelection`
    /// (`packages/tui/src/tui-alt-screen.ts:865-874`), which stops the
    /// autoscroll but deliberately keeps the click-count history: two clicks
    /// separated by an intervening clear still count as a double click.
    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.selection_dragging = false;
        self.stop_selection_autoscroll();
    }

    /// Take the text the driver must copy to the clipboard, if a
    /// copy-on-select release queued one.
    ///
    /// The App deliberately owns no terminal handle: the driver decides how
    /// to copy (upstream defaults to an OSC 52 write, with an injectable
    /// clipboard override — `packages/tui/src/tui-alt-screen.ts:1449-1462`).
    /// See [`crate::clipboard::osc52_sequence`] for that default.
    pub fn take_clipboard_request(&mut self) -> Option<String> {
        self.pending_clipboard.take()
    }

    /// Paint the active selection into the already-rendered message area by
    /// adding the reversed-video modifier to the selected cells.
    fn apply_selection_highlight(&self, area: Rect, buf: &mut Buffer) {
        let Some((start, end)) = self.selection.and_then(|selection| selection.bounds()) else {
            return;
        };
        if area.width == 0 || area.height == 0 {
            return;
        }
        let (visible_start, lines) = self.messages.visible_lines(area.width, area.height);
        let visible_end = visible_start + lines.len();
        let first = start.line.max(visible_start);
        let last = end.line.min(visible_end.saturating_sub(1));
        if first > last {
            return;
        }
        for line_idx in first..=last {
            let row = (line_idx - visible_start) as u16;
            let y = area.y + row;
            let text = crate::styled::plain_text(&lines[row as usize]);
            let len = text.chars().count();
            let from = if line_idx == start.line {
                start.col.min(len)
            } else {
                0
            };
            let to = if line_idx == end.line {
                selection_end_column(&end, len)
            } else {
                len
            };
            for col in from..to {
                let x = area.x + col as u16;
                if x >= area.x + area.width {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.modifier |= Modifier::REVERSED;
                }
            }
        }
    }

    /// Lines per page — one message-viewport height, but never zero so a
    /// key press before the first render is still a no-op instead of a
    /// panic.
    fn message_page(&self) -> usize {
        let (_, height) = self.viewport();
        height.max(1) as usize
    }

    /// Largest valid scroll offset for the viewport the App last rendered.
    fn max_scroll(&self) -> usize {
        let (width, height) = self.viewport();
        if height == 0 {
            return 0;
        }
        self.messages
            .line_count(width)
            .saturating_sub(height as usize)
    }

    /// Current scroll offset, resolving `MessageView::scroll_to_top`'s
    /// `usize::MAX` sentinel against the last rendered viewport so it can
    /// be scrolled back down from.
    fn resolved_scroll(&self) -> usize {
        let max = self.max_scroll();
        match self.messages.scroll_offset() {
            usize::MAX => max,
            offset => offset.min(max),
        }
    }

    /// Scroll the chat log up (towards older output) by `lines`. Returns
    /// true when the viewport actually moved, so the caller can skip a
    /// redraw.
    ///
    /// Mirrors `tui.altScreen.pageUp` / `lineUp`
    /// (`packages/tui/src/keybindings.ts:160-176`) — the fullscreen TUI
    /// owns the scrollback because the alternate screen hides the
    /// terminal's own.
    pub fn scroll_viewport_up(&mut self, lines: usize) -> bool {
        let next = self
            .resolved_scroll()
            .saturating_add(lines.max(1))
            .min(self.max_scroll());
        if next == self.resolved_scroll() {
            return false;
        }
        self.messages.set_scroll_from_bottom(next);
        true
    }

    /// Scroll the chat log down (towards the tail) by `lines`. Reaching
    /// the tail re-attaches the viewport to new output. Returns true when
    /// the viewport actually moved.
    pub fn scroll_viewport_down(&mut self, lines: usize) -> bool {
        let current = self.resolved_scroll();
        if current == 0 {
            return false;
        }
        // `set_scroll_from_bottom(0)` also re-attaches to the tail.
        self.messages
            .set_scroll_from_bottom(current.saturating_sub(lines.max(1)));
        true
    }

    /// Jump the chat log to the oldest line — `tui.altScreen.top`.
    pub fn scroll_viewport_to_top(&mut self) -> bool {
        let max = self.max_scroll();
        if max == 0 || (self.resolved_scroll() == max && !self.messages.is_following()) {
            return false;
        }
        self.messages.set_scroll_from_bottom(max);
        true
    }

    /// Jump the chat log back to the tail — `tui.altScreen.bottom`. New
    /// output pins the viewport again.
    pub fn scroll_viewport_to_bottom(&mut self) -> bool {
        if self.messages.scroll_offset() == 0 {
            return false;
        }
        self.messages.scroll_to_bottom();
        true
    }

    /// Render the App into a `Buffer` at the given area.
    ///
    /// This is also the drag autoscroll's clock: a pending edge autoscroll
    /// advances one line before the frame is painted, so the highlight in
    /// the frame already reflects the moved viewport. The driver redraws on
    /// a 50 ms interval, which is the beat upstream gets from
    /// `setInterval(autoScrollSelection, 50)`
    /// (`packages/tui/src/tui-alt-screen.ts:1264-1297`). The receiver is
    /// `&mut self` for that reason; callers that only need pixels (snapshot
    /// tests, `transcript` helpers) use [`App::render_snapshot`], which does
    /// not tick.
    pub fn render_to_buffer(&mut self, area: Rect, buf: &mut Buffer) {
        let _ = self.advance_selection_autoscroll();
        // Record the geometry first so the refresh below indexes the exact
        // viewport this frame is about to paint.
        self.record_viewport(area);
        // Keep the search results in step with the transcript they indexed —
        // streaming output and `/clear` both change the corpus under an open
        // bar, which is where upstream refreshes it too (from `render`).
        let _ = self.refresh_search();
        self.render_to_buffer_impl(area, buf);
    }

    /// Remember the message viewport's geometry as of a render: the width the
    /// log wraps at, its height (the page size), and its top-left cell so
    /// pointer coordinates can be mapped back into it.
    fn record_viewport(&self, area: Rect) {
        let message_height = area.height.saturating_sub(2);
        self.viewport_width.store(area.width, Ordering::Relaxed);
        self.viewport_height
            .store(message_height, Ordering::Relaxed);
        self.viewport_origin.0.store(area.x, Ordering::Relaxed);
        self.viewport_origin.1.store(area.y, Ordering::Relaxed);
    }

    /// Paint the App without advancing the autoscroll clock.
    fn render_to_buffer_impl(&self, area: Rect, buf: &mut Buffer) {
        // Layout: message view fills the top, prompt the bottom row,
        // status bar the row above the prompt.
        let status_height = 1u16;
        let prompt_height = 1u16;
        let message_height = area.height.saturating_sub(status_height + prompt_height);
        let message_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: message_height,
        };
        let status_area = Rect {
            x: area.x,
            y: area.y + message_height,
            width: area.width,
            height: status_height,
        };
        let prompt_area = Rect {
            x: area.x,
            y: area.y + message_height + status_height,
            width: area.width,
            height: prompt_height,
        };

        // Record the viewport the scroll keys clamp against. Keys arrive
        // between renders, so the previous render's geometry is what they
        // see — exactly what the reader was looking at.
        self.record_viewport(area);

        self.messages
            .render_to_buffer_themed(message_area, buf, &self.theme);
        // Selection highlight goes on top of the message cells but under
        // any modal, so an open selector or dialog stays readable.
        self.apply_selection_highlight(message_area, buf);
        self.apply_search_highlight(message_area, buf);
        self.status_bar
            .render_to_buffer_themed(&self.status_data, status_area, buf, &self.theme);

        // Prompt line.
        let line = self.prompt.render_line(area.width);
        for (col, ch) in line.chars().enumerate() {
            let x = prompt_area.x + col as u16;
            if x >= prompt_area.x + prompt_area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, prompt_area.y)) {
                cell.set_char(ch);
            }
        }

        // Selector overlay — when open, draw on top of everything
        // except the prompt and status.
        if let Some(selector) = &self.selector {
            let lines = selector.render_styled_lines(area.width);
            let start_row = area.y + 1;
            for (offset, line) in lines.iter().enumerate() {
                let y = start_row + offset as u16;
                if y >= area.y + message_height {
                    break;
                }
                write_styled_line(buf, area.x, y, area.width, line, &self.theme);
            }
        }

        // Settings overlay — drawn after the selector so an accidental
        // overlap (both open) still leaves the `/settings` modal readable.
        if let Some(settings) = &self.settings {
            let lines = settings.render_styled_lines(area.width);
            let start_row = area.y + 1;
            for (offset, line) in lines.iter().enumerate() {
                let y = start_row + offset as u16;
                if y >= area.y + message_height {
                    break;
                }
                // Blank the row first: the settings modal is the whole
                // point of the screen while it is open.
                for col in 0..area.width {
                    if let Some(cell) = buf.cell_mut((area.x + col, y)) {
                        cell.set_char(' ');
                    }
                }
                write_styled_line(buf, area.x, y, area.width, line, &self.theme);
            }
        }

        // Extension dialog overlay — topmost, so it wins over the
        // selector if both are somehow open.
        if let Some(dialog) = &self.dialog {
            let lines = dialog.render_lines(area.width);
            let start_row = area.y;
            for (offset, line) in lines.iter().enumerate() {
                let y = start_row + offset as u16;
                if y >= area.y + message_height {
                    break;
                }
                // Blank the row first: a modal must be readable even
                // when the message view underneath is full of text.
                for col in 0..area.width {
                    if let Some(cell) = buf.cell_mut((area.x + col, y)) {
                        cell.set_char(' ');
                    }
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

        // Transcript search bar — anchored to the top-right of the message
        // area (`showOverlay(component, { anchor: "top-right", width: "40%",
        // minWidth: 32, margin: 1 })`,
        // `packages/tui/src/tui-alt-screen.ts:512-518`). Drawn last so the bar
        // stays readable if an extension dialog arrives while it is open.
        if let Some(state) = &self.search {
            if let Some(rect) = search_bar_rect(message_area) {
                let layout = render_search_bar(&state.bar, rect.width);
                for (offset, line) in layout.lines.iter().enumerate() {
                    let y = rect.y + offset as u16;
                    if y >= rect.y + rect.height {
                        break;
                    }
                    write_styled_line(buf, rect.x, y, rect.width, line, &self.theme);
                }
            }
        }
    }

    /// Render the App into a flat snapshot (used by the snapshot tests
    /// in `tests/snapshot.rs`).
    pub fn render_snapshot(&self, width: u16, height: u16) -> RenderSnapshot {
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let mut buf = Buffer::empty(area);
        self.render_to_buffer_impl(area, &mut buf);
        let lines = buf
            .content()
            .chunks(width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>();
        RenderSnapshot {
            width,
            height,
            lines,
            prompt_placeholder: self.config.prompt_placeholder.clone(),
            prompt_buffer: self.prompt.text().to_string(),
            settings_open: self.settings_open(),
            settings_lines: self
                .settings
                .as_ref()
                .map(|settings| settings.render_lines(width))
                .unwrap_or_default(),
            selector_open: self.selector_open(),
            selector_title: self.selector.as_ref().map(|s| s.title().to_string()),
            selector_items: self
                .selector
                .as_ref()
                // The rows that pass the selector's search filter (the
                // scroll window is a rendering detail and is applied by
                // `Selector::render_lines`).
                .map(|s| s.visible_items().cloned().collect())
                .unwrap_or_default(),
            selector_cursor: self.selector.as_ref().map(|s| s.cursor()),
            dialog_open: self.dialog.is_some(),
            dialog_kind: self.dialog.as_ref().map(|d| d.kind()),
            dialog_title: self.dialog.as_ref().map(|d| d.title().to_string()),
            dialog_lines: self
                .dialog
                .as_ref()
                .map(|d| d.render_lines(width))
                .unwrap_or_default(),
            search_open: self.search_open(),
            search_query: self
                .search
                .as_ref()
                .map(|state| state.bar.query().to_string())
                .unwrap_or_default(),
            search_lines: self
                .search
                .as_ref()
                .map(|state| {
                    crate::search::search_bar_text(&render_search_bar(&state.bar, width).lines)
                })
                .unwrap_or_default(),
            status: self.status_data.clone(),
        }
    }

    /// Convert a raw `crossterm` event into an [`InputEvent`]. Used by
    /// the binary entry point.
    pub fn translate_event(event: CtEvent) -> InputEvent {
        match event {
            CtEvent::Key(key) => InputEvent::from(key),
            CtEvent::Mouse(mouse) => match mouse.kind {
                CtMouseEventKind::ScrollUp => InputEvent::Mouse {
                    up: true,
                    alt: mouse.modifiers.contains(CtModifiers::ALT),
                },
                CtMouseEventKind::ScrollDown => InputEvent::Mouse {
                    up: false,
                    alt: mouse.modifiers.contains(CtModifiers::ALT),
                },
                CtMouseEventKind::Down(button) => InputEvent::MouseGesture(MouseGesture::new(
                    MouseGestureKind::Press(mouse_button(button)),
                    mouse.column,
                    mouse.row,
                    mouse.modifiers.contains(CtModifiers::ALT),
                )),
                CtMouseEventKind::Up(button) => InputEvent::MouseGesture(MouseGesture::new(
                    MouseGestureKind::Release(mouse_button(button)),
                    mouse.column,
                    mouse.row,
                    mouse.modifiers.contains(CtModifiers::ALT),
                )),
                CtMouseEventKind::Drag(button) => InputEvent::MouseGesture(MouseGesture::new(
                    MouseGestureKind::Drag(mouse_button(button)),
                    mouse.column,
                    mouse.row,
                    mouse.modifiers.contains(CtModifiers::ALT),
                )),
                CtMouseEventKind::Moved => InputEvent::MouseGesture(MouseGesture::new(
                    MouseGestureKind::Move,
                    mouse.column,
                    mouse.row,
                    mouse.modifiers.contains(CtModifiers::ALT),
                )),
                // Horizontal wheel goes through the same `routeWheel`
                // upstream ignores for the chat log: no horizontal
                // scrolling exists yet.
                _ => InputEvent::Ignored,
            },
            CtEvent::Resize(w, h) => InputEvent::Resize {
                width: w,
                height: h,
            },
            _ => InputEvent::Ignored,
        }
    }

    /// Translate a slice of `crossterm` events.
    pub fn translate_events<I: IntoIterator<Item = CtEvent>>(events: I) -> Vec<InputEvent> {
        events.into_iter().map(Self::translate_event).collect()
    }
}

// Quiet unused-import warning for the parking_lot::Mutex, which we
// keep available for downstream code that wants to wrap state behind
// the App handle.
#[allow(dead_code)]
fn _keep_mutex_path() -> Arc<Mutex<()>> {
    Arc::new(Mutex::new(()))
}

// Quiet unused-editor-action warning — the import is there for the
// public method signature contract.
#[allow(dead_code)]
const _: EditorAction = EditorAction::None;

/// Base index `next` / `previous` navigation steps from
/// (`refreshSearch`, `packages/tui/src/tui-alt-screen.ts:614-622`): the exact
/// match when the key still resolves, else the clamped previous selection,
/// else `-1` so the caller wraps from the ends.
fn search_base_index(exact: Option<usize>, previous: Option<usize>, len: usize) -> i64 {
    if let Some(index) = exact {
        return index as i64;
    }
    previous
        .map(|index| index as i64)
        .unwrap_or(-1)
        .min(len as i64 - 1)
}

/// Map a crossterm mouse button onto the component-level [`MouseButton`].
fn mouse_button(button: crossterm::event::MouseButton) -> MouseButton {
    match button {
        crossterm::event::MouseButton::Left => MouseButton::Left,
        crossterm::event::MouseButton::Middle => MouseButton::Middle,
        crossterm::event::MouseButton::Right => MouseButton::Right,
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    fn plain_segments(line: &str) -> Vec<(usize, usize, bool, bool)> {
        word_segments(line)
            .into_iter()
            .map(|segment| {
                (
                    segment.start,
                    segment.end,
                    segment.selectable,
                    segment.joiner,
                )
            })
            .collect()
    }

    #[test]
    fn word_segments_measure_character_columns() {
        assert_eq!(
            plain_segments("ab cd"),
            vec![
                (0, 2, true, false),
                (2, 3, false, false),
                (3, 5, true, false),
            ]
        );
    }

    #[test]
    fn word_segments_mark_joiners_and_word_like_runs() {
        // "/" and "-" are selectable joiners; punctuation and spaces are not.
        // UAX #29 keeps `file.rs` together (`.` is a MidNumLet), while the
        // `/` and `-` are the joiner segments.
        assert_eq!(
            plain_segments("path/to-file.rs"),
            vec![
                (0, 4, true, false),
                (4, 5, true, true),
                (5, 7, true, false),
                (7, 8, true, true),
                (8, 15, true, false),
            ]
        );
        assert_eq!(plain_segments(" "), vec![(0, 1, false, false)]);
    }

    #[test]
    fn joiners_glue_pickable_neighbours_but_not_punctuation() {
        let segments = word_segments("a-b c");
        // "a" joins "-"; "-" joins "b"; "b" and " " do not join.
        assert!(word_segments_can_join(&segments[0], &segments[1]));
        assert!(word_segments_can_join(&segments[1], &segments[2]));
        assert!(!word_segments_can_join(&segments[2], &segments[3]));
    }

    #[test]
    fn selection_end_column_is_exclusive_for_boundaries() {
        let exclusive = SelectionPoint::boundary(0, 4);
        assert_eq!(selection_end_column(&exclusive, 10), 4);
        assert_eq!(
            selection_end_column(&exclusive, 3),
            3,
            "clamped to the line"
        );

        let inclusive = SelectionPoint::cell(0, 4);
        assert_eq!(selection_end_column(&inclusive, 10), 5);
        assert_eq!(
            selection_end_column(&inclusive, 5),
            5,
            "clamped to the line"
        );
    }

    #[test]
    fn selection_bounds_ignore_the_boundary_flag_when_ordering() {
        let selection = Selection {
            anchor: SelectionPoint::boundary(3, 0),
            focus: SelectionPoint::cell(1, 5),
            granularity: SelectionGranularity::Word,
            initial: None,
        };
        let (start, end) = selection.bounds().expect("non-empty");
        assert_eq!(start.line, 1);
        assert_eq!(end.line, 3);
        // Equal coordinates read as empty even when the flags differ.
        let empty = Selection {
            anchor: SelectionPoint::cell(2, 2),
            focus: SelectionPoint::boundary(2, 2),
            granularity: SelectionGranularity::Word,
            initial: None,
        };
        assert!(empty.bounds().is_none());
    }
}
