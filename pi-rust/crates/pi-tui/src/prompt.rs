//! Prompt component — wraps an [`Editor`] with the bottom-of-screen
//! prompt chrome (label, border, placeholder). Mirrors the role of
//! `packages/tui/components/editor.ts` together with the `prompt()`
//! shell in `packages/coding-agent/src/modes/interactive/interactive-mode.ts`.
//!
//! # The composer window
//!
//! A draft taller than the rows the composer has is shown through a
//! *window*: only `max_rows` rows are painted. Which rows those are is a
//! decision that has to survive across frames — a window re-anchored from
//! scratch on every keystroke would jump a whole page when the caret
//! crossed a page boundary — so [`Prompt::render_frame`] takes the window
//! start the caller remembers and returns the value for the next frame
//! (upstream keeps the same number in `Editor.scrollOffset`,
//! `packages/tui/src/components/editor.ts:304`). The window then follows
//! the caret one row at a time, and the rows it hides are reported **in the
//! border** (` ↑ 3 more `), which is upstream's scrollable top / bottom
//! border (`createScrollBorder`, `packages/tui/src/components/editor.ts:266`).
//!
//! # The composer border
//!
//! Upstream's editor paints one rule above the draft and one below it and
//! no side borders at all — `renderTopBorder` / `renderBottomBorder` push
//! `"─".repeat(width)` (or the scroll form) around the content lines, and
//! the comment on the content loop says so outright: *"no side borders,
//! just horizontal lines above and below"*
//! (`packages/tui/src/components/editor.ts:499-506,597-601`). The rules are
//! painted in the editor's *border colour*, which the interactive mode
//! switches between the bash-mode colour and the thinking level's colour
//! (`updateEditorBorderColor`, `interactive-mode.ts:4166-4174`).
//!
//! This port renders the same two rules ([`PromptRowKind::Border`] rows, so
//! the App can colour them), costs the same two rows
//! ([`PROMPT_BORDER_ROWS`]), and keeps its `> ` label gutter inside them.
//! A window taller than one row that cannot afford the rules falls back to
//! the port's older in-gutter `↑` / `↓` markers so a clipped draft always
//! says so either way.
//!
//! [`Editor`]: crate::Editor

use crate::editor::{Editor, EditorAction, HistorySearchStatus};
use crate::input::{InputEvent, Key, KeyCode};
use crate::visual_text::{cell_width, cells, VisualLayout};

/// Rows the composer border costs: one rule above the draft, one below it.
///
/// Upstream's editor renders `1 + <visible lines> + 1` lines for the same
/// reason (`packages/tui/src/components/editor.ts:550-601`). Callers that
/// budget chrome (the App's `plan_chrome`) must add this on top of the
/// draft's own row count.
pub const PROMPT_BORDER_ROWS: usize = 2;

/// What one rendered composer row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptRowKind {
    /// A draft row (upstream's "content line").
    Body,
    /// The reverse-search status row while `Ctrl+R` is open.
    Search,
    /// A `─` rule above or below the draft (upstream `renderTopBorder` /
    /// `renderBottomBorder`).
    Border,
}

/// One rendered composer row: its text plus what kind of row it is.
///
/// The kind exists so the App can paint a whole [`PromptRowKind::Border`]
/// row in the composer's border colour instead of guessing from the text
/// (a rule and a row of dashes the user typed look identical).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptRow {
    /// The row's text, padded to the composer width.
    pub text: String,
    /// What the row is.
    pub kind: PromptRowKind,
}

impl PromptRow {
    fn body(text: String) -> Self {
        Self {
            text,
            kind: PromptRowKind::Body,
        }
    }

    fn search(text: String) -> Self {
        Self {
            text,
            kind: PromptRowKind::Search,
        }
    }

    fn border(text: String) -> Self {
        Self {
            text,
            kind: PromptRowKind::Border,
        }
    }
}

/// A rendered composer frame: every row top to bottom, plus the window start
/// the next frame has to resume from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptFrame {
    /// Painted rows, top to bottom (border / search / body / border).
    pub rows: Vec<PromptRow>,
    /// Draft row the body window starts at.
    pub scroll: usize,
}

/// Action returned from [`Prompt::handle_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptAction {
    /// No state change worth redrawing.
    None,
    /// Buffer or placeholder changed — caller redraws.
    Changed,
    /// User submitted the prompt.
    Submit(String),
    /// User pressed Ctrl+C — caller interrupts the current operation.
    Interrupt,
    /// User pressed Ctrl+D on an empty buffer — caller exits.
    Eof,
}

/// Bottom-of-screen prompt with editable buffer.
#[derive(Debug, Clone)]
pub struct Prompt {
    editor: Editor,
    placeholder: String,
    label: String,
    /// Whether the composer paints upstream's top / bottom rules when the
    /// region can afford them (`PROMPT_BORDER_ROWS` plus one draft row).
    border: bool,
}

impl Default for Prompt {
    fn default() -> Self {
        Self::new("> ")
    }
}

