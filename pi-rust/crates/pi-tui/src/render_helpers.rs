//! Rendering helper functions extracted from [`App`].
//!
//! These are pure rendering functions that paint into a ratatui [`Buffer`].
//! They have been separated from [`crate::app::App`] to reduce file size
//! and improve maintainability.

use std::sync::atomic::Ordering;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::widgets::Widget;

use crate::app::{SearchState, SelectionPoint};
use crate::editor::is_bash_mode;
use crate::keybindings::get_keybindings;
use crate::locale::format_chord;
use crate::message::MessageView;
use crate::prompt::Prompt;
use crate::slash_menu::SlashMenuWidget;
use crate::styled::{
    plain_text, write_styled_line, write_styled_line_ellipsized, SpanStyle, StyledLine, StyledSpan,
};
use crate::theme::{thinking_border_color, Theme, ThemeBg, ThemeColor};
use crate::viewport::{ScrollbarGeometry, ViewportGeometry};
use crate::width::{char_columns, columns};

/// Leading half of the "jump to latest" pill's label.
const SCROLL_TO_END_LABEL: &str = " ↓ Jump to latest message ";

/// Leading half of the "cut above" hint's label.
const TRUNCATED_ABOVE_LEAD: &str = " ⋯ ";

/// Paint the search matches into the already-rendered message area.
///
/// Non-current matches get an underline, the current one bold + reverse —
/// upstream's `searchMatchStyle` / `searchCurrentMatchStyle`
/// (`packages/tui/src/tui-alt-screen.ts:118-121`). The port keeps the
/// themed foreground already in the cell instead of replacing it, so a
/// highlighted token stays readable in any theme.
pub(crate) fn apply_search_highlight(
    messages: &MessageView,
    search: Option<&SearchState>,
    area: Rect,
    buf: &mut Buffer,
) {
    let Some(state) = search else {
        return;
    };
    if state.matches.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let (visible_start, lines) = messages.visible_lines(area.width, area.height);
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
            // Search segments are terminal-cell spans (see
            // [`crate::search`]); the buffer is addressed in cells too, so
            // the clamp is a cell width, not a character count.
            let len = crate::width::columns(&text);
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

/// Paint the chat-log scrollbar into the message viewport's last column.
///
/// Upstream's `paintScrollbar` / `renderScrollView`
/// (`packages/tui/src/layout.ts:280-326`): the track is a dim vertical
/// rule, the thumb a heavier block. While the bar is hovered or dragged
/// the thumb switches to a solid block and both parts go bold — see the
/// module docs on why the port adds the modifier. The bar is a no-op when
/// the transcript fits the viewport ([`ViewportGeometry::scrollbar_geometry`] is
/// `None`).
pub fn apply_scrollbar(
    scrollbar_hover: bool,
    scrollbar_drag: Option<crate::viewport::ScrollbarDrag>,
    geometry: Option<ScrollbarGeometry>,
    theme: &Theme,
    area: Rect,
    buf: &mut Buffer,
) {
    let Some(geometry) = geometry else {
        return;
    };
    if area.width == 0 || area.height == 0 {
        return;
    }
    let active = scrollbar_hover || scrollbar_drag.is_some();
    let thumb_end = geometry.thumb_top.saturating_add(geometry.thumb_height);
    for row in 0..geometry.track_height {
        let y = geometry.track_top + row;
        let in_thumb = y >= geometry.thumb_top && y < thumb_end;
        let (glyph, slot) = if in_thumb {
            (if active { '█' } else { '┃' }, ThemeColor::ScrollbarThumb)
        } else {
            ('│', ThemeColor::ScrollbarTrack)
        };
        let style = if active {
            SpanStyle::fg(slot).bold()
        } else {
            SpanStyle::fg(slot)
        };
        if let Some(cell) = buf.cell_mut((geometry.column, y)) {
            cell.set_char(glyph);
            cell.set_style(style.to_style(theme));
        }
    }
}

/// Record the "jump to latest" pill rectangle in the viewport for pointer hit testing.
pub fn record_scroll_to_end(
    viewport: &ViewportGeometry,
    row: u16,
    column: u16,
    width: u16,
) {
    viewport.scroll_to_end.0.store(row, Ordering::Relaxed);
    viewport.scroll_to_end.1.store(column, Ordering::Relaxed);
    viewport.scroll_to_end.2.store(width, Ordering::Relaxed);
}

/// Clear the "jump to latest" pill rectangle record.
pub fn clear_scroll_to_end(viewport: &ViewportGeometry) {
    viewport.scroll_to_end.2.store(0, Ordering::Relaxed);
}

/// Record the "truncated above" hint rectangle in the viewport for pointer hit testing.
pub fn record_truncated_above(
    viewport: &ViewportGeometry,
    row: u16,
    column: u16,
    width: u16,
) {
    viewport.truncated_above.0.store(row, Ordering::Relaxed);
    viewport.truncated_above.1.store(column, Ordering::Relaxed);
    viewport.truncated_above.2.store(width, Ordering::Relaxed);
}

/// Clear the "truncated above" hint rectangle record.
pub fn clear_truncated_above(viewport: &ViewportGeometry) {
    viewport.truncated_above.2.store(0, Ordering::Relaxed);
}

/// The pill's label: ` ↓ Jump to latest message · <shortcut> `, exactly
/// upstream's string (down to the leading space) with the shortcut
/// resolved from `tui.altScreen.bottom`
/// (`packages/coding-agent/src/modes/interactive/tui-renderer.ts:29-33`).
///
/// An unbound action drops the ` · <shortcut>` half rather than
/// rendering an empty shortcut, which is the same rule `/hotkeys`
/// follows.
fn scroll_to_end_label() -> StyledSpan {
    let shortcut = get_keybindings()
        .get_keys("tui.altScreen.bottom")
        .iter()
        .map(|chord| format_chord(chord))
        .collect::<Vec<_>>()
        .join("/");
    let text = if shortcut.is_empty() {
        SCROLL_TO_END_LABEL.to_string()
    } else {
        format!("{SCROLL_TO_END_LABEL}· {shortcut} ")
    };
    StyledSpan {
        text,
        style: SpanStyle::fg_bg(ThemeColor::Text, ThemeBg::SelectedBg),
        link: None,
    }
}

/// Composite the pill onto the bottom row of the message viewport when
/// the reader has scrolled away from the tail, and record where it
/// landed for the pointer.
///
/// Upstream's `compositeScrollToEndIndicator`
/// (`packages/tui/src/tui-alt-screen.ts:1617-1637`): only when the scroll
/// view follows the end but is not at it, drawn on the viewport's last
/// row, horizontally centred, truncated at — and never wider than — the
/// space left of the scrollbar column.
pub fn paint_scroll_to_end(
    viewport: &ViewportGeometry,
    is_following: bool,
    max_scroll: usize,
    scrollbar_geometry: Option<ScrollbarGeometry>,
    message_area: Rect,
    theme: &Theme,
    buf: &mut Buffer,
) {
    // Every path out of here clears the record, so a stale rectangle can
    // never keep swallowing clicks after the pill is gone.
    clear_scroll_to_end(viewport);
    if message_area.width == 0 || message_area.height == 0 {
        return;
    }
    // Nothing to jump to when the transcript already fits, and no pill
    // while the viewport is pinned to the tail.
    if is_following || max_scroll == 0 {
        return;
    }
    let available = match scrollbar_geometry {
        Some(geometry) if geometry.column > message_area.x => {
            (geometry.column - message_area.x).min(message_area.width)
        }
        _ => message_area.width,
    };
    let label = scroll_to_end_label();
    let label_width = crate::hyperlink::visible_width(&label.text) as u16;
    if label_width == 0 || available == 0 {
        return;
    }
    let width = label_width.min(available);
    let column = message_area.x + (available - width) / 2;
    let row = message_area.y + message_area.height - 1;
    // Blank the covered cells first: the pill is shorter than the
    // transcript line underneath it, and ratatui only emits the cells
    // this buffer changed, so an unblanked row left the old text bleeding
    // through (`reset` also drops the covered cells' colours).
    for offset in 0..width {
        if let Some(cell) = buf.cell_mut((column + offset, row)) {
            cell.reset();
        }
    }
    let line = [label];
    write_styled_line_ellipsized(buf, column, row, width, &line, theme);
    record_scroll_to_end(viewport, row, column, width);
}

/// The hint's label: ` ⋯ <n> line(s) above · <shortcut> ` — the prompt
/// half of the affordance, resolved from `tui.altScreen.top` (the key it
/// actually points at) with the same unbound-action rule
/// [`scroll_to_end_label`] follows.
fn truncated_above_label(hidden: usize) -> StyledSpan {
    let shortcut = get_keybindings()
        .get_keys("tui.altScreen.top")
        .iter()
        .map(|chord| format_chord(chord))
        .collect::<Vec<_>>()
        .join("/");
    let noun = if hidden == 1 { "line" } else { "lines" };
    let base = format!("{TRUNCATED_ABOVE_LEAD}{hidden} {noun} above ");
    let text = if shortcut.is_empty() {
        base
    } else {
        format!("{base}· {shortcut} ")
    };
    StyledSpan {
        text,
        style: SpanStyle::fg(ThemeColor::Muted),
        link: None,
    }
}

/// How many lines of the block at the viewport's top edge are off-screen
/// above it, or `None` when there is nothing to disclose.
///
/// The transcript is laid out as items, and [`MessageView::item_line_ranges`]
/// gives each item's row span in the same layout the renderer uses, so a
/// top edge that falls strictly *inside* one span is exactly "this block
/// is cut" — as opposed to a top edge that coincides with a boundary,
/// which every scrolled transcript has and which hides nothing.
///
/// Only reported while the viewport is pinned to the tail. A detached
/// reader already has the jump-to-latest pill telling them where they
/// are; showing both would spend a second transcript row to say what the
/// first one already says.
pub fn truncated_above_lines(messages: &MessageView, width: u16, height: u16, is_following: bool, resolved_scroll: usize) -> Option<usize> {
    // One row is not a viewport: the hint would be the whole transcript.
    if width == 0 || height < 2 {
        return None;
    }
    if !is_following || resolved_scroll != 0 {
        return None;
    }
    let total = messages.line_count(width);
    let start = total.saturating_sub(height as usize);
    if start == 0 {
        return None;
    }
    let (item_start, _) = messages
        .item_line_ranges(width)
        .into_iter()
        .find(|(from, to)| *from <= start && start < *to)?;
    let hidden = start - item_start;
    (hidden > 0).then_some(hidden)
}

/// Disclose that the block at the top edge of the viewport continues
/// above it, on the viewport's first row, left-aligned and never wider
/// than the space left of the scrollbar column.
///
/// The covered row is a continuation row of a block the reader cannot see
/// the head of, so the trade is one unreadable old line for knowing that
/// there is something to look for — the alternative (paint nothing, as
/// upstream does) leaves a cut block looking like a block that starts
/// there.
pub fn paint_truncated_above(
    viewport: &ViewportGeometry,
    messages: &MessageView,
    scrollbar_geometry: Option<ScrollbarGeometry>,
    width: u16,
    height: u16,
    is_following: bool,
    resolved_scroll: usize,
    message_area: Rect,
    theme: &Theme,
    buf: &mut Buffer,
) {
    // Every path out clears the record, so a stale rectangle can never
    // keep swallowing clicks after the hint is gone.
    clear_truncated_above(viewport);
    if message_area.width == 0 || message_area.height < 2 {
        return;
    }
    let Some(hidden) = truncated_above_lines(messages, width, height, is_following, resolved_scroll) else {
        return;
    };
    let available = match scrollbar_geometry {
        Some(geometry) if geometry.column > message_area.x => {
            (geometry.column - message_area.x).min(message_area.width)
        }
        _ => message_area.width,
    };
    let label = truncated_above_label(hidden);
    let label_width = crate::hyperlink::visible_width(&label.text) as u16;
    if label_width == 0 || available == 0 {
        return;
    }
    let width = label_width.min(available);
    let column = message_area.x;
    let row = message_area.y;
    // Blank the covered cells first, for the same reason the pill does:
    // ratatui only emits the cells this buffer changed, so an unblanked
    // row would let the covered text bleed through the hint's tail.
    for offset in 0..width {
        if let Some(cell) = buf.cell_mut((column + offset, row)) {
            cell.reset();
        }
    }
    let line = [label];
    write_styled_line_ellipsized(buf, column, row, width, &line, theme);
    record_truncated_above(viewport, row, column, width);
}

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

/// Paint the active selection into the already-rendered message area by
/// adding the reversed-video modifier to the selected cells.
pub(crate) fn apply_selection_highlight(
    messages: &MessageView,
    selection: Option<&crate::app::Selection>,
    area: Rect,
    buf: &mut Buffer,
) {
    let Some(selection) = selection else {
        return;
    };
    let Some((start, end)) = selection.bounds() else {
        return;
    };
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (visible_start, lines) = messages.visible_lines(area.width, area.height);
    let visible_end = visible_start + lines.len();
    let first = start.line.max(visible_start);
    let last = end.line.min(visible_end.saturating_sub(1));
    if first > last {
        return;
    }
    for line_idx in first..=last {
        let row = (line_idx - visible_start) as u16;
        let y = area.y + row;
        let text = plain_text(&lines[row as usize]);
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
        // `from` / `to` are character offsets; the buffer is addressed in
        // terminal cells. A wide glyph covers two cells, so painting one
        // reversed cell per character would leave the right half of every
        // CJK glyph unhighlighted and drift left of the text.
        let from = crate::width::columns_before(&text, from);
        let to = crate::width::columns_before(&text, to);
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

/// Paint the active composer drag-selection into the editor area by adding
/// the reversed-video modifier to the selected cells.
///
/// This mirrors `apply_selection_highlight` for the transcript, but operates
/// on the composer rows painted by `paint_prompt`. The composer cells are
/// painted first (by `paint_prompt`), then this function overlays the REVERSED
/// modifier on the selected range.
pub fn apply_composer_selection_highlight(
    editor: &crate::editor::Editor,
    composer_rect: Rect,
    body_width: usize,
    scroll: usize,
    label: &str,
    buf: &mut Buffer,
) {
    let Some((start, end)) = editor.composer_selection() else {
        return;
    };
    if composer_rect.width == 0 || composer_rect.height == 0 {
        return;
    }
    let display = editor.display_text();
    let layout = crate::visual_text::VisualLayout::new(&display, body_width);
    let rows = layout.rows();

    let start_row = layout.row_of(start);
    let end_row = layout.row_of(end);

    let visible_start = scroll;
    let visible_end = visible_start + composer_rect.height as usize;
    let first = start_row.max(visible_start);
    let last = end_row.min(visible_end.saturating_sub(1));
    if first > last {
        return;
    }

    let label = columns(label);
    for row_idx in first..=last {
        let visual = match rows.get(row_idx) {
            Some(v) => v,
            None => continue,
        };
        let y = composer_rect.y + (row_idx - visible_start) as u16;
        if y >= composer_rect.y + composer_rect.height {
            break;
        }

        let row_len = visual.text.chars().count();
        // `start` and `end` are byte offsets (from `cursor_at`), but `visual.start`
        // is a character index.  Binary-search `source` to find the character position
        // of each byte offset within this row.
        let byte_to_char_idx = |byte_offset: usize| -> usize {
            // `source` stores byte offsets of characters in the full display string.
            // Find the rightmost i where `source[i] <= byte_offset`.
            match visual.source.binary_search(&byte_offset) {
                Ok(i) => i,
                Err(i) => i.saturating_sub(1),
            }
        };
        let start_char = byte_to_char_idx(start);
        let end_char = byte_to_char_idx(end);
        let from = if row_idx == start_row {
            start_char.min(row_len)
        } else {
            0
        };
        let to = if row_idx == end_row {
            // `end` is inclusive; +1 moves past the last char so its trailing cell
            // is highlighted. Clamp at `row_len` for the trailing-position case.
            (end_char + 1).min(row_len)
        } else {
            row_len
        };

        // Convert character offsets to terminal-cell columns, matching how
        // `apply_selection_highlight` maps characters to buffer cells.
        let from_col = crate::width::columns_before(&visual.text, from);
        let to_col = crate::width::columns_before(&visual.text, to);

        for col in from_col..to_col {
            // Body starts after the label column; match paint_prompt's layout.
            let x = composer_rect.x + label as u16 + col as u16;
            if x >= composer_rect.x + composer_rect.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.modifier |= Modifier::REVERSED;
            }
        }
    }
}

/// Paint extension region lines into `rect`.
///
/// Width truncation is *marked* (`…`), never silent: every extension
/// widget — the startup header, the above/below-editor widgets, the
/// footer, and a `custom` overlay — goes through here, and a row that is
/// wider than the terminal is the normal case on a 44-column window. A
/// silent clip made `hints hidden on a short terminal — Alt+H sho`
/// indistinguishable from a line whose author wrote exactly that
/// (LUM-1412). Height truncation stays silent by design: a block with a
/// taller tail than the region is already summarised by the caller's own
/// content, and a mark on the region's last row would be confusable with
/// a width clip.
pub fn paint_extension_lines(rect: Rect, lines: &[StyledLine], theme: &Theme, buf: &mut Buffer) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    for (row, line) in lines.iter().enumerate() {
        if row as u16 >= rect.height {
            break;
        }
        write_styled_line_ellipsized(
            buf,
            rect.x,
            rect.y + row as u16,
            rect.width,
            line,
            theme,
        );
    }
}

/// Record the composer rectangle for the pointer hit test, or clear it
/// when the frame painted no composer (`None`).
pub fn record_composer_area(viewport: &ViewportGeometry, rect: Option<Rect>) {
    match rect {
        Some(rect) => {
            viewport.composer_origin.0.store(rect.x, Ordering::Relaxed);
            viewport.composer_origin.1.store(rect.y, Ordering::Relaxed);
            viewport.composer_size.0.store(rect.width, Ordering::Relaxed);
            viewport.composer_size.1.store(rect.height, Ordering::Relaxed);
        }
        None => viewport.composer_size.0.store(0, Ordering::Relaxed),
    }
}

/// Record the autocomplete dropdown rectangle and the candidate window it
/// painted, or clear both when the frame painted no dropdown.
///
/// `first_item` is the index of the first candidate row drawn and
/// `item_rows` how many candidate rows followed; the trailing `(n/m)`
/// counter row is excluded because it is not clickable. Clearing is not
/// optional: a stale rectangle would keep swallowing clicks meant for the
/// transcript after the dropdown closed.
pub fn record_autocomplete_area(
    viewport: &ViewportGeometry,
    rect: Option<Rect>,
    first_item: usize,
    item_rows: usize,
) {
    match rect {
        Some(rect) => {
            viewport.autocomplete_origin.0.store(rect.x, Ordering::Relaxed);
            viewport.autocomplete_origin.1.store(rect.y, Ordering::Relaxed);
            viewport.autocomplete_size.0.store(rect.width, Ordering::Relaxed);
            viewport.autocomplete_size.1.store(rect.height, Ordering::Relaxed);
            viewport.autocomplete_first_item.store(first_item, Ordering::Relaxed);
            viewport.autocomplete_item_rows.store(item_rows, Ordering::Relaxed);
        }
        None => {
            viewport.autocomplete_size.0.store(0, Ordering::Relaxed);
            viewport.autocomplete_size.1.store(0, Ordering::Relaxed);
            viewport.autocomplete_item_rows.store(0, Ordering::Relaxed);
        }
    }
}

/// Paint the queued-messages block into `rect`.
///
/// The block is only planned when something is queued
/// ([`MessageView::pending_block_rows`]), so an empty queue paints
/// nothing and the rows stay with the transcript. Rows are blanked before
/// the line is written: this region is carved out of what the transcript
/// painted last frame, and the block's spacer row is genuinely empty.
pub fn paint_pending_block(
    messages: &MessageView,
    rect: Rect,
    theme: &Theme,
    buf: &mut Buffer,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let dequeue_chord = dequeue_chord();
    let lines = messages.pending_lines(&dequeue_chord);
    for (row, line) in lines.iter().enumerate() {
        let row = u16::try_from(row).unwrap_or(u16::MAX);
        if row >= rect.height {
            break;
        }
        let y = rect.y + row;
        for col in 0..rect.width {
            if let Some(cell) = buf.cell_mut((rect.x + col, y)) {
                cell.reset();
            }
        }
        // `…` in the last column the line did not fit in, instead of the
        // silent clip every pre-LUM-1412 region performed.
        write_styled_line_ellipsized(buf, rect.x, y, rect.width, line, theme);
    }
}

/// Resolved through the live keybinding table, so an override in
/// `keybindings.json` shows up here exactly as it does in `/hotkeys` and
/// the startup header (LUM-1447/1450). The fallback is the shipped
/// default from upstream's table (`packages/coding-agent/src/core/keybindings.ts:138`:
/// `alt+up`, `alt+q` on Windows), used when no `app.*` table is installed
/// — a bare `pi-tui` host, which is what the frame tests are.
fn dequeue_chord() -> String {
    // A **raw** chord into `format_chord`, so the unknown-id fallback is
    // spelled the way every other surface spells it (`Alt+Up`), while a
    // resolvable id keeps the registry's effective set — the same rule the
    // startup header's hint rows use.
    let fallback = if cfg!(windows) { "alt+q" } else { "alt+up" };
    crate::keybindings::key_text_or(
        "app.message.dequeue",
        &format_chord(fallback),
    )
}

/// Paint the built-in prompt into the editor region. The region may be
/// taller than one row when the buffer wraps (see
/// [`crate::Prompt::render_lines`]); the cap lives in
/// [`crate::app::AppConfig::composer_max_rows`].
///
/// The frame is also where the composer window's two remembered facts
/// come from: the draft rows it showed (`composer_scroll`) and the rows
/// it could show (`composer_window`). Both are read back by the key path
/// before [`crate::Prompt::render_lines`] runs again.
/// The chord the queued-messages hint advertises (`app.message.dequeue`).
///
/// Resolved through the live keybinding table, so an override in
/// `keybindings.json` shows up here exactly as it does in `/hotkeys` and
/// the startup header (LUM-1447/1450). The fallback is the shipped
/// default from upstream's table (`packages/coding-agent/src/core/keybindings.ts:138`:
/// `alt+up`, `alt+q` on Windows), used when no `app.*` table is installed
/// — a bare `pi-tui` host, which is what the frame tests are.
pub fn paint_prompt(
    prompt: &Prompt,
    composer_max_rows: usize,
    thinking_level: pi_agent_core::ThinkingLevel,
    viewport: &ViewportGeometry,
    rect: Rect,
    theme: &Theme,
    buf: &mut Buffer,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    // The composer's rectangle is the pointer's click target for the
    // caret (see `App::prompt_mouse_gesture`), and the pointer arrives
    // between renders.
    record_composer_area(viewport, Some(rect));
    let max_rows = (rect.height as usize).min(composer_max_rows.max(1));
    // Record the body width the wrap uses, so the next key press measures
    // the draft the same way this frame did (see
    // `App::composer_body_width`).
    viewport.composer_body_width.store(prompt.body_width(rect.width) as u16, Ordering::Relaxed);
    viewport.composer_window.store(max_rows as u16, Ordering::Relaxed);
    let scroll = viewport.composer_scroll.load(Ordering::Relaxed);
    let (lines, scroll) = prompt.render_lines(rect.width, max_rows, scroll);
    viewport.composer_scroll.store(scroll, Ordering::Relaxed);
    // Upstream paints the editor chrome in `bashMode` while the buffer is
    // a `!` submission, otherwise in the thinking level's border colour
    // (`updateEditorBorderColor`,
    // `interactive-mode.ts:4166-4174`). The Rust prompt has no border, so
    // the label carries the colour instead: bash mode wins, the thinking
    // level colours everything else.
    let label_slot = if is_bash_mode(&prompt.text()) {
        ThemeColor::BashMode
    } else {
        thinking_border_color(thinking_level)
    };
    let label_style = Some(SpanStyle::fg(label_slot).to_style(theme));
    let label_width = columns(prompt.label()) as u16;
    for (row, line) in lines.iter().enumerate() {
        let y = rect.y + row as u16;
        if y >= rect.y + rect.height {
            break;
        }
        // This loop paints cells directly (the label owns a style of its
        // own, so it cannot go through `write_styled_line`), which means
        // it has to advance by **columns**: a CJK or emoji glyph occupies
        // two cells, and the cell its second column covers must be
        // blanked so a stale glyph from the previous frame cannot show
        // through. Without this a Chinese draft was written one cell per
        // character and the row collapsed to a fraction of the draft
        // (`> 中` for a 44-column draft, LUM-1418).
        let mut col = 0usize;
        for ch in line.chars() {
            let glyph_width = char_columns(ch);
            if col + glyph_width > rect.width as usize {
                break;
            }
            let x = rect.x + col as u16;
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(ch);
                if let Some(style) = label_style {
                    // Only the first row owns the label; subsequent rows
                    // are blank-padded with spaces.
                    if row == 0 && col < label_width as usize {
                        cell.set_style(style);
                    }
                }
            }
            for offset in 1..glyph_width {
                if let Some(cell) = buf.cell_mut((rect.x + (col + offset) as u16, y)) {
                    cell.reset();
                }
            }
            col += glyph_width;
        }
    }
}

