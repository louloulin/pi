//! Per-rectangle mouse dispatch to the open modal overlays — the Rust port of
//! upstream's `MouseRegion` / `dispatchMouseToOverlay` layer.
//!
//! Upstream gives an overlay's rectangle the mouse before the layout
//! underneath sees anything: `dispatchMouseToOverlay` walks
//! `renderedOverlayLayouts` topmost-first, skips rectangles that do not
//! contain the pointer, translates the screen coordinates into overlay-local
//! ones and reports a hit even when the component did not handle the event
//! (`packages/tui/src/tui.ts:824-847`,
//! `packages/tui/src/tui-alt-screen.ts:912-922`). A click that a component
//! handled clears the log selection (`tui-alt-screen.ts:1330-1339`).
//!
//! These tests pin the App-level contract: the gesture is consumed by the hit
//! region (no fall-through to the chat log), the region's owner receives
//! region-local coordinates, the rectangle edges are exclusive, the regions
//! follow the keyboard layer order, and a committed click clears a chat-log
//! selection without disturbing LUM-1124's copy-on-select behaviour.

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId, UiRequest};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::dialog::Dialog;
use pi_tui::input::{InputEvent, MouseButton, MouseGesture, MouseGestureKind};
use pi_tui::mouse_region::{MouseRegion, MouseRegionPoint};
use pi_tui::selector::{Selector, SelectorItem};
use pi_tui::settings::{SettingItem, SettingsList};
use ratatui::layout::Rect;

const WIDTH: u16 = 40;
/// Snapshot height; the message viewport is this minus the status bar and the
/// prompt row, i.e. 22 rows — tall enough that the settings overlay is not
/// clipped, so its rectangle really ends inside the viewport.
const HEIGHT: u16 = 24;
/// Rows the message viewport covers.
const VIEWPORT_ROWS: u16 = HEIGHT - 2;

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
        std::sync::Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let config = AppConfig {
        session_id: "mouse-region".into(),
        ..AppConfig::default()
    };
    App::new(&agent, config)
}

/// An App with a filled chat log, rendered once so `viewport` /
/// `viewport_origin` describe a real screen.
fn log_app() -> App {
    let mut app = app();
    for i in 0..40 {
        app.info(format!("line {i}"));
    }
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    app
}

fn settings_items() -> Vec<SettingItem> {
    vec![
        SettingItem::new("autocompact", "Auto-compact").with_values(["true", "false"], "true"),
        SettingItem::new("theme", "Theme").with_values(["dark", "light"], "dark"),
        SettingItem::new("warnings", "Warnings").with_values(["on", "off"], "on"),
    ]
}

fn open_settings(app: &mut App) {
    app.open_settings(SettingsList::new(settings_items(), 10).searchable(true));
}

fn open_selector(app: &mut App) {
    app.open_selector(Selector::new(
        "models",
        vec![SelectorItem::new("model:faux", "faux")],
    ));
}

fn pending_confirm() -> Dialog {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    Dialog::new(
        UiRequest::Confirm {
            title: "Delete files?".into(),
            body: "This cannot be undone.".into(),
        },
        tx,
    )
}

fn gesture(kind: MouseGestureKind, x: u16, y: u16) -> InputEvent {
    InputEvent::gesture(MouseGesture::new(kind, x, y, false))
}

fn press(x: u16, y: u16) -> InputEvent {
    gesture(MouseGestureKind::Press(MouseButton::Left), x, y)
}

fn drag(x: u16, y: u16) -> InputEvent {
    gesture(MouseGestureKind::Drag(MouseButton::Left), x, y)
}

fn release(x: u16, y: u16) -> InputEvent {
    gesture(MouseGestureKind::Release(MouseButton::Left), x, y)
}

/// Select the first visible log line, the way a user drag-selects it.
fn select_a_line(app: &mut App) {
    app.step(press(2, 0));
    app.step(drag(8, 0));
    assert!(app.has_selection(), "the drag must have selected something");
}

#[test]
fn the_open_modal_rectangles_describe_the_rendered_overlays() {
    let mut app = log_app();
    open_settings(&mut app);
    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    let (origin_x, origin_y) = app.viewport_origin();

    let regions = app.mouse_regions();
    assert_eq!(regions.len(), 1, "only the settings modal is open");
    // The overlay starts one row below the message area's top (the search
    // line) and covers as many rows as it rendered, clipped to the viewport.
    let expected_rows = (snapshot.settings_lines.len() as u16).min(VIEWPORT_ROWS - 1);
    assert_eq!(
        regions[0].rect(),
        Rect::new(origin_x, origin_y + 1, WIDTH, expected_rows),
        "settings lines: {:?}",
        snapshot.settings_lines
    );
}