impl Prompt {
    /// Construct a prompt with the given label.
    ///
    /// The composer border is **opt-in** ([`Prompt::set_border`]), so every
    /// existing caller keeps the row arithmetic it was written against; the
    /// App forwards [`crate::AppConfig::composer_border`] and the interactive
    /// driver turns it on.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            editor: Editor::new(),
            placeholder: String::new(),
            label: label.into(),
            border: false,
        }
    }

    /// Set the placeholder shown when the buffer is empty.
    pub fn set_placeholder(&mut self, placeholder: impl Into<String>) {
        self.placeholder = placeholder.into();
    }

    /// Turn the composer border on or off (see [`Prompt::border_enabled`]).
    ///
    /// On, the composer paints the two rules upstream's editor draws around
    /// the draft — as tall as `PROMPT_BORDER_ROWS` more than the draft. Off
    /// (the default) reproduces the pre-border composer.
    pub fn set_border(&mut self, border: bool) {
        self.border = border;
    }

    /// Whether the composer border is enabled.
    pub fn border_enabled(&self) -> bool {
        self.border
    }

    /// Whether a bordered frame at `max_rows` really paints the rules: one
    /// row each plus at least one draft row between them.
    pub fn border_visible(&self, max_rows: usize) -> bool {
        self.border && max_rows > PROMPT_BORDER_ROWS
    }

    /// Borrow the underlying editor.
    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    /// Mutable borrow of the underlying editor.
    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }

    /// The draft the prompt shows: chip sentinels expanded to their
    /// `[Image #N]` labels. Use [`Prompt::editor`] for the raw buffer.
    ///
    /// A paste marker (`[paste #N +12 lines]`) stays literal here, exactly
    /// as it is drawn — that is upstream `getText()`. Use
    /// [`Prompt::expanded_text`] for the content a submission carries.
    pub fn text(&self) -> String {
        self.editor.display_text()
    }

    /// The draft with paste markers expanded to the text they stand for
    /// (upstream `getExpandedText`). This is the form the model and the
    /// external editor get.
    pub fn expanded_text(&self) -> String {
        self.editor.expanded_text()
    }

    /// Cursor column within [`Prompt::text`].
    pub fn cursor(&self) -> usize {
        self.editor.display_cursor()
    }

    /// Place the caret at a character offset in [`Prompt::text`] — the
    /// pointer's click path (see [`Editor::place_display_cursor`]).
    pub fn place_cursor(&mut self, display: usize) -> EditorAction {
        self.editor.place_display_cursor(display)
    }

    /// The pasted image chips attached to the draft, in buffer order.
    pub fn images(&self) -> &[pi_protocol::ImageContent] {
        self.editor.image_attachments()
    }

    /// Number of image chips attached to the draft.
    pub fn image_count(&self) -> usize {
        self.editor.image_count()
    }

    /// Borrow the label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Borrow the placeholder.
    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }

    /// Clear the buffer without touching history.
    pub fn clear(&mut self) {
        self.editor.clear();
    }

    /// Push a submitted prompt onto the editor's history.
    pub fn push_history(&mut self, text: impl Into<String>) {
        self.editor.push_history(text);
    }

    /// Push a submitted prompt together with the draft state a recall must
    /// restore (raw chip buffer + attachments). See
    /// [`Editor::push_history_entry`].
    pub fn push_history_entry(
        &mut self,
        text: impl Into<String>,
        raw: Option<String>,
        images: Vec<pi_protocol::ImageContent>,
    ) {
        self.editor.push_history_entry(text, raw, images);
    }

    /// Attach the cross-session history file (see
    /// [`Editor::set_history_store`]).
    pub fn set_history_store(&mut self, store: crate::history_store::HistoryStore) {
        self.editor.set_history_store(store);
    }

    /// Drop the in-session history and delete the persistent file
    /// (`/clear-history`).
    pub fn clear_persisted_history(&mut self) {
        self.editor.clear_persisted_history();
    }

    /// The reverse-search row, when `Ctrl+R` search is open.
    ///
    /// Mirrors codex's footer line while its history search is active
    /// (`codex-rs/tui/src/bottom_pane/chat_composer/history_search.rs:383-420`,
    /// `reverse-i-search: <query>` plus a phase hint). `None` when no search
    /// is open. The row is truncated **and padded** to `width` so the
    /// composer region repaints it completely on every frame.
    pub fn history_search_row(&self, width: u16) -> Option<String> {
        let status = self.editor.history_search_status()?;
        let query = self.editor.history_search_query().unwrap_or_default();
        let mut line = format!("reverse-i-search: {query}");
        match status {
            HistorySearchStatus::Idle => {}
            HistorySearchStatus::Match => line.push_str("  Enter accept · Esc cancel"),
            HistorySearchStatus::NoMatch => line.push_str("  no match · Esc cancel"),
        }
        let width = width as usize;
        let mut row = column_truncate(&line, width);
        while cells(&row) < width {
            row.push(' ');
        }
        Some(row)
    }

    /// Reset the prompt — clear buffer and history. Used by `/clear`.
    pub fn reset(&mut self) {
        self.editor.clear();
        self.editor.clear_history();
    }

    /// Render the prompt into the bottom row of the screen. The caller
    /// is responsible for selecting the area.
    pub fn render_line(&self, width: u16) -> String {
        let label = self.label.as_str();
        let label_width = cells(label);
        let available = (width as usize).saturating_sub(label_width);
        let text = self.editor.display_text();
        let cursor = self.editor.display_cursor();
        let (before, after) = split_at_char(&text, cursor);
        let mut line = String::new();
        line.push_str(label);
        if text.is_empty() && !self.placeholder.is_empty() {
            // Placeholder is truncated to `available` columns so we do
            // not overflow the line.
            let placeholder = column_truncate(&self.placeholder, available);
            line.push_str(&placeholder);
        } else {
            line.push_str(before);
            line.push('▍');
            line.push_str(after);
        }
        // Pad to width.
        while cells(&line) < width as usize {
            line.push(' ');
        }
        line
    }

    /// Process a key event.
    pub fn handle_key(&mut self, key: Key) -> PromptAction {
        match self.editor.handle_key(key) {
            EditorAction::None => PromptAction::None,
            EditorAction::Changed => PromptAction::Changed,
            EditorAction::Submit(text) => PromptAction::Submit(text),
            EditorAction::Interrupt => PromptAction::Interrupt,
            EditorAction::Eof => PromptAction::Eof,
        }
    }

    /// Process an input event.
    pub fn handle_event(&mut self, event: InputEvent) -> PromptAction {
        let InputEvent::Key(key) = event else {
            return PromptAction::None;
        };
        self.handle_key(key)
    }

    /// Whether the prompt currently has any buffer text.
    pub fn is_empty(&self) -> bool {
        self.editor.is_empty()
    }

    /// True when the prompt is currently showing the placeholder (no
    /// buffer text). The TUI uses this to decide whether to render the
    /// placeholder.
    pub fn shows_placeholder(&self) -> bool {
        self.editor.is_empty()
    }

    /// Detect a "submit" key (Enter) on a non-empty buffer. Returns the
    /// trimmed text if the key was Enter, otherwise `None`.
    pub fn try_submit(&mut self, key: Key) -> Option<String> {
        if key.code == KeyCode::Enter && !self.editor.is_empty() {
            Some(self.editor.display_text())
        } else {
            None
        }
    }

    /// How many visual rows the prompt needs to render its current buffer
    /// at `width`, clamped to `max_rows`. Always at least 1 — an empty
    /// prompt still occupies one row.
    ///
    /// The buffer is word-wrapped the same way [`Prompt::render_lines`]
    /// does, so callers can reserve the row count ahead of time and the
    /// resulting layout does not jump when the buffer is typed into.
    pub fn body_width(&self, width: u16) -> usize {
        wrap_available(width as usize, cells(&self.label)).max(1)
    }

    /// Columns of `width` the draft itself may use (the label takes the
    /// rest, and at least one column always remains).
    ///
    /// Exposed because the composer's word wrap is not only a rendering
    /// concern: [`crate::Editor`]'s vertical cursor motion has to measure
    /// the draft with the same number, or the caret lands on a row the
    /// renderer did not draw it on. The App hands this value to the editor
    /// before every key press.
    pub fn line_count(&self, width: u16, draft_cap: usize) -> usize {
        let width = width as usize;
        if width == 0 || draft_cap == 0 {
            return 1;
        }
        let search_rows = usize::from(self.editor.history_search_active());
        let rows = self.body_line_count(width, draft_cap) + search_rows;
        // `draft_cap` bounds the *draft* rows; the rules are chrome on top of
        // it, exactly like upstream's editor, whose visible-line budget
        // (`maxVisibleLines`) is separate from the two border lines it draws
        // around them (`components/editor.ts:532-601`).
        if self.border {
            rows + PROMPT_BORDER_ROWS
        } else {
            rows
        }
    }

    /// Rows the draft itself needs, clamped to `max_rows` (no search row).
    fn body_line_count(&self, width: usize, max_rows: usize) -> usize {
        let text = self.editor.display_text();
        if text.is_empty() {
            return 1;
        }
        let rows = VisualLayout::new(&text, self.body_width(width as u16)).len();
        rows.clamp(1, max_rows.max(1))
    }

    /// Rows hidden above / below the window for the current draft at
    /// `body_rows` (the numbers the border and the in-gutter markers
    /// report).
    fn border_hidden(&self, width: usize, body_rows: usize, start: usize) -> (usize, usize) {
        let text = self.editor.display_text();
        if text.is_empty() {
            return (0, 0);
        }
        let total = VisualLayout::new(&text, self.body_width(width as u16)).len();
        let show = total.min(body_rows);
        (start, total - (start + show))
    }

    /// Render the composer as a full frame: the top rule, the optional
    /// reverse-search row, the draft rows and the bottom rule.
    ///
    /// This is [`Prompt::render_lines`] plus the row kinds, so the App can
    /// paint the rules in the composer's border colour
    /// ([`PromptRowKind::Border`]).
    pub fn render_frame(
        &self,
        width: u16,
        draft_cap: usize,
        region_rows: usize,
        scroll: usize,
    ) -> PromptFrame {
        let width = width as usize;
        let region_rows = region_rows.max(1);
        let draft_cap = draft_cap.max(1);
        if width == 0 {
            return PromptFrame {
                rows: vec![PromptRow::body(String::new())],
                scroll: 0,
            };
        }
        let bordered = self.border_visible(region_rows);
        let chrome = if bordered { PROMPT_BORDER_ROWS } else { 0 };
        let search = self.history_search_row(width as u16);
        let search_rows = usize::from(search.is_some());
        let body_rows = draft_cap
            .min(
                region_rows
                    .saturating_sub(chrome)
                    .saturating_sub(search_rows),
            )
            .max(1);
        // The in-gutter `↑` / `↓` markers are the fallback for a frame that
        // cannot afford rules; a bordered frame reports the window on the
        // rules themselves (upstream's `createScrollBorder`).
        let (body, start) = self.render_body(width, body_rows, scroll, !bordered);
        let (hidden_above, hidden_below) = if bordered {
            self.border_hidden(width, body_rows, start)
        } else {
            (0, 0)
        };
        let mut rows: Vec<PromptRow> = Vec::with_capacity(region_rows);
        if bordered {
            rows.push(PromptRow::border(border_row('↑', hidden_above, width)));
        }
        if let Some(row) = search {
            rows.push(PromptRow::search(row));
        }
        rows.extend(body.into_iter().map(PromptRow::body));
        if bordered {
            rows.push(PromptRow::border(border_row('↓', hidden_below, width)));
        }
        rows.truncate(region_rows);
        PromptFrame {
            rows,
            scroll: start,
        }
    }

    /// Render the prompt into 1..=`region_rows` lines at the given width,
    /// continuing the composer window at `scroll`.
    ///
    /// Text-only view of [`Prompt::render_frame`] with the draft cap equal
    /// to the region, kept for callers that only need the pixels.
    pub fn render_lines(&self, width: u16, max_rows: usize, scroll: usize) -> (Vec<String>, usize) {
        let frame = self.render_frame(width, max_rows, max_rows, scroll);
        (
            frame.rows.into_iter().map(|row| row.text).collect(),
            frame.scroll,
        )
    }

    /// The composer body rows (everything between the rules).
    ///
    /// `markers` selects the in-gutter `↑` / `↓` window markers, which only
    /// a frame without rules (see [`Prompt::border_visible`]) still needs.
    fn render_body(
        &self,
        width: usize,
        max_rows: usize,
        scroll: usize,
        markers: bool,
    ) -> (Vec<String>, usize) {
        let label_width = cells(&self.label);
        let available = self.body_width(width as u16);
        let text = self.editor.display_text();

        if text.is_empty() {
            // Empty buffer — placeholder on the first row, blank padded
            // continuation rows so the row count matches `max_rows`.
            let mut out = Vec::with_capacity(max_rows);
            out.push(self.empty_row(width, available));
            let indent = " ".repeat(label_width);
            while out.len() < max_rows {
                let row_index = out.len();
                let prefix = if row_index == 0 {
                    self.label.as_str()
                } else {
                    indent.as_str()
                };
                out.push(blank_row(prefix, width));
            }
            return (out, 0);
        }

        let layout = VisualLayout::new(&text, available);
        let (cursor_row, cursor_col) = layout.caret(self.editor.display_cursor());
        let total_rows = layout.len();
        let show_rows = total_rows.min(max_rows);
        let skip = follow_cursor(scroll, cursor_row, total_rows, show_rows);
        let indent = " ".repeat(label_width);
        let hidden_above = skip;
        let hidden_below = total_rows - (skip + show_rows);

        let mut out: Vec<String> = Vec::with_capacity(show_rows);
        for index in skip..skip + show_rows {
            let row = &layout.rows()[index];
            let draw_cursor = index == cursor_row;
            // The label belongs to the draft's own first row; a scrolled
            // window marks where the draft continues instead.
            let prefix = self.window_prefix(
                WindowView {
                    index,
                    skip,
                    show_rows,
                    hidden_above,
                    hidden_below,
                },
                &indent,
                markers,
            );
            out.push(build_prompt_row(
                &prefix,
                &row.text,
                if draw_cursor { cursor_col } else { usize::MAX },
                width,
                draw_cursor,
            ));
        }
        // Pad with blank rows if the cap exceeds the natural row count.
        // Continuation rows (anything past the first) get the indent so
        // the visual column of the buffer stays aligned; the first row
        // gets the label so the user still sees what mode they are in.
        let first_prefix = self.label.as_str();
        let cont_prefix = indent.as_str();
        while out.len() < max_rows {
            let row_index = out.len();
            let prefix = if row_index == 0 {
                first_prefix
            } else {
                cont_prefix
            };
            out.push(blank_row(prefix, width));
        }
        (out, skip)
    }

    /// The chrome column the row at `index` is drawn with, where the window
    /// shows draft rows `skip..skip + show_rows` out of `total_rows`.
    ///
    /// The draft's own first row always keeps the label — the user must not
    /// lose the mode marker just because the draft scrolled. Every other row
    /// is indented, except the window's first / last row when the draft
    /// continues above / below it: those carry `↑` / `↓` and the number of
    /// hidden rows, which is upstream's scroll border
    /// (`createScrollBorder`, `packages/tui/src/components/editor.ts:266`)
    /// expressed in the one column the port's chrome owns.
    fn window_prefix(&self, view: WindowView, indent: &str, markers: bool) -> String {
        let WindowView {
            index,
            skip,
            show_rows,
            hidden_above,
            hidden_below,
        } = view;
        if index == 0 {
            return self.label.clone();
        }
        if !markers {
            // The border carries `↑ N more` / `↓ N more` instead.
            return indent.to_string();
        }
        let gutter = cells(&self.label);
        // A one-row window has no room for two separate markers: it is both
        // the first and the last visible row, so it reports both sides.
        if show_rows == 1 && hidden_above > 0 && hidden_below > 0 {
            return scroll_marker('↕', hidden_above + hidden_below, gutter);
        }
        if index == skip && hidden_above > 0 {
            return scroll_marker('↑', hidden_above, gutter);
        }
        if index + 1 == skip + show_rows && hidden_below > 0 {
            return scroll_marker('↓', hidden_below, gutter);
        }
        indent.to_string()
    }

    /// Empty single-row representation (placeholder + padding).
    fn empty_row(&self, width: usize, available: usize) -> String {
        let mut line = String::with_capacity(width);
        line.push_str(&self.label);
        if !self.placeholder.is_empty() {
            line.push_str(&column_truncate(&self.placeholder, available));
        }
        while cells(&line) < width {
            line.push(' ');
        }
        line
    }
}

