//! Chat-log scrollbar: geometry, hover highlight, drag and track clicks.
//!
//! Upstream keeps one `ScrollView` per pane and hit-tests the one under the
//! pointer (`packages/tui/src/tui-alt-screen.ts:1041-1109`). The App has a
//! single scrollable region — the message log — so the port collapses that
//! per-view model onto [`App::scrollbar_geometry`]; the module docs of
//! `pi_tui::app` spell out the deviations (no transient hide delay, bold
//! active styling, `render_snapshot` stays content-only).
//!
//! The palette values come from `assets/themes/dark.json`:
//! `scrollbarTrack` is `darkGray` and `scrollbarThumb` is `text`.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, ScrollbarGeometry, StepOutcome};
use pi_tui::input::{InputEvent, MouseButton, MouseGesture, MouseGestureKind};
use pi_tui::{Selector, SelectorItem};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

const WIDTH: u16 = 40;
/// Message viewport is this minus the status bar, the prompt row, and
/// the editor border row (Phase 2 / G3), i.e. 7 rows — the same canonical
/// size the other App tests use.
const HEIGHT: u16 = 10;
const VIEWPORT: u16 = HEIGHT - 3;
/// Rightmost column of the viewport = the scrollbar's column.
const BAR: u16 = WIDTH - 1;

/// Dark palette.
const TRACK: Color = Color::Rgb(80, 80, 80);
const THUMB: Color = Color::Rgb(212, 212, 212);

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
            session_id: "scrollbar".into(),
            ..AppConfig::default()
        },
    )
}

/// An App whose log holds `count` one-line items, rendered once so the
/// viewport is recorded.
fn app_with_lines(count: usize) -> App {
    let mut app = app();
    for i in 0..count {
        app.info(format!("line {i}"));
    }
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    app
}

/// The interactive render path (with the scrollbar overlay).
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

fn style_at(buf: &Buffer, x: u16, y: u16) -> Style {
    buf.cell((x, y)).expect("cell in bounds").style()
}

fn symbol_at(buf: &Buffer, x: u16, y: u16) -> String {
    buf.cell((x, y))
        .expect("cell in bounds")
        .symbol()
        .to_string()
}

fn move_to(x: u16, y: u16) -> InputEvent {
    InputEvent::gesture(MouseGesture::new(MouseGestureKind::Move, x, y, false))
}

fn press(x: u16, y: u16) -> InputEvent {
    InputEvent::gesture(MouseGesture::new(
        MouseGestureKind::Press(MouseButton::Left),
        x,
        y,
        false,
    ))
}

fn drag(x: u16, y: u16) -> InputEvent {
    InputEvent::gesture(MouseGesture::new(
        MouseGestureKind::Drag(MouseButton::Left),
        x,
        y,
        false,
    ))
}

fn release(x: u16, y: u16) -> InputEvent {
    InputEvent::gesture(MouseGesture::new(
        MouseGestureKind::Release(MouseButton::Left),
        x,
        y,
        false,
    ))
}

