//! Message view — renders the conversation log (user / assistant /
//! tool) on screen and folds incremental assistant updates into the
//! last in-flight assistant block.
//!
//! The component is terminal-agnostic: the [`render_lines`](MessageView::render_lines)
//! method produces a sequence of pre-wrapped text lines for a given
//! width, and the [`MessageView::render_to_buffer`] method writes the
//! lines into a `ratatui::buffer::Buffer` for snapshot tests.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};

use pi_protocol::{ToolCall, ToolResult};

use crate::styled::{
    plain_text, themed_text, write_plain_row, write_styled_line_hyperlinked, SpanStyle, StyledLine,
    StyledSpan,
};
use crate::styles::SelectListStyles;
use crate::theme::{Theme, ThemeColor};
use crate::width::{char_columns, columns, is_cjk_break};

/// Logical role — drives the visual prefix and the message-view
/// rendering. Mirrors the `user` / `assistant` / `tool` distinction the
/// TS message components make, plus [`Role::Info`] for command-reference
/// blocks that must not read as user input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// User-typed prompt.
    User,
    /// Assistant response.
    Assistant,
    /// Tool call + result block.
    Tool,
    /// System / command output block (slash-command help, `/hotkeys`).
    ///
    /// Rendered with an info prefix so the reader can tell it apart from a
    /// user prompt: before this variant existed `/help`'s body shared the
    /// composer's `> ` prefix (LUM-1238 §15.4).
    Info,
}

/// Label a collapsed thinking block renders in place of its text
/// (upstream's `hiddenThinkingLabel` default,
/// `packages/coding-agent/src/modes/interactive/components/assistant-message.ts:30`).
pub const HIDDEN_THINKING_LABEL: &str = "Thinking...";

/// Lines a collapsed tool block previews by default.
///
/// The slice follows Martty's fixed 4-line tail (`src/transcript.rs:1698`)
/// rather than upstream's `FALLBACK_PREVIEW_LINES = 10`: the interactive
/// transcript is narrow, and the goal is that one `read` or `bash` cannot
/// flood it. Override it per view with
/// [`MessageView::with_tool_preview_lines`] / [`MessageView::set_tool_preview_lines`]
/// (the driver maps a user setting onto those).
pub const TOOL_PREVIEW_LINES: usize = 4;

/// The `… (+M lines, Ctrl+O to expand)` line a collapsed tool block shows.
///
/// Kept as a free function so the renderer and any driver-side hint agree on
/// the wording. The chord is **resolved from the live keybindings**
/// (`key_hint_or`, upstream's `keyHint`), not hardcoded: a `keybindings.json`
/// override for `app.tools.expand` moves both the key that folds the block
/// and the text that advertises it, and an explicitly unbound id drops the
/// chord instead of pointing at a dead key (LUM-1447). `app.tools.expand`
/// lives in the coding-agent's `app.*` table, so the shipped default stands
/// in when only the bare `pi-tui` registry is installed — the same fallback
/// `Editor::matches_app_exit` uses for `app.exit`.
pub fn tool_fold_hint(hidden: usize) -> String {
    format!(
        "… (+{hidden} lines, {})",
        crate::keybindings::key_hint_or("app.tools.expand", "Ctrl+O", "to expand")
    )
}

/// A driver-rendered tool block: a call header plus the result body.
///
/// The split exists so folding can keep the header visible. Requirement 1 of
/// the audit slice is about the *result*: "工具结果超过 N 行时只渲染最后 N 行"
/// — dropping the call line would hide which tool ran, so [`ToolBlock::header`]
/// is never folded and only [`ToolBlock::body`] is.
pub struct ToolBlock {
    /// Call-summary lines (e.g. `bash ls .`), always shown.
    pub header: Vec<StyledLine>,
    /// Result lines; folded to the tail preview when collapsed.
    pub body: Vec<StyledLine>,
}

impl ToolBlock {
    /// A block from its two halves.
    pub fn new(header: Vec<StyledLine>, body: Vec<StyledLine>) -> Self {
        Self { header, body }
    }

    /// True when there is nothing to paint.
    pub fn is_empty(&self) -> bool {
        self.header.is_empty() && self.body.is_empty()
    }
}

impl std::fmt::Debug for ToolBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolBlock")
            .field("header", &self.header.len())
            .field("body", &self.body.len())
            .finish()
    }
}

/// Driver-supplied rich renderer for tool blocks.
///
/// The interactive driver (`pi-coding-agent`) owns the renderer family in
/// `tools/render.rs`; `pi-tui` must not depend on that crate (the dependency
/// runs the other way). So the driver implements this trait and hands the App
/// the **already styled** lines, and the App only decides how many of those
/// lines a collapsed preview keeps. That is the interface the audit calls
/// for: "App 接受已渲染的带样式的行 + 折叠预览行数".
///
/// Both halves are keyed by the provider tool-call id, exactly like the
/// renderer session the driver wraps.
pub trait ToolBlockRenderer: Send {
    /// Feed the call arguments at execution start.
    ///
    /// A stateful renderer uses this to cache derived state (the write
    /// highlight cache, the read language) before the result arrives; the
    /// returned summary lines are not shown by the live log, which keeps its
    /// own `[tool:…]` streaming header until the result lands.
    fn begin_tool(&mut self, call: &ToolCall);

    /// Render a finished result into a [`ToolBlock`], or `None` when the tool
    /// has no rich presentation (the App then falls back to the plain
    /// `[tool:…] args → result` body).
    ///
    /// `width` is the message viewport width in cells, for renderers that
    /// size a block against the terminal (inline images).
    fn finish_tool(&mut self, result: &ToolResult, width: u16) -> Option<ToolBlock>;
}

/// One entry in the rendered message log.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageItem {
    /// Role this entry represents.
    pub role: Role,
    /// Plain text body. For `Role::Tool` the body is the formatted
    /// call / result pair.
    pub text: String,
    /// Reasoning / "thinking" text streamed before the body. Empty when the
    /// provider sent none. Rendered above [`MessageItem::text`] (collapsed to
    /// [`HIDDEN_THINKING_LABEL`] when [`MessageView::hide_thinking`] is set)
    /// and only ever populated on assistant items.
    pub thinking: String,
    /// True while an assistant message is still streaming (the TUI
    /// shows a caret indicator).
    pub streaming: bool,
    /// Pre-rendered, already-styled call header for a tool block.
    ///
    /// The driver's [`ToolBlockRenderer`] supplies it alongside
    /// [`MessageItem::tool_lines`]. It is always painted (never folded) so a
    /// collapsed block still says which tool ran; `None` keeps the plain
    /// `[tool:…]` text as the whole block.
    pub tool_header: Option<Vec<StyledLine>>,
    /// Pre-rendered, already-styled result lines for a tool block.
    ///
    /// `pi-tui` cannot depend on the crate that owns the rich tool renderers
    /// (`pi-coding-agent`), so the driver renders the block and hands the
    /// lines over through [`ToolBlockRenderer`]. When present these replace
    /// the result half of [`MessageItem::text`]; `None` keeps the single-line
    /// `[tool:…]` format.
    pub tool_lines: Option<Vec<StyledLine>>,
    /// Per-block expand override.
    ///
    /// `None` follows [`MessageView::tools_expanded`]. A click on the block
    /// sets `Some(...)`; a global [`MessageView::toggle_tools_expanded`]
    /// clears every override again so one chord really does toggle all.
    pub tool_expanded: Option<bool>,
}

impl MessageItem {
    /// Convenience constructor for a user prompt.
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            text: text.into(),
            thinking: String::new(),
            streaming: false,
            tool_header: None,
            tool_lines: None,
            tool_expanded: None,
        }
    }

    /// Convenience constructor for a finalized assistant message.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            text: text.into(),
            thinking: String::new(),
            streaming: false,
            tool_header: None,
            tool_lines: None,
            tool_expanded: None,
        }
    }

    /// Convenience constructor for an in-flight assistant block (used
    /// when a `MessageStart` arrives and before `MessageEnd`).
    pub fn assistant_streaming() -> Self {
        Self {
            role: Role::Assistant,
            text: String::new(),
            thinking: String::new(),
            streaming: true,
            tool_header: None,
            tool_lines: None,
            tool_expanded: None,
        }
    }

    /// Convenience constructor for a tool execution block.
    pub fn tool(text: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            text: text.into(),
            thinking: String::new(),
            streaming: false,
            tool_header: None,
            tool_lines: None,
            tool_expanded: None,
        }
    }
}

/// One in-flight tool call, keyed by its provider call id.
///
/// Mirrors upstream's `pendingTools: Map<string, ToolExecutionComponent>`
/// (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:430`):
/// the map is what makes a streamed tool call produce *one* transcript block
/// instead of one per delta. `index` points back into the rendered item list.
#[derive(Debug, Clone)]
struct ToolStream {
    /// Index of the call's [`MessageItem`] in `MessageView::items`.
    index: usize,
    /// Tool name; empty until the first delta or execution start names it.
    name: String,
    /// JSON arguments collected so far.
    args: String,
    /// True between execution start and end.
    running: bool,
}

