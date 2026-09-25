//! The "jump to latest" pill on the transcript's last row.
//!
//! Upstream composites `scrollToEndIndicator` over the bottom of the scroll
//! view whenever the view is following the end but is not *at* it
//! (`packages/tui/src/tui-alt-screen.ts:1617-1637`), and hit-tests the same
//! rectangle so a click on the pill jumps back to the tail
//! (`packages/tui/src/tui-alt-screen.ts:1017-1024`). The label, including the
//! shortcut from `tui.altScreen.bottom`, comes from the interactive-mode
//! renderer (`packages/coding-agent/src/modes/interactive/tui-renderer.ts:29-33`).
//!
//! The pill is screen furniture, like the scrollbar: it is painted on the
//! live `render_to_buffer` frame only, never into the flat `render_snapshot`
//! that backs `/transcript`.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::input::{MouseButton, MouseGesture, MouseGestureKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

const WIDTH: u16 = 44;
/// Message viewport is this minus the status bar, the prompt row, and the
/// editor border row (Phase 2 / G3), i.e. 7 rows — the pill's row is
/// `VIEWPORT - 1`.
const HEIGHT: u16 = 10;
const VIEWPORT: u16 = HEIGHT - 3;
const PILL_ROW: u16 = VIEWPORT - 1;
/// Rightmost column of the viewport = the scrollbar's column.
const BAR: u16 = WIDTH - 1;

/// Dark palette: `text` is `#d4d4d4`, `selectedBg` is `#3a3a4a`.
const TEXT: Color = Color::Rgb(212, 212, 212);
const SELECTED_BG: Color = Color::Rgb(58, 58, 74);

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: Some("Faux".into()),
        context_window: 1024,
        max_output_tokens: 256,
    }
}

fn app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    App::new(
        &agent,
        AppConfig {
            session_id: "scroll-to-end".into(),
            ..AppConfig::default()
        },
    )
}

/// An App with `count` one-line items in the log.
fn app_with_lines(count: usize) -> App {
    let mut app = app();
    for i in 0..count {
        app.info(format!("line {i}"));
    }
    app
}

/// The interactive render path (scrollbar + pill).
fn render(app: &mut App) -> Buffer {
    let area = Rect {
        x: 0,
        y: 0,
        width: WIDTH,
        height: HEIGHT,
    };
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    buf
}