/// Paint the composer's autocomplete dropdown into the rows directly
/// above the editor, on top of the message view.
///
/// The dropdown itself belongs to [`crate::Editor`] (candidates,
/// `SelectList` layout, windowing, selection); the App only places it and
/// hands the rows to the buffer. Upstream draws the list above the input
/// and grows it towards older output (`Editor.renderAutocomplete`), so the
/// rows are anchored to the editor's top edge and the prompt line is never
/// covered. Without this the provider could be installed and still show
/// nothing, which is exactly the state LUM-1236 found: the engine and the
/// keyboard map were in place, the paint call was not.
///
/// The rows arrive as theme-slot spans from the shared `SelectList` layout
/// (LUM-1305), so the dropdown paints the same roles the modal pickers do —
/// plain label, `muted` description column, `accent` over `selectedBg` for
/// the highlighted row — instead of the App re-deriving a style from the
/// text.
pub fn paint_autocomplete(
    _messages: &MessageView,
    prompt: &Prompt,
    viewport: &ViewportGeometry,
    message_area: Rect,
    editor_area: Rect,
    theme: &Theme,
    buf: &mut Buffer,
) {
    let editor = prompt.editor();
    if !editor.is_showing_autocomplete() || editor_area.width == 0 {
        // No dropdown (or no editor row to anchor it to): clear the hit
        // box so the pointer cannot land on a rectangle no longer drawn.
        record_autocomplete_area(viewport, None, 0, 0);
        return;
    }
    // The dropdown borrows rows from the transcript viewport: never
    // paint over the header / above-editor regions, and never over the
    // editor row itself.
    let available = editor_area.y.saturating_sub(message_area.y) as usize;
    if available == 0 {
        record_autocomplete_area(viewport, None, 0, 0);
        return;
    }
    let width = editor_area.width as usize;
    // The candidate window the renderer is about to draw, so the pointer
    // hit test reads the same arithmetic the paint did. The `(n/m)`
    // counter row is not a candidate: it is excluded from `item_rows`.
    let (window_start, window_end) = editor.autocomplete_window();
    let candidate_rows = window_end.saturating_sub(window_start);
    let mut rows = editor.autocomplete_render_styled_lines(width);
    if rows.is_empty() {
        record_autocomplete_area(viewport, None, 0, 0);
        return;
    }
    // On a short terminal keep the candidates nearest the prompt. The
    // list is already windowed around the selection, so the tail is the
    // part the user is actually steering — the rows dropped here shift
    // the first painted candidate, which the hit test has to know.
    let dropped = rows.len().saturating_sub(available);
    if dropped > 0 {
        rows.drain(..dropped);
    }
    let first_row = editor_area.y - rows.len() as u16;
    record_autocomplete_area(
        viewport,
        Some(Rect {
            x: editor_area.x,
            y: first_row,
            width: editor_area.width,
            height: rows.len() as u16,
        }),
        window_start + dropped,
        candidate_rows.saturating_sub(dropped),
    );
    for (offset, line) in rows.iter().enumerate() {
        let y = first_row + offset as u16;
        // Blank the borrowed row first: a candidate is shorter than the
        // transcript line it covers, and ratatui only emits the cells this
        // buffer changed — an unblanked row left the old output bleeding
        // through on the right of the dropdown. `reset` also drops the
        // covered cell's colours, so the row behind cannot tint the one on
        // top. Same rule as the selector overlay below.
        for col in 0..editor_area.width {
            if let Some(cell) = buf.cell_mut((editor_area.x + col, y)) {
                cell.reset();
            }
        }
        write_styled_line(buf, editor_area.x, y, editor_area.width, line, theme);
    }
}

/// Paint the slash menu overlay directly above the editor.
///
/// The menu appears when the user types `/` in the editor and shows
/// matching slash commands with descriptions. Mirrors Martty's slash menu UI.
pub fn paint_slash_menu(
    slash_menu: &crate::slash_menu::SlashMenu,
    editor_area: Rect,
    theme: &Theme,
    buf: &mut Buffer,
) {
    if !slash_menu.is_visible() || editor_area.width == 0 {
        return;
    }
    let widget = SlashMenuWidget::new(slash_menu, editor_area, theme);
    widget.render(editor_area, buf);
}

/// Chords are left-aligned into at least this many columns on a `?` overlay
/// row, so the descriptions line up (codex draws the same table in
/// `bottom_pane/footer.rs`). The actual column is the widest chord in the set
/// plus 3 for padding and the separating space.
const SHORTCUT_CHORD_COLUMN: usize = 12;

/// The `?` overlay title (English).
const SHORTCUT_OVERLAY_TITLE_EN: &str = "Keyboard Shortcuts";
/// The `?` overlay title (Chinese).
const SHORTCUT_OVERLAY_TITLE_ZH: &str = "键盘快捷键";

/// The `?` overlay close hint (English).
const SHORTCUT_OVERLAY_CLOSE_EN: &str = "Press ? or Esc to close";
/// The `?` overlay close hint (Chinese).
const SHORTCUT_OVERLAY_CLOSE_ZH: &str = "按 ? 或 Esc 关闭";