/// Which queue a not-yet-delivered prompt belongs to. Mirrors the
/// `streamingBehavior` upstream passes to `session.prompt` — `"steer"`
/// for input submitted with Enter while a turn streams, `"followUp"`
/// for the explicit `app.message.followUp` chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingMessageKind {
    /// Injected ahead of a follow-up (upstream `"steer"`).
    Steer,
    /// Delivered after every steering message (upstream `"followUp"`).
    FollowUp,
}

/// Label a steering prompt renders under in the queued-messages block.
/// Verbatim upstream (`interactive-mode.ts:4373`).
///
/// Upstream has no `i18n` table for this block (unlike the startup header), so
/// the Rust port keeps the literal English labels rather than inventing a
/// translation pair the reference implementation does not have.
pub const PENDING_STEER_LABEL: &str = "Steering: ";
/// Label a follow-up prompt renders under (upstream `interactive-mode.ts:4378`).
pub const PENDING_FOLLOW_UP_LABEL: &str = "Follow-up: ";
/// Lead-in glyph of the queued-messages hint row (upstream `interactive-mode.ts:4382`).
pub const PENDING_HINT_LEAD: &str = "\u{21b3} ";
/// Hint copy that follows the `app.message.dequeue` chord (upstream
/// `interactive-mode.ts:4382`).
pub const PENDING_HINT_TEXT: &str = "to edit all queued messages";

/// Conversation log rendered by the TUI. Holds an ordered list of
/// [`MessageItem`] entries and supports incremental updates so the
/// TUI redraws only the tail while the assistant streams.
#[derive(Debug)]
pub struct MessageView {
    items: Vec<MessageItem>,
    /// In-flight tool calls, keyed by call id. See [`ToolStream`].
    tool_streams: HashMap<String, ToolStream>,
    /// Scroll offset — lines from the bottom. `0` means pinned to the
    /// tail (latest message visible).
    scroll_from_bottom: usize,
    /// True once the reader scrolled away from the tail. While detached,
    /// appended items and streaming deltas must not move the viewport —
    /// otherwise scrolling back to read earlier output would be
    /// impossible during a long turn. Upstream calls this
    /// "follow the tail" and flips it in `alt-screen` scroll handling
    /// (`packages/tui/src/keybindings.ts:160-165`).
    detached: bool,
    /// Width of the most recent [`MessageView::render_styled_lines`] call.
    /// Together with `last_render_lines` it lets an append work out how many
    /// lines it added, so a detached viewport can stay anchored to the text
    /// the reader is looking at.
    last_render_width: AtomicU16,
    /// Line count of the most recent render (`0` before the first one).
    last_render_lines: AtomicUsize,
    /// When true, assistant bodies are rendered through
    /// [`crate::markdown::render_markdown`] instead of the plain-text
    /// path. Off by default so existing callers and snapshots keep their
    /// byte-identical output.
    markdown: bool,
    /// When true, markdown link labels render as OSC 8 hyperlinks and drop
    /// the inline `(url)` suffix. Off by default; the driver turns it on from
    /// [`crate::hyperlink::supports_hyperlinks`] via `AppConfig::hyperlinks`.
    hyperlinks: bool,
    /// When true, an assistant item's thinking text is collapsed to a single
    /// [`HIDDEN_THINKING_LABEL`] line instead of being shown. False by
    /// default, matching upstream's `hideThinkingBlock`
    /// (`packages/coding-agent/src/core/settings-manager.ts:962`), so reasoning
    /// is visible unless the reader hides it with `app.thinking.toggle`.
    hide_thinking: bool,
    /// Prompts the user submitted while a turn was in flight, waiting to be
    /// delivered once it ends. Upstream keeps two queues
    /// (`session.getSteeringMessages()` / `getFollowUpMessages()`) and shows
    /// them in a container above the editor
    /// (`updatePendingMessagesDisplay`, `interactive-mode.ts:4366-4385`); the
    /// port renders the same block through [`MessageView::pending_lines`] and
    /// `App::paint_pending_block`.
    pending_steering: Vec<String>,
    /// See [`MessageView::pending_steering`].
    pending_follow_up: Vec<String>,
    /// Whether tool blocks render expanded. False by default: a tool result
    /// can be thousands of lines, so the collapsed preview is the safe first
    /// impression. Toggled by `app.tools.expand` (Ctrl+O).
    tools_expanded: bool,
    /// Lines a collapsed tool block previews. See [`TOOL_PREVIEW_LINES`].
    tool_preview_lines: usize,
}

impl Default for MessageView {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            tool_streams: HashMap::new(),
            scroll_from_bottom: 0,
            detached: false,
            last_render_width: AtomicU16::new(0),
            last_render_lines: AtomicUsize::new(0),
            markdown: false,
            hyperlinks: false,
            hide_thinking: false,
            pending_steering: Vec::new(),
            pending_follow_up: Vec::new(),
            tools_expanded: false,
            tool_preview_lines: TOOL_PREVIEW_LINES,
        }
    }
}

impl Clone for MessageView {
    fn clone(&self) -> Self {
        Self {
            items: self.items.clone(),
            tool_streams: self.tool_streams.clone(),
            scroll_from_bottom: self.scroll_from_bottom,
            detached: self.detached,
            last_render_width: AtomicU16::new(self.last_render_width.load(Ordering::Relaxed)),
            last_render_lines: AtomicUsize::new(self.last_render_lines.load(Ordering::Relaxed)),
            markdown: self.markdown,
            hyperlinks: self.hyperlinks,
            hide_thinking: self.hide_thinking,
            pending_steering: self.pending_steering.clone(),
            pending_follow_up: self.pending_follow_up.clone(),
            tools_expanded: self.tools_expanded,
            tool_preview_lines: self.tool_preview_lines,
        }
    }
}

impl MessageView {
    /// Construct an empty view.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable/disable markdown rendering for assistant bodies (builder form).
    pub fn with_markdown(mut self, enabled: bool) -> Self {
        self.markdown = enabled;
        self
    }

    /// Whether assistant bodies are rendered as markdown.
    pub fn markdown(&self) -> bool {
        self.markdown
    }

    /// Enable/disable markdown rendering for assistant bodies in place.
    pub fn set_markdown(&mut self, enabled: bool) {
        self.markdown = enabled;
    }

    /// Enable/disable OSC 8 hyperlinks for markdown links (builder form).
    pub fn with_hyperlinks(mut self, enabled: bool) -> Self {
        self.hyperlinks = enabled;
        self
    }

    /// Whether markdown links render as OSC 8 hyperlinks.
    pub fn hyperlinks(&self) -> bool {
        self.hyperlinks
    }

    /// Enable/disable OSC 8 hyperlinks in place.
    pub fn set_hyperlinks(&mut self, enabled: bool) {
        self.hyperlinks = enabled;
    }

    /// Whether assistant thinking blocks are collapsed (builder form).
    pub fn with_hide_thinking(mut self, hidden: bool) -> Self {
        self.hide_thinking = hidden;
        self
    }

    /// Whether assistant thinking blocks are collapsed.
    pub fn hide_thinking(&self) -> bool {
        self.hide_thinking
    }

    /// Whether assistant thinking blocks are shown. The inverse of
    /// [`MessageView::hide_thinking`], matching the upstream boolean
    /// (`hideThinkingBlock = false` means "visible").
    pub fn thinking_visible(&self) -> bool {
        !self.hide_thinking
    }

    /// Collapse or show assistant thinking blocks in place. Takes effect on
    /// the next render; the transcript items themselves are untouched, so the
    /// text survives a collapse / expand round trip.
    pub fn set_hide_thinking(&mut self, hidden: bool) {
        self.hide_thinking = hidden;
    }

    /// Show or collapse assistant thinking blocks in place.
    pub fn set_thinking_visible(&mut self, visible: bool) {
        self.hide_thinking = !visible;
    }

    /// Whether tool blocks render expanded (builder form).
    pub fn with_tools_expanded(mut self, expanded: bool) -> Self {
        self.tools_expanded = expanded;
        self
    }

    /// Whether tool blocks render expanded when they carry no per-block
    /// override.
    pub fn tools_expanded(&self) -> bool {
        self.tools_expanded
    }

    /// Set the global expand state in place. Does not touch per-block
    /// overrides; use [`MessageView::toggle_tools_expanded`] for the chord so
    /// one press really toggles every block.
    pub fn set_tools_expanded(&mut self, expanded: bool) {
        self.tools_expanded = expanded;
    }

    /// Flip the global expand state and clear every per-block override.
    ///
    /// Clearing is what makes the chord "toggle all blocks" rather than
    /// "toggle the default for blocks nobody clicked": after it, every block
    /// follows the new global value. Returns the new global state so the
    /// driver can echo it in the status bar, matching upstream's
    /// `setToolsExpanded`.
    pub fn toggle_tools_expanded(&mut self) -> bool {
        self.tools_expanded = !self.tools_expanded;
        for item in &mut self.items {
            item.tool_expanded = None;
        }
        self.tools_expanded
    }

