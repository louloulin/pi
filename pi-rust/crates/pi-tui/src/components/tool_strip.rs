//! Tool strip — a 1-row indicator above the status bar that lights up
//! while a tool is running or the model is thinking.
//!
//! Borrowed from `nanopi/src/mode/tui.rs` (the docked-bottom 5-row layout:
//! overlay + status + input + cwd + stats). nanopi dedicates one row to
//! "tool running / thinking" so the user can tell at a glance whether the
//! session is busy without parsing the footer. The pi-rust port already
//! has a `status_bar` row for that role, but the spinner is buried among
//! tokens and model info — easy to miss on a glance. This module owns
//! the *extra* row above it that resolves to "tool running / thinking /
//! blank".
//!
//! The strip is **stateless**: the caller decides which
//! [`ToolStripState`] applies this frame and the renderer paints one row
//! accordingly. Rendering into a fresh buffer uses [`write_styled_line`]
//! so the row's colours come straight from the theme — no escape codes
//! leak into the cell text. When the state is [`ToolStripState::Idle`]
//! the strip is a no-op (the caller still owns the row, so the row's
//! rest is whatever the App painted last frame).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::cell::Cell;
use std::time::{Duration, Instant};

use crate::components::loader::{Spinner, SPINNER_INTERVAL_MS};
use crate::theme::{Theme, ThemeBg, ThemeColor};
use crate::utils::styled::{write_styled_line, SpanStyle, StyledLine, StyledSpan};

/// What the strip is currently showing.
///
/// Idle rows are blank — the caller does not allocate a strip row at
/// all, but the renderer treats Idle and Busy as the same thing so a
/// caller that always reserves the row still gets the cheap path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStripState<'a> {
    /// No tool running, no thinking in flight. The strip is blank.
    Idle,
    /// A tool is running. `name` is the tool display label (e.g.
    /// `bash ls -la`), `started_at` is when the current run started.
    RunningTool {
        name: &'a str,
        started_at: Instant,
    },
    /// The model is thinking. `started_at` is when the thinking began.
    Thinking { started_at: Instant },
}

impl<'a> ToolStripState<'a> {
    /// Build a state from the current App snapshot. Centralises the
    /// "is there a tool running?" decision so the render loop does not
    /// grow a parallel data path. Returns [`Idle`](Self::Idle) when no
    /// tool is in flight and the model is not streaming reasoning.
    pub fn from_app<F>(is_busy: bool, tool_name: F, thinking: bool, run_started: Option<Instant>, thinking_started: Option<Instant>) -> Self
    where
        F: FnOnce() -> Option<&'a str>,
    {
        if let Some(name) = tool_name() {
            if let Some(start) = run_started {
                return ToolStripState::RunningTool {
                    name,
                    started_at: start,
                };
            }
        }
        if thinking {
            if let Some(start) = thinking_started {
                return ToolStripState::Thinking { started_at: start };
            }
        }
        let _ = is_busy;
        ToolStripState::Idle
    }

    /// Whether the strip should allocate a row at all. Callers that
    /// follow nanopi's "always reserve 1 row" design get `true`
    /// unconditionally; callers that fold the strip into the existing
    /// status row only allocate when this returns `true`.
    pub fn is_active(self) -> bool {
        !matches!(self, ToolStripState::Idle)
    }
}

/// The renderer. Holds the spinner cursor so successive frames move the
/// glyph without the caller having to thread a frame number through the
/// App. The cursor lives in a [`Cell`] so the render path — which only
/// has `&self` access to the App — can still advance it.
#[derive(Debug, Clone)]
pub struct ToolStrip {
    spinner: Cell<Spinner>,
    /// When the spinner was last advanced. The renderer itself advances
    /// when the elapsed time has crossed [`SPINNER_INTERVAL_MS`], so a
    /// caller that just forwards `render` calls per tick gets animation
    /// for free.
    last_advance: Cell<Instant>,
}

impl Default for ToolStrip {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolStrip {
    /// New renderer with a fresh spinner.
    pub fn new() -> Self {
        Self {
            spinner: Cell::new(Spinner::new()),
            last_advance: Cell::new(Instant::now()),
        }
    }

