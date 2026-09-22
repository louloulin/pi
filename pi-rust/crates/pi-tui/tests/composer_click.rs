//! Pointer placement in the composer.
//!
//! Both reference TUIs let the pointer position the draft's caret, and the
//! Rust port used to do nothing at all: the transcript viewport stops above
//! the composer (`App::selection_point` returns `None` for a composer row), so
//! a click that landed on the draft was neither a caret placement nor a
//! selection — nobody consumed it.
//!
//! Upstream's rule is the one implemented here
//! (`Editor.handleMouse`'s click branch,
//! `packages/tui/src/components/editor.ts:615-670`; codex routes the same
//! gesture through `chat_composer/mouse.rs`):
//!
//! * a left click on a draft cell puts the caret on the character under the
//!   pointer;
//! * a click on the label gutter belongs to the draft's first column;
//! * a click past the end of a soft-wrapped row snaps back onto that row's
//!   last character instead of spilling onto the next visual row;
//! * a click on a padding row below a short draft, or on the `Ctrl+R` search
//!   row, is absorbed without moving the caret;
//! * a click on the composer never starts or extends a chat-log selection;
//! * the click is not an edit: the draft is byte-for-byte unchanged.
//!
//! Every assertion reads the **rendered frame**, not internal state: the
//! `▍` marker is placed where the user sees it, and the click coordinates are
//! computed from the same frame, so the gutter width and the windowing are the
//! renderer's business rather than the test's.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, ImageContent, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::input::{
    InputEvent, KeyCode, KeyModifiers, MouseButton, MouseGesture, MouseGestureKind,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const WIDTH: u16 = 40;
const HEIGHT: u16 = 14;
/// The caret glyph `Prompt` draws (`build_prompt_row`).
const CARET: char = '▍';

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
    App::new(&agent, AppConfig::default())
}

fn image(data: &str) -> ImageContent {
    ImageContent {
        mime_type: "image/png".into(),
        data: data.into(),
    }
}

fn frame(app: &mut App) -> Buffer {
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    buf
}

/// The text of one rendered row.
fn row_text(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width)
        .map(|x| {
            buf.cell((x, y))
                .map(|cell| cell.symbol().chars().next().unwrap_or(' '))
                .unwrap_or(' ')
        })
        .collect()
}

/// The first row whose text contains `needle`.
fn row_with(buf: &Buffer, needle: &str) -> u16 {
    (0..buf.area.height)
        .find(|y| row_text(buf, *y).contains(needle))
        .unwrap_or_else(|| panic!("no row contains {needle:?}"))
}

/// Absolute cell of the first occurrence of `needle` on the row that carries
/// it (character column, which is also the cell column: the composer draws one
/// character per cell).
fn cell_of(buf: &Buffer, needle: &str) -> (u16, u16) {
    let y = row_with(buf, needle);
    let chars: Vec<char> = row_text(buf, y).chars().collect();
    let target: Vec<char> = needle.chars().collect();
    let x = chars
        .windows(target.len())
        .position(|window| window == target.as_slice())
        .unwrap_or_else(|| panic!("no column on row {y} carries {needle:?}"));
    (x as u16, y)
}

/// Absolute cell of the caret marker in a frame, if it is on screen.
fn caret_cell(buf: &Buffer) -> Option<(u16, u16)> {
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            if buf
                .cell((x, y))
                .map(|cell| cell.symbol().starts_with(CARET))
                .unwrap_or(false)
            {
                return Some((x, y));
            }
        }
    }
    None
}

fn gesture(kind: MouseGestureKind, x: u16, y: u16) -> InputEvent {
    InputEvent::gesture(MouseGesture::new(kind, x, y, false))
}

/// A click: a left press and its release on the same cell. Returns the press's
/// outcome (the release is always a no-op by design).
fn click(app: &mut App, x: u16, y: u16) -> StepOutcome {
    let pressed = app.step(gesture(MouseGestureKind::Press(MouseButton::Left), x, y));
    app.step(gesture(MouseGestureKind::Release(MouseButton::Left), x, y));
    pressed
}

#[test]
fn a_click_puts_the_caret_on_the_character_under_the_pointer() {
    let mut app = app();
    app.set_editor_text("hello world");
    let buf = frame(&mut app);
    // The caret starts at the end of the draft.
    let (caret_x, _) = caret_cell(&buf).expect("a caret is painted");
    let (target_x, target_y) = cell_of(&buf, "llo");
    assert!(target_x < caret_x);

    assert_eq!(click(&mut app, target_x, target_y), StepOutcome::Redraw);
    let buf = frame(&mut app);
    assert_eq!(
        caret_cell(&buf),
        Some((target_x, target_y)),
        "the caret moved onto the clicked character"
    );
    assert_eq!(app.editor_text(), "hello world");
}

#[test]
fn a_click_on_the_label_gutter_is_the_first_column() {
    let mut app = app();
    app.set_editor_text("hello");
    // Move the caret into the middle first, so column 0 cannot be a no-op.
    let buf = frame(&mut app);
    let (mid_x, mid_y) = cell_of(&buf, "llo");
    click(&mut app, mid_x, mid_y);
    let buf = frame(&mut app);
    let (caret_x, _) = caret_cell(&buf).expect("a caret is painted");
    assert!(caret_x > 0);

    // The caret now splits the rendered word ("> he▍llo"), so the first draft
    // character is found by a pair that survives the split.
    let (first_x, first_y) = cell_of(&frame(&mut app), "he");
    // Column 0 of the row is the label's own first cell.
    assert_eq!(click(&mut app, 0, first_y), StepOutcome::Redraw);
    assert_eq!(caret_cell(&frame(&mut app)), Some((first_x, first_y)));
}

#[test]
fn a_click_on_the_current_cell_costs_no_repaint() {
    let mut app = app();
    app.set_editor_text("hello");
    let (x, y) = cell_of(&frame(&mut app), "hello");
    assert_eq!(click(&mut app, x, y), StepOutcome::Redraw);
    assert_eq!(click(&mut app, x, y), StepOutcome::Idle);
}

#[test]
fn a_click_past_a_soft_wrapped_row_stays_on_that_row() {
    let mut app = app();
    // 80 characters at a 38-column body width: the draft wraps over several
    // visual rows, and the first body row is full.
    app.set_editor_text(&"w".repeat(80));
    let buf = frame(&mut app);
    let (_, first_y) = cell_of(&buf, "wwww");
    // The row's last cell belongs to the row's last character.
    let last_x = WIDTH - 1;
    assert_eq!(click(&mut app, last_x, first_y), StepOutcome::Redraw);
    let clicked_cell = caret_cell(&frame(&mut app));
    assert_eq!(
        clicked_cell,
        Some((last_x, first_y)),
        "a click on the row's last cell stays on that row"
    );
}

#[test]
fn an_open_reverse_search_absorbs_the_composer_pointer() {
    let mut app = app();
    app.info("an earlier prompt");
    app.set_editor_text("hello world");
    let before = caret_cell(&frame(&mut app)).expect("a caret is painted");
    // `Ctrl+R` opens the reverse search, which owns the composer until it is
    // accepted or cancelled (exactly as it owns the keyboard).
    assert_eq!(
        app.step(InputEvent::key(KeyCode::Char('r'), KeyModifiers::CONTROL)),
        StepOutcome::Redraw
    );
    let (search_x, search_y) = cell_of(&frame(&mut app), "reverse-i-search:");
    let (draft_x, draft_y) = cell_of(&frame(&mut app), "world");
    // The composer region grows *upward* (it is docked at the bottom), so the
    // search row takes the row the region gained.
    let (_, _, _, height) = app.composer_area();
    assert_eq!(height, 2, "the search row grew the composer by one row");
    assert_eq!(search_y + 1, draft_y, "the draft is below the search row");

    // Neither row places a caret while the search is open.
    assert_eq!(click(&mut app, search_x, search_y), StepOutcome::Idle);
    assert_eq!(click(&mut app, draft_x, draft_y), StepOutcome::Idle);
    assert_eq!(
        caret_cell(&frame(&mut app)),
        Some(before),
        "the caret did not move"
    );

    // Cancel the search and the pointer works again, on the row it is on.
    assert_eq!(
        app.step(InputEvent::key(KeyCode::Esc, KeyModifiers::NONE)),
        StepOutcome::Redraw
    );
    let (target_x, target_y) = cell_of(&frame(&mut app), "llo");
    assert_eq!(click(&mut app, target_x, target_y), StepOutcome::Redraw);
    assert_eq!(caret_cell(&frame(&mut app)), Some((target_x, target_y)));
}