/// The visible window of the draft, as the rows that carry the label and the
/// `↑` / `↓` markers need to know it.
#[derive(Debug, Clone, Copy)]
struct WindowView {
    /// Draft row being drawn.
    index: usize,
    /// First draft row the window shows.
    skip: usize,
    /// How many draft rows the window shows.
    show_rows: usize,
    /// Draft rows hidden above the window.
    hidden_above: usize,
    /// Draft rows hidden below it.
    hidden_below: usize,
}

/// One composer rule, `─` across `width` columns.
///
/// With `hidden` rows outside the window the rule reports them instead of
/// being a plain line, which is upstream's `createScrollBorder`
/// (`packages/tui/src/components/editor.ts:266-282`):
///
/// * ` ↑ 3 more ` centred in the rule, when the label plus one column each
///   side fits;
/// * otherwise the left-aligned short form `─── ↑ 3 more ` with the rest
///   filled in;
/// * otherwise the label truncated to whatever fits and closed with `...`.
///
/// `hidden == 0` returns `"─".repeat(width)` — the plain rule upstream
/// draws when nothing is scrolled away. Every width here is **columns**
/// (`cells`), like the rest of this module.
fn border_row(direction: char, hidden: usize, width: usize) -> String {
    if hidden == 0 {
        return "─".repeat(width);
    }
    scroll_border(direction, hidden, width)
}