#[test]
fn a_gesture_inside_a_modal_is_consumed_with_region_local_coordinates() {
    let mut app = log_app();
    open_settings(&mut app);
    let _ = app.render_snapshot(WIDTH, HEIGHT);

    let region = app.mouse_regions()[0];
    let rect = region.rect();
    // A cell inside the overlay maps into the overlay's own coordinates...
    let point = region
        .capture(MouseGesture::new(
            MouseGestureKind::Press(MouseButton::Left),
            rect.x + 4,
            rect.y + 2,
            false,
        ))
        .expect("the cell is inside the settings overlay");
    assert_eq!(point, MouseRegionPoint::new(4, 2));

    // ...and the App consumes the gesture there instead of handing it to the
    // chat log underneath: a drag inside the modal starts no selection.
    assert_eq!(app.step(press(rect.x + 4, rect.y + 2)), StepOutcome::Idle);
    assert_eq!(app.step(drag(rect.x + 8, rect.y + 2)), StepOutcome::Idle);
    assert!(!app.has_selection());
    assert_eq!(app.selection_text(), None);
}

#[test]
fn gestures_outside_every_modal_rectangle_are_still_swallowed() {
    let mut app = log_app();
    open_selector(&mut app);
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    let rect = app.mouse_regions()[0].rect();
    assert_eq!(rect.y, app.viewport_origin().1 + 1, "the selector sits low");

    // Row 0 is above the modal, the last row is below it; neither reaches
    // the chat log.
    assert_eq!(app.step(press(2, 0)), StepOutcome::Idle);
    assert_eq!(app.step(drag(8, 0)), StepOutcome::Idle);
    for (x, y) in [
        (rect.x + 1, rect.y + rect.height),
        (rect.x + rect.width, rect.y + 1),
    ] {
        assert_eq!(app.step(press(x, y)), StepOutcome::Idle, "({x}, {y})");
        assert_eq!(app.step(release(x, y)), StepOutcome::Idle, "({x}, {y})");
    }
    assert!(!app.has_selection());
}

#[test]
fn a_click_inside_a_modal_clears_the_chat_log_selection() {
    let mut app = log_app();
    select_a_line(&mut app);
    open_settings(&mut app);
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    let rect = app.mouse_regions()[0].rect();

    assert_eq!(app.step(press(rect.x + 3, rect.y + 1)), StepOutcome::Idle);
    assert_eq!(
        app.step(release(rect.x + 3, rect.y + 1)),
        StepOutcome::Redraw,
        "the committed click redraws without the highlight"
    );
    assert!(!app.has_selection());
    assert_eq!(app.selection_text(), None);
}

#[test]
fn a_selector_click_clears_the_chat_log_selection() {
    let mut app = log_app();
    select_a_line(&mut app);
    open_selector(&mut app);
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    let rect = app.mouse_regions()[0].rect();

    app.step(press(rect.x + 2, rect.y + 1));
    assert_eq!(
        app.step(release(rect.x + 2, rect.y + 1)),
        StepOutcome::Redraw
    );
    assert!(!app.has_selection());
    assert!(app.selector_open(), "the click must not dismiss the modal");
}

#[test]
fn a_press_inside_a_modal_keeps_the_selection_until_the_release() {
    let mut app = log_app();
    select_a_line(&mut app);
    let selected = app.selection_text();
    open_settings(&mut app);
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    let rect = app.mouse_regions()[0].rect();

    assert_eq!(app.step(press(rect.x + 3, rect.y + 1)), StepOutcome::Idle);
    assert_eq!(
        app.selection_text(),
        selected,
        "a press alone must not drop the selection"
    );
}

#[test]
fn a_click_only_commits_when_the_release_lands_on_the_press_cell() {
    let mut app = log_app();
    select_a_line(&mut app);
    let selected = app.selection_text();
    open_settings(&mut app);
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    let rect = app.mouse_regions()[0].rect();

    // Press on one cell, release on another inside the same modal: this is a
    // drag, not a click (`tui-alt-screen.ts:1312-1315`).
    app.step(press(rect.x + 3, rect.y + 1));
    assert_eq!(app.step(release(rect.x + 5, rect.y + 1)), StepOutcome::Idle);
    assert_eq!(app.selection_text(), selected);

    // The release after a press that missed every rectangle cannot clear
    // either.
    app.step(press(2, VIEWPORT_ROWS - 1));
    assert_eq!(app.step(release(rect.x + 3, rect.y + 1)), StepOutcome::Idle);
    assert_eq!(app.selection_text(), selected);
}