    /// Current spinner frame. Exposed so tests can pin a frame and so
    /// the App can hand the same spinner out to other consumers (the
    /// status bar's busy indicator).
    pub fn spinner(&self) -> Spinner {
        self.spinner.get()
    }

    /// Mutable spinner handle. The App advances this from its existing
    /// tick so the spinner stays in sync with the rest of the UI.
    pub fn spinner_mut(&mut self) -> &mut Spinner {
        self.spinner.get_mut()
    }

    /// Pin the spinner clock. Exposed so tests can force the next
    /// render to cross the [`SPINNER_INTERVAL_MS`] threshold without
    /// sleeping for 80 ms.
    pub fn set_last_advance(&self, instant: Instant) {
        self.last_advance.set(instant);
    }

    /// Render one row of the strip into `buf`. The caller owns the row
    /// — a [`Rect`] with `height == 1` (or `height == 0` for the
    /// no-row case, which is a no-op).
    ///
    /// `Idle` states paint a Reset row so any stale content from a
    /// previous frame is wiped. The live path paints into a freshly
    /// reset back buffer, so this is mostly defensive — but the snapshot
    /// tests render into a fresh buffer that starts filled with blanks
    /// anyway, so the cost is negligible.
    ///
    /// Takes `&self` (not `&mut self`) so the App's `render_to_buffer_impl`
    /// — which is called from a `&self` context — can paint the strip
    /// without threading a mutable handle through. The spinner and the
    /// `last_advance` clock advance through interior mutability.
    pub fn render(&self, state: ToolStripState<'_>, area: Rect, buf: &mut Buffer, theme: &Theme) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let now = Instant::now();
        if now.duration_since(self.last_advance.get()) >= Duration::from_millis(SPINNER_INTERVAL_MS) {
            let mut spinner = self.spinner.get();
            spinner.advance();
            self.spinner.set(spinner);
            self.last_advance.set(now);
        }
        let line = build_line(state, now, self.spinner.get().frame(), area.width as usize);
        write_styled_line(buf, area.x, area.y, area.width, &line, theme);
    }
}

/// Build the styled line for a single strip row. Split out so the tests
/// can pin the exact spans without spinning up a buffer.
fn build_line<'a>(state: ToolStripState<'a>, now: Instant, frame: char, width: usize) -> StyledLine {
    match state {
        ToolStripState::Idle => blank_row(width),
        ToolStripState::RunningTool { name, started_at } => {
            let elapsed = now.duration_since(started_at);
            tool_running_line(frame, name, elapsed, width)
        }
        ToolStripState::Thinking { started_at } => {
            let elapsed = now.duration_since(started_at);
            thinking_line(frame, elapsed, width)
        }
    }
}

/// A blank Reset row. nanopi paints a blank row when nothing is
/// running; we mirror that to keep the strip's "always paints something"
/// invariant simple. `width` pads the row so every cell carries the
/// Reset style — without the pad the live render leaves stale content
/// from the previous frame visible at the row's right edge.
fn blank_row(width: usize) -> StyledLine {
    let cols = width.max(1);
    vec![StyledSpan::new(" ".repeat(cols), SpanStyle::PLAIN)]
}