/// Upstream `createScrollBorder`.
///
/// Upstream's middle branch (the left-aligned `─── ↑ N more ` form) is
/// unreachable in practice: the centred branch's gate is `label_width + 2 <=
/// width` while the short form needs `width >= label_width + 3`, so the third
/// branch is what a too-narrow rule falls into. It is ported anyway, and
/// documented here instead of being silently dropped, because the two
/// reachable forms are what the tests assert.
fn scroll_border(direction: char, hidden: usize, width: usize) -> String {
    let label = format!(" {direction} {hidden} more ");
    let label_width = cells(&label);
    if label_width + 2 <= width {
        let left = (width - label_width) / 2;
        let mut out = "─".repeat(left);
        out.push_str(&label);
        out.push_str(&"─".repeat(width - left - label_width));
        return out;
    }
    let indicator = format!("─── {direction} {hidden} more ");
    let indicator_width = cells(&indicator);
    if indicator_width <= width {
        let mut out = indicator;
        out.push_str(&"─".repeat(width - indicator_width));
        return out;
    }
    // Even the short form is too wide: keep its head and close with `...`,
    // exactly like upstream's `sliceByColumn(indicator, 0, w, true) + "..."`
    // [`column_truncate`] is the column-aware equivalent of that slice.
    let ellipsis = column_truncate("...", width);
    let head = width.saturating_sub(cells(&ellipsis));
    let mut out = column_truncate(&indicator, head);
    out.push_str(&ellipsis);
    out
}

