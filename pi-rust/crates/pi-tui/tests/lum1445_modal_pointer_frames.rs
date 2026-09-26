//! LUM-1445 — the modal picker under the pointer, rendered.
//!
//! Frame source for `docs/screenshots/lum1445-modal-pointer-*.png`:
//! `cargo test -- --nocapture` prints the cell grid a real
//! [`pi_tui::App::render_to_buffer`] produced and `scripts/frame_to_png.py`
//! paints it. Two panels, both 76×18:
//!
//! 1. the picker open over the transcript, highlight on the first row
//!    (`→ faux-a`) — the reference frame;
//! 2. after a **left click on the `faux-c` row**: the press moved the
//!    highlight onto it, the release activated it (the App hands the value
//!    to the driver, which closes the picker), and the transcript rows the
//!    list covered are unchanged — the pointer never reached the log.
//!
//! Why not `scripts/pty_capture.py`: this Windows runner has no PTY (see
//! `docs/LUM1426_POINTER_COLUMNS.md` §9). These are frames, so they prove the
//! painted geometry and the highlight, not the click timing.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::input::{InputEvent, MouseButton, MouseGesture, MouseGestureKind};
use pi_tui::message::MessageItem;
use pi_tui::selector::{Selector, SelectorItem};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const COLS: u16 = 76;
const ROWS: u16 = 18;

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
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1445-modal-pointer-frames".into(),
            ..AppConfig::default()
        },
    );
    for line in [
        "Which model should this session use?",
        "The picker is painted over the transcript, so it owns the pointer.",
        "alpha beta gamma delta epsilon zeta eta theta",
        "iota kappa lambda mu nu xi omicron pi rho sigma",
        "tau upsilon phi chi psi omega",
    ] {
        app.messages_mut().push(MessageItem::user(line.to_string()));
    }
    app.open_selector(Selector::new(
        "Pick a model",
        vec![
            SelectorItem::new("model:faux-a", "faux-a").with_description("first model"),
            SelectorItem::new("model:faux-b", "faux-b").with_description("second model"),
            SelectorItem::new("model:faux-c", "faux-c").with_description("third model"),
            SelectorItem::new("model:faux-d", "faux-d").with_description("fourth model"),
        ],
    ));
    let _ = app.render_snapshot(COLS, ROWS);
    app
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

fn row_with(buf: &Buffer, needle: &str) -> u16 {
    (0..ROWS)
        .find(|y| row_text(buf, *y).contains(needle))
        .unwrap_or_else(|| panic!("no rendered row contains {needle:?}"))
}

fn frame(app: &mut App) -> Buffer {
    let area = Rect::new(0, 0, COLS, ROWS);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    buf
}

/// One buffer row as plain text (wide glyphs collapse to one cell, exactly
/// like the other frame dumps).
fn row(buf: &Buffer, y: u16) -> String {
    let mut text = String::new();
    let mut skip = 0usize;
    for x in 0..COLS {
        let Some(cell) = buf.cell((x, y)) else {
            break;
        };
        if skip > 0 {
            skip -= 1;
            continue;
        }
        skip = pi_tui::width::columns(cell.symbol()).saturating_sub(1);
        text.push_str(cell.symbol());
    }
    text
}

fn dump(app: &mut App, caption: &str) {
    let buf = frame(app);
    println!("PANEL {caption}");
    println!("FRAME DUMP cols={COLS} rows={ROWS}");
    for y in 0..ROWS {
        println!("|{}|", row(&buf, y));
    }
    println!("END FRAME DUMP");
    println!(
        "  picker_open={} cursor={:?}",
        app.selector_open(),
        app.selector().map(|s| s.cursor())
    );
}

fn gesture(kind: MouseGestureKind, x: u16, y: u16) -> InputEvent {
    InputEvent::gesture(MouseGesture::new(kind, x, y, false))
}

#[test]
fn frame_dump_before_the_click() {
    let mut app = app();
    let buf = frame(&mut app);
    assert!(row_text(&buf, row_with(&buf, "→ faux-a")).contains("→ faux-a"));
    dump(
        &mut app,
        "before the click: `→ faux-a` highlighted, picker owns the pointer",
    );
}

#[test]
fn frame_dump_after_the_click() {
    let mut app = app();
    let buf = frame(&mut app);
    let target = row_with(&buf, "faux-c");
    // The rows below the list (transcript tail, prompt, status bar) captured
    // before the click, so the frame can show the pointer did not reach them.
    let covered_before: Vec<String> = ((target + 2)..ROWS).map(|y| row_text(&buf, y)).collect();

    app.step(gesture(
        MouseGestureKind::Press(MouseButton::Left),
        4,
        target,
    ));
    app.step(gesture(
        MouseGestureKind::Release(MouseButton::Left),
        4,
        target,
    ));
    let after = frame(&mut app);
    assert!(
        row_text(&after, row_with(&after, "→ faux-c")).contains("→ faux-c"),
        "the click should have left the highlight on `faux-c`"
    );
    assert!(
        !row_text(&after, row_with(&after, "faux-a")).contains('→'),
        "`faux-a` is no longer the highlighted row"
    );
    assert_eq!(
        app.take_selector_commit().as_deref(),
        Some("model:faux-c"),
        "the click committed the row's value for the driver"
    );
    assert_eq!(app.messages().scroll_offset(), 0);
    let covered_after: Vec<String> = ((target + 2)..ROWS).map(|y| row_text(&after, y)).collect();
    assert_eq!(
        covered_before, covered_after,
        "the transcript below the picker must not move under a click"
    );
    dump(
        &mut app,
        "after a left click on `faux-c`: highlight moved, value committed, transcript untouched",
    );
}