fn row(buf: &Buffer, y: u16) -> String {
    (0..WIDTH)
        .map(|x| buf.cell((x, y)).expect("cell in bounds").symbol())
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Scroll `lines` away from the tail. Renders first: `scroll_viewport_up`
/// clamps against the viewport recorded by the last render, which is only
/// known once something has been painted.
fn scroll_away(app: &mut App, lines: usize) {
    let _ = render(app);
    assert!(app.scroll_viewport_up(lines), "the log has scrollback");
    assert!(!app.messages().is_following());
}

#[test]
fn pill_is_painted_while_scrolled_away_and_cleared_at_the_tail() {
    let mut app = app_with_lines(40);

    // Following the tail: the last row of the transcript is content.
    let buf = render(&mut app);
    assert!(app.messages().is_following());
    assert_eq!(app.scroll_to_end_rect(), None);
    assert!(!row(&buf, PILL_ROW).contains("Jump to latest"));

    // One page up detaches the viewport, which is what raises the pill.
    assert_eq!(
        app.step_key(pi_tui::input::Key::new(
            pi_tui::input::KeyCode::PageUp,
            pi_tui::input::KeyModifiers::NONE,
        )),
        StepOutcome::Redraw
    );
    let buf = render(&mut app);
    assert!(!app.messages().is_following());
    let line = row(&buf, PILL_ROW);
    assert!(line.contains("↓ Jump to latest message"), "{line:?}");

    let rect = app.scroll_to_end_rect().expect("pill was painted");
    assert_eq!(rect.y, PILL_ROW);
    assert_eq!(rect.height, 1);
    // Centred on the viewport (the scrollbar column is not part of the
    // pill's budget, so the exact centre can land one column over).
    let centre = bar_or_width(&app) as i32 - rect.width as i32;
    assert!((2 * rect.x as i32 - centre).abs() <= 2, "{rect:?}");

    // End re-pins the tail and the pill goes away with it.
    assert_eq!(
        app.step_key(pi_tui::input::Key::new(
            pi_tui::input::KeyCode::End,
            pi_tui::input::KeyModifiers::NONE,
        )),
        StepOutcome::Redraw
    );
    let buf = render(&mut app);
    assert_eq!(app.scroll_to_end_rect(), None);
    assert!(!row(&buf, PILL_ROW).contains("Jump to latest"));
}

#[test]
fn pill_labels_the_bound_shortcut() {
    let mut app = app_with_lines(40);
    scroll_away(&mut app, 4);
    let buf = render(&mut app);
    // `tui.altScreen.bottom` is bound to `end` in the default table, and a
    // chord is formatted for the reader (`End`), not echoed raw.
    assert!(row(&buf, PILL_ROW).contains("↓ Jump to latest message · End"));
}

#[test]
fn pill_uses_the_selected_background_palette() {
    let mut app = app_with_lines(40);
    scroll_away(&mut app, 4);
    let buf = render(&mut app);
    let rect = app.scroll_to_end_rect().expect("pill was painted");
    let cell = buf.cell((rect.x + 1, rect.y)).expect("cell in bounds");
    assert_eq!(cell.style().bg, Some(SELECTED_BG));
    assert_eq!(cell.style().fg, Some(TEXT));
    // A neighbouring transcript cell keeps its own (unselected) background.
    let outside = buf.cell((0, rect.y)).expect("cell in bounds");
    assert_ne!(outside.style().bg, Some(SELECTED_BG));
}

#[test]
fn pill_needs_scrollback() {
    // Two lines in an 8-row viewport never scroll, so there is nothing to
    // jump to and the pill stays off.
    let mut app = app_with_lines(2);
    let buf = render(&mut app);
    assert!(app.messages().is_following());
    assert_eq!(app.scroll_to_end_rect(), None);
    assert!(!row(&buf, PILL_ROW).contains("Jump to latest"));
}

#[test]
fn pill_stops_left_of_the_scrollbar_column() {
    let mut app = app_with_lines(60);
    scroll_away(&mut app, 4);
    let buf = render(&mut app);
    let rect = app.scroll_to_end_rect().expect("pill was painted");
    // The scrollbar owns the last column; the pill's width budget stops
    // before it (`compositeScrollToEndIndicator` limits `availableWidth` to
    // the scrollbar's column).
    assert!(rect.x + rect.width <= BAR, "{rect:?}");
    assert!(app.scrollbar_geometry().is_some());
    let bar_cell = buf.cell((BAR, rect.y)).expect("cell in bounds");
    assert!(
        matches!(bar_cell.symbol(), "┃" | "█" | "│"),
        "{:?}",
        bar_cell.symbol()
    );
}

#[test]
fn clicking_the_pill_returns_to_the_tail() {
    let mut app = app_with_lines(40);
    scroll_away(&mut app, 6);
    let buf = render(&mut app);
    let rect = app.scroll_to_end_rect().expect("pill was painted");
    // Sanity: the click lands well below the viewport top and above the
    // prompt row, i.e. on the pill and not on the editor.
    assert!(rect.y < VIEWPORT);
    assert!(!row(&buf, PILL_ROW).is_empty());

    let x = rect.x + rect.width / 2;
    assert_eq!(
        app.step_mouse_gesture(gesture(rect.y, x)),
        StepOutcome::Redraw
    );
    assert!(app.messages().is_following());
    assert_eq!(app.messages().scroll_offset(), 0);

    // Re-rendering at the tail clears the record, so the click cannot be
    // replayed against a rectangle that is no longer on screen.
    let _ = render(&mut app);
    assert_eq!(app.scroll_to_end_rect(), None);
}

#[test]
fn clicking_below_the_pill_still_starts_a_selection() {
    let mut app = app_with_lines(40);
    scroll_away(&mut app, 6);
    let buf = render(&mut app);
    let rect = app.scroll_to_end_rect().expect("pill was painted");
    // One row below the pill (and still inside the viewport when there is
    // room) the press behaves exactly as before.
    let outside_y = if rect.y + 1 < VIEWPORT {
        rect.y + 1
    } else {
        rect.y.saturating_sub(1)
    };
    assert!(!row(&buf, outside_y).is_empty() || outside_y == rect.y - 1);
    let _ = app.step_mouse_gesture(gesture(outside_y, 0));
    // A press outside the pill never jumps: the reader stays detached.
    assert!(!app.messages().is_following());
}

#[test]
fn transcript_snapshot_stays_free_of_the_pill() {
    let mut app = app_with_lines(40);
    scroll_away(&mut app, 4);
    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    assert!(!snapshot.lines[PILL_ROW as usize].contains("Jump to latest"));
    // And the record stays empty, so `/transcript` cannot leave a live
    // rectangle behind either.
    assert_eq!(app.scroll_to_end_rect(), None);
}

fn gesture(y: u16, x: u16) -> MouseGesture {
    MouseGesture::new(MouseGestureKind::Press(MouseButton::Left), x, y, false)
}

/// Width the pill centres itself in: the viewport up to the scrollbar column
/// when the bar is on screen, the whole viewport otherwise.
fn bar_or_width(app: &App) -> u16 {
    match app.scrollbar_geometry() {
        Some(geometry) => geometry.column,
        None => WIDTH,
    }
}