fn geometry(app: &App) -> ScrollbarGeometry {
    app.scrollbar_geometry().expect("content overflows")
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

#[test]
fn geometry_is_hidden_before_the_first_render_and_without_overflow() {
    let mut fresh = app();
    fresh.info("one line");
    assert_eq!(
        fresh.scrollbar_geometry(),
        None,
        "no viewport is recorded before the first render"
    );

    // Exactly one viewport of content still fits: the bar only exists when
    // there is something to scroll.
    let fits = app_with_lines(VIEWPORT as usize);
    assert_eq!(fits.scrollbar_geometry(), None);

    let overflows = app_with_lines(VIEWPORT as usize + 1);
    assert!(
        overflows.scrollbar_geometry().is_some(),
        "one line past the viewport shows the bar"
    );
}

#[test]
fn geometry_tracks_the_content_ratio_and_pins_the_thumb_to_the_bottom() {
    let mut app = app_with_lines(40);
    // 40 lines at width 40, viewport 7 (Phase 2 / G3 reserves a row for
    // the editor border): max scroll is 33 and the thumb keeps the
    // 7/40 ratio, floored at the two-row minimum.
    let tail = geometry(&app);
    assert_eq!(tail.column, BAR);
    assert_eq!(tail.track_top, 0);
    assert_eq!(tail.track_height, VIEWPORT);
    assert_eq!(tail.max_scroll, 33);
    assert_eq!(tail.thumb_height, 2);
    assert_eq!(
        tail.thumb_top + tail.thumb_height,
        VIEWPORT,
        "a tail-pinned viewport puts the thumb flush with the track's end"
    );

    // Scrolling to the top puts the thumb at the track's start.
    assert!(app.scroll_viewport_up(usize::MAX));
    assert_eq!(geometry(&app).thumb_top, 0);

    // Halfway up the log is halfway up the track.
    assert!(app.scroll_viewport_down(16));
    let middle = geometry(&app);
    assert_eq!(
        middle.thumb_top, 2,
        "16/33 of five free rows rounds to two (Phase 2 / G3: max_scroll=33)"
    );
    assert_eq!(middle.thumb_height, 2);

    // Back to the tail pins it again.
    assert!(app.scroll_viewport_to_bottom());
    assert_eq!(geometry(&app).thumb_top + 2, VIEWPORT);
}

#[test]
fn the_bar_is_painted_only_by_the_interactive_path() {
    let mut app = app_with_lines(40);

    let buf = render(&mut app);
    assert_eq!(
        symbol_at(&buf, BAR, 0),
        "│",
        "track rule in the last column"
    );
    assert_eq!(symbol_at(&buf, BAR, 6), "┃", "idle thumb glyph");
    assert_eq!(
        style_at(&buf, BAR, 0).fg,
        Some(TRACK),
        "track uses the scrollbarTrack slot"
    );
    assert_eq!(
        style_at(&buf, BAR, 6).fg,
        Some(THUMB),
        "thumb uses the scrollbarThumb slot"
    );
    assert!(
        !style_at(&buf, BAR, 6).add_modifier.contains(Modifier::BOLD),
        "an idle bar is not emphasised"
    );

    // The flat snapshot (and `/transcript`) stays content-only.
    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    assert!(
        snapshot.lines.iter().all(|line| !line.contains('│')),
        "render_snapshot omits the pointer affordance"
    );
}

#[test]
fn the_bar_is_absent_when_the_content_fits() {
    let mut app = app_with_lines(3);
    let buf = render(&mut app);
    for y in 0..HEIGHT {
        assert_ne!(
            symbol_at(&buf, BAR, y),
            "│",
            "row {y} must stay bare when nothing overflows"
        );
        assert_ne!(symbol_at(&buf, BAR, y), "┃");
    }
}

// ---------------------------------------------------------------------------
// Hover highlight
// ---------------------------------------------------------------------------

#[test]
fn hovering_the_bar_highlights_track_and_thumb() {
    let mut app = app_with_lines(40);
    assert!(!app.scrollbar_hovered());

    // A bare move over the thumb reveals the active rendering.
    assert_eq!(app.step(move_to(BAR, 6)), StepOutcome::Redraw);
    assert!(app.scrollbar_hovered());
    let buf = render(&mut app);
    assert_eq!(symbol_at(&buf, BAR, 6), "█", "the active thumb is solid");
    assert!(
        style_at(&buf, BAR, 6).add_modifier.contains(Modifier::BOLD),
        "the active thumb is bold"
    );
    assert!(
        style_at(&buf, BAR, 0).add_modifier.contains(Modifier::BOLD),
        "the active track is bold too"
    );
    assert_eq!(style_at(&buf, BAR, 0).fg, Some(TRACK));

    // Leaving the bar clears the highlight.
    assert_eq!(app.step(move_to(5, 3)), StepOutcome::Redraw);
    assert!(!app.scrollbar_hovered());
    let buf = render(&mut app);
    assert_eq!(symbol_at(&buf, BAR, 6), "┃");
    assert!(!style_at(&buf, BAR, 6).add_modifier.contains(Modifier::BOLD));
}

#[test]
fn hover_needs_the_last_column_and_a_track_row() {
    let mut app = app_with_lines(40);
    // One column to the left of the bar.
    assert_eq!(app.step(move_to(BAR - 1, 3)), StepOutcome::Idle);
    assert!(!app.scrollbar_hovered());
    // Past the track (the status-bar row).
    assert_eq!(app.step(move_to(BAR, VIEWPORT)), StepOutcome::Idle);
    assert!(!app.scrollbar_hovered());
}

#[test]
fn a_modal_owns_the_bar_while_it_is_open() {
    let mut app = app_with_lines(40);
    app.open_selector(Selector::new(
        "models",
        vec![SelectorItem::new("model:faux", "faux")],
    ));
    assert_eq!(app.step(move_to(BAR, 3)), StepOutcome::Idle);
    assert!(!app.scrollbar_hovered());
    let _ = app.step(press(BAR, 3));
    assert!(
        !app.scrollbar_dragging(),
        "an overlay guards the scrollbar hit-test"
    );
}

// ---------------------------------------------------------------------------
// Clicks and drags
// ---------------------------------------------------------------------------

#[test]
fn pressing_the_track_jumps_to_the_pointer() {
    let mut app = app_with_lines(40);
    // Phase 2 / G3: viewport shrunk from 8 to 7, so the tail-pinned thumb
    // sits at row 5 (was 6).
    assert_eq!(geometry(&app).thumb_top, 5, "starts at the tail");

    // Row 4 is on the track (the thumb spans 5..7). The press centres the
    // thumb on the pointer, i.e. jumps to the matching ratio.
    assert_eq!(app.step(press(BAR, 4)), StepOutcome::Redraw);
    assert!(app.scrollbar_dragging());
    assert_eq!(
        geometry(&app).thumb_top,
        3,
        "clicking halfway up the track scrolls halfway up the log"
    );
}

#[test]
fn pressing_the_thumb_does_not_jump_and_a_drag_reaches_both_extremes() {
    let mut app = app_with_lines(40);
    // Phase 2 / G3: tail-pinned thumb at row 5 (was 6).
    assert_eq!(geometry(&app).thumb_top, 5);

    // A press on the thumb starts the drag without moving the viewport.
    assert_eq!(app.step(press(BAR, 5)), StepOutcome::Idle);
    assert!(app.scrollbar_dragging());
    assert_eq!(geometry(&app).thumb_top, 5);

    // Dragging to the top row scrolls to the top of the log.
    assert_eq!(app.step(drag(BAR, 0)), StepOutcome::Redraw);
    assert_eq!(geometry(&app).thumb_top, 0);
    assert_eq!(app.step(drag(BAR, 0)), StepOutcome::Idle, "no further move");

    // Dragging back to the bottom pins it to the track's end.
    assert_eq!(app.step(drag(BAR, VIEWPORT - 1)), StepOutcome::Redraw);
    let bottom = geometry(&app);
    assert_eq!(bottom.thumb_top + bottom.thumb_height, VIEWPORT);

    // Midpoint: three free rows up the track is half the log.
    assert_eq!(app.step(drag(BAR, 3)), StepOutcome::Redraw);
    assert_eq!(geometry(&app).thumb_top, 3);

    // The release ends the drag; a later move is not a scroll any more.
    assert_eq!(app.step(release(BAR, 3)), StepOutcome::Idle);
    assert!(!app.scrollbar_dragging());
    let frozen = geometry(&app).thumb_top;
    assert_eq!(app.step(drag(BAR, 0)), StepOutcome::Idle);
    assert_eq!(geometry(&app).thumb_top, frozen);
}

#[test]
fn a_pointer_drag_does_not_start_a_text_selection() {
    let mut app = app_with_lines(40);
    // The press lands on the bar, not in the text: no selection is armed and
    // the clipboard stays untouched.
    let _ = app.step(press(BAR, 6));
    assert!(!app.has_selection());
    assert_eq!(app.selection_text(), None);

    // A button-less move over the transcript must not steal the drag (it
    // would be a bare pointer move), and it must not clear the hover.
    assert_eq!(app.step(move_to(5, 3)), StepOutcome::Idle);
    assert!(app.scrollbar_dragging());
    assert!(app.scrollbar_hovered());
    assert!(!app.has_selection());

    // The drag keeps tracking the thumb after the pointer left the bar.
    assert_eq!(app.step(drag(BAR, 0)), StepOutcome::Redraw);
    assert_eq!(geometry(&app).thumb_top, 0);
    let _ = app.step(release(BAR, 0));
    assert_eq!(app.take_clipboard_request(), None);
}