    /// Lines a collapsed tool block previews (builder form).
    pub fn with_tool_preview_lines(mut self, lines: usize) -> Self {
        self.tool_preview_lines = lines;
        self
    }

    /// Lines a collapsed tool block previews.
    pub fn tool_preview_lines(&self) -> usize {
        self.tool_preview_lines
    }

    /// Set the collapsed preview height in place.
    pub fn set_tool_preview_lines(&mut self, lines: usize) {
        self.tool_preview_lines = lines;
    }

    /// Flip one tool block's expand state, taking the current effective state
    /// as the starting point.
    ///
    /// Returns the block's new state, or `None` when `index` is not a tool
    /// block (an out-of-range index, a user/assistant message). This is the
    /// mouse-click path: a click toggles the block under the pointer and
    /// leaves every other block alone.
    pub fn toggle_tool_at(&mut self, index: usize) -> Option<bool> {
        let global = self.tools_expanded;
        let item = self.items.get_mut(index)?;
        if item.role != Role::Tool {
            return None;
        }
        let next = !item.tool_expanded.unwrap_or(global);
        item.tool_expanded = Some(next);
        Some(next)
    }

    /// Index of the item whose rendered block covers `line` at `width`.
    ///
    /// Uses the exact line accounting of the renderer, so the hit test and
    /// the painted transcript cannot disagree. Used by the App's mouse
    /// handling to find the tool block under a click.
    pub fn item_index_at_line(&self, line: usize, width: u16) -> Option<usize> {
        self.item_line_ranges(width)
            .iter()
            .position(|(start, end)| line >= *start && line < *end)
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
        self.repin_if_following();
    }

    /// Drop every item from the log. `/clear` uses this.
    pub fn clear(&mut self) {
        self.items.clear();
        self.tool_streams.clear();
        self.scroll_from_bottom = 0;
        self.detached = false;
        self.pending_steering.clear();
        self.pending_follow_up.clear();
    }

    /// Queue a prompt the user submitted while a turn was in flight.
    /// Nothing is ever dropped: the text is rendered as a dim
    /// `Steering:` / `Follow-up:` line until the driver delivers it.
    pub fn push_pending(&mut self, kind: PendingMessageKind, text: impl Into<String>) {
        match kind {
            PendingMessageKind::Steer => self.pending_steering.push(text.into()),
            PendingMessageKind::FollowUp => self.pending_follow_up.push(text.into()),
        }
        self.repin_if_following();
    }

    /// Number of queued (not yet delivered) prompts.
    pub fn pending_len(&self) -> usize {
        self.pending_steering.len() + self.pending_follow_up.len()
    }

    /// True when no prompt is queued.
    pub fn pending_is_empty(&self) -> bool {
        self.pending_len() == 0
    }

    /// Queued prompts in delivery order: every steering message first
    /// (FIFO), then every follow-up (FIFO). Upstream builds the same order
    /// when it restores the queues to the editor
    /// (`[...steering, ...followUp]`).
    pub fn pending(&self) -> Vec<(PendingMessageKind, &str)> {
        self.pending_steering
            .iter()
            .map(|text| (PendingMessageKind::Steer, text.as_str()))
            .chain(
                self.pending_follow_up
                    .iter()
                    .map(|text| (PendingMessageKind::FollowUp, text.as_str())),
            )
            .collect()
    }

    /// Rows the queued-messages block occupies for the current queues: the
    /// blank spacer row upstream inserts, one row per queued prompt, and the
    /// `… to edit all queued messages` hint row. Zero when nothing is queued,
    /// so a host with an empty queue keeps the frame geometry it had before
    /// LUM-1469.
    ///
    /// Upstream builds the same shape in `updatePendingMessagesDisplay`
    /// (`interactive-mode.ts:4366-4385`: `Spacer(1)`, one `TruncatedText` per
    /// steering prompt, same for follow-ups, then the hint).
    pub fn pending_block_rows(&self) -> u16 {
        if self.pending_is_empty() {
            0
        } else {
            u16::try_from(self.pending_len() + 2).unwrap_or(u16::MAX)
        }
    }

    /// Upstream's queued-messages block, as styled lines: a blank spacer, one
    /// line per queued prompt (`Steering: …` then `Follow-up: …`, the same
    /// delivery order as [`MessageView::pending`]), then the
    /// `↳ <chord> to edit all queued messages` hint.
    ///
    /// Every line is dim and **one row tall**: upstream renders each entry
    /// through `TruncatedText(text, 1, 0)`, which keeps only the text before
    /// the first newline and clips the rest to the available width. The clip
    /// mark is added by the painter
    /// ([`crate::styled::write_styled_line_ellipsized`]), so a queued draft
    /// that does not fit is marked rather than silently cut (LUM-1412).
    ///
    /// `dequeue_chord` is the resolved `app.message.dequeue` display text; an
    /// empty string means the reader unbound the chord, and the hint then
    /// drops the dead chord instead of advertising it.
    pub fn pending_lines(&self, dequeue_chord: &str) -> Vec<StyledLine> {
        if self.pending_is_empty() {
            return Vec::new();
        }
        let dim = SpanStyle::fg(ThemeColor::Dim);
        let mut lines: Vec<StyledLine> = Vec::with_capacity(self.pending_len() + 2);
        // Upstream's `Spacer(1)`: one blank row separates the block from the
        // transcript tail.
        lines.push(Vec::new());
        for (kind, text) in self.pending() {
            let label = match kind {
                PendingMessageKind::Steer => PENDING_STEER_LABEL,
                PendingMessageKind::FollowUp => PENDING_FOLLOW_UP_LABEL,
            };
            let mut line = String::with_capacity(label.len() + text.len());
            line.push_str(label);
            line.push_str(text.split('\n').next().unwrap_or(""));
            lines.push(vec![StyledSpan::new(line, dim)]);
        }
        let hint = if dequeue_chord.is_empty() {
            format!("{PENDING_HINT_LEAD}{PENDING_HINT_TEXT}")
        } else {
            format!("{PENDING_HINT_LEAD}{dequeue_chord} {PENDING_HINT_TEXT}")
        };
        lines.push(vec![StyledSpan::new(hint, dim)]);
        lines
    }

    /// Remove and return the next queued prompt, steering before follow-up.
    pub fn take_next_pending(&mut self) -> Option<String> {
        let next = if !self.pending_steering.is_empty() {
            Some(self.pending_steering.remove(0))
        } else if !self.pending_follow_up.is_empty() {
            Some(self.pending_follow_up.remove(0))
        } else {
            None
        };
        if next.is_some() {
            self.repin_if_following();
        }
        next
    }

    /// Remove and return every queued prompt in delivery order (steering
    /// then follow-up).
    pub fn take_all_pending(&mut self) -> Vec<(PendingMessageKind, String)> {
        let mut out: Vec<(PendingMessageKind, String)> = self
            .pending_steering
            .drain(..)
            .map(|text| (PendingMessageKind::Steer, text))
            .collect();
        out.extend(
            self.pending_follow_up
                .drain(..)
                .map(|text| (PendingMessageKind::FollowUp, text)),
        );
        if !out.is_empty() {
            self.repin_if_following();
        }
        out
    }

    /// Drop every queued prompt without delivering it.
    pub fn clear_pending(&mut self) {
        if self.pending_is_empty() {
            return;
        }
        self.pending_steering.clear();
        self.pending_follow_up.clear();
        self.repin_if_following();
    }

    /// Re-pin to the tail unless the reader scrolled away.
    ///
    /// A detached viewport measures its scroll offset from the tail, so an
    /// append would otherwise slide the visible text upwards by exactly the
    /// number of lines it added. Add that many lines back so the reader keeps
    /// looking at the same content while the turn streams on.
    fn repin_if_following(&mut self) {
        if !self.detached {
            self.scroll_from_bottom = 0;
            return;
        }
        let width = self.last_render_width.load(Ordering::Relaxed);
        let before = self.last_render_lines.load(Ordering::Relaxed);
        if before == 0 {
            // Nothing rendered yet, so there is no viewport to anchor.
            return;
        }
        let after = self.render_styled_lines(width).len();
        self.scroll_from_bottom = self
            .scroll_from_bottom
            .saturating_add(after.saturating_sub(before));
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
        self.repin_if_following();
    }

    /// Append a thinking / reasoning delta to the trailing assistant block.
    ///
    /// Thinking arrives interleaved with text in the same assistant message
    /// (Anthropic's extended thinking, OpenAI reasoning deltas), so the delta
    /// joins the trailing in-flight assistant item when there is one and
    /// otherwise opens a fresh streaming item — the same rule
    /// [`MessageView::append_assistant_delta`] uses, which is what keeps the
    /// streamed order of thinking / text / thinking intact.
    pub fn append_thinking_delta(&mut self, delta: &str) {
        match self.items.last_mut() {
            Some(item) if item.role == Role::Assistant && item.streaming => {
                item.thinking.push_str(delta);
            }
            _ => {
                let mut item = MessageItem::assistant_streaming();
                item.thinking.push_str(delta);
                self.items.push(item);
            }
        }
        self.repin_if_following();
    }