/// A narrow terminal only has room for one column of shortcuts.
const SHORTCUT_COLUMN_MIN: usize = 10;

/// The overlay separator is not wider than this.
const SHORTCUT_RULE_MAX: usize = 60;

use crate::locale::Locale;

/// Paint the `?` shortcut overlay directly above the composer.
///
/// Bottom-anchored to the editor row and clipped to the message viewport,
/// so the startup header above and the composer / status rows below are
/// untouched (the rule the selector overlay follows). The panel borrows
/// transcript rows instead of claiming a chrome row: opening help must not
/// reflow the conversation the reader was looking at, and closing it
/// restores the same frame.
pub fn paint_shortcut_overlay(
    shortcut_overlay_visible: bool,
    hint_entries: &[(String, String)],
    locale: &Locale,
    editor_area: Rect,
    message_area: Rect,
    theme: &Theme,
    buf: &mut Buffer,
) {
    if !shortcut_overlay_visible || editor_area.width == 0 {
        return;
    }
    let available = editor_area.y.saturating_sub(message_area.y) as usize;
    if available == 0 {
        return;
    }
    let width = editor_area.width as usize;
    let mut lines = shortcut_overlay_lines(hint_entries, locale, width);
    if lines.is_empty() {
        return;
    }
    // A short terminal keeps the head (title + the first chords); the tail
    // is the `…`-free part a reader can reach with a taller window, and
    // `/hotkeys` remains the exhaustive list.
    lines.truncate(available);
    let first_row = editor_area.y - lines.len() as u16;
    for (offset, line) in lines.iter().enumerate() {
        let y = first_row + offset as u16;
        // Blank the borrowed row first: overlay rows are shorter than the
        // transcript lines they cover, and ratatui only emits the cells
        // this buffer changed — an unblanked row leaves the transcript
        // bleeding through (same rule as the dropdown and the modals).
        for col in 0..editor_area.width {
            if let Some(cell) = buf.cell_mut((editor_area.x + col, y)) {
                cell.reset();
            }
        }
        write_styled_line(buf, editor_area.x, y, editor_area.width, line, theme);
    }
}

