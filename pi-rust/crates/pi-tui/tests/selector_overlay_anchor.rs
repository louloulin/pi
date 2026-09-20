//! The selector overlay (`/model`, `/tree`, `/resume`, …) is a **message
//! viewport** overlay.
//!
//! Two properties this suite pins, both found by driving the real binary on a
//! pseudo terminal (LUM-1235):
//!
//! 1. It is anchored to the message view — opening it must not touch a single
//!    row above the transcript, so the 20-row startup header stays readable
//!    while `/model` is up. The pre-fix port anchored the overlay to the
//!    terminal origin (`area.y + 1`) and clipped it against the message
//!    *height*, so the picker was painted over the header while the transcript
//!    underneath was left untouched.
//! 2. It blanks the rows it covers. A picker line is shorter than the
//!    transcript line underneath it, and ratatui only emits the cells a frame
//!    changed, so an unblanked row let the transcript bleed through the modal
//!    (`Pick a modelerrupt`, where `errupt` came from `Esc to interrupt`).

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::selector::{Selector, SelectorItem};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const WIDTH: u16 = 80;
const HEIGHT: u16 = 40;

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
    let config = AppConfig {
        session_id: "selector-anchor".into(),
        // The driver turns the built-in header on; with it on, the picker has
        // to keep its hands off the rows above the transcript.
        startup_header: true,
        ..AppConfig::default()
    };
    App::new(&agent, config)
}

/// Fill the transcript with lines wider than the picker, so any unblanked cell
/// shows up as a `W` on the modal's row.
fn fill_transcript(app: &mut App) {
    for _ in 0..40 {
        app.info("W".repeat(200));
    }
}

fn render(app: &mut App) -> Buffer {
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    buf
}

fn row_text(buf: &Buffer, y: u16) -> String {
    (0..WIDTH)
        .map(|x| buf.cell((x, y)).expect("in bounds").symbol().to_string())
        .collect()
}

/// `(top row, height)` of the message viewport, as recorded by a render.
fn viewport(app: &mut App) -> (u16, u16) {
    let _ = render(app);
    (app.viewport_origin().1, app.viewport().1)
}

fn selector() -> Selector {
    Selector::new(
        "Pick a model",
        vec![
            SelectorItem::new("model:alpha", "Alpha").with_description("alpha-inc"),
            SelectorItem::new("model:beta", "Beta").with_description("beta-inc"),
            SelectorItem::new("model:gamma", "Gamma").with_description("gamma-inc"),
        ],
    )
}

#[test]
fn opening_a_selector_leaves_every_row_above_the_message_view_untouched() {
    let mut app = app();
    fill_transcript(&mut app);
    let (top, _) = viewport(&mut app);
    assert!(
        top > 0 && row_text(&render(&mut app), 0).trim_end() == "pi v0.1.0",
        "the startup header should push the view down, top={top}"
    );
    let before = render(&mut app);

    app.open_selector(selector());
    let after = render(&mut app);

    for y in 0..top {
        assert_eq!(
            row_text(&after, y),
            row_text(&before, y),
            "row {y} is above the message view and must not be painted over"
        );
    }
}

#[test]
fn the_selector_starts_one_row_below_the_message_view_top() {
    let mut app = app();
    fill_transcript(&mut app);
    let (top, _) = viewport(&mut app);

    app.open_selector(selector());
    let after = render(&mut app);

    assert_eq!(row_text(&after, top + 1).trim_end(), "Pick a model");
    let second_item = row_text(&after, top + 4);
    assert!(second_item.contains("Beta"), "{second_item:?}");
}

#[test]
fn selector_rows_are_blanked_before_they_are_painted() {
    let mut app = app();
    fill_transcript(&mut app);
    let (top, _) = viewport(&mut app);

    app.open_selector(selector());
    let after = render(&mut app);

    // Title + separator + three items.
    for offset in 1..=5 {
        let row = row_text(&after, top + offset);
        assert!(
            !row.contains('W'),
            "transcript text bled through the selector at row {}: {row:?}",
            top + offset
        );
    }
}

#[test]
fn the_selector_is_clipped_to_the_message_view() {
    let mut app = app();
    fill_transcript(&mut app);
    let (top, height) = viewport(&mut app);

    // Far more items than the viewport has rows: the overlay must stop at the
    // view's bottom edge instead of running into the prompt and status rows.
    let items: Vec<_> = (0..40)
        .map(|i| SelectorItem::new(format!("model:{i}"), format!("Item {i}")))
        .collect();
    app.open_selector(Selector::new("Pick a model", items).with_max_visible(40));
    let after = render(&mut app);

    let painted = (top..HEIGHT)
        .skip(1)
        .filter(|y| row_text(&after, *y).contains("Item "))
        .count();
    assert!(painted > 0, "the picker should still paint rows");
    assert!(
        painted < height as usize,
        "the picker painted {painted} rows but the view is {height} rows high from row {top}"
    );
}