    /// Start a new streaming assistant block — used when the TUI sees
    /// a `MessageStart` event for a new turn.
    pub fn begin_assistant_stream(&mut self, model: &str) {
        // A new assistant message starts a fresh tool-call batch. Any stream
        // still open here belongs to an aborted / never-executed call, so it
        // is dropped rather than leaking into the next message.
        self.tool_streams.clear();
        let mut item = MessageItem::assistant_streaming();
        let _ = write!(item.text, "[{model}]");
        self.items.push(item);
        self.repin_if_following();
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
        self.push(MessageItem::tool(format_tool(name, args, result, is_error)));
    }

    /// Append a tool execution block whose result body is `lines` (already
    /// styled by the driver's [`ToolBlockRenderer`]).
    ///
    /// Used by tests and by callers that render out of band; the live path is
    /// [`MessageView::finish_tool_execution_with_lines`].
    pub fn push_tool_styled(&mut self, text: impl Into<String>, lines: Vec<StyledLine>) {
        let mut item = MessageItem::tool(text);
        item.tool_lines = Some(lines);
        self.push(item);
    }

    /// [`MessageView::push_tool_styled`] with a separate, always-visible call
    /// header.
    pub fn push_tool_block(
        &mut self,
        text: impl Into<String>,
        header: Vec<StyledLine>,
        body: Vec<StyledLine>,
    ) {
        let mut item = MessageItem::tool(text);
        item.tool_header = Some(header);
        item.tool_lines = Some(body);
        self.push(item);
    }
    /// Start (or look up) the streamed tool-call block for `call_id`.
    ///
    /// The first `ToolCallDelta` for a call carries its provider id and name;
    /// later deltas carry only argument fragments, and the execution / result
    /// events are keyed by the same id. Routing all of them through this map
    /// is what keeps one call to one block, mirroring upstream's `pendingTools`
    /// (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:430`).
    ///
    /// `name` is ignored when the block already exists (the first delta names
    /// it). Returns `true` when a new block was appended.
    pub fn begin_tool_stream(&mut self, call_id: &str, name: Option<&str>) -> bool {
        if self.tool_streams.contains_key(call_id) {
            // A provider may name the call on a later fragment; fill the
            // name in rather than opening a second block.
            let update = {
                let stream = self
                    .tool_streams
                    .get_mut(call_id)
                    .expect("contains_key checked above");
                if stream.name.is_empty() {
                    name.filter(|n| !n.is_empty()).map(|n| {
                        stream.name = n.to_string();
                        (
                            stream.index,
                            streaming_tool_text(&stream.name, &stream.args, stream.running),
                        )
                    })
                } else {
                    None
                }
            };
            if let Some((index, text)) = update {
                self.items[index].text = text;
                self.repin_if_following();
            }
            return false;
        }
        let name = name.unwrap_or("").to_string();
        self.items
            .push(MessageItem::tool(streaming_tool_text(&name, "", false)));
        let index = self.items.len() - 1;
        self.tool_streams.insert(
            call_id.to_string(),
            ToolStream {
                index,
                name,
                args: String::new(),
                running: false,
            },
        );
        self.repin_if_following();
        true
    }

    /// Append an argument fragment to an existing streamed tool call.
    ///
    /// Returns `false` when `call_id` is unknown, so the caller can decide
    /// whether to open the block first (upstream always has an id on every
    /// delta; this port's [`ToolCallDelta`](pi_agent_core::AgentEvent) only
    /// carries one on the first, so a stray fragment is dropped rather than
    /// creating an anonymous second block).
    pub fn append_tool_stream_args(&mut self, call_id: &str, delta: &str) -> bool {
        let Some(stream) = self.tool_streams.get_mut(call_id) else {
            return false;
        };
        stream.args.push_str(delta);
        let (index, name, args) = (stream.index, stream.name.clone(), stream.args.clone());
        self.items[index].text = streaming_tool_text(&name, &args, stream.running);
        self.repin_if_following();
        true
    }

    /// Mark a streamed tool call as executing, creating the block when the
    /// provider never streamed deltas for it (upstream's
    /// `tool_execution_start` branch creates the component if missing,
    /// `interactive-mode.ts:3325-3345`).
    pub fn start_tool_execution(&mut self, call_id: &str, name: &str, args: &str) {
        match self.tool_streams.get_mut(call_id) {
            Some(stream) => {
                if stream.name.is_empty() && !name.is_empty() {
                    stream.name = name.to_string();
                }
                if stream.args.is_empty() && !args.is_empty() {
                    stream.args = args.to_string();
                }
                stream.running = true;
                let (index, name, args) = (stream.index, stream.name.clone(), stream.args.clone());
                self.items[index].text = streaming_tool_text(&name, &args, true);
            }
            None => {
                self.items
                    .push(MessageItem::tool(streaming_tool_text(name, args, true)));
                let index = self.items.len() - 1;
                self.tool_streams.insert(
                    call_id.to_string(),
                    ToolStream {
                        index,
                        name: name.to_string(),
                        args: args.to_string(),
                        running: true,
                    },
                );
            }
        }
        self.repin_if_following();
    }

    /// Finish a tool call: write its result into the *same* block the deltas
    /// and execution start used, then retire the mapping (upstream's
    /// `tool_execution_end` updates and `pendingTools.delete`s,
    /// `interactive-mode.ts:3349-3362`).
    pub fn finish_tool_execution(
        &mut self,
        call_id: &str,
        duration_ms: u64,
        result: &str,
        is_error: bool,
    ) {
        self.finish_tool_execution_with_lines(call_id, duration_ms, result, is_error, None);
    }

    /// [`MessageView::finish_tool_execution`] with the driver's rich body.
    ///
    /// `styled` is what the driver's [`ToolBlockRenderer`] returned for this
    /// result; `None` (or an unrenderable tool) keeps the plain
    /// `[tool:…] args → result` body. The rich lines and the plain body are
    /// stored together so `/transcript`, copy and search keep working off the
    /// text while the screen paints the styled body. The block's header is
    /// stored separately from its body so a collapsed preview can keep the
    /// call summary on screen (requirement 1 folds the *result*).
    pub fn finish_tool_execution_with_lines(
        &mut self,
        call_id: &str,
        duration_ms: u64,
        result: &str,
        is_error: bool,
        styled: Option<ToolBlock>,
    ) {
        let _ = duration_ms;
        let (header, body) = match styled {
            Some(block) => (Some(block.header), Some(block.body)),
            None => (None, None),
        };
        match self.tool_streams.remove(call_id) {
            Some(stream) => {
                let index = stream.index;
                self.items[index].text = format_tool(&stream.name, &stream.args, result, is_error);
                self.items[index].tool_header = header;
                self.items[index].tool_lines = body;
                self.items[index].tool_expanded = None;
                self.repin_if_following();
            }
            // No start event for this id (synthetic / replayed turn): keep
            // the standalone block the previous implementation produced.
            None => {
                let mut item = MessageItem::tool(format_tool("", "", result, is_error));
                item.tool_header = header;
                item.tool_lines = body;
                self.push(item);
            }
        }
    }

    /// Push a free-form info message — used by `/help`, slash
    /// command output, and TUI-side notices.
    ///
    /// Shares the user prefix (`> `) for backwards compatibility with the
    /// inline notices the App has always printed this way; command-reference
    /// blocks that must not read as user input go through
    /// [`MessageView::push_info_block`].
    pub fn push_info(&mut self, text: impl Into<String>) {
        self.push(MessageItem {
            role: Role::User,
            text: text.into(),
            thinking: String::new(),
            streaming: false,
            tool_header: None,
            tool_lines: None,
            tool_expanded: None,
        });
    }

    /// Push a system info block — a command-reference body such as `/help`
    /// or `/hotkeys`.
    ///
    /// Distinct from [`MessageView::push_info`]: the block renders with the
    /// `· ` info prefix (role [`Role::Info`]) so it is never mistaken for a
    /// user prompt, which is the LUM-1238 §15.4 fix.
    pub fn push_info_block(&mut self, text: impl Into<String>) {
        self.push(MessageItem {
            role: Role::Info,
            text: text.into(),
            thinking: String::new(),
            streaming: false,
            tool_header: None,
            tool_lines: None,
            tool_expanded: None,
        });
    }

    /// True when the log is currently pinned to the tail. The TUI
    /// re-pins whenever it appends a new item or a delta.
    pub fn is_pinned_to_bottom(&self) -> bool {
        self.scroll_from_bottom == 0
    }

    /// Whether the viewport follows the tail: new items and streaming
    /// deltas re-pin it to the bottom. False once the reader scrolled
    /// away (see [`MessageView::set_following`]).
    pub fn is_following(&self) -> bool {
        !self.detached
    }