/// How many columns of the prompt row are available to the buffer
/// (after subtracting the label).
fn wrap_available(width: usize, label_width: usize) -> usize {
    width.saturating_sub(label_width)
}

/// Build a padded blank row with the given prefix.
fn blank_row(prefix: &str, width: usize) -> String {
    let mut line = String::with_capacity(width);
    line.push_str(prefix);
    while cells(&line) < width {
        line.push(' ');
    }
    line
}

/// Build a single prompt row (label + body + cursor + padding) padded to
/// `width` columns. `cursor_in_row == usize::MAX` means "no cursor on
/// this row"; any smaller value is the absolute **column** inside the body
/// where the `▍` marker should appear.
///
/// The marker is inserted between two characters, so its column has to be
/// turned back into a character index: it goes before the first character
/// whose cell range starts past `cursor_in_row`. Under an all-ASCII draft
/// that is the same index the layout's column reports; for a CJK draft it
/// keeps the marker on the same cell the layout measured (LUM-1336).
fn build_prompt_row(
    prefix: &str,
    body: &str,
    cursor_in_row: usize,
    width: usize,
    draw_cursor: bool,
) -> String {
    let mut line = String::with_capacity(width + 1);
    line.push_str(prefix);
    let body_chars: Vec<char> = body.chars().collect();
    if draw_cursor {
        let mut col = 0usize;
        let mut at = body_chars.len();
        for (index, ch) in body_chars.iter().enumerate() {
            if col >= cursor_in_row {
                at = index;
                break;
            }
            col += cell_width(*ch);
        }
        for ch in &body_chars[..at] {
            line.push(*ch);
        }
        line.push('▍');
        for ch in &body_chars[at..] {
            line.push(*ch);
        }
    } else {
        for ch in &body_chars {
            line.push(*ch);
        }
    }
    while cells(&line) < width {
        line.push(' ');
    }
    line
}

