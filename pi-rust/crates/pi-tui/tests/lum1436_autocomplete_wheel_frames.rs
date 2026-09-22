//! LUM-1436 — the autocomplete dropdown under a wheel notch, rendered.
//!
//! Frame source for `docs/screenshots/lum1436-autocomplete-wheel-*.png`:
//! `cargo test -- --nocapture` prints the cell grid a real
//! [`pi_tui::App::render_to_buffer`] produced and `scripts/frame_to_png.py`
//! paints it. Two panels, both 76×16:
//!
//! 1. the dropdown open over the transcript, highlight on the first candidate
//!    (`❯ help`) — the reference frame;
//! 2. after a **wheel notch down** at the list's own row: the highlight moved
//!    to `❯ model`, the draft is still `/` and the transcript row the list
//!    covers is unchanged, i.e. the notch never reached the chat log.
//!
//! Why not `scripts/pty_capture.py`: this Windows runner has no PTY (see
//! `docs/LUM1426_POINTER_COLUMNS.md` §9). These are frames, so they prove the
//! painted geometry and the highlight, not the wheel timing.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
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

fn app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1436-autocomplete-wheel-frames".into(),
            ..AppConfig::default()
        },
    );
    for line in [
        "Show me how the composer completes a slash command.",
        "The dropdown is painted over the transcript, so it owns the wheel.",
        "alpha beta gamma delta epsilon zeta eta theta",
        "iota kappa lambda mu nu xi omicron pi rho sigma",
        "tau upsilon phi chi psi omega",
    ] {
        app.messages_mut().push(MessageItem::user(line.to_string()));
    }
    app.prompt_mut()
        .editor_mut()
        .set_autocomplete_provider(Arc::new(CombinedAutocompleteProvider::new(
            vec![
                SlashCommand::new("help").with_description("Show this help text"),
                SlashCommand::new("model").with_description("Choose a model"),
                SlashCommand::new("clear").with_description("Clear the transcript"),
            ],
            ".",
        )));
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
        "  draft={:?} dropdown_open={} selected={}",
        app.editor_text(),
        app.prompt().editor().is_showing_autocomplete(),
        app.prompt().editor().autocomplete_selected()
    );
}

#[test]
fn frame_dump_before_the_wheel_notch() {
    let mut app = app();
    app.step(InputEvent::Key(Key::char('/')));
    let buf = frame(&mut app);
    assert!(row_text(&buf, row_with(&buf, "❯ help")).contains("❯ help"));
    dump(
        &mut app,
        "before the notch: `❯ help` highlighted, draft `/`",
    );
}

#[test]
fn frame_dump_after_the_wheel_notch() {
    let mut app = app();
    app.step(InputEvent::Key(Key::char('/')));
    let buf = frame(&mut app);
    let list_row = row_with(&buf, "❯ help");
    // The rows the list borrows, captured before the notch so the frame can
    // show they did not move.
    let covered_before: Vec<String> = ((list_row.saturating_sub(3))..list_row)
        .map(|y| row_text(&buf, y))
        .collect();

    app.step(InputEvent::wheel(false, false, 2, list_row));
    let after = frame(&mut app);
    assert!(
        row_text(&after, row_with(&after, "❯ model")).contains("❯ model"),
        "the notch should have moved the highlight onto `model`"
    );
    assert!(
        !row_text(&after, list_row).contains('❯'),
        "`help` is no longer the highlighted row"
    );
    assert_eq!(app.editor_text(), "/");
    assert_eq!(app.messages().scroll_offset(), 0);
    let covered_after: Vec<String> = ((list_row.saturating_sub(3))..list_row)
        .map(|y| row_text(&after, y))
        .collect();
    assert_eq!(
        covered_before, covered_after,
        "the transcript above the list must not scroll under a claimed notch"
    );
    dump(
        &mut app,
        "after one notch down on the list: `❯ model`, draft `/`, transcript untouched",
    );
}