    /// Follow or stop following the tail.
    ///
    /// `false` keeps the current viewport where it is while the log keeps
    /// growing; `true` snaps back to the tail immediately. The App drives
    /// this from its scroll keys.
    pub fn set_following(&mut self, following: bool) {
        self.detached = !following;
        if following {
            self.scroll_from_bottom = 0;
        }
    }

    /// Scroll up by one line (away from the tail).
    pub fn scroll_up(&mut self) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(1);
        self.detached = true;
    }

    /// Scroll down by one line (toward the tail).
    pub fn scroll_down(&mut self) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(1);
        self.detached = self.scroll_from_bottom != 0;
    }

    /// Pin to the tail.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_from_bottom = 0;
        self.detached = false;
    }

    /// Pin to the head.
    ///
    /// Uses [`usize::MAX`] as "scrolled to the very top" sentinel, which
    /// the render path clamps against the real line count. Callers that
    /// need to scroll back down from here should prefer
    /// [`MessageView::set_scroll_from_bottom`] with a count computed from
    /// [`MessageView::line_count`].
    pub fn scroll_to_top(&mut self) {
        self.scroll_from_bottom = usize::MAX;
        self.detached = true;
    }

    /// Current scroll offset (lines from the bottom). `0` means
    /// pinned.
    pub fn scroll_offset(&self) -> usize {
        self.scroll_from_bottom
    }

    /// Set the scroll offset explicitly (lines back from the tail).
    ///
    /// `0` re-attaches the viewport to the tail; any other value detaches
    /// it. Used by the App, which clamps the value against the real line
    /// count for the current width.
    pub fn set_scroll_from_bottom(&mut self, offset: usize) {
        self.scroll_from_bottom = offset;
        self.detached = offset != 0;
    }

    /// Number of rendered lines at `width`.
    ///
    /// The App uses this to clamp scrolling and to size a page (one
    /// viewport height) without duplicating the layout.
    pub fn line_count(&self, width: u16) -> usize {
        self.render_styled_lines(width).len()
    }

    /// Render the log into a flat vector of pre-wrapped lines,
    /// prepending a one-character role prefix to each line. The TUI
    /// slices the returned vector into the visible viewport.
    pub fn render_lines(&self, width: u16) -> Vec<String> {
        self.render_styled_lines(width)
            .iter()
            .map(|line| plain_text(line))
            .collect()
    }
    /// Themed variant of [`MessageView::render_lines`].
    ///
    /// The visible text is identical to the plain render; the user body uses
    /// `userMessageText` (`user-message.ts:48`), the assistant body `text`, the
    /// tool body `toolOutput` (`tool-execution.ts:165`), the user/tool prefixes
    /// `accent`/`muted`, and the streaming caret `dim`. When markdown is
    /// enabled (see [`MessageView::with_markdown`]) the assistant body is
    /// rendered by [`crate::markdown::render_markdown`] instead and carries the
    /// `mdHeading` / `mdCode` / … slots.
    pub fn render_lines_themed(&self, width: u16, styles: &SelectListStyles<'_>) -> Vec<String> {
        let theme = styles.theme();
        self.render_styled_lines(width)
            .iter()
            .map(|line| themed_text(line, theme))
            .collect()
    }

    /// Render the log as theme-slot spans, one line per entry, wrapping at
    /// `width`.
    ///
    /// This is the single layout implementation behind [`render_lines`] (plain
    /// text), [`render_lines_themed`] (ANSI strings) and the App's themed
    /// buffer path. Markdown links follow [`MessageView::hyperlinks`].
    ///
    /// [`render_lines`]: MessageView::render_lines
    /// [`render_lines_themed`]: MessageView::render_lines_themed
    pub fn render_styled_lines(&self, width: u16) -> Vec<StyledLine> {
        self.render_styled_lines_with_links(width, self.hyperlinks)
    }

    /// [`MessageView::render_styled_lines`] with an explicit hyperlink
    /// capability, overriding [`MessageView::hyperlinks`].
    pub fn render_styled_lines_with_links(&self, width: u16, hyperlinks: bool) -> Vec<StyledLine> {
        let text_width = text_width_for(width);

        let mut out: Vec<StyledLine> = Vec::new();
        for item in &self.items {
            out.extend(self.item_lines(item, text_width, hyperlinks));
        }

        // Queued prompts are **not** part of the log: they render in the
        // composer-adjacent block ([`MessageView::pending_lines`]), exactly
        // where upstream keeps `pendingMessagesContainer`
        // (`interactive-mode.ts:878-892`). Before LUM-1469 they were appended
        // here, at the tail of the transcript, which made a not-yet-sent
        // prompt scrollable, selectable transcript content.
        if out.is_empty() {
            out.push(Vec::new());
        }
        self.last_render_width.store(width, Ordering::Relaxed);
        self.last_render_lines.store(out.len(), Ordering::Relaxed);
        out
    }

    /// `(start, end)` line ranges of every item in the rendered log, in
    /// render order.
    ///
    /// Built from the exact same [`MessageView::item_lines`] the renderer
    /// paints, so a hit test on a screen row cannot disagree with what is
    /// drawn there. `end` is exclusive, matching `slice` ranges.
    pub fn item_line_ranges(&self, width: u16) -> Vec<(usize, usize)> {
        let text_width = text_width_for(width);
        let mut ranges = Vec::with_capacity(self.items.len());
        let mut start = 0usize;
        for item in &self.items {
            let count = self.item_lines(item, text_width, self.hyperlinks).len();
            ranges.push((start, start + count));
            start += count;
        }
        ranges
    }

    /// The styled lines one item contributes to the log.
    ///
    /// This is the single per-item layout implementation behind
    /// [`MessageView::render_styled_lines_with_links`],
    /// [`MessageView::item_line_ranges`] and the App's hit testing. Thinking
    /// precedes the body inside an assistant item, matching upstream's ordered
    /// `message.content` walk (`assistant-message.ts:106-160`); a
    /// whitespace-only thinking block is skipped, exactly like upstream's
    /// `content.thinking.trim()`.
    fn item_lines(
        &self,
        item: &MessageItem,
        text_width: usize,
        hyperlinks: bool,
    ) -> Vec<StyledLine> {
        let (prefix, prefix_style, body_style) = match item.role {
            Role::User => (
                "> ",
                SpanStyle::fg(ThemeColor::Accent),
                SpanStyle::fg(ThemeColor::UserMessageText),
            ),
            Role::Assistant => ("  ", SpanStyle::PLAIN, SpanStyle::fg(ThemeColor::Text)),
            Role::Tool => (
                "* ",
                SpanStyle::fg(ThemeColor::Muted),
                SpanStyle::fg(ThemeColor::ToolOutput),
            ),
            Role::Info => (
                "· ",
                SpanStyle::fg(ThemeColor::Muted),
                SpanStyle::fg(ThemeColor::CustomMessageText),
            ),
        };

        let mut out: Vec<StyledLine> = Vec::new();
        if item.role == Role::Assistant && !item.thinking.trim().is_empty() {
            out.extend(self.thinking_lines(
                &item.thinking,
                text_width,
                prefix,
                prefix_style,
                hyperlinks,
            ));
        }

        if self.markdown && item.role == Role::Assistant {
            out.extend(markdown_lines(
                &item.text,
                text_width,
                prefix,
                prefix_style,
                item.streaming,
                hyperlinks,
            ));
            return out;
        }

        let mut lines = if item.role == Role::Tool {
            self.tool_body_lines(item, text_width, prefix, prefix_style, body_style)
        } else {
            plain_lines(&item.text, text_width, prefix, prefix_style, body_style)
        };
        if item.role == Role::Assistant && item.streaming {
            if let Some(first) = lines.first_mut() {
                first.push(StyledSpan::new(" ▍", SpanStyle::fg(ThemeColor::Dim)));
            }
        }
        out.extend(lines);
        out
    }

    /// The lines of a tool block: an always-visible header followed by the
    /// result body, the latter folded to the collapsed preview when neither
    /// the per-block override nor [`MessageView::tools_expanded`] says
    /// otherwise.
    ///
    /// When the driver supplied [`MessageItem::tool_header`] /
    /// [`MessageItem::tool_lines`] those styled lines are used verbatim
    /// (prefix added, inline-image rows left alone) and only the body is
    /// folded; otherwise the plain `[tool:…]` block is laid out as before.
    /// Folding is a *tail* slice (upstream's `FALLBACK_PREVIEW_LINES` also
    /// keeps the bottom, which is where the interesting output of `bash` /
    /// `read` sits), preceded by the [`tool_fold_hint`] line so the reader
    /// knows how many lines are hidden and how to reveal them.
    fn tool_body_lines(
        &self,
        item: &MessageItem,
        text_width: usize,
        prefix: &str,
        prefix_style: SpanStyle,
        body_style: SpanStyle,
    ) -> Vec<StyledLine> {
        // The header (the call summary the renderer styled) is never folded:
        // a collapsed block must still say which tool ran. Only the result
        // body below it is tail-sliced.
        let mut out: Vec<StyledLine> = match &item.tool_header {
            Some(header) => prefix_styled_lines(header, prefix, prefix_style),
            None => Vec::new(),
        };
        let full = match &item.tool_lines {
            Some(lines) => prefix_styled_lines(lines, prefix, prefix_style),
            // No driver renderer: the whole plain `[tool:…] args → result`
            // line is both header and body, exactly as before this slice.
            None if item.tool_header.is_none() => {
                plain_lines(&item.text, text_width, prefix, prefix_style, body_style)
            }
            None => Vec::new(),
        };
        let expanded = item.tool_expanded.unwrap_or(self.tools_expanded);
        if expanded || full.len() <= self.tool_preview_lines {
            out.extend(full);
            return out;
        }
        let hidden = full.len() - self.tool_preview_lines;
        out.reserve(self.tool_preview_lines + 1);
        out.push(vec![
            StyledSpan::new(prefix, prefix_style),
            StyledSpan::new(tool_fold_hint(hidden), SpanStyle::fg(ThemeColor::Muted)),
        ]);
        out.extend(full.into_iter().skip(hidden));
        out
    }

    /// The lines the message view shows for a viewport of `width` ×
    /// `height`, together with the index of the first one in the full
    /// rendered log.
    ///
    /// This is the single source of truth for the visible window:
    /// [`MessageView::render_to_buffer`] draws it and the [`App`](crate::App)
    /// maps pointer coordinates onto it for text selection, so a rendered
    /// row and a selectable row can never drift apart.
    pub fn visible_lines(&self, width: u16, height: u16) -> (usize, Vec<StyledLine>) {
        self.visible_lines_with_links(width, height, self.hyperlinks)
    }

    /// [`MessageView::visible_lines`] with an explicit hyperlink capability.
    ///
    /// The caller must pass the same capability it draws with, or selection
    /// and search would map columns against a different link rendering than
    /// the screen shows.
    pub fn visible_lines_with_links(
        &self,
        width: u16,
        height: u16,
        hyperlinks: bool,
    ) -> (usize, Vec<StyledLine>) {
        let lines = self.render_styled_lines_with_links(width, hyperlinks);
        let total = lines.len();
        // `scroll_to_top`'s sentinel is `usize::MAX`, so the sum has to saturate:
        // without it a direct caller of this public API panics in a debug build
        // (found by LUM-1455's transcript test, which pins the viewport to the
        // head). A saturated skip is `0`, which is exactly "the first line".
        let skip = total.saturating_sub((height as usize).saturating_add(self.scroll_from_bottom));
        let start = skip.min(total);
        let end = (start + height as usize).min(total);
        (start, lines[start..end].to_vec())
    }

    /// Render the trailing `height` lines of the log into a
    /// `ratatui::buffer::Buffer`. Used by the [`App`](crate::App) and
    /// by the snapshot tests in `tests/snapshot.rs`.
    pub fn render_to_buffer(&self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        self.render_to_buffer_impl(area, buf, None, false);
    }

    /// Themed variant of [`MessageView::render_to_buffer`]: every written cell
    /// carries the [`Style`](ratatui::style::Style) for its span's theme slot,
    /// so the App's buffer path consumes the theme without ANSI strings.
    pub fn render_to_buffer_themed(
        &self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
        theme: &Theme,
    ) {
        self.render_to_buffer_impl(area, buf, Some(theme), self.hyperlinks);
    }

    /// [`MessageView::render_to_buffer_themed`] with an explicit hyperlink
    /// capability: markdown links render as OSC 8 sequences when `hyperlinks`
    /// is true and as the inline `(url)` fallback when it is false.
    pub fn render_to_buffer_themed_with_links(
        &self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
        theme: &Theme,
        hyperlinks: bool,
    ) {
        self.render_to_buffer_impl(area, buf, Some(theme), hyperlinks);
    }

    fn render_to_buffer_impl(
        &self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
        theme: Option<&Theme>,
        hyperlinks: bool,
    ) {
        let (_, lines) = self.visible_lines_with_links(area.width, area.height, hyperlinks);
        for (row, line) in lines.iter().enumerate() {
            let y = area.y + row as u16;
            if y >= area.y + area.height {
                break;
            }
            match theme {
                Some(theme) => write_styled_line_hyperlinked(
                    buf, area.x, y, area.width, line, theme, hyperlinks,
                ),
                None => {
                    write_plain_row(buf, area.x, y, area.width, &plain_text(line));
                }
            }
        }
    }

    /// Render an assistant item's thinking text at `width`: a single collapsed
    /// [`HIDDEN_THINKING_LABEL`] line when [`MessageView::hide_thinking`] is
    /// set, otherwise the reasoning itself.
    ///
    /// The visible form is laid out exactly like the body (markdown when
    /// [`MessageView::markdown`] is on, wrapped plain text otherwise) and then
    /// recoloured into the `thinkingText` slot with italics, which mirrors
    /// upstream passing `color` / `italic` overrides to the thinking
    /// `Markdown` component (`assistant-message.ts:147-158`).
    fn thinking_lines(
        &self,
        thinking: &str,
        width: usize,
        prefix: &str,
        prefix_style: SpanStyle,
        hyperlinks: bool,
    ) -> Vec<StyledLine> {
        let style = SpanStyle::fg(ThemeColor::ThinkingText).italic();
        if self.hide_thinking {
            return vec![vec![
                StyledSpan::new(prefix, prefix_style),
                StyledSpan::new(HIDDEN_THINKING_LABEL, style),
            ]];
        }
        let mut lines = if self.markdown {
            markdown_lines(thinking, width, prefix, prefix_style, false, hyperlinks)
        } else {
            plain_lines(thinking, width, prefix, prefix_style, style)
        };
        // Recolour every body span (everything after the role prefix).
        // Inline-image rows stay verbatim: they are escape sequences, so
        // prefixing or restyling them would corrupt the picture.
        let verbatim = if self.markdown {
            image_row_mask(&lines)
        } else {
            vec![false; lines.len()]
        };
        for (idx, line) in lines.iter_mut().enumerate() {
            if verbatim[idx] {
                continue;
            }
            for span in line.iter_mut().skip(1) {
                span.style.fg = Some(ThemeColor::ThinkingText);
                span.style.italic = true;
            }
        }
        lines
    }
}