/// First row of the composer window for this frame, given the window start
/// the caller remembered (`start`).
///
/// The window follows the caret one row at a time: it shrinks to the caret's
/// row when the caret moved above it, and grows to `caret + 1 - show_rows`
/// when the caret moved below it. That is upstream's rule in `render`
/// (`packages/tui/src/components/editor.ts:532-540`), and it is what makes
/// the window *smooth*: re-anchoring on the caret's page instead (the
/// pre-LUM-1317 compromise) moved the window by a whole page as soon as the
/// caret crossed a page boundary. The last window is clamped, so the tail of
/// a long draft is always fully visible.
fn follow_cursor(start: usize, cursor_row: usize, total_rows: usize, show_rows: usize) -> usize {
    let show_rows = show_rows.max(1);
    let max_start = total_rows.saturating_sub(show_rows);
    let mut start = start.min(max_start);
    if cursor_row < start {
        start = cursor_row;
    } else if cursor_row >= start + show_rows {
        start = cursor_row + 1 - show_rows;
    }
    start.min(max_start)
}

/// Chrome column for a scrolled composer window: `↑` / `↓` plus as many
/// digits of the hidden row count as the gutter holds.
///
/// Upstream writes the count into the editor's top / bottom border
/// (`createScrollBorder`, `packages/tui/src/components/editor.ts:266`),
/// which the Rust prompt does not have. The columns the label occupies are
/// its only stable chrome, so the marker lives there and the count is
/// dropped when the gutter cannot hold it — the arrow is the affordance, the
/// count is the detail.
fn scroll_marker(direction: char, hidden: usize, gutter: usize) -> String {
    let mut marker = String::new();
    if gutter == 0 {
        return marker;
    }
    marker.push(direction);
    let count = hidden.to_string();
    // One of the gutter's columns is the arrow itself.
    if count.chars().count() < gutter {
        marker.push_str(&count);
    }
    while marker.chars().count() < gutter {
        marker.push(' ');
    }
    marker
}

/// Split a string into `(before, after)` halves at the nth character
/// (not byte) boundary. If `idx` is out of range, the entire string is
/// returned as the first half.
fn split_at_char(text: &str, idx: usize) -> (&str, &str) {
    if idx >= text.chars().count() {
        return (text, "");
    }
    for (count, (byte_idx, _)) in text.char_indices().enumerate() {
        if count == idx {
            return (&text[..byte_idx], &text[byte_idx..]);
        }
    }
    (text, "")
}

