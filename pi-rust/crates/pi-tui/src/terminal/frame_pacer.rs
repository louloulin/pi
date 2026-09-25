//! Differential render + frame pacing.
//!
//! Upstream's `TuiBase.renderInternal` writes the active tree into a
//! buffer and only flushes the changed cells to the terminal
//! (`packages/tui/src/tui.ts:702-748`). This module ports that
//! behavior:
//!
//! * [`FramePacer`] owns the buffer + diff state and emits ANSI
//!   sequences only for changed cells.
//! * [`DifferentialRender`] is the cell-level diff helper that
//!   compares two `Buffer`s row-by-row.
//!
//! `FramePacer` is intentionally minimal: it tracks the previous
//! frame's cells, renders the current frame, and produces an ANSI
//! string of `move_to + style + symbol` updates. The host decides
//! when to flush (typically once per app tick or when something
//! explicitly requests a redraw).

use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

/// Compute the diff between `previous` and `current`, returning one
/// [`CellDiff`] per changed cell.
///
/// Both buffers must share the same `area`. Coordinates in the
/// returned diffs are buffer-local.
pub fn diff_cells(previous: &Buffer, current: &Buffer) -> Vec<CellDiff> {
    let mut diffs = Vec::new();
    let area = current.area;
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            let p = previous.cell((x, y));
            let c = current.cell((x, y));
            if let (Some(prev), Some(cur)) = (p, c) {
                if cells_differ(prev, cur) {
                    diffs.push(CellDiff {
                        x,
                        y,
                        symbol: cur.symbol().to_string(),
                        style: cur.style(),
                    });
                }
            }
        }
    }
    diffs
}

/// One changed cell in a differential frame.
#[derive(Debug, Clone)]
pub struct CellDiff {
    pub x: u16,
    pub y: u16,
    pub symbol: String,
    pub style: Style,
}

fn cells_differ(a: &Cell, b: &Cell) -> bool {
    a.symbol() != b.symbol() || a.style() != b.style()
}

/// Encode a single cell into the ANSI sequence the host writes to the
/// terminal: `move_to` + style + symbol.
fn encode_cell(diff: &CellDiff) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = write!(out, "\x1b[{};{}H", diff.y + 1, diff.x + 1);
    out.push_str(&style_to_ansi(diff.style));
    out.push_str(&diff.symbol);
    out
}

fn style_to_ansi(style: Style) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if let Some(fg) = style.fg {
        let _ = write!(out, "{}", color_to_ansi(fg, true));
    }
    if let Some(bg) = style.bg {
        let _ = write!(out, "{}", color_to_ansi(bg, false));
    }
    if style.add_modifier.contains(Modifier::BOLD) {
        out.push_str("\x1b[1m");
    }
    if style.add_modifier.contains(Modifier::ITALIC) {
        out.push_str("\x1b[3m");
    }
    if style.add_modifier.contains(Modifier::UNDERLINED) {
        out.push_str("\x1b[4m");
    }
    if style.add_modifier.contains(Modifier::REVERSED) {
        out.push_str("\x1b[7m");
    }
    out.push_str("\x1b[0m");
    out
}

fn color_to_ansi(c: Color, is_fg: bool) -> String {
    use std::fmt::Write as _;
    let prefix = if is_fg { "38;2" } else { "48;2" };
    match c {
        Color::Rgb(r, g, b) => format!("\x1b[{};{};{};{}m", prefix, r, g, b),
        Color::Red => format!("\x1b[{};255;0;0m", prefix),
        Color::Green => format!("\x1b[{};0;255;0m", prefix),
        Color::Yellow => format!("\x1b[{};255;255;0m", prefix),
        Color::Blue => format!("\x1b[{};0;0;255m", prefix),
        Color::Magenta => format!("\x1b[{};255;0;255m", prefix),
        Color::Cyan => format!("\x1b[{};0;255;255m", prefix),
        Color::White => format!("\x1b[{};255;255;255m", prefix),
        Color::Black => format!("\x1b[{};0;0;0m", prefix),
        Color::Gray => format!("\x1b[{};128;128;128m", prefix),
        Color::DarkGray => format!("\x1b[{};64;64;64m", prefix),
        Color::LightRed => format!("\x1b[{};255;96;96m", prefix),
        Color::LightGreen => format!("\x1b[{};96;255;96m", prefix),
        Color::LightYellow => format!("\x1b[{};255;255;96m", prefix),
        Color::LightBlue => format!("\x1b[{};96;96;255m", prefix),
        Color::LightMagenta => format!("\x1b[{};255;96;255m", prefix),
        Color::LightCyan => format!("\x1b[{};96;255;255m", prefix),
        _ => format!("\x1b[{};200;200;200m", prefix),
    }
}

/// Encode a list of cell diffs into a single ANSI string.
pub fn encode_diffs(diffs: &[CellDiff]) -> String {
    let mut out = String::new();
    for diff in diffs {
        out.push_str(&encode_cell(diff));
    }
    out.push_str("\x1b[0m");
    out
}