/// Format a finished tool call exactly like [`MessageView::push_tool`].
///
/// Kept as a free function so the streaming path
/// ([`MessageView::finish_tool_execution`]) and the one-shot path
/// ([`MessageView::push_tool`]) cannot drift apart.
fn format_tool(name: &str, args: &str, result: &str, is_error: bool) -> String {
    if is_error {
        let mut text = format!("[tool:{name}] error");
        if !result.is_empty() {
            let _ = write!(text, ": {result}");
        }
        text
    } else {
        let mut text = format!("[tool:{name}]");
        if !args.is_empty() {
            let _ = write!(text, " {args}");
        }
        if !result.is_empty() {
            let _ = write!(text, " → {result}");
        }
        text
    }
}

/// The text of an in-flight tool block: `[tool:name] args` while streaming,
/// `[tool:name] args (running)` once execution starts.
fn streaming_tool_text(name: &str, args: &str, running: bool) -> String {
    let mut text = format!("[tool:{name}]");
    if !args.is_empty() {
        text.push(' ');
        text.push_str(args);
    }
    if running {
        text.push_str(" (running)");
    }
    text
}

/// Text width left for a body after the two-cell role prefix (`"> "` / `"* "`).
fn text_width_for(width: u16) -> usize {
    (width as usize).saturating_sub(2).max(1)
}

/// Prepend the role prefix to pre-rendered, already-styled lines.
///
/// Inline-image rows — the escape sequence itself and the blank rows an image
/// was told to occupy — pass through **verbatim**, for the same reason
/// [`markdown_lines`] does: prefixing an escape sequence corrupts it, and
/// writing into an image's rows would draw the prefix over the picture.
fn prefix_styled_lines(
    lines: &[StyledLine],
    prefix: &str,
    prefix_style: SpanStyle,
) -> Vec<StyledLine> {
    let verbatim = image_row_mask(lines);
    lines
        .iter()
        .enumerate()
        .map(|(idx, line)| {
            if verbatim[idx] {
                return line.clone();
            }
            let mut spans: StyledLine = vec![StyledSpan::new(prefix, prefix_style)];
            spans.extend(line.iter().cloned());
            spans
        })
        .collect()
}

/// Wrap `body` and prepend the role prefix to every line, without the
/// streaming caret (the caller owns caret placement so the plain and
/// markdown paths agree). Shares [`wrap_text`] with the markdown fallback, so
/// a thinking block and a plain body wrap identically.
fn plain_lines(
    body: &str,
    width: usize,
    prefix: &str,
    prefix_style: SpanStyle,
    body_style: SpanStyle,
) -> Vec<StyledLine> {
    let wrapped = wrap_text(body, width);
    if wrapped.is_empty() {
        return vec![vec![StyledSpan::new(prefix, prefix_style)]];
    }
    wrapped
        .into_iter()
        .map(|line| {
            vec![
                StyledSpan::new(prefix, prefix_style),
                StyledSpan::new(line, body_style),
            ]
        })
        .collect()
}