/// Build the `?` overlay's hint lines from the current keybinding table.
fn shortcut_overlay_lines(
    entries: &[(String, String)],
    locale: &Locale,
    width: usize,
) -> Vec<StyledLine> {
    if entries.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<StyledLine> = Vec::with_capacity(entries.len() + 3);
    lines.push(vec![
        StyledSpan::new(
            format!(
                "  {}",
                locale.tr(SHORTCUT_OVERLAY_TITLE_EN, SHORTCUT_OVERLAY_TITLE_ZH)
            ),
            SpanStyle::fg(ThemeColor::Accent).bold(),
        ),
        StyledSpan::new(
            format!(
                "  {}",
                locale.tr(SHORTCUT_OVERLAY_CLOSE_EN, SHORTCUT_OVERLAY_CLOSE_ZH)
            ),
            SpanStyle::fg(ThemeColor::Dim),
        ),
    ]);
    lines.push(vec![StyledSpan::new(
        "─".repeat(width.min(SHORTCUT_RULE_MAX)),
        SpanStyle::fg(ThemeColor::Dim),
    )]);
    // Two columns only when each one can hold a cell; a second column
    // whose descriptions wrap mid-word reads worse than one long list.
    let chord_column = entries
        .iter()
        .map(|(chord, _)| columns(chord) + 3)
        .max()
        .unwrap_or(0)
        .max(SHORTCUT_CHORD_COLUMN);
    let cell_width = width / 2;
    if cell_width >= SHORTCUT_COLUMN_MIN {
        let half = entries.len().div_ceil(2);
        for index in 0..half {
            let mut row: StyledLine = Vec::new();
            push_shortcut_cell(&mut row, &entries[index], cell_width, chord_column);
            if let Some(entry) = entries.get(index + half) {
                push_shortcut_cell(&mut row, entry, cell_width, chord_column);
            }
            lines.push(row);
        }
    } else {
        for entry in entries {
            let mut row: StyledLine = Vec::new();
            push_shortcut_cell(&mut row, entry, width, chord_column);
            lines.push(row);
        }
    }
    lines
}

/// Append one shortcut entry (a chord + description) to a [`StyledLine`].
/// `width` is the row width, `chord_column` the minimum column reserved
/// for the chord.
fn push_shortcut_cell(
    row: &mut StyledLine,
    (chord, description): &(String, String),
    width: usize,
    chord_column: usize,
) {
    // Chord
    row.push(StyledSpan::new(
        format!("  {chord}"),
        SpanStyle::fg(ThemeColor::Accent).bold(),
    ));
    // Description — truncated to fit the remaining row width.
    let remaining = width.saturating_sub(chord_column);
    let desc = if description.len() > remaining {
        let mut chars = description.chars();
        let truncated: String = chars.by_ref().take(remaining.saturating_sub(1)).collect();
        format!("{truncated}…")
    } else {
        description.clone()
    };
    row.push(StyledSpan::new(format!("  {desc}"), SpanStyle::fg(ThemeColor::Text)));
}