#[test]
fn the_rectangle_edges_are_exclusive() {
    let mut app = log_app();
    select_a_line(&mut app);
    let selected = app.selection_text();
    open_settings(&mut app);
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    let rect = app.mouse_regions()[0].rect();
    let (last_x, last_y) = (rect.x + rect.width - 1, rect.y + rect.height - 1);
    assert!(
        last_y < VIEWPORT_ROWS - 1,
        "the overlay must end above the bottom row for the test to mean anything"
    );

    for (x, y) in [
        (rect.x + rect.width, rect.y + 1),
        (rect.x + 1, rect.y + rect.height),
    ] {
        app.step(press(x, y));
        assert_eq!(app.step(release(x, y)), StepOutcome::Idle, "({x}, {y})");
        assert_eq!(app.selection_text(), selected, "({x}, {y}) is outside");
    }

    // The last covered cell is inside, so a click there commits.
    app.step(press(last_x, last_y));
    assert_eq!(app.step(release(last_x, last_y)), StepOutcome::Redraw);
    assert!(!app.has_selection());
}

#[test]
fn a_modal_click_never_copies_and_copy_on_select_still_copies_once() {
    let mut app = log_app();
    select_a_line(&mut app);
    let selected = app.selection_text().expect("a selection");
    assert_eq!(app.step(release(8, 0)), StepOutcome::Idle);
    assert_eq!(
        app.take_clipboard_request().as_deref(),
        Some(selected.as_str()),
        "copy-on-select copies on the release that ends the drag"
    );
    assert!(app.has_selection(), "the selection stays visible");

    open_settings(&mut app);
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    let rect = app.mouse_regions()[0].rect();
    app.step(press(rect.x + 3, rect.y + 1));
    app.step(release(rect.x + 3, rect.y + 1));

    assert_eq!(
        app.take_clipboard_request(),
        None,
        "clicking a modal must not queue another clipboard write"
    );
}

#[test]
fn modal_rectangles_follow_the_keyboard_layer_order() {
    let mut app = log_app();
    open_selector(&mut app);
    open_settings(&mut app);
    assert!(app.open_dialog(pending_confirm()));
    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    let (origin_x, origin_y) = app.viewport_origin();

    let regions = app.mouse_regions();
    assert_eq!(regions.len(), 3, "one rectangle per open modal");
    let dialog = regions[0].rect();
    let settings = regions[1].rect();
    let selector = regions[2].rect();

    // `step_key` gives the dialog the keyboard first, then settings, then the
    // selector; mouse dispatch keeps that order, so the dialog is topmost.
    assert_eq!(dialog.y, origin_y, "the dialog starts at the top");
    assert_eq!(settings.y, origin_y + 1);
    assert_eq!(selector.y, origin_y + 1);

    assert_eq!(dialog.height, snapshot.dialog_lines.len() as u16);
    assert_eq!(settings.height, snapshot.settings_lines.len() as u16);
    assert_eq!(
        selector.height,
        app.selector()
            .expect("selector")
            .render_styled_lines(WIDTH)
            .len() as u16
    );
    for rect in [dialog, settings, selector] {
        assert_eq!(rect.x, origin_x);
        assert_eq!(rect.width, WIDTH);
    }
}

#[test]
fn a_modal_rectangle_is_clipped_to_the_message_viewport() {
    let mut app = log_app();
    // 40 items with descriptions render far more rows than the viewport has,
    // so the rectangle stops at the last message row.
    let items: Vec<SettingItem> = (0..40)
        .map(|i| {
            SettingItem::new(format!("item-{i}"), format!("Item {i}"))
                .with_description("A description that takes another row")
                .with_values(["on", "off"], "on")
        })
        .collect();
    app.open_settings(SettingsList::new(items, 100));
    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    assert!(snapshot.settings_lines.len() as u16 > VIEWPORT_ROWS - 1);

    let rect = app.mouse_regions()[0].rect();
    assert_eq!(rect.height, VIEWPORT_ROWS - 1);
    assert_eq!(
        rect.y + rect.height,
        VIEWPORT_ROWS,
        "the status bar row stays outside the overlay"
    );
}

#[test]
fn there_are_no_rectangles_before_the_first_render() {
    // A fresh App has no geometry yet, so nothing can be hit — but the open
    // modal still swallows the gesture.
    let mut fresh = app();
    open_settings(&mut fresh);
    assert!(fresh.mouse_regions().is_empty());
    assert_eq!(fresh.step(press(0, 0)), StepOutcome::Idle);
    assert_eq!(fresh.step(release(0, 0)), StepOutcome::Idle);
    assert!(!fresh.has_selection());
    assert!(fresh.settings_open());
}

#[test]
fn a_region_can_be_built_from_a_plain_rectangle() {
    // The public surface `pi-coding-agent` may reuse: an absolute rectangle
    // hit-tests against absolute cells.
    let region = MouseRegion::new(Rect::new(10, 4, 6, 3));
    assert_eq!(region.hit(10, 4), Some(MouseRegionPoint::new(0, 0)));
    assert_eq!(region.hit(15, 6), Some(MouseRegionPoint::new(5, 2)));
    assert_eq!(region.hit(16, 6), None);
    assert_eq!(region.hit(15, 7), None);
    assert!(!region.is_empty());
}