/// The tool-running line: full-width navy bg, spinner glyph in
/// `bar_fg`/bold, the tool name, then `  Elapsed X.Ys` in `bar_hint`
/// italic.
///
/// Matches nanopi `tui.rs:5322` (the `blue_bg = Color::Indexed(24)` that
/// paints the entire row, fg `bar_fg = Color::Indexed(230)`).
///
/// `width` is the full row width: the function pads with a
/// bg-carrying blank span so the navy background reaches the row's
/// right edge. Without the pad, [`write_styled_line`] leaves the trailing
/// cells at `Color::Reset` and the user sees a half-painted row.
fn tool_running_line(frame: char, name: &str, elapsed: Duration, width: usize) -> StyledLine {
    let elapsed_text = format!("Elapsed {:.1}s", elapsed.as_secs_f64());
    let head = format!("{frame} {name}  {elapsed_text}");
    let head_cols = crate::utils::width::columns(&head);
    let pad_cols = width.saturating_sub(head_cols);
    let spinner_style = SpanStyle::fg_bg(ThemeColor::ToolTitle, ThemeBg::ToolPendingBg).bold();
    let body_style = SpanStyle::fg_bg(ThemeColor::ToolTitle, ThemeBg::ToolPendingBg);
    let elapsed_style =
        SpanStyle::fg_bg(ThemeColor::FgSecondary, ThemeBg::ToolPendingBg).italic();
    let head_spans: StyledLine = vec![
        StyledSpan::new(format!("{frame} "), spinner_style),
        StyledSpan::new(name.to_string(), body_style),
        StyledSpan::new("  ".to_string(), body_style),
        StyledSpan::new(elapsed_text, elapsed_style),
    ];
    let mut line = head_spans;
    if pad_cols > 0 {
        line.push(StyledSpan::new(" ".repeat(pad_cols), body_style));
    }
    line
}

