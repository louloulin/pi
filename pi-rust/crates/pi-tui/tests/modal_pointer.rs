//! LUM-1445 — the pointer face of the modal lists (picker / extension dialog
//! / `/settings`).
//!
//! Upstream gives the topmost overlay the pointer before anything else
//! (`dispatchMouseToOverlay`, `packages/tui/src/tui.ts:824-847`) and the two
//! list components the pickers are built on act on it:
//! `SelectList::handleMouse` moves the highlight one row per wheel notch and
//! activates the row a left click commits
//! (`packages/tui/src/components/select-list.ts:110-148`), and
//! `SettingsList::handleMouse` does the same while skipping the search rows
//! (`packages/tui/src/components/settings-list.ts:186-210`). A notch that
//! lands on a list is consumed; one that lands next to it is *deferred* by
//! `shouldDeferViewportInputToOverlay`, so the transcript behind an open
//! modal never scrolls either way
//! (`packages/tui/src/tui-alt-screen.ts:645-694`).
//!
//! The modals are painted straight into the cell buffer here (there is no
//! component tree), so the frame records the item rows it drew and the App
//! maps a pointer cell back onto one. These tests pin that mapping: the wheel
//! moves exactly one row and clamps, the title / rule / counter rows are not
//! items, a press only highlights and a same-cell click commits, and the
//! transcript never moves under a modal.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId, UiRequest, UiResponse};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::dialog::Dialog;
use pi_tui::input::{InputEvent, MouseButton, MouseGesture, MouseGestureKind};
use pi_tui::selector::{Selector, SelectorItem};
use pi_tui::settings::{SettingItem, SettingsList};

const WIDTH: u16 = 60;
const HEIGHT: u16 = 16;

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: Some("Faux".into()),
        context_window: 2048,
        max_output_tokens: 512,
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
            session_id: "lum1445-modal-pointer".into(),
            ..AppConfig::default()
        },
    )
}

/// An App whose transcript overflows the viewport, so "the log did not move"
/// is an assertion about a scrollable view rather than about a short log.
fn log_app() -> App {
    let mut app = app();
    for i in 0..40 {
        app.info(format!("log line {i}"));
    }
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    app
}

fn items() -> Vec<SelectorItem> {
    vec![
        SelectorItem::new("model:faux-a", "faux-a").with_description("first model"),
        SelectorItem::new("model:faux-b", "faux-b").with_description("second model"),
        SelectorItem::new("model:faux-c", "faux-c").with_description("third model"),
        SelectorItem::new("model:faux-d", "faux-d").with_description("fourth model"),
    ]
}

/// Open the picker and render once, so the frame records where its rows are.
fn open_picker(app: &mut App) -> Vec<String> {
    app.open_selector(Selector::new("models", items()));
    app.render_snapshot(WIDTH, HEIGHT).lines
}

/// Open the picker with a window of two rows out of four, so it paints an
/// `(n/m)` counter row underneath the items.
fn open_windowed_picker(app: &mut App) -> Vec<String> {
    app.open_selector(Selector::new("models", items()).with_max_visible(2));
    app.render_snapshot(WIDTH, HEIGHT).lines
}

fn settings_items() -> Vec<SettingItem> {
    vec![
        SettingItem::new("autocompact", "Auto-compact").with_values(["true", "false"], "true"),
        SettingItem::new("theme", "Theme").with_values(["dark", "light"], "dark"),
        SettingItem::new("warnings", "Warnings").with_values(["on", "off"], "on"),
    ]
}

fn open_settings(app: &mut App) -> Vec<String> {
    app.open_settings(SettingsList::new(settings_items(), 10));
    app.render_snapshot(WIDTH, HEIGHT).lines
}

/// A `ctx.ui.select` dialog plus the receiver its answer travels on.
fn select_dialog() -> (Dialog, tokio::sync::oneshot::Receiver<Option<UiResponse>>) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let dialog = Dialog::new(
        UiRequest::Select {
            title: "Pick one".into(),
            options: vec!["alpha".into(), "beta".into(), "gamma".into()],
        },
        tx,
    );
    (dialog, rx)
}

fn confirm_dialog() -> Dialog {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    Dialog::new(
        UiRequest::Confirm {
            title: "Delete files?".into(),
            body: "This cannot be undone.".into(),
        },
        tx,
    )
}

/// Rendered row of the first line containing `needle`.
fn row_of(lines: &[String], needle: &str) -> u16 {
    lines
        .iter()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no rendered row contains {needle:?}: {lines:?}")) as u16
}

