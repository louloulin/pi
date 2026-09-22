//! LUM-1431 — the composer's autocomplete dropdown owns the pointer.
//!
//! Upstream checks the dropdown rectangle **before** the editor body and
//! before the screen-level text selection (`Editor.handleMouse`,
//! `packages/tui/src/components/editor.ts:618-638`, which forwards to
//! `SelectList.handleMouse`). The port painted the list — `App::paint_autocomplete`
//! has drawn it since LUM-1236 — but no mouse target existed for it, so a click
//! on a candidate started a text selection of the transcript rows the list was
//! covering, and the only way to pick a candidate was Up/Down + Tab.
//!
//! These tests drive the real frame and the real pointer path: they open the
//! dropdown by typing `/`, read the candidate row out of the rendered
//! `ratatui::Buffer`, and send `InputEvent::MouseGesture`s to the cells that
//! frame put the list on. Nothing reads `App` internals, so the assertions
//! cannot agree with the geometry by sharing its arithmetic.
//!
//! Semantics mirrored from `SelectList.handleMouse`:
//!
//! * a **press** on a row highlights it and takes the pointer (no selection
//!   behind the list, no completion applied yet);
//! * a **click** (release on the same cell) applies that candidate and closes
//!   the dropdown;
//! * a release on a **different** cell drops the gesture without applying;
//! * the trailing `(n/m)` counter row is not a candidate and is not clickable;
//! * once the dropdown closes, the cells it covered belong to the transcript
//!   again (the hit box is cleared with the frame that stopped painting it).

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::autocomplete::{CombinedAutocompleteProvider, SlashCommand};
use pi_tui::input::{InputEvent, Key, MouseButton, MouseGesture, MouseGestureKind};
use pi_tui::message::MessageItem;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const WIDTH: u16 = 60;
const ROWS: u16 = 16;

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

/// An app with the autocomplete provider installed and a transcript long
/// enough that the dropdown borrows rows that already hold message text —
/// which is exactly the collision these tests are about.
fn app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1431-autocomplete-mouse".into(),
            ..AppConfig::default()
        },
    );
    for index in 0..40 {
        app.messages_mut().push(MessageItem::user(format!(
            "alpha{index} beta{index} gamma{index}"
        )));
    }
    app.prompt_mut()
        .editor_mut()
        .set_autocomplete_provider(Arc::new(CombinedAutocompleteProvider::new(commands(), ".")));
    let _ = app.render_snapshot(WIDTH, ROWS);
    app
}

/// Open the dropdown by typing `/`, the way a terminal delivers it.
fn open_dropdown(app: &mut App) {
    app.step(InputEvent::Key(Key::char('/')));
    assert!(
        app.prompt().editor().is_showing_autocomplete(),
        "typing `/` should open the dropdown"
    );
}

fn frame(app: &mut App) -> Buffer {
    let area = Rect::new(0, 0, WIDTH, ROWS);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    buf
}

fn row_text(buf: &Buffer, y: u16) -> String {
    (0..WIDTH)
        .map(|x| {
            buf.cell((x, y))
                .expect("cell in bounds")
                .symbol()
                .to_string()
        })
        .collect()
}

/// Screen row whose text contains `needle`.
fn row_with(buf: &Buffer, needle: &str) -> u16 {
    (0..ROWS)
        .find(|y| row_text(buf, *y).contains(needle))
        .unwrap_or_else(|| panic!("no rendered row contains {needle:?}"))
}

fn press(x: u16, y: u16) -> InputEvent {
    InputEvent::MouseGesture(MouseGesture::new(
        MouseGestureKind::Press(MouseButton::Left),
        x,
        y,
        false,
    ))
}

fn release(x: u16, y: u16) -> InputEvent {
    InputEvent::MouseGesture(MouseGesture::new(
        MouseGestureKind::Release(MouseButton::Left),
        x,
        y,
        false,
    ))
}

fn drag(x: u16, y: u16) -> InputEvent {
    InputEvent::MouseGesture(MouseGesture::new(
        MouseGestureKind::Drag(MouseButton::Left),
        x,
        y,
        false,
    ))
}

fn click(app: &mut App, x: u16, y: u16) {
    let _ = app.step(press(x, y));
    let _ = app.step(release(x, y));
}

/// Column the candidate label starts at (the two-cell `❯ ` / `  ` gutter).
const LABEL_COLUMN: u16 = 1;

#[test]
fn a_press_on_a_candidate_highlights_it_without_applying_it() {
    let mut app = app();
    open_dropdown(&mut app);
    let buf = frame(&mut app);
    let model_row = row_with(&buf, "model");
    assert!(
        !row_text(&buf, model_row).starts_with('❯'),
        "the best match starts selected, not `model`"
    );

    let _ = app.step(press(LABEL_COLUMN, model_row));

    // The press moved the highlight to the pressed row …
    let buf = frame(&mut app);
    assert!(
        row_text(&buf, model_row).starts_with("❯ model"),
        "pressing a row should highlight it: {:?}",
        row_text(&buf, model_row)
    );
    // … and did not apply anything: the draft is still the bare `/`.
    assert_eq!(app.editor_text(), "/");
    assert!(app.prompt().editor().is_showing_autocomplete());
    assert!(
        !app.has_selection(),
        "a press on a candidate must not start a transcript selection"
    );
}