/// Truncate `text` to at most `max` **columns**, keeping whole characters
/// (a wide glyph is never cut in half).
fn column_truncate(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let width = cell_width(ch);
        if used + width > max {
            break;
        }
        used += width;
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::KeyModifiers;

    #[test]
    fn renders_label_and_cursor() {
        let mut prompt = Prompt::new("> ");
        prompt.editor_mut().insert_str("abc");
        let line = prompt.render_line(20);
        assert!(line.starts_with("> "));
        // Cursor is at the end of the buffer, so the visual "▍"
        // appears after the text.
        assert!(line.contains("abc▍"));
    }

    #[test]
    fn renders_placeholder_when_empty() {
        let mut prompt = Prompt::new("> ");
        prompt.set_placeholder("type a prompt");
        let line = prompt.render_line(20);
        assert!(line.starts_with("> "));
        assert!(line.contains("type a prompt"));
        assert!(!line.contains('▍'));
    }

    #[test]
    fn submit_emits_text_and_caller_resets() {
        let mut prompt = Prompt::new("> ");
        prompt.editor_mut().insert_str("hello");
        let action = prompt.handle_key(Key::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            PromptAction::Submit(text) => assert_eq!(text, "hello"),
            other => panic!("unexpected action: {:?}", other),
        }
    }

    #[test]
    fn interrupt_and_eof_bubble_through() {
        let mut prompt = Prompt::new("> ");
        assert_eq!(
            prompt.handle_key(Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            PromptAction::Interrupt,
        );
        assert_eq!(
            prompt.handle_key(Key::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            PromptAction::Eof,
        );
    }

    #[test]
    fn reset_clears_buffer_and_history() {
        let mut prompt = Prompt::new("> ");
        prompt.editor_mut().insert_str("hello");
        prompt.push_history("hello");
        prompt.reset();
        assert!(prompt.is_empty());
        assert_eq!(prompt.editor().history_len(), 0);
    }

    /// An empty prompt still claims one row so callers always get a
    /// paintable region, and on a region that can afford the composer
    /// border the row count carries its two rules.
    #[test]
    fn line_count_is_at_least_one_for_empty_and_short_buffers() {
        let mut prompt = Prompt::new("> ");
        assert_eq!(prompt.line_count(20, 8), 1, "borderless by default");
        prompt.set_border(true);
        assert_eq!(prompt.line_count(20, 8), 3, "one draft row plus two rules");
        prompt.editor_mut().insert_str("hello");
        assert_eq!(prompt.line_count(20, 8), 3);
        // The cap bounds the *draft*; the rules are chrome on top of it.
        assert_eq!(prompt.line_count(20, 1), 3);
        prompt.set_border(false);
        assert_eq!(prompt.line_count(20, 1), 1);
    }

    /// The composer paints upstream's two rules unless the caller turns them
    /// off or the region it is granted cannot afford them.
    #[test]
    fn border_visibility_follows_the_region_height() {
        let mut prompt = Prompt::new("> ");
        prompt.set_border(true);
        prompt.editor_mut().insert_str("hello");
        // A region with room for two rules and a draft row draws them…
        assert_eq!(prompt.render_frame(20, 8, 3, 0).rows.len(), 3);
        // …and the same draft in a two-row region degrades to the bare rows.
        let squeezed = prompt.render_frame(20, 8, 2, 0);
        assert_eq!(squeezed.rows.len(), 2);
        assert!(squeezed
            .rows
            .iter()
            .all(|row| row.kind == PromptRowKind::Body));
        prompt.set_border(false);
        assert!(!prompt.border_visible(20));
        assert_eq!(prompt.line_count(20, 8), 1, "no rules, one draft row");
    }

    /// Word-wrap: at width 10 (label "> " = 2, body width 8) the buffer
    /// "hello world foo bar" splits across three rows, and the two rules
    /// ride on top of them.
    #[test]
    fn line_count_wraps_long_buffers() {
        let mut prompt = Prompt::new("> ");
        prompt.set_border(true);
        prompt.editor_mut().insert_str("hello world foo bar");
        assert_eq!(prompt.line_count(10, 8), 5, "3 draft rows + 2 rules");
        prompt.set_border(false);
        assert_eq!(prompt.line_count(10, 8), 3, "the draft alone");
        assert_eq!(prompt.line_count(10, 2), 2, "no rules below 3 rows");
    }

    /// Hard line breaks (`\n`) become row boundaries even when the
    /// individual pieces are short.
    #[test]
    fn line_count_respects_hard_breaks() {
        let mut prompt = Prompt::new("> ");
        prompt.set_border(true);
        prompt.editor_mut().insert_str("first\nsecond");
        assert_eq!(prompt.line_count(20, 8), 4, "2 draft rows + 2 rules");
    }

    /// The row count is clamped to `max_rows`, so callers that pass a
    /// small cap never see more rows than the cap.
    #[test]
    fn line_count_clamps_to_max_rows() {
        let mut prompt = Prompt::new("> ");
        prompt.editor_mut().insert_str("a b c d e f g h i j k l m");
        // Width 10 (body 8) → at least 5 rows; cap to 3.
        assert_eq!(prompt.line_count(10, 3), 3);
    }

    /// `render_lines` with `max_rows == 1` and a buffer that needs more
    /// rows shows the cursor row (the scroll behaviour) and marks the
    /// draft as continuing above it. The label only appears on the very
    /// first row of the draft, so a scrolled window uses the marker
    /// column, not the label.
    #[test]
    fn render_lines_with_one_row_keeps_the_legacy_compat() {
        let mut prompt = Prompt::new("> ");
        prompt.editor_mut().insert_str("abcdefghij");
        let (lines, scroll) = prompt.render_lines(8, 1, 0);
        assert_eq!(lines.len(), 1);
        assert_eq!(scroll, 1, "the window follows the caret off the first row");
        // Body width is 8 - 2 (label) = 6; only the trailing 4 chars fit.
        assert!(lines[0].starts_with('↑'), "{:?}", lines[0]);
        assert!(!lines[0].contains("abcdef"));
        assert!(lines[0].contains("ghij"));
        assert!(lines[0].contains('▍'));
    }

    /// Multi-row render: one rule above, one below, label only on the
    /// draft's first row, continuation rows indented to keep the buffer
    /// column aligned. No side borders — upstream's editor says so in as
    /// many words (`components/editor.ts:597`).
    #[test]
    fn render_lines_multi_row_indents_continuation_rows() {
        let mut prompt = Prompt::new("> ");
        prompt.set_border(true);
        prompt.editor_mut().insert_str("hello world foo bar");
        let (lines, scroll) = prompt.render_lines(10, 8, 0);
        assert!(lines.len() >= 5, "got {} lines", lines.len());
        assert_eq!(scroll, 0, "nothing is hidden when the draft fits");
        let top = &lines[0];
        let bottom = &lines[lines.len() - 1];
        assert_eq!(top, &"─".repeat(10), "plain rule while nothing is hidden");
        assert_eq!(bottom, &"─".repeat(10));
        assert!(
            !lines.iter().any(|line| line.contains('│')),
            "no side borders"
        );
        assert!(lines[1].starts_with("> "), "{:?}", lines[1]);
        for cont in &lines[2..lines.len() - 1] {
            assert!(
                cont.starts_with("  "),
                "continuation row must align with the label: {:?}",
                cont
            );
        }
    }

    /// The row kinds let the App colour the rules without guessing: the
    /// frame is exactly `Border … Body … Border`, with the search row
    /// (when open) inside the box.
    #[test]
    fn render_frame_marks_the_rules_and_the_search_row() {
        let mut prompt = Prompt::new("> ");
        prompt.set_border(true);
        prompt.editor_mut().insert_str("hello");
        let frame = prompt.render_frame(12, 5, 5, 0);
        let kinds: Vec<PromptRowKind> = frame.rows.iter().map(|row| row.kind).collect();
        assert_eq!(kinds.first(), Some(&PromptRowKind::Border));
        assert_eq!(kinds.last(), Some(&PromptRowKind::Border));
        assert!(kinds[1..kinds.len() - 1]
            .iter()
            .all(|kind| *kind == PromptRowKind::Body));
        // The reverse search owns a row inside the box, right under the top
        // rule, and the box keeps its shape.
        prompt.editor_mut().begin_history_search();
        let frame = prompt.render_frame(12, 5, 5, 0);
        let kinds: Vec<PromptRowKind> = frame.rows.iter().map(|row| row.kind).collect();
        assert_eq!(kinds[0], PromptRowKind::Border);
        assert_eq!(kinds[1], PromptRowKind::Search);
        assert_eq!(kinds.last(), Some(&PromptRowKind::Border));
        assert_eq!(
            frame.rows.len(),
            5,
            "the region never needs a row it has not got"
        );
    }

    /// Upstream `createScrollBorder`: the hidden-row count is centred in the
    /// rule, and a rule that cannot fit the label is truncated with `...`.
    #[test]
    fn scroll_borders_report_the_hidden_rows_like_upstream() {
        assert_eq!(border_row('↑', 0, 6), "──────");
        assert_eq!(border_row('↑', 3, 20), "───── ↑ 3 more ─────");
        assert_eq!(border_row('↓', 123, 30), "───────── ↓ 123 more ─────────");
        // Too narrow even for the short form: head of it plus `...`.
        assert_eq!(border_row('↑', 3, 11), "─── ↑ 3 ...");
        assert_eq!(border_row('↓', 7, 3), "...");
    }

    /// A clipped draft reports its hidden rows on the rule (upstream's
    /// `createScrollBorder`) instead of in the label gutter.
    #[test]
    fn a_clipped_draft_reports_its_window_on_the_rules() {
        let mut prompt = Prompt::new("> ");
        prompt.set_border(true);
        prompt
            .editor_mut()
            .insert_str("alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima");
        // Width 20 (body 18) → 5 draft rows; a 5-row region keeps 3 of them
        // and the caret is at the end, so only the top rule reports.
        let (lines, scroll) = prompt.render_lines(20, 5, 0);
        assert_eq!(lines.len(), 5);
        assert!(lines[0].contains('↑'), "top rule: {:?}", lines[0]);
        assert!(lines[0].contains("more"), "{:?}", lines[0]);
        assert_eq!(lines[0].chars().count(), 20, "the rule spans the width");
        assert_eq!(
            lines[4],
            "─".repeat(20),
            "nothing is hidden below the caret"
        );
        assert!(scroll > 0, "the window followed the caret");
        // The gutter no longer carries markers, so the draft's own rows are
        // label-indented exactly like the unscrolled case.
        assert!(lines[2].starts_with("  "), "{:?}", lines[2]);
    }

    /// A frame that cannot afford the rules keeps the port's older in-gutter
    /// `↑` / `↓` markers, so a clipped draft always says that there is more.
    #[test]
    fn a_frame_without_rules_keeps_the_gutter_markers() {
        let mut prompt = Prompt::new("> ");
        prompt
            .editor_mut()
            .insert_str("a b c d e f g h i j k l m n o p");
        let (lines, scroll) = prompt.render_lines(6, 2, 0);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with('↑'), "{:?}", lines[0]);
        assert!(scroll > 0);
        // The caret sits on the last draft row, which is the second visible
        // row — nothing is below it, so only the top marker shows.
        assert!(lines[1].contains('▍'), "{:?}", lines[1]);
        assert!(!lines[1].starts_with('↓'), "{:?}", lines[1]);
    }

    /// The cursor marker shows on the visual row that contains the
    /// cursor, not on rows past it.
    #[test]
    fn render_lines_places_the_cursor_on_the_right_row() {
        let mut prompt = Prompt::new("> ");
        // 14 chars; width 6 (label "> ") → body width 4.
        prompt.editor_mut().insert_str("abcdefghij klm");
        let (lines, _) = prompt.render_lines(6, 8, 0);
        let cursors: Vec<_> = lines.iter().map(|line| line.contains('▍')).collect();
        // Exactly one row carries the cursor.
        let cursor_rows = cursors.iter().filter(|c| **c).count();
        assert_eq!(cursor_rows, 1, "cursors on rows: {cursors:?}");
    }

    /// When the draft needs more rows than `max_rows`, the window follows
    /// the cursor and the rule reports the rows it hid; the caller gets the
    /// window start back so the next frame continues from it.
    #[test]
    fn render_lines_caps_to_max_rows_keeping_the_cursor() {
        let mut prompt = Prompt::new("> ");
        prompt.set_border(true);
        prompt
            .editor_mut()
            .insert_str("a b c d e f g h i j k l m n o p");
        // Width 20 (body 18) → 2 draft rows; a 3-row region is one rule, one
        // draft row and one rule.
        let (lines, scroll) = prompt.render_lines(20, 3, 0);
        assert_eq!(lines.len(), 3);
        assert_eq!(scroll, 1, "the single draft row is the caret's row");
        // The cursor row must still be in the rendered slice.
        assert!(lines.iter().any(|line| line.contains('▍')));
        // The top rule says how much is above it; nothing is below, so the
        // bottom rule stays a plain line.
        assert!(lines[0].contains('↑'), "{:?}", lines[0]);
        assert_eq!(lines[2], "─".repeat(20));
    }

    /// An empty prompt renders the placeholder + padding across the
    /// requested row count, inside the rules.
    #[test]
    fn render_lines_empty_buffer_uses_placeholder() {
        let mut prompt = Prompt::new("> ");
        prompt.set_border(true);
        prompt.set_placeholder("type a prompt");
        let (lines, scroll) = prompt.render_lines(20, 3, 0);
        assert_eq!(lines.len(), 3);
        assert_eq!(scroll, 0);
        assert_eq!(lines[0], "─".repeat(20));
        assert!(lines[1].contains("type a prompt"), "{:?}", lines[1]);
        assert_eq!(lines[2], "─".repeat(20));
        for row in &lines[1..] {
            // No label / no body / padded with spaces — the cursor only
            // appears on rows that own the buffer, which is none here.
            assert!(!row.contains('▍'));
        }
    }
}