fn press(x: u16, y: u16) -> InputEvent {
    InputEvent::gesture(MouseGesture::new(
        MouseGestureKind::Press(MouseButton::Left),
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

fn click(app: &mut App, x: u16, y: u16) {
    app.step(press(x, y));
    app.step(release(x, y));
}

// ---------------------------------------------------------------------------
// Picker (`/model`, `/session`, …)
// ---------------------------------------------------------------------------

#[test]
fn a_notch_on_a_picker_row_moves_the_highlight_one_row() {
    let mut app = log_app();
    let lines = open_picker(&mut app);
    let second = row_of(&lines, "faux-b");
    let cursor = app.selector().expect("picker open").cursor();
    assert_eq!(cursor, 0, "the picker starts on its first row");

    assert_eq!(
        app.step(InputEvent::wheel(false, false, 2, second)),
        StepOutcome::Redraw
    );
    assert_eq!(app.selector().expect("picker open").cursor(), 1);

    let first = row_of(&lines, "faux-a");
    assert_eq!(
        app.step(InputEvent::wheel(true, false, 2, first)),
        StepOutcome::Redraw
    );
    assert_eq!(app.selector().expect("picker open").cursor(), 0);
}

#[test]
fn a_notch_clamps_at_the_ends_instead_of_wrapping() {
    let mut app = log_app();
    let lines = open_picker(&mut app);
    let first = row_of(&lines, "faux-a");
    let last = row_of(&lines, "faux-d");

    // The keyboard's `Up` wraps; the wheel clamps (upstream reduces a notch
    // to `delta = wheelDelta < 0 ? -1 : 1` and clamps,
    // `select-list.ts:110-116`).
    assert_eq!(
        app.step(InputEvent::wheel(true, false, 2, first)),
        StepOutcome::Idle
    );
    assert_eq!(app.selector().expect("picker open").cursor(), 0);

    app.step(InputEvent::wheel(false, false, 2, last));
    let len = app.selector().expect("picker open").filtered_len();
    app.selector_mut().expect("picker open").set_cursor(len - 1);
    assert_eq!(
        app.step(InputEvent::wheel(false, false, 2, last)),
        StepOutcome::Idle
    );
    assert_eq!(
        app.selector().expect("picker open").cursor(),
        len - 1,
        "a notch past the last row stays there"
    );
}

#[test]
fn the_title_rule_and_counter_rows_are_not_items() {
    let mut app = log_app();
    let lines = open_windowed_picker(&mut app);
    let title = row_of(&lines, "models");
    let rule = title + 1;
    let counter = row_of(&lines, "(1/4)");

    for y in [title, rule, counter] {
        assert_eq!(
            app.step(InputEvent::wheel(false, false, 2, y)),
            StepOutcome::Idle,
            "row {y} carries no item"
        );
    }
    assert_eq!(
        app.selector().expect("picker open").cursor(),
        0,
        "none of those rows moved the highlight"
    );

    // ...and a click there activates nothing.
    click(&mut app, 2, rule);
    assert_eq!(app.take_selector_commit(), None);
    assert!(app.selector_open(), "the picker must stay open");
}

#[test]
fn the_transcript_never_moves_under_a_picker_notch() {
    let mut app = log_app();
    app.scroll_viewport_up(4);
    let scrolled = app.messages().scroll_offset();
    assert!(
        scrolled > 0,
        "the log must be scrolled for this to mean something"
    );
    let lines = open_picker(&mut app);
    let row = row_of(&lines, "faux-b");

    // A notch on the list moves the list...
    assert_eq!(
        app.step(InputEvent::wheel(false, false, 2, row)),
        StepOutcome::Redraw
    );
    // ...and one beside it is deferred rather than routed to the viewport.
    assert_eq!(
        app.step(InputEvent::wheel(false, false, 2, HEIGHT - 1)),
        StepOutcome::Idle
    );
    assert_eq!(
        app.messages().scroll_offset(),
        scrolled,
        "the chat log behind the picker must not scroll"
    );
}

#[test]
fn a_press_highlights_the_row_and_the_click_commits_it() {
    let mut app = log_app();
    let lines = open_picker(&mut app);
    let third = row_of(&lines, "faux-c");

    assert_eq!(app.step(press(4, third)), StepOutcome::Redraw);
    assert_eq!(
        app.selector().expect("picker open").cursor(),
        2,
        "the press highlights the pressed row"
    );
    assert_eq!(
        app.take_selector_commit(),
        None,
        "a press alone must not activate anything"
    );

    assert_eq!(app.step(release(4, third)), StepOutcome::Redraw);
    assert_eq!(
        app.take_selector_commit().as_deref(),
        Some("model:faux-c"),
        "the click hands the row's value to the driver"
    );
    assert_eq!(app.take_selector_commit(), None, "taken once");
    assert!(
        app.selector_open(),
        "the driver owns what the choice means, so the picker stays open"
    );
}

#[test]
fn a_release_on_another_row_does_not_commit() {
    let mut app = log_app();
    let lines = open_picker(&mut app);
    let second = row_of(&lines, "faux-b");
    let fourth = row_of(&lines, "faux-d");

    app.step(press(4, second));
    assert_eq!(app.step(release(4, fourth)), StepOutcome::Idle);
    assert_eq!(
        app.take_selector_commit(),
        None,
        "a press and release on different rows is a drag, not a click"
    );
}

#[test]
fn a_stale_row_stops_owning_the_pointer_once_the_frame_closed_it() {
    let mut app = log_app();
    let lines = open_picker(&mut app);
    let row = row_of(&lines, "faux-b");
    app.close_selector();
    let _ = app.render_snapshot(WIDTH, HEIGHT);

    click(&mut app, 4, row);
    assert_eq!(
        app.take_selector_commit(),
        None,
        "the closed picker must not still be clickable"
    );
}

// ---------------------------------------------------------------------------
// Extension dialog (`ctx.ui.select`)
// ---------------------------------------------------------------------------

#[test]
fn a_select_dialog_takes_the_wheel_and_the_click() {
    let mut app = log_app();
    let (dialog, mut rx) = select_dialog();
    assert!(app.open_dialog(dialog));
    let lines = app.render_snapshot(WIDTH, HEIGHT).lines;
    let first = row_of(&lines, "alpha");
    let gamma = row_of(&lines, "gamma");
    assert!(
        lines[first as usize].starts_with('→'),
        "{:?}",
        lines[first as usize]
    );

    // One notch down, then a click two rows further down.
    assert_eq!(
        app.step(InputEvent::wheel(false, false, 2, first)),
        StepOutcome::Redraw
    );
    assert_eq!(app.dialog().expect("dialog open").select_cursor(), 1);

    click(&mut app, 4, gamma);
    assert!(!app.dialog_open(), "the click answers the dialog");
    assert_eq!(
        rx.try_recv().expect("the answer travelled"),
        Some(UiResponse::Select {
            value: "gamma".into()
        })
    );
}

#[test]
fn a_confirm_dialog_owns_no_list_rows() {
    let mut app = log_app();
    assert!(app.open_dialog(confirm_dialog()));
    let lines = app.render_snapshot(WIDTH, HEIGHT).lines;
    let accept = row_of(&lines, "accept");

    assert_eq!(
        app.step(InputEvent::wheel(false, false, 2, accept)),
        StepOutcome::Idle
    );
    click(&mut app, 4, accept);
    assert!(
        app.dialog_open(),
        "a click on a confirm dialog's hint line is not an answer"
    );
}

// ---------------------------------------------------------------------------
// `/settings`
// ---------------------------------------------------------------------------

#[test]
fn a_settings_row_click_selects_and_activates_it() {
    let mut app = log_app();
    let lines = open_settings(&mut app);
    let theme = row_of(&lines, "Theme");

    assert_eq!(app.step(press(4, theme)), StepOutcome::Redraw);
    assert_eq!(
        app.take_pending_setting_change(),
        None,
        "the press only moves the cursor"
    );
    assert_eq!(
        app.step(release(4, theme)),
        StepOutcome::Redraw,
        "the click activates the row, like Enter"
    );
    assert_eq!(
        app.take_pending_setting_change(),
        Some(("theme".to_string(), "light".to_string())),
        "activating a value row advances it, exactly like Enter"
    );
}

#[test]
fn a_settings_click_off_the_item_rows_changes_nothing() {
    let mut app = log_app();
    app.open_settings(SettingsList::new(settings_items(), 10).searchable(true));
    let lines = app.render_snapshot(WIDTH, HEIGHT).lines;
    let search = row_of(&lines, "> ");

    // The search box and its spacer are the list's own two rows, and the hint
    // line is below the last item (`settings-list.ts:186-191`).
    click(&mut app, 4, search);
    click(&mut app, 4, search + 1);
    click(&mut app, 4, row_of(&lines, "Enter/Space to change"));
    assert_eq!(app.take_pending_setting_change(), None);
    assert_eq!(app.take_pending_setting_activation(), None);
}