/// The thinking line: italic sage text on Reset, spinner glyph + the
/// word `thinking` + `(elapsed)`. `width` pads the row so the Reset
/// style reaches the row's right edge.
fn thinking_line(frame: char, elapsed: Duration, width: usize) -> StyledLine {
    let elapsed_text = format!("{:.1}s", elapsed.as_secs_f64());
    let style = SpanStyle::fg(ThemeColor::ThinkingText).italic();
    let head = format!("{frame} thinking ({elapsed_text})");
    let head_cols = crate::utils::width::columns(&head);
    let pad_cols = width.saturating_sub(head_cols);
    let mut line: StyledLine = vec![
        StyledSpan::new(format!("{frame} "), style),
        StyledSpan::new("thinking ".to_string(), style),
        StyledSpan::new(format!("({elapsed_text})"), style),
    ];
    if pad_cols > 0 {
        line.push(StyledSpan::new(" ".repeat(pad_cols), style));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::builtin_theme;
    use crate::theme::ColorMode;
    use ratatui::buffer::Cell;

    fn cell_at<'a>(buf: &'a Buffer, x: u16, y: u16) -> &'a Cell {
        buf.cell((x, y)).expect("cell exists")
    }

    fn empty_buf(width: u16) -> Buffer {
        Buffer::empty(Rect {
            x: 0,
            y: 0,
            width,
            height: 1,
        })
    }

    /// Idle renders a blank row — no spinner, no name, no elapsed.
    #[test]
    fn idle_renders_a_blank_row() {
        let theme = builtin_theme("dark", ColorMode::TrueColor).expect("dark theme");
        let mut buf = empty_buf(40);
        let strip = ToolStrip::new();
        strip.render(ToolStripState::Idle, Rect { x: 0, y: 0, width: 40, height: 1 }, &mut buf, &theme);
        let text: String = (0..40).map(|x| cell_at(&buf, x, 0).symbol().to_string()).collect();
        assert_eq!(text.trim_end(), "", "idle row should be blank");
    }

    /// Tool running paints the spinner + tool name + elapsed onto a
    /// `ToolPendingBg` background. The whole row's cells carry the bg,
    /// not just the text columns.
    #[test]
    fn tool_running_paints_full_width_pending_bg() {
        let theme = builtin_theme("dark", ColorMode::TrueColor).expect("dark theme");
        let mut buf = empty_buf(60);
        let strip = ToolStrip::new();
        let started = Instant::now() - Duration::from_millis(1200);
        strip.render(
            ToolStripState::RunningTool {
                name: "bash ls -la",
                started_at: started,
            },
            Rect { x: 0, y: 0, width: 60, height: 1 },
            &mut buf,
            &theme,
        );
        // Locate the first text column — cells before it are spinner, cells
        // after it are blank-but-styled (the user sees a full-width navy
        // bar with text on the left, not a text run followed by
        // uncoloured background).
        let first_text_x = (0..60)
            .find(|&x| cell_at(&buf, x, 0).symbol() != " ")
            .unwrap_or(60);
        // Find the trailing edge of the text so the post-text region
        // is well-defined.
        let last_text_x = (0..60)
            .rfind(|&x| cell_at(&buf, x, 0).symbol() != " ")
            .unwrap_or(0);
        for x in (last_text_x + 1)..60 {
            let cell = cell_at(&buf, x, 0);
            // Symbol is space, but the bg carries ToolPendingBg.
            assert_eq!(
                cell.symbol(),
                " ",
                "post-text cell at col {x} should be space"
            );
            assert_eq!(
                cell.style().bg,
                Some(ratatui::style::Color::Rgb(40, 40, 50)),
                "post-text cell at col {x} must carry ToolPendingBg"
            );
        }
        // The spinner cell is the first non-blank cell, with bold + ToolPendingBg.
        let spinner_cell = cell_at(&buf, first_text_x, 0);
        assert!(
            spinner_cell.symbol().chars().any(|c| !c.is_whitespace()),
            "spinner glyph present at col {first_text_x}"
        );
        assert_eq!(
            spinner_cell.style().bg,
            Some(ratatui::style::Color::Rgb(40, 40, 50)),
            "spinner must sit on ToolPendingBg (40,40,50)"
        );
    }

    /// Thinking paints italic sage text without a bg — visually
    /// distinct from a tool run so the user does not confuse them.
    #[test]
    fn thinking_uses_italic_thinking_text_no_bg() {
        let theme = builtin_theme("dark", ColorMode::TrueColor).expect("dark theme");
        let mut buf = empty_buf(40);
        let strip = ToolStrip::new();
        let started = Instant::now() - Duration::from_millis(2500);
        strip.render(
            ToolStripState::Thinking { started_at: started },
            Rect { x: 0, y: 0, width: 40, height: 1 },
            &mut buf,
            &theme,
        );
        let text: String = (0..40).map(|x| cell_at(&buf, x, 0).symbol().to_string()).collect();
        assert!(
            text.contains("thinking"),
            "thinking row should include the word, got {text:?}"
        );
        assert!(
            text.contains("2.5s"),
            "thinking row should include elapsed seconds, got {text:?}"
        );
        let first_text_x = (0..40).find(|&x| cell_at(&buf, x, 0).symbol() != " ").unwrap();
        let cell = cell_at(&buf, first_text_x, 0);
        assert!(
            cell.style().add_modifier.contains(ratatui::style::Modifier::ITALIC),
            "thinking row must be italic"
        );
        assert!(
            cell.style().bg.is_none()
                || matches!(cell.style().bg, Some(ratatui::style::Color::Reset)),
            "thinking row must not paint a bg (Reset or None), got {:?}",
            cell.style().bg
        );
    }

    /// The spinner advances once per ~80 ms so a sequence of renders
    /// produces a moving glyph. Pin the frame text after a known
    /// advance to confirm the cursor moves.
    #[test]
    fn spinner_advances_per_render_within_the_interval() {
        let theme = builtin_theme("dark", ColorMode::TrueColor).expect("dark theme");
        let mut buf = empty_buf(20);
        let strip = ToolStrip::new();
        let started = Instant::now() - Duration::from_millis(200);
        strip.render(
            ToolStripState::Thinking { started_at: started },
            Rect { x: 0, y: 0, width: 20, height: 1 },
            &mut buf,
            &theme,
        );
        let first_frame = cell_at(&buf, 0, 0).symbol().to_string();
        // Force an advance by rewinding the timestamp so the next render
        // crosses the interval.
        strip.last_advance.set(Instant::now() - Duration::from_millis(SPINNER_INTERVAL_MS * 2));
        let mut buf2 = empty_buf(20);
        strip.render(
            ToolStripState::Thinking { started_at: started },
            Rect { x: 0, y: 0, width: 20, height: 1 },
            &mut buf2,
            &theme,
        );
        let second_frame = cell_at(&buf2, 0, 0).symbol().to_string();
        assert_ne!(
            first_frame, second_frame,
            "spinner frame must change after the interval elapses"
        );
    }
}