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

use crate::styled::{
    plain_text, themed_text, write_styled_line_hyperlinked, SpanStyle, StyledLine, StyledSpan,
};
use crate::styles::SelectListStyles;
use crate::theme::{Theme, ThemeColor};

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

/// Conversation log rendered by the TUI. Holds an ordered list of
/// [`MessageItem`] entries and supports incremental updates so the
/// TUI redraws only the tail while the assistant streams.
#[derive(Debug, Default)]
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
        let _ = duration_ms;
        match self.tool_streams.remove(call_id) {
            Some(stream) => {
                self.items[stream.index].text =
                    format_tool(&stream.name, &stream.args, result, is_error);
                self.repin_if_following();
            }
            // No start event for this id (synthetic / replayed turn): keep
            // the standalone block the previous implementation produced.
            None => self.push_tool("", "", result, is_error),
        }
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
        let prefix_width = 2usize; // "> " or "* "
        let text_width = (width as usize).saturating_sub(prefix_width).max(1);

        let mut out: Vec<StyledLine> = Vec::new();
        for item in &self.items {
            let (prefix, body, prefix_style, body_style) = match item.role {
                Role::User => (
                    "> ",
                    item.text.clone(),
                    SpanStyle::fg(ThemeColor::Accent),
                    SpanStyle::fg(ThemeColor::UserMessageText),
                ),
                Role::Assistant => (
                    "  ",
                    item.text.clone(),
                    SpanStyle::PLAIN,
                    SpanStyle::fg(ThemeColor::Text),
                ),
                Role::Tool => (
                    "* ",
                    item.text.clone(),
                    SpanStyle::fg(ThemeColor::Muted),
                    SpanStyle::fg(ThemeColor::ToolOutput),
                ),
            };

            if self.markdown && item.role == Role::Assistant {
                out.extend(markdown_lines(
                    &item.text,
                    text_width,
                    prefix,
                    prefix_style,
                    item.streaming,
                    hyperlinks,
                ));
                continue;
            }

            let wrapped = wrap_text(&body, text_width);
            if wrapped.is_empty() {
                out.push(vec![StyledSpan::new(prefix, prefix_style)]);
                continue;
            }
            for (idx, line) in wrapped.iter().enumerate() {
                let mut spans = vec![
                    StyledSpan::new(prefix, prefix_style),
                    StyledSpan::new(line.clone(), body_style),
                ];
                if idx == 0 && item.role == Role::Assistant && item.streaming {
                    spans.push(StyledSpan::new(" ▍", SpanStyle::fg(ThemeColor::Dim)));
                }
                out.push(spans);
            }
        }

        if out.is_empty() {
            out.push(Vec::new());
        }
        self.last_render_width.store(width, Ordering::Relaxed);
        self.last_render_lines.store(out.len(), Ordering::Relaxed);
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
        let skip = total.saturating_sub(height as usize + self.scroll_from_bottom);
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
                    for (col, ch) in plain_text(line).chars().enumerate() {
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