#[test]
fn a_click_on_the_composer_never_starts_a_selection() {
    let mut app = app();
    for i in 0..20 {
        app.info(format!("line {i}"));
    }
    app.set_editor_text("hello world");
    let (x, y) = cell_of(&frame(&mut app), "world");
    app.step(gesture(MouseGestureKind::Press(MouseButton::Left), x, y));
    // A drag while the composer press is in flight must not extend a
    // transcript selection (the composer has no drag-select of its own).
    assert_eq!(
        app.step(gesture(MouseGestureKind::Drag(MouseButton::Left), 20, 1)),
        StepOutcome::Idle
    );
    assert_eq!(app.selection_text(), None);
    app.step(gesture(MouseGestureKind::Release(MouseButton::Left), 20, 1));
    assert_eq!(app.selection_text(), None);
}

#[test]
fn the_click_does_not_edit_the_draft() {
    let mut app = app();
    app.set_editor_text("hello world");
    // A selection elsewhere proves the click did not route to the transcript.
    let (x, y) = cell_of(&frame(&mut app), "llo");
    click(&mut app, x, y);
    assert_eq!(app.editor_text(), "hello world");
    let buf = frame(&mut app);
    assert_eq!(
        row_text(&buf, y).trim_end(),
        "> he▍llo world",
        "the click moved the caret and changed nothing else"
    );
}

/// A folded paste renders as a multi-column `[paste #N +M lines]` marker, so
/// the grid ↔ draft mapping has to measure it exactly like a chip label —
/// otherwise a click anywhere after the marker would land somewhere else in
/// the draft (LUM-1318 folded pastes, LUM-1327 pointer placement).
#[test]
fn a_click_after_a_folded_paste_marker_lands_on_the_clicked_character() {
    let mut app = app();
    let pasted = (1..=200)
        .map(|row| format!("pasted line {row}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.paste_text(&pasted);
    // Typed after the marker through the ordinary insert path, so the folded
    // paste stays folded (`set_editor_text` is a programmatic write and drops
    // stale payloads by design).
    app.paste_text(" tail");
    // The 200-line paste is one marker, so the draft is `[paste #1 +200
    // lines] tail` on a single row.
    assert_eq!(app.prompt().editor().paste_count(), 1);
    let buf = frame(&mut app);
    let (tail_x, tail_y) = cell_of(&buf, "tail");
    let (marker_x, marker_y) = cell_of(&buf, "[paste");

    assert_eq!(click(&mut app, tail_x, tail_y), StepOutcome::Redraw);
    assert_eq!(caret_cell(&frame(&mut app)), Some((tail_x, tail_y)));

    // A click *inside* the label resolves to the marker's own byte, never to
    // a byte in the middle of its label text — the caret lands on the marker
    // boundary just past it (the same rule a chip label follows).
    let label_width = "[paste #1 +200 lines]".chars().count() as u16;
    let (inside_x, inside_y) = cell_of(&frame(&mut app), "+200 lines");
    assert_ne!(inside_x, marker_x, "the click is inside the label");
    assert_eq!(click(&mut app, inside_x, inside_y), StepOutcome::Redraw);
    assert_eq!(
        caret_cell(&frame(&mut app)),
        Some((marker_x + label_width, marker_y)),
        "a click inside the label snaps onto the marker's own byte"
    );

    // Same rule for the other attachment kind: a chip is one sentinel that
    // renders ten columns wide.
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut chips = App::new(&agent, AppConfig::default());
    chips.paste_image(image("one"));
    chips.paste_text(" tail");
    let chip_x = cell_of(&frame(&mut chips), "[Image").0;
    let (chip_inside_x, chip_inside_y) = cell_of(&frame(&mut chips), "mage");
    assert_eq!(
        click(&mut chips, chip_inside_x, chip_inside_y),
        StepOutcome::Redraw
    );
    assert_eq!(
        caret_cell(&frame(&mut chips)),
        Some((chip_x + "[Image #1]".chars().count() as u16, chip_inside_y)),
        "the marker rule is the chip rule"
    );
    assert_eq!(chips.editor_text(), "[Image #1] tail");

    // Neither click edited the draft: the marker still carries all 200 lines.
    assert_eq!(app.prompt().editor().paste_count(), 1);
    assert_eq!(app.editor_text(), format!("{pasted} tail"));
}

#[test]
fn a_click_on_the_second_row_of_a_multi_line_draft_uses_that_row() {
    let mut app = app();
    app.set_editor_text("first line\nsecond line");
    let buf = frame(&mut app);
    let (_, second_y) = cell_of(&buf, "second");
    let (target_x, _) = cell_of(&buf, "cond");
    assert_eq!(click(&mut app, target_x, second_y), StepOutcome::Redraw);
    assert_eq!(caret_cell(&frame(&mut app)), Some((target_x, second_y)));
    assert_eq!(app.editor_text(), "first line\nsecond line");
}