#[test]
fn a_click_on_a_candidate_applies_the_completion_and_closes_the_dropdown() {
    let mut app = app();
    open_dropdown(&mut app);
    let buf = frame(&mut app);
    let clear_row = row_with(&buf, "clear");

    click(&mut app, LABEL_COLUMN, clear_row);

    assert_eq!(app.editor_text(), "/clear ");
    assert!(
        !app.prompt().editor().is_showing_autocomplete(),
        "applying a candidate closes the dropdown"
    );
    // The row the list borrowed goes back to the transcript.
    let buf = frame(&mut app);
    assert!(
        !row_text(&buf, clear_row).contains("clear"),
        "the dropdown should be gone from the frame: {:?}",
        row_text(&buf, clear_row)
    );
}

#[test]
fn a_drag_from_a_candidate_extends_no_selection() {
    let mut app = app();
    open_dropdown(&mut app);
    let buf = frame(&mut app);
    let help_row = row_with(&buf, "help");

    let _ = app.step(press(LABEL_COLUMN, help_row));
    let _ = app.step(drag(LABEL_COLUMN + 20, help_row));

    assert!(
        !app.has_selection(),
        "dragging across a candidate must not select the transcript behind it"
    );
    assert_eq!(app.editor_text(), "/");
}

#[test]
fn a_release_on_another_cell_does_not_apply() {
    let mut app = app();
    open_dropdown(&mut app);
    let buf = frame(&mut app);
    let copy_row = row_with(&buf, "copy");

    let _ = app.step(press(LABEL_COLUMN, copy_row));
    // Released one row lower: upstream drops the gesture (`mousePressMoved`)
    // instead of synthesising a click on the row the pointer happens to be on.
    let _ = app.step(release(LABEL_COLUMN, copy_row + 1));

    assert_eq!(app.editor_text(), "/");
    assert!(app.prompt().editor().is_showing_autocomplete());

    // A follow-up click still works — the pending press was cleared.
    click(&mut app, LABEL_COLUMN, copy_row);
    assert_eq!(app.editor_text(), "/copy ");
}

#[test]
fn the_counter_row_is_not_a_candidate() {
    let mut app = app();
    // Six commands in a three-row window: the list shows three candidates plus
    // the `(n/6)` counter, which must not be clickable.
    app.set_autocomplete_max_visible(3);
    open_dropdown(&mut app);
    let buf = frame(&mut app);
    let counter_row = row_with(&buf, "(1/6)");

    click(&mut app, LABEL_COLUMN, counter_row);

    assert_eq!(app.editor_text(), "/", "the counter row is not a candidate");
    assert!(app.prompt().editor().is_showing_autocomplete());
    assert!(!app.has_selection());
}

#[test]
fn a_windowed_click_applies_the_pressed_candidate_not_the_row_it_re_centred_to() {
    let mut app = app();
    app.set_autocomplete_max_visible(3);
    open_dropdown(&mut app);
    let buf = frame(&mut app);
    let clear_row = row_with(&buf, "clear");

    // Pressing `clear` (index 2) re-centres the three-row window on it, so the
    // row under the pointer now shows a different candidate. The click must
    // still apply the one that was pressed (upstream's
    // `clickedIndex = mousePressedIndex ?? itemIndex`).
    let _ = app.step(press(LABEL_COLUMN, clear_row));
    let re_centred = frame(&mut app);
    assert!(
        !row_text(&re_centred, clear_row).contains("clear"),
        "pressing re-centres the window, so the row under the pointer moved: {:?}",
        row_text(&re_centred, clear_row)
    );
    let _ = app.step(release(LABEL_COLUMN, clear_row));

    assert_eq!(app.editor_text(), "/clear ");
}

#[test]
fn the_hit_box_is_cleared_when_the_dropdown_closes() {
    let mut app = app();
    open_dropdown(&mut app);
    let buf = frame(&mut app);
    let help_row = row_with(&buf, "help");
    let cell = (LABEL_COLUMN, help_row);

    click(&mut app, cell.0, cell.1);
    assert!(!app.prompt().editor().is_showing_autocomplete());

    // The same row now belongs to the transcript again: a double click on a
    // word there selects it. (The word column is read from the frame *after*
    // the dropdown closed, so the pointer lands on a real glyph.)
    let after = frame(&mut app);
    let text = row_text(&after, cell.1);
    assert!(
        !text.contains("help"),
        "the dropdown row should be transcript text again: {text:?}"
    );
    let word_col = text
        .find("alpha")
        .map(|byte| text[..byte].chars().count() as u16 + 1)
        .expect("a transcript word on the borrowed row");

    let _ = app.step(press(word_col, cell.1));
    let _ = app.step(release(word_col, cell.1));
    let _ = app.step(press(word_col, cell.1));

    assert!(
        app.selection_text().is_some(),
        "once the dropdown is gone, a click on those cells selects transcript text"
    );
}