/// Render an assistant body through the markdown renderer, prepending the
/// role prefix to every line and appending the streaming caret to the last
/// line. A whitespace-only body falls back to a single prefixed blank line
/// (matching the plain-text path).
///
/// A line the markdown renderer produced as an inline-image row — a kitty or
/// iTerm2 escape sequence, plus the blank rows that image occupies — is passed
/// through **verbatim**: prefixing an escape sequence would corrupt it, and
/// writing text into the rows an image was told to occupy would draw the role
/// prefix over the picture. A streaming body whose last line belongs to such
/// an image gets no caret for the same reason.
fn markdown_lines(
    body: &str,
    width: usize,
    prefix: &str,
    prefix_style: SpanStyle,
    streaming: bool,
    hyperlinks: bool,
) -> Vec<StyledLine> {
    let lines = crate::markdown::render_markdown_with_links(body, width, hyperlinks);
    if lines.is_empty() {
        return vec![vec![StyledSpan::new(prefix, prefix_style)]];
    }
    let verbatim = image_row_mask(&lines);
    let last = lines.len() - 1;
    lines
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            if verbatim[idx] {
                return line;
            }
            let mut spans: StyledLine = vec![StyledSpan::new(prefix, prefix_style)];
            spans.extend(line);
            if streaming && idx == last {
                spans.push(StyledSpan::new(" ▍", SpanStyle::fg(ThemeColor::Dim)));
            }
            spans
        })
        .collect()
}

/// Which rendered lines are part of an inline-image block.
///
/// A run of consecutive lines that are blank or image escape remains verbatim
/// as soon as one of its lines is an escape, so the iTerm2 shape (blank rows,
/// then the sequence on the last) is kept intact just like the kitty shape
/// (the sequence first, the reserved rows after).
fn image_row_mask(lines: &[StyledLine]) -> Vec<bool> {
    let is_image: Vec<bool> = lines
        .iter()
        .map(|line| crate::terminal_image::is_image_line(&plain_text(line)))
        .collect();
    let mut verbatim = vec![false; lines.len()];
    let mut i = 0usize;
    while i < lines.len() {
        let mut j = i;
        let mut has_image = false;
        while j < lines.len() && (is_image[j] || lines[j].is_empty()) {
            has_image |= is_image[j];
            j += 1;
        }
        if has_image {
            verbatim[i..j].fill(true);
        }
        i = if j == i { i + 1 } else { j };
    }
    verbatim
}