/// Frame pacer — owns the previous frame's buffer, produces a
/// differential update on each render, and clears the cursor home on
/// reset.
///
/// Mirrors `TuiBase.renderInternal` (`packages/tui/src/tui.ts:702-748`)
/// and the `FramePacer` helper in the upstream main loop.
pub struct FramePacer {
    previous: Option<Buffer>,
    full_redraw: bool,
}

impl FramePacer {
    /// Build a new pacer.
    pub fn new() -> Self {
        Self { previous: None, full_redraw: true }
    }

    /// Force the next frame to be a full redraw.
    pub fn invalidate(&mut self) {
        self.full_redraw = true;
    }

    /// Diff the new frame against the cached previous frame and return
    /// the ANSI to send to the terminal. The new frame is cached as
    /// the new baseline.
    ///
    /// `renderer` writes into `next`. The pacer owns the previous
    /// frame's cells, so callers don't need to clone the buffer.
    pub fn render_with<F>(&mut self, area: Rect, mut renderer: F) -> String
    where
        F: FnMut(&mut Buffer),
    {
        let mut next = Buffer::empty(area);
        renderer(&mut next);

        if self.full_redraw || self.previous.is_none() {
            self.full_redraw = false;
            self.previous = Some(next.clone());
            return encode_full(&next);
        }

        let prev = self.previous.as_ref().unwrap();
        let diffs = diff_cells(prev, &next);
        self.previous = Some(next);
        encode_diffs(&diffs)
    }

    /// Drop the cached previous frame so the next render is a full
    /// redraw.
    pub fn reset(&mut self) {
        self.previous = None;
        self.full_redraw = true;
    }
}

impl Default for FramePacer {
    fn default() -> Self {
        Self::new()
    }
}

fn encode_full(buf: &Buffer) -> String {
    let mut out = String::new();
    out.push_str("\x1b[?25l"); // hide cursor
    out.push_str(&encode_diffs(&diff_cells(&Buffer::empty(buf.area), buf)));
    out.push_str("\x1b[?25h"); // show cursor
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(buf: &mut Buffer, x: u16, y: u16, ch: char, style: Style) {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_symbol(&ch.to_string());
            cell.set_style(style);
        }
    }

    #[test]
    fn diff_picks_up_symbol_change() {
        let area = Rect::new(0, 0, 3, 1);
        let mut prev = Buffer::empty(area);
        let mut cur = Buffer::empty(area);
        fill(&mut prev, 0, 0, 'a', Style::default());
        fill(&mut cur, 0, 0, 'b', Style::default());
        let diffs = diff_cells(&prev, &cur);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].symbol, "b");
    }

    #[test]
    fn diff_ignores_unchanged_cells() {
        let area = Rect::new(0, 0, 3, 1);
        let mut prev = Buffer::empty(area);
        let mut cur = Buffer::empty(area);
        fill(&mut prev, 1, 0, 'a', Style::default());
        fill(&mut cur, 1, 0, 'a', Style::default());
        let diffs = diff_cells(&prev, &cur);
        assert!(diffs.is_empty());
    }

    #[test]
    fn frame_pacer_first_render_is_full_redraw() {
        let mut pacer = FramePacer::new();
        let area = Rect::new(0, 0, 2, 1);
        let out = pacer.render_with(area, |buf| {
            fill(buf, 0, 0, 'x', Style::default());
            fill(buf, 1, 0, 'y', Style::default());
        });
        assert!(out.contains('x'));
        assert!(out.contains('y'));
    }

    #[test]
    fn frame_pacer_subsequent_renders_are_minimal() {
        let mut pacer = FramePacer::new();
        let area = Rect::new(0, 0, 2, 1);
        pacer.render_with(area, |buf| {
            fill(buf, 0, 0, 'x', Style::default());
            fill(buf, 1, 0, 'y', Style::default());
        });
        let out = pacer.render_with(area, |buf| {
            fill(buf, 0, 0, 'x', Style::default());
            fill(buf, 1, 0, 'z', Style::default());
        });
        // Only one cell changed: 'y' -> 'z'. The other cell's symbol
        // does not appear in the diff.
        assert!(!out.contains('x'));
        assert!(out.contains('z'));
    }

    #[test]
    fn frame_pacer_invalidate_forces_full_redraw() {
        let mut pacer = FramePacer::new();
        let area = Rect::new(0, 0, 1, 1);
        pacer.render_with(area, |buf| fill(buf, 0, 0, 'a', Style::default()));
        pacer.invalidate();
        let out = pacer.render_with(area, |buf| fill(buf, 0, 0, 'a', Style::default()));
        assert!(out.contains('a'));
    }

    #[test]
    fn frame_pacer_reset_clears_cache() {
        let mut pacer = FramePacer::new();
        pacer.reset();
        let area = Rect::new(0, 0, 1, 1);
        let out = pacer.render_with(area, |buf| fill(buf, 0, 0, 'a', Style::default()));
        assert!(out.contains('a'));
    }
}