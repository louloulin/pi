//! Mouse-wheel routing for the composer's autocomplete dropdown.
//!
//! Upstream hands a wheel notch to `dispatchMouseToOverlay` →
//! `dispatchMouseToLayout` *before* `routeWheel` reaches the chat log
//! (`packages/tui/src/tui-alt-screen.ts:679-694`), which is how a notch over
//! the composer's `SelectList` scrolls its candidates instead of the
//! transcript behind it (`packages/tui/src/components/select-list.ts:110-121`,
//! reached through `Editor.handleMouse`, `components/editor.ts:618-638`).
//!
//! These are behaviour tests: they drive the real [`App::step`] wheel path and
//! read the dropdown's selected index / the transcript's scroll offset back
//! out. The frozen frames live in `lum1436_autocomplete_wheel_frames.rs`.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::autocomplete::{CombinedAutocompleteProvider, SlashCommand};
use pi_tui::input::{InputEvent, Key};
use pi_tui::message::MessageItem;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const COLS: u16 = 76;
const ROWS: u16 = 16;

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

fn commands() -> Vec<SlashCommand> {
    vec![
        SlashCommand::new("help").with_description("Show this help text"),
        SlashCommand::new("model").with_description("Choose a model"),
        SlashCommand::new("clear").with_description("Clear the transcript"),
        SlashCommand::new("copy").with_description("Copy the last reply"),
        SlashCommand::new("name").with_description("Name this session"),
        SlashCommand::new("new").with_description("Start a new session"),
    ]
}

/// An App with `lines` transcript items, a slash-command provider and one
/// render behind it (so the viewport and the dropdown geometry are known).
fn app_with_lines(lines: usize) -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1436-autocomplete-wheel".into(),
            ..AppConfig::default()
        },
    );
    for i in 0..lines {
        app.messages_mut()
            .push(MessageItem::user(format!("transcript line {i}")));
    }
    app.prompt_mut()
        .editor_mut()
        .set_autocomplete_provider(Arc::new(CombinedAutocompleteProvider::new(commands(), ".")));
    let _ = app.render_snapshot(COLS, ROWS);
    app
}

/// Open the dropdown by typing `/` and re-render so the list is painted.
fn open_dropdown(app: &mut App) {
    app.step(InputEvent::Key(Key::char('/')));
    assert!(
        app.prompt().editor().is_showing_autocomplete(),
        "typing `/` should have opened the dropdown"
    );
    let _ = app.render_snapshot(COLS, ROWS);
}

fn frame(app: &mut App) -> Buffer {
    let area = Rect::new(0, 0, COLS, ROWS);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    buf
}

fn row_text(buf: &Buffer, y: u16) -> String {
    (0..COLS)
        .map(|x| {
            buf.cell((x, y))
                .expect("cell in bounds")
                .symbol()
                .to_string()
        })
        .collect()
}

/// The painted row containing `needle`.
fn row_with(app: &mut App, needle: &str) -> u16 {
    let buf = frame(app);
    (0..ROWS)
        .find(|y| row_text(&buf, *y).contains(needle))
        .unwrap_or_else(|| panic!("no painted row contains {needle:?}"))
}

fn selected(app: &App) -> usize {
    app.prompt().editor().autocomplete_selected()
}

#[test]
fn wheel_over_the_dropdown_moves_the_highlight_and_not_the_transcript() {
    let mut app = app_with_lines(30);
    open_dropdown(&mut app);
    let list_row = row_with(&mut app, "→ help");
    assert_eq!(selected(&app), 0);
    assert_eq!(app.messages().scroll_offset(), 0);

    assert_eq!(
        app.step(InputEvent::wheel(false, false, 2, list_row)),
        StepOutcome::Redraw
    );
    assert_eq!(selected(&app), 1, "one notch = one candidate");
    assert_eq!(
        app.messages().scroll_offset(),
        0,
        "the list owns the notch; the transcript behind it must not move"
    );
    assert_eq!(
        app.editor_text(),
        "/",
        "steering the list is not a completion"
    );

    // …and back up.
    let _ = app.render_snapshot(COLS, ROWS);
    assert_eq!(
        app.step(InputEvent::wheel(true, false, 2, list_row)),
        StepOutcome::Redraw
    );
    assert_eq!(selected(&app), 0);
    assert_eq!(app.messages().scroll_offset(), 0);
}

#[test]
fn wheel_over_the_dropdown_clamps_at_both_ends() {
    let mut app = app_with_lines(30);
    open_dropdown(&mut app);
    let list_row = row_with(&mut app, "→ help");

    // Already on the first candidate: a notch up is still the list's, so it
    // reports Idle *and* leaves the transcript where it is.
    assert_eq!(
        app.step(InputEvent::wheel(true, false, 2, list_row)),
        StepOutcome::Idle
    );
    assert_eq!(selected(&app), 0);
    assert_eq!(app.messages().scroll_offset(), 0);

    // Six commands; walk to the last one and try to go past it. Unlike the
    // keyboard's Up/Down the wheel clamps instead of wrapping
    // (`SelectList.handleMouse`).
    for _ in 0..5 {
        let _ = app.render_snapshot(COLS, ROWS);
        app.step(InputEvent::wheel(false, false, 2, list_row));
    }
    assert_eq!(selected(&app), 5, "five notches down from the first of six");
    let _ = app.render_snapshot(COLS, ROWS);
    assert_eq!(
        app.step(InputEvent::wheel(false, false, 2, list_row)),
        StepOutcome::Idle
    );
    assert_eq!(selected(&app), 5, "must not wrap to the first candidate");
    assert_eq!(app.messages().scroll_offset(), 0);
}

#[test]
fn alt_wheel_over_the_dropdown_still_steps_one_candidate() {
    // Alt multiplies the *viewport* step by five, but the list reads the
    // sign of the notch only.
    let mut app = app_with_lines(30);
    open_dropdown(&mut app);
    let list_row = row_with(&mut app, "→ help");

    assert_eq!(
        app.step(InputEvent::wheel(false, true, 2, list_row)),
        StepOutcome::Redraw
    );
    assert_eq!(selected(&app), 1);
    assert_eq!(app.messages().scroll_offset(), 0);
}

#[test]
fn wheel_on_the_counter_row_still_steers_the_list() {
    // The `(n/m)` line is part of the painted list block, so upstream's
    // `renderedAutocompleteHeight` test covers it even though it is not a
    // click target.
    let mut app = app_with_lines(30);
    app.set_autocomplete_max_visible(3);
    open_dropdown(&mut app);
    let counter_row = row_with(&mut app, "(1/6)");

    assert_eq!(
        app.step(InputEvent::wheel(false, false, 2, counter_row)),
        StepOutcome::Redraw
    );
    assert_eq!(selected(&app), 1);
    assert_eq!(app.messages().scroll_offset(), 0);
}

#[test]
fn wheel_outside_the_dropdown_still_scrolls_the_transcript() {
    let mut app = app_with_lines(30);
    open_dropdown(&mut app);
    let list_row = row_with(&mut app, "→ help");
    assert!(list_row > 0, "the list borrows rows below the top");

    // A row above the list is transcript, so the notch is unclaimed and
    // `routeWheel` scrolls the log.
    assert_eq!(
        app.step(InputEvent::wheel(true, false, 2, 0)),
        StepOutcome::Redraw
    );
    assert_eq!(app.messages().scroll_offset(), 1);
    assert_eq!(selected(&app), 0, "the list highlight is untouched");
    assert_eq!(app.editor_text(), "/");
}

#[test]
fn the_wheel_returns_to_the_transcript_once_the_dropdown_closes() {
    let mut app = app_with_lines(30);
    open_dropdown(&mut app);
    let list_row = row_with(&mut app, "→ help");

    // A space ends the command context, so the provider yields nothing and
    // the list closes — the row it borrowed is transcript again.
    app.step(InputEvent::Key(Key::char(' ')));
    assert!(!app.prompt().editor().is_showing_autocomplete());
    let _ = app.render_snapshot(COLS, ROWS);

    assert_eq!(
        app.step(InputEvent::wheel(true, false, 2, list_row)),
        StepOutcome::Redraw
    );
    assert_eq!(app.messages().scroll_offset(), 1);
}

#[test]
fn wheel_over_the_scrollbar_updates_the_hover_from_the_notch_cell() {
    // Upstream's `routeWheel` ends with `updateScrollbarHover(event.x,
    // event.y)`, so a notch over the bar lights it up without a move event.
    let mut app = app_with_lines(60);
    let _ = app.render_snapshot(COLS, ROWS);
    assert!(app.messages().is_following());
    assert!(!app.scrollbar_hovered());
    let column = app
        .scrollbar_geometry()
        .expect("a long transcript paints a bar")
        .column;

    // Scrolling to the tail is already a no-op, so the only reason this
    // reports a redraw is the hover flag.
    assert_eq!(
        app.step(InputEvent::wheel(false, false, column, 3)),
        StepOutcome::Redraw
    );
    assert!(app.scrollbar_hovered());
    assert!(app.messages().is_following());

    // A notch away from the bar clears it again.
    assert_eq!(
        app.step(InputEvent::wheel(false, false, 0, 3)),
        StepOutcome::Redraw
    );
    assert!(!app.scrollbar_hovered());
}