/// Split `text` at its hard line boundaries, losing nothing.
///
/// A lone `\n`, a lone `\r` and the `\r\n` pair each count as exactly one
/// break, mirroring upstream's `text.split(/\r\n|\r|\n/)`
/// (`packages/tui/src/utils.ts:850`). Splitting on `\n` alone would leave a
/// stray `\r` on every CRLF row, and splitting on both characters separately
/// would invent an empty row between the two halves of a `\r\n`.
fn split_hard_lines(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut idx = 0usize;
    while idx < bytes.len() {
        // Only ASCII bytes can be line breaks, and they can never appear as a
        // UTF-8 continuation byte, so slicing here is always on a char
        // boundary.
        match bytes[idx] {
            b'\r' => {
                out.push(&text[start..idx]);
                idx += if bytes.get(idx + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                start = idx;
            }
            b'\n' => {
                out.push(&text[start..idx]);
                idx += 1;
                start = idx;
            }
            _ => idx += 1,
        }
    }
    out.push(&text[start..]);
    out
}

/// Word-aware wrap that prefers to break at word boundaries and
/// falls back to hard-wrapping at `width` when a single word is longer
/// than the available space.
///
/// Hard line breaks survive: the body is split into source lines first and each
/// one is wrapped on its own, so a newline the author wrote is a layout
/// instruction instead of whitespace to collapse. Upstream does the same
/// (`wrapTextWithAnsi`, `packages/tui/src/utils.ts:843-866`), which is why the
/// `/help` legend — 20-odd pre-laid-out rows — no longer collapses into one
/// paragraph (LUM-1259 §2.5).
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for source_line in split_hard_lines(text) {
        lines.extend(wrap_single_line(source_line, width));
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Wrap one hard line.
///
/// A line that already fits is returned **verbatim**, keeping the indentation
/// and column alignment its author laid out — the same early return upstream's
/// `wrapSingleLine` makes (`packages/tui/src/utils.ts:873-876`). That early
/// return is what keeps a command-reference row such as
/// `"  /help     show this help text"` intact; the word-wrap below only ever
/// runs on a row that is genuinely too wide, and then it keeps the row's own
/// leading indent on its first output line (upstream's continuation lines are
/// flush left as well).
fn wrap_single_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    if width == 0 || display_width(line) <= width {
        return vec![line.to_string()];
    }
    if line.trim().is_empty() {
        // A run of indentation wider than the row is still a blank row; the
        // word-wrap has nothing to lay out (upstream returns `[line.trimEnd()]`
        // here, i.e. `[""]`).
        return vec![String::new()];
    }
    let indent: String = line.chars().take_while(|ch| ch.is_whitespace()).collect();
    let lines = wrap_words(line, width);
    match lines.split_first() {
        Some((first, rest)) if !indent.is_empty() => {
            let mut out = Vec::with_capacity(lines.len());
            out.push(format!("{indent}{first}"));
            out.extend(rest.iter().cloned());
            out
        }
        _ => lines,
    }
}

/// The word-wrap itself: greedily fill rows up to `width` **columns**
/// ([`crate::width`]), collapsing whitespace runs to a single space, with a
/// hard break for a word that is wider than the row.
///
/// Every CJK character is its own wrap token and carries no separator: a
/// script without inter-word spaces allows a break between any two adjacent
/// characters (upstream's second wrap-opportunity rule,
/// `splitIntoTokensWithAnsi` in `packages/tui/src/utils.ts:786`). Without it a
/// whole CJK sentence is one "word", so it could only ever be force-broken
/// instead of filling each row to the column budget.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let text = text.trim_start();
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for word in split_words(text) {
        for (token, glue) in word_tokens(word) {
            let token_width = display_width(token);
            if token_width > width && !glue {
                // Flush whatever we have, then hard-wrap the long word.
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                    current_width = 0;
                }
                for chunk in hard_wrap(token, width) {
                    lines.push(chunk);
                }
                continue;
            }
            // A CJK token is glued to its neighbours; everything else is a
            // fresh word and takes one separating space.
            let sep = if current.is_empty() || glue { 0 } else { 1 };
            if current_width + sep + token_width > width && !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current.push_str(token);
                current_width = token_width;
            } else {
                if sep == 1 {
                    current.push(' ');
                    current_width += 1;
                }
                current.push_str(token);
                current_width += token_width;
            }
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

/// Split one whitespace-delimited word into `(token, is_cjk_char)` wrap
/// tokens.
///
/// Every CJK character becomes a token of its own — its boundary is a legal
/// break — while a run of non-CJK characters stays a single token, exactly
/// like upstream's `splitIntoTokensWithAnsi` (`packages/tui/src/utils.ts:786`)
/// flushes its pending word when it meets a CJK grapheme.
fn word_tokens(word: &str) -> Vec<(&str, bool)> {
    let mut out: Vec<(&str, bool)> = Vec::new();
    let mut start = 0usize;
    for (idx, ch) in word.char_indices() {
        if !is_cjk_break(ch) {
            continue;
        }
        if start < idx {
            out.push((&word[start..idx], false));
        }
        out.push((&word[idx..idx + ch.len_utf8()], true));
        start = idx + ch.len_utf8();
    }
    if start < word.len() {
        out.push((&word[start..], false));
    }
    out
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

/// Columns a string occupies — the crate-wide width rule
/// ([`crate::width`]). A CJK ideograph is two columns, an emoji two, a
/// combining mark none.
fn display_width(s: &str) -> usize {
    columns(s)
}

fn hard_wrap(word: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for ch in word.chars() {
        let glyph_width = char_columns(ch);
        // A single glyph wider than the row is emitted on its own row rather
        // than preceded by an empty one (upstream's `breakLongWord` does the
        // same: a 2-column glyph in a 1-column row overflows by one and is
        // still drawn, because no terminal can render it any narrower).
        if !current.is_empty() && current_width + glyph_width > width {
            out.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(ch);
        current_width += glyph_width;
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

    /// `scroll_to_top` stores `usize::MAX`; the visible window must resolve that
    /// sentinel to the head of the log instead of overflowing on the sum
    /// (LUM-1455 — a direct caller of this public API used to panic in a debug
    /// build).
    #[test]
    fn visible_lines_accept_the_scroll_to_top_sentinel() {
        let mut view = MessageView::new();
        for index in 0..40 {
            view.push(MessageItem::tool(format!("tool {index}")));
        }
        view.scroll_to_top();
        let total = view.line_count(40);
        assert!(total > 10);

        let (start, lines) = view.visible_lines(40, 10);
        assert_eq!(start, 0, "the sentinel means the top of the log");
        assert_eq!(lines.len(), 10);
        assert!(
            plain_text(&lines[0]).contains("tool 0"),
            "{:?}",
            plain_text(&lines[0])
        );
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

    #[test]
    fn wrap_text_keeps_hard_line_breaks() {
        // The regression LUM-1259 §2.5 pinned: a pre-laid-out block must not be
        // re-flowed into one paragraph.
        let lines = wrap_text("a\nb\nc", 40);
        assert_eq!(lines, vec!["a", "b", "c"]);
    }

    #[test]
    fn wrap_text_splits_crlf_and_lone_cr_once() {
        assert_eq!(wrap_text("a\r\nb", 40), vec!["a", "b"]);
        assert_eq!(wrap_text("a\rb", 40), vec!["a", "b"]);
        // A trailing break is a real (empty) last row, matching upstream's
        // `split(/\r\n|\r|\n/)`.
        assert_eq!(wrap_text("a\n", 40), vec!["a", ""]);
    }

    #[test]
    fn wrap_text_keeps_blank_separator_rows() {
        let lines = wrap_text("head\n\ntail", 40);
        assert_eq!(lines, vec!["head", "", "tail"]);
    }

    #[test]
    fn wrap_text_passes_a_row_that_fits_through_verbatim() {
        // Indentation and the column run between command and description are
        // layout, not whitespace to collapse (upstream's `wrapSingleLine`
        // early return).
        let row = "  /help     show this help text";
        assert_eq!(wrap_text(row, 40), vec![row]);
    }

    #[test]
    fn wrap_text_wraps_an_over_long_row_and_keeps_its_indent() {
        let lines = wrap_text("  alpha beta gamma delta", 12);
        assert_eq!(lines, vec!["  alpha beta", "gamma delta"]);
        assert!(lines.iter().all(|line| display_width(line) <= 12));
    }

    #[test]
    fn wrap_text_of_a_whitespace_line_wider_than_the_row_is_blank() {
        assert_eq!(wrap_text("      ", 2), vec![""]);
        // …while one that fits is kept as-is.
        assert_eq!(wrap_text("    ", 4), vec!["    "]);
    }

    #[test]
    fn wrap_text_empty_input_is_one_blank_row() {
        assert_eq!(wrap_text("", 20), vec![""]);
    }

    /// Build a view with one tool block whose rich body is `count` styled
    /// lines, so the fold / expand assertions can count exact lines.
    fn view_with_tool_body(count: usize) -> MessageView {
        let mut view = MessageView::new();
        view.start_tool_execution("call-1", "bash", "{\"command\":\"echo hi\"}");
        let header = vec![vec![StyledSpan::new(
            "bash echo hi",
            SpanStyle::fg(ThemeColor::ToolTitle),
        )]];
        let body: Vec<StyledLine> = (0..count)
            .map(|idx| {
                vec![StyledSpan::new(
                    format!("body-{idx}"),
                    SpanStyle::fg(ThemeColor::ToolOutput),
                )]
            })
            .collect();
        view.finish_tool_execution_with_lines(
            "call-1",
            3,
            "body",
            false,
            Some(ToolBlock::new(header, body)),
        );
        view
    }

    #[test]
    fn collapsed_tool_block_keeps_the_header_tail_and_a_hint() {
        let view = view_with_tool_body(10);
        let lines = view.render_lines(40);
        // Header + hint + the 4-line preview.
        assert_eq!(lines.len(), 2 + TOOL_PREVIEW_LINES);
        assert_eq!(lines[0], "* bash echo hi");
        assert_eq!(lines[1], "* … (+6 lines, Ctrl+O to expand)");
        // The preview is the *tail*: body-6 … body-9.
        assert_eq!(
            &lines[2..],
            &["* body-6", "* body-7", "* body-8", "* body-9"]
        );
        // Rich styling survives the fold, on both the header and the body.
        let styled = view.render_styled_lines(40);
        assert!(styled[0]
            .iter()
            .any(|span| span.style.fg == Some(ThemeColor::ToolTitle)));
        assert!(styled[2]
            .iter()
            .any(|span| span.style.fg == Some(ThemeColor::ToolOutput)));
    }

    #[test]
    fn tool_preview_lines_is_injectable_and_a_short_block_never_folds() {
        let mut view = view_with_tool_body(3);
        view.set_tool_preview_lines(10);
        assert_eq!(view.tool_preview_lines(), 10);
        let lines = view.render_lines(40);
        // Header + 3 body lines, no hint: it fits the preview.
        assert_eq!(lines.len(), 4);
        assert!(!lines.iter().any(|line| line.contains("Ctrl+O")));
    }

    #[test]
    fn toggle_tools_expanded_reveals_every_line_and_clears_overrides() {
        let mut view = view_with_tool_body(10);
        assert!(!view.tools_expanded());
        // A per-block click expands just this block …
        assert_eq!(view.toggle_tool_at(0), Some(true));
        // Header + every body line.
        assert_eq!(view.render_lines(40).len(), 11);
        // … the chord then toggles every block: global collapse wins, and the
        // overrides are cleared so the next chord really is global.
        assert!(view.toggle_tools_expanded());
        assert_eq!(view.render_lines(40).len(), 11);
        assert!(!view.toggle_tools_expanded());
        assert_eq!(view.render_lines(40).len(), 2 + TOOL_PREVIEW_LINES);
        assert!(view.toggle_tools_expanded());
        assert_eq!(view.render_lines(40).len(), 11);
    }

    #[test]
    fn toggle_tool_at_ignores_non_tool_items() {
        let mut view = MessageView::new();
        view.push(MessageItem::user("hello"));
        assert_eq!(view.toggle_tool_at(0), None);
        assert_eq!(view.toggle_tool_at(99), None);
    }

    #[test]
    fn item_index_at_line_maps_rows_to_items() {
        let mut view = MessageView::new();
        view.push(MessageItem::user("hello"));
        view.push(MessageItem::assistant("one\ntwo"));
        let ranges = view.item_line_ranges(40);
        assert_eq!(ranges.len(), 2);
        assert_eq!(view.item_index_at_line(0, 40), Some(0));
        assert_eq!(view.item_index_at_line(ranges[1].0, 40), Some(1));
        assert_eq!(view.item_index_at_line(ranges[1].1 - 1, 40), Some(1));
        assert_eq!(view.item_index_at_line(999, 40), None);
    }

    /// LUM-1469: the queued-messages block is upstream's `Spacer(1)` +
    /// one row per prompt + the hint row — zero rows when nothing is queued.
    #[test]
    fn pending_block_rows_follow_the_queue() {
        let mut view = MessageView::new();
        assert_eq!(view.pending_block_rows(), 0);
        assert!(view.pending_lines("Alt+Up").is_empty());
        view.push_pending(PendingMessageKind::Steer, "steer me");
        assert_eq!(view.pending_block_rows(), 3);
        view.push_pending(PendingMessageKind::FollowUp, "then me");
        assert_eq!(view.pending_block_rows(), 4);
        view.take_all_pending();
        assert_eq!(view.pending_block_rows(), 0);
    }

    /// The block's shape: blank spacer, `Steering:` rows, `Follow-up:` rows,
    /// then the hint — the order upstream builds (`interactive-mode.ts:4370-4383`).
    #[test]
    fn pending_lines_render_labels_then_the_dequeue_hint() {
        let mut view = MessageView::new();
        view.push_pending(PendingMessageKind::FollowUp, "follow me");
        view.push_pending(PendingMessageKind::Steer, "steer me");
        let lines = view.pending_lines("Alt+Up");
        let text: Vec<String> = lines.iter().map(|line| plain_text(line)).collect();
        assert_eq!(text[0], "");
        // Steering first, whatever the insertion order (delivery order).
        assert_eq!(text[1], "Steering: steer me");
        assert_eq!(text[2], "Follow-up: follow me");
        assert_eq!(text[3], "\u{21b3} Alt+Up to edit all queued messages");
    }

    /// A queued draft is one row: only the text before the first newline is
    /// kept, exactly like upstream's `TruncatedText(text, 1, 0)`.
    #[test]
    fn pending_lines_keep_only_the_first_line_of_a_draft() {
        let mut view = MessageView::new();
        view.push_pending(PendingMessageKind::Steer, "first\nsecond\nthird");
        let lines = view.pending_lines("Alt+Up");
        assert_eq!(plain_text(&lines[1]), "Steering: first");
        assert_eq!(lines.len(), 3);
    }

    /// An unbound `app.message.dequeue` must not leave a dead chord on the
    /// hint row (the rule LUM-1447 set for every other hint surface).
    #[test]
    fn the_dequeue_hint_drops_an_unbound_chord() {
        let mut view = MessageView::new();
        view.push_pending(PendingMessageKind::Steer, "queued");
        let lines = view.pending_lines("");
        assert_eq!(
            plain_text(lines.last().expect("hint row")),
            "\u{21b3} to edit all queued messages"
        );
    }
}
