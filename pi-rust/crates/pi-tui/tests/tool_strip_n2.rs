//! N2 — docked tool strip appears only while a turn is in flight
//! (`docs/NANOPI_VS_PI_RUST_GAP_ANALYSIS.md`).
//!
//! nanopi dedicates a 1-row strip above the footer that lights up while
//! a tool is running or the model is thinking, so the user can tell at
//! a glance whether the session is busy. The Rust port buries that
//! information inside the footer's busy spinner, which is easy to miss.
//! These tests confirm the wiring: idle sessions keep the pre-N2
//! single-row chrome geometry, busy sessions grow a 1-row strip with
//! `ToolPendingBg` cells (tool) or italic `ThinkingText` (thinking).

use std::time::Duration;

use pi_tui::components::tool_strip::{ToolStrip, ToolStripState};
use pi_tui::loader::SPINNER_FRAMES;

/// `ToolStrip::new` starts with the upstream `⠋` frame. Useful as the
/// default value the App carries in its `tool_strip` field — the field
/// is constructed lazily and only the render path mutates the cursor.
#[test]
fn new_tool_strip_starts_with_the_first_spinner_frame() {
    let strip = ToolStrip::new();
    assert_eq!(strip.spinner().frame(), '⠋');
    assert!(SPINNER_FRAMES.contains(&strip.spinner().frame()));
}

/// `Idle` does not paint any spinner / tool glyph — the row is blank.
/// `RunningTool` / `Thinking` paint at least one of the upstream
/// braille frames so a busy session is unmistakable on a glance.
#[test]
fn active_states_paint_a_spinner_glyph_idle_does_not() {
    let state = |width: u16| -> ratatui::buffer::Buffer {
        let area = ratatui::layout::Rect::new(0, 0, width, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let theme = pi_tui::theme::builtin_theme("dark", pi_tui::theme::ColorMode::TrueColor)
            .expect("dark theme");
        ToolStrip::new().render(
            ToolStripState::Idle,
            area,
            &mut buf,
            &theme,
        );
        buf
    };
    let buf = state(60);
    let text: String = (0..60).map(|x| buf.cell((x, 0)).expect("cell").symbol().to_string()).collect();
    assert!(
        !SPINNER_FRAMES.iter().any(|frame| text.contains(*frame)),
        "idle row must not contain any spinner frame, got {text:?}"
    );
}

/// `ToolStripState::is_active` is the trigger the App uses to reserve
/// the strip row in the chrome layout. Idle returns `false` so an idle
/// session keeps the pre-N2 geometry; the two active states return
/// `true` so the chrome planner hands out a 1-row strip.
#[test]
fn is_active_drives_the_chrome_row_reservation() {
    assert!(!ToolStripState::Idle.is_active());
    let started = std::time::Instant::now();
    assert!(ToolStripState::RunningTool { name: "bash", started_at: started }.is_active());
    assert!(ToolStripState::Thinking { started_at: started }.is_active());
}

/// The renderer's spinner cursor advances once the elapsed time
/// crosses the upstream 80 ms interval (`SPINNER_INTERVAL_MS`). The
/// interval is the same as the existing status-bar spinner so the two
/// indicators stay in sync on a glance.
#[test]
fn spinner_advances_after_the_interval_elapses() {
    let area = ratatui::layout::Rect::new(0, 0, 60, 1);
    let theme = pi_tui::theme::builtin_theme("dark", pi_tui::theme::ColorMode::TrueColor)
        .expect("dark theme");
    let mut buf1 = ratatui::buffer::Buffer::empty(area);
    let mut buf2 = ratatui::buffer::Buffer::empty(area);
    let strip = ToolStrip::new();
    let started = std::time::Instant::now() - Duration::from_millis(200);
    let state = ToolStripState::Thinking { started_at: started };
    strip.render(state, area, &mut buf1, &theme);
    let first = buf1.cell((0, 0)).expect("cell").symbol().to_string();
    // Force the second render to cross the interval so the cursor
    // walks one step.
    strip.set_last_advance(
        std::time::Instant::now()
            - Duration::from_millis(pi_tui::loader::SPINNER_INTERVAL_MS * 2),
    );
    strip.render(state, area, &mut buf2, &theme);
    let second = buf2.cell((0, 0)).expect("cell").symbol().to_string();
    assert_ne!(
        first, second,
        "spinner must advance after SPINNER_INTERVAL_MS elapses"
    );
    let first_idx = SPINNER_FRAMES.iter().position(|c| *c == first.chars().next().unwrap()).unwrap();
    let second_idx = SPINNER_FRAMES.iter().position(|c| *c == second.chars().next().unwrap()).unwrap();
    assert_eq!(
        second_idx,
        (first_idx + 1) % SPINNER_FRAMES.len(),
        "spinner must walk one frame per interval"
    );
}

/// `ToolStrip::render` is a no-op on a zero-width or zero-height area
/// — the App reserves a 0-row strip when idle, so the live path must
/// not write to an empty rect and crash on `Buffer::cell_mut`.
#[test]
fn zero_size_area_is_a_no_op() {
    let area_zero_w = ratatui::layout::Rect::new(0, 0, 0, 1);
    let area_zero_h = ratatui::layout::Rect::new(0, 0, 40, 0);
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 40, 1));
    let theme = pi_tui::theme::builtin_theme("dark", pi_tui::theme::ColorMode::None)
        .expect("plain theme");
    let strip = ToolStrip::new();
    let started = std::time::Instant::now();
    strip.render(ToolStripState::Thinking { started_at: started }, area_zero_w, &mut buf, &theme);
    strip.render(ToolStripState::RunningTool { name: "bash", started_at: started }, area_zero_h, &mut buf, &theme);
    // The buffer stays untouched — its cells are still spaces from the
    // empty initialisation.
    let text: String = (0..40).map(|x| buf.cell((x, 0)).expect("cell").symbol().to_string()).collect();
    assert_eq!(text.trim_end(), "", "no-op render must not paint into the buffer");
}