//! LUM-1431 — the autocomplete dropdown under the pointer, rendered.
//!
//! Frame source for `docs/screenshots/lum1431-autocomplete-mouse*.png`:
//! `cargo test -- --nocapture` prints the cell grid a real
//! [`pi_tui::App::render_to_buffer`] produced and `scripts/frame_to_png.py`
//! paints it. Three panels, all 76×16:
//!
//! 1. the dropdown open over the transcript after a **press on `model`** — the
//!    highlight moved to the pressed row (`❯ model`) and the draft is still
//!    `/`, which is what a click must not skip;
//! 2. a six-command list in a three-row window — the trailing `(1/6)` counter
//!    is painted but is not a candidate;
//! 3. after the **click** on `model`: the completion was applied (`/model `),
//!    the list is gone and the transcript row it borrowed is back.
//!
//! Why not `scripts/pty_capture.py`: this Windows runner has no PTY (see
//! `docs/LUM1426_POINTER_COLUMNS.md` §9). These are frames, so they prove the
//! painted geometry and the highlight, not the keystroke timing.

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

fn app(commands: Vec<SlashCommand>) -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1431-autocomplete-frames".into(),
            ..AppConfig::default()
        },
    );
    for line in [
        "Show me how the composer completes a slash command.",
        "The dropdown is painted over the transcript, so it owns the pointer.",
        "alpha beta gamma delta epsilon zeta eta theta",
        "iota kappa lambda mu nu xi omicron pi rho sigma",
        "tau upsilon phi chi psi omega",
    ] {
        app.messages_mut()
            .push(MessageItem::user(line.to_string()));
    }
    app.prompt_mut()
        .editor_mut()
        .set_autocomplete_provider(Arc::new(CombinedAutocompleteProvider::new(commands, ".")));
    let _ = app.render_snapshot(COLS, ROWS);
    app
}

fn base_commands() -> Vec<SlashCommand> {
    vec![
        SlashCommand::new("help").with_description("Show this help text"),
        SlashCommand::new("model").with_description("Choose a model"),
        SlashCommand::new("clear").with_description("Clear the transcript"),
    ]
}

fn six_commands() -> Vec<SlashCommand> {
    vec![
        SlashCommand::new("help").with_description("Show this help text"),
        SlashCommand::new("model").with_description("Choose a model"),
        SlashCommand::new("clear").with_description("Clear the transcript"),
        SlashCommand::new("copy").with_description("Copy the last reply"),
        SlashCommand::new("name").with_description("Name this session"),
        SlashCommand::new("new").with_description("Start a new session"),
    ]
}

fn press(app: &mut App, x: u16, y: u16) {
    let _ = app.step(InputEvent::MouseGesture(MouseGesture::new(
        MouseGestureKind::Press(MouseButton::Left),
        x,
        y,
        false,
    )));
}

fn release(app: &mut App, x: u16, y: u16) {
    let _ = app.step(InputEvent::MouseGesture(MouseGesture::new(
        MouseGestureKind::Release(MouseButton::Left),
        x,
        y,
        false,
    )));
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

/// One buffer row as plain text (the dropdown's selected row is marked by
/// `❯`, and the theme's background is not carried by a text dump).
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
    // A text-only sanity line so a failure is visible without a PNG.
    println!(
        "  draft={:?} dropdown_open={}",
        app.editor_text(),
        app.prompt().editor().is_showing_autocomplete()
    );
}

#[test]
fn frame_dump_for_the_dropdown_press_screenshot() {
    let mut app = app(base_commands());
    app.step(InputEvent::Key(Key::char('/')));
    let buf = frame(&mut app);
    let model_row = row_with(&buf, "model");
    press(&mut app, 1, model_row);
    assert!(
        row(&frame(&mut app), model_row).starts_with("❯ model"),
        "the press should have moved the highlight onto `model`"
    );
    assert_eq!(app.editor_text(), "/", "a press must not apply the candidate");
    dump(&mut app, "press on `model`: highlight moved, nothing applied");
}

#[test]
fn frame_dump_for_the_dropdown_counter_screenshot() {
    let mut app = app(six_commands());
    app.set_autocomplete_max_visible(3);
    app.step(InputEvent::Key(Key::char('/')));
    let buf = frame(&mut app);
    assert!(row_text(&buf, row_with(&buf, "(1/6)")).contains("(1/6)"));
    dump(&mut app, "six commands in a three-row window: `(1/6)` is not clickable");
}

#[test]
fn frame_dump_for_the_dropdown_click_screenshot() {
    let mut app = app(base_commands());
    app.step(InputEvent::Key(Key::char('/')));
    let buf = frame(&mut app);
    let model_row = row_with(&buf, "model");
    press(&mut app, 1, model_row);
    release(&mut app, 1, model_row);
    assert_eq!(app.editor_text(), "/model ");
    assert!(!app.prompt().editor().is_showing_autocomplete());
    dump(&mut app, "click on `model`: completion applied, list closed");
}
