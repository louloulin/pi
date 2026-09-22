//! Drag-select and copy-on-select inside the composer.
//!
//! LUM-1327 gave the composer its pointer *clicks* (caret placement) but kept
//! swallowing drags: `step_composer_mouse_gesture` answered
//! `Some(StepOutcome::Idle)` for every drag / release that started on a
//! composer cell, so a drag over the draft selected nothing and copied
//! nothing. Both reference TUIs do better than that, in two different places:
//!
//! * upstream pi-ts deliberately leaves press / drag / release to the
//!   renderer's screen-level selection — the editor's `handleMouse` returns
//!   `undefined` for anything but a click and the renderer synthesizes that
//!   click when press and release land on one cell without movement
//!   (`packages/tui/src/components/editor.ts:634-638`,
//!   `packages/tui/src/tui-alt-screen.ts:1310-1348`);
//! * codex keeps the same gesture inside the composer
//!   (`chat_composer/mouse.rs` → textarea `handle_mouse` → `copy_selection`),
//!   where a drag selects and the release copies when `copyOnSelect` is on.
//!
//! The port has no screen-level selection, so this is codex's shape with the
//! renderer's *clamping* rule: the selection lives in the composer and a drag
//! that leaves the composer extends to its nearest edge.
//!
//! Covered here (all through the **rendered frame**, like
//! `tests/composer_click.rs`, so the gutter width, the windowing and the `▍`
//! marker stay the renderer's business):
//!
//! * the reversed-video highlight covers exactly the dragged characters;
//! * `App::composer_selection_text` is the draft fragment those cells show;
//! * the release queues the clipboard write the driver turns into OSC 52;
//! * a click still places the caret and copies nothing (LUM-1327 acceptance);
//! * `copyOnSelect = false` keeps the highlight but queues nothing;
//! * a multi-row drag crosses lines, a chip keeps offsets aligned;
//! * a drag that leaves the composer clamps into it, and never reaches the
//!   chat log underneath;
//! * a click / drag is not an edit: the draft and the undo stack are
//!   untouched;
//! * typing drops the highlight.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, ImageContent, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::input::{InputEvent, Key, MouseButton, MouseGesture, MouseGestureKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

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

fn app_with(config: AppConfig) -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    App::new(&agent, config)
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

fn reversed(buf: &Buffer, x: u16, y: u16) -> bool {
    buf.cell((x, y))
        .map(|cell| cell.modifier.contains(Modifier::REVERSED))
        .unwrap_or(false)
}

/// The reversed cells of one row, as `(first_x, last_x)` runs.
fn reversed_runs(buf: &Buffer, y: u16) -> Vec<(u16, u16)> {
    let mut runs: Vec<(u16, u16)> = Vec::new();
    let mut x = 0;
    while x < buf.area.width {
        if !reversed(buf, x, y) {
            x += 1;
            continue;
        }
        let start = x;
        while x + 1 < buf.area.width && reversed(buf, x + 1, y) {
            x += 1;
        }
        runs.push((start, x));
        x += 1;
    }
    runs
}

/// What a reader sees highlighted on one row: the characters of its reversed
/// cells, with the caret glyph — which `Prompt` *inserts* into the row —
/// removed. This is the assertion that ties the highlight to the selection
/// text, whatever the caret did to the columns.
fn highlight_text(buf: &Buffer, y: u16) -> String {
    reversed_runs(buf, y)
        .iter()
        .flat_map(|(start, end)| *start..=*end)
        .filter_map(|x| {
            buf.cell((x, y))
                .map(|cell| cell.symbol().chars().next().unwrap_or(' '))
        })
        .filter(|ch| *ch != CARET)
        .collect()
}

/// Absolute cell of the first cell carrying `ch` — the only sane lookup on a
/// row holding wide glyphs, whose second cell is a filler space.
fn cell_of_ch(buf: &Buffer, ch: char) -> (u16, u16) {
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            if buf
                .cell((x, y))
                .map(|cell| cell.symbol().starts_with(ch))
                .unwrap_or(false)
            {
                return (x, y);
            }
        }
    }
    panic!("no cell carries {ch:?}");
}

fn image(data: &str) -> ImageContent {
    ImageContent {
        mime_type: "image/png".into(),
        data: data.into(),
    }
}

/// A one-row draft, rendered once so the pointer path has a composer
/// rectangle to hit-test against.
fn drafted(text: &str) -> App {
    let mut app = app();
    app.set_editor_text(text);
    let _ = frame(&mut app);
    app
}

#[test]
fn a_drag_selects_the_characters_under_the_pointer_and_highlights_them() {
    let mut app = drafted("hello world");
    let buf = frame(&mut app);
    // "llo wo": the first `l` of `llo` through the `o` of `wor`.
    let (from_x, y) = cell_of(&buf, "llo");
    let (to_x, _) = cell_of(&buf, "orld");

    assert_eq!(app.step(press(from_x, y)), StepOutcome::Redraw);
    assert_eq!(
        app.composer_selection_text(),
        None,
        "a press alone selects nothing"
    );
    assert_eq!(app.step(drag(to_x, y)), StepOutcome::Redraw);
    assert_eq!(
        app.composer_selection_text().as_deref(),
        Some("llo wo"),
        "the fragment between the pressed and the dragged character"
    );

    // The highlight covers exactly those cells — reversed, like the chat
    // log's selection — and nothing else on the row.
    let buf = frame(&mut app);
    assert_eq!(
        highlight_text(&buf, y),
        "llo wo",
        "the reversed cells spell the selected fragment"
    );
    assert_eq!(reversed_runs(&buf, y).len(), 1, "one contiguous run");
    assert!(!reversed(&buf, from_x - 1, y), "the gutter is not selected");
    assert!(
        !reversed(&buf, to_x + 2, y),
        "the padding past the selection is not selected"
    );

    // The release finishes the gesture and queues the clipboard write.
    assert_eq!(app.step(release(to_x, y)), StepOutcome::Idle);
    assert_eq!(app.take_clipboard_request().as_deref(), Some("llo wo"));
    assert_eq!(
        app.composer_selection_text().as_deref(),
        Some("llo wo"),
        "the highlight survives the release"
    );
    assert_eq!(
        app.composer_selection_text().as_deref(),
        Some("llo wo"),
        "and the driver gets exactly the highlighted text"
    );
}

#[test]
fn a_drag_reports_the_draft_fragment_even_when_it_runs_backwards() {
    let mut app = drafted("hello world");
    let buf = frame(&mut app);
    let (from_x, y) = cell_of(&buf, "orld");
    let (to_x, _) = cell_of(&buf, "llo");
    app.step(press(from_x, y));
    app.step(drag(to_x, y));
    assert_eq!(app.composer_selection_text().as_deref(), Some("llo wo"));
    assert_eq!(highlight_text(&frame(&mut app), y), "llo wo");
}

#[test]
fn a_press_outside_the_composer_drops_its_highlight() {
    let mut app = app();
    for i in 0..20 {
        app.info(format!("line {i}"));
    }
    app.set_editor_text("hello world");
    let buf = frame(&mut app);
    let (from_x, y) = cell_of(&buf, "llo");
    let (to_x, _) = cell_of(&buf, "orld");
    app.step(press(from_x, y));
    app.step(drag(to_x, y));
    app.step(release(to_x, y));
    assert_eq!(app.composer_selection_text().as_deref(), Some("llo wo"));

    // A press on the transcript is a new gesture: the composer's highlight
    // goes with the pointer that moved on (upstream re-anchors on every
    // press), and what the transcript does with the press is its own
    // business — this only pins that the stale composer highlight is gone.
    app.step(press(2, 0));
    assert_eq!(app.composer_selection_text(), None);
    assert_eq!(reversed_runs(&frame(&mut app), y), Vec::new());
}

#[test]
fn a_release_updates_the_selection_and_the_caret_to_its_own_cell() {
    // Terminals coalesce motion: the release can carry the last pointer
    // position without a drag event for it, so the release has to finish the
    // gesture at *its* cell rather than at the last motion sample.
    let mut app = drafted("hello world");
    let buf = frame(&mut app);
    let (from_x, y) = cell_of(&buf, "llo");
    let (to_x, _) = cell_of(&buf, "orld");

    app.step(press(from_x, y));
    app.step(drag(from_x + 1, y));
    app.step(release(to_x, y));
    assert_eq!(app.composer_selection_text().as_deref(), Some("llo wo"));
    assert_eq!(
        caret_cell(&frame(&mut app)),
        Some((to_x, y)),
        "the caret ends where the release landed"
    );
}

#[test]
fn a_click_still_places_the_caret_and_copies_nothing() {
    // LUM-1327 acceptance 3, unchanged: press and release on one cell is a
    // click, and a click is neither a selection nor an edit.
    let mut app = drafted("hello world");
    let buf = frame(&mut app);
    let (target_x, target_y) = cell_of(&buf, "llo");
    let undo_before = app.prompt().editor().undo_len();

    assert_eq!(app.step(press(target_x, target_y)), StepOutcome::Redraw);
    assert_eq!(app.step(release(target_x, target_y)), StepOutcome::Idle);

    assert_eq!(caret_cell(&frame(&mut app)), Some((target_x, target_y)));
    assert_eq!(app.composer_selection_text(), None);
    assert_eq!(app.take_clipboard_request(), None, "a click copies nothing");
    assert_eq!(app.selection_text(), None, "and starts no chat selection");
    assert_eq!(app.editor_text(), "hello world");
    assert_eq!(
        app.prompt().editor().undo_len(),
        undo_before,
        "a click pushes no undo"
    );
}

#[test]
fn a_drag_across_rows_selects_through_the_line_break() {
    let mut app = drafted("first line\nsecond line");
    let buf = frame(&mut app);
    let (from_x, first_y) = cell_of(&buf, "irst");
    let (to_x, second_y) = cell_of(&buf, "econd");
    // `cell_of` returns the row's first cell (the `e` of `econd`); one column
    // right is the `c`, so the drag ends on the third character of
    // `second line`.
    let to_x = to_x + 1;
    assert!(second_y > first_y, "the second line is on its own row");

    app.step(press(from_x, first_y));
    app.step(drag(to_x, second_y));
    assert_eq!(
        app.composer_selection_text().as_deref(),
        Some("irst line\nsec")
    );

    let buf = frame(&mut app);
    assert_eq!(highlight_text(&buf, first_y), "irst line");
    assert_eq!(highlight_text(&buf, second_y), "sec");
    // Nothing else on screen is highlighted, at either edge.
    assert!(!reversed(&buf, 0, 0));
    assert!(!reversed(&buf, 0, HEIGHT - 1));
}

#[test]
fn a_drag_that_leaves_the_composer_clamps_into_it() {
    let mut app = drafted("hello world");
    let buf = frame(&mut app);
    let (from_x, y) = cell_of(&buf, "llo");
    // Up and far left: the pointer leaves the composer entirely.
    app.step(press(from_x, y));
    assert_eq!(app.step(drag(0, 0)), StepOutcome::Redraw);
    assert_eq!(
        app.composer_selection_text().as_deref(),
        Some("hel"),
        "the selection runs to the composer's own left edge"
    );
    assert_eq!(
        app.selection_text(),
        None,
        "and the transcript underneath is untouched"
    );
    let buf = frame(&mut app);
    assert_eq!(
        highlight_text(&buf, y),
        "hel",
        "the highlight reaches the composer's left edge"
    );
    assert!(
        !reversed(&buf, 0, y) && !reversed(&buf, 1, y),
        "the label gutter is not the draft"
    );
}

#[test]
fn a_drag_below_the_draft_clamps_to_the_end_of_the_draft() {
    let mut app = drafted("hello");
    let buf = frame(&mut app);
    let (from_x, y) = cell_of(&buf, "llo");
    app.step(press(from_x, y));
    // The padding rows below a short draft are still the composer's.
    assert_eq!(app.step(drag(WIDTH - 1, HEIGHT - 1)), StepOutcome::Redraw);
    assert_eq!(app.composer_selection_text().as_deref(), Some("llo"));
    assert_eq!(app.selection_text(), None);
}

#[test]
fn copy_on_select_off_keeps_the_highlight_but_queues_nothing() {
    let mut app = app_with(AppConfig {
        copy_on_select: false,
        ..AppConfig::default()
    });
    app.set_editor_text("hello world");
    let _ = frame(&mut app);
    let buf = frame(&mut app);
    let (from_x, y) = cell_of(&buf, "llo");
    let (to_x, _) = cell_of(&buf, "orld");

    app.step(press(from_x, y));
    app.step(drag(to_x, y));
    app.step(release(to_x, y));
    assert_eq!(app.composer_selection_text().as_deref(), Some("llo wo"));
    assert_eq!(
        app.take_clipboard_request(),
        None,
        "copy-on-select is off, so nothing is queued"
    );
    assert_eq!(
        highlight_text(&frame(&mut app), y),
        "llo wo",
        "the highlight is a rendering of the selection, not of the copy"
    );
}

#[test]
fn a_drag_that_returns_to_its_start_selects_nothing() {
    let mut app = drafted("hello world");
    let buf = frame(&mut app);
    let (x, y) = cell_of(&buf, "llo");
    let (other_x, _) = cell_of(&buf, "orld");

    app.step(press(x, y));
    app.step(drag(other_x, y));
    app.step(drag(x, y));
    assert_eq!(app.composer_selection_text(), None, "an empty selection");
    app.step(release(x, y));
    assert_eq!(
        app.take_clipboard_request(),
        None,
        "an empty selection copies nothing (upstream needs anchor !== focus)"
    );
}

#[test]
fn typing_after_a_drag_drops_the_highlight() {
    let mut app = drafted("hello world");
    let buf = frame(&mut app);
    let (from_x, y) = cell_of(&buf, "llo");
    let (to_x, _) = cell_of(&buf, "orld");
    app.step(press(from_x, y));
    app.step(drag(to_x, y));
    app.step(release(to_x, y));
    assert_eq!(app.composer_selection_text().as_deref(), Some("llo wo"));

    // A key press moves the caret / changes the draft the offsets were
    // measured against, so the selection goes away with it.
    app.step(InputEvent::Key(Key::char('x')));
    assert_eq!(app.composer_selection_text(), None);
    assert_eq!(
        reversed_runs(&frame(&mut app), y),
        Vec::new(),
        "and its highlight with it"
    );
}

#[test]
fn a_wide_character_is_highlighted_on_both_of_its_cells() {
    // LUM-1336 put the composer's layout on terminal **columns**, so a CJK
    // glyph owns two cells: the highlight has to cover both of them, and the
    // one-cell caret marker (here inserted *between* the glyphs) has to shift
    // them the way the frame painted it.
    let mut app = drafted("你好ab");
    let buf = frame(&mut app);
    let (from_x, y) = cell_of_ch(&buf, '你');
    let (to_x, _) = cell_of_ch(&buf, '好');

    app.step(press(from_x, y));
    app.step(drag(to_x, y));
    assert_eq!(app.composer_selection_text().as_deref(), Some("你好"));
    let buf = frame(&mut app);
    // 你 (2 cells) + the caret marker + 好 (2 cells): five cells, no gap, and
    // nothing past them.
    assert_eq!(reversed_runs(&buf, y), vec![(from_x, from_x + 4)]);
    assert!(!reversed(&buf, from_x + 5, y));
}

#[test]
fn a_chip_in_the_draft_keeps_the_cells_and_the_text_in_step() {
    let mut app = app();
    app.set_editor_text("ok");
    // The chip lands at the caret, i.e. after `ok`, so the draft renders as
    // `ok[Image #1]`. Chip labels are eleven characters of the *display*
    // draft, which is what the offsets are measured in — mixing them up is
    // exactly the "selection slips by ten columns" bug this pins.
    app.paste_image(image("AAAA"));
    let _ = frame(&mut app);
    let buf = frame(&mut app);
    let (from_x, y) = cell_of(&buf, "ok");
    let (to_x, _) = cell_of(&buf, "1]");
    let expected: String = row_text(&buf, y)
        .chars()
        .skip(from_x as usize)
        .take((to_x - from_x + 1) as usize)
        .collect();

    app.step(press(from_x, y));
    app.step(drag(to_x, y));
    assert_eq!(
        app.composer_selection_text().as_deref(),
        Some(expected.as_str()),
        "the selected text is exactly the highlighted cells"
    );
    assert_eq!(expected, "ok[Image #1");
    assert_eq!(highlight_text(&frame(&mut app), y), expected);
}

#[test]
fn a_caret_left_of_the_selection_does_not_shift_the_highlight() {
    // The `▍` marker is *inserted* mid-row, so every cell to its right moves
    // one column: the highlight has to follow the layout, not the column
    // arithmetic. Drag leftwards so the caret ends up inside the selection.
    let mut app = drafted("hello world");
    let buf = frame(&mut app);
    let (from_x, y) = cell_of(&buf, "orld");
    let (to_x, _) = cell_of(&buf, "llo");
    app.step(press(from_x, y));
    app.step(drag(to_x, y));

    let buf = frame(&mut app);
    // The caret sits at the drag's end, one cell left of the selection's
    // first character: the marker is inserted there and the whole highlight
    // shifts one cell right with it.
    let caret = caret_cell(&buf).expect("the drag leaves a caret");
    assert_eq!(caret, (to_x, y), "the caret follows the drag's end");
    assert_eq!(
        highlight_text(&buf, y),
        "llo wo",
        "the shifted cells still spell the selection"
    );
    assert_eq!(reversed_runs(&buf, y), vec![(to_x + 1, from_x + 1)]);
}

#[test]
fn a_composer_drag_never_starts_a_chat_log_selection() {
    // LUM-1327's rule, kept: the transcript's own selection belongs to the
    // transcript. (The drag now produces a *composer* selection — see
    // `a_drag_selects_the_characters_under_the_pointer_and_highlights_them`.)
    let mut app = drafted("hello world");
    let buf = frame(&mut app);
    let (from_x, y) = cell_of(&buf, "llo");
    app.step(press(from_x, y));
    app.step(drag(WIDTH - 1, HEIGHT - 1));
    app.step(release(WIDTH - 1, HEIGHT - 1));
    assert_eq!(app.selection_text(), None);
    assert!(!app.has_selection());
}
