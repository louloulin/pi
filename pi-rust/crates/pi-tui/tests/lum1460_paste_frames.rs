//! LUM-1460 frame source — the composer's paste channel.
//!
//! `cargo test -- --nocapture` prints the cell grids a real
//! [`pi_tui::App::render_to_buffer`] produced and `scripts/frame_to_png.py`
//! paints them (this Windows runner has no PTY — see
//! `docs/LUM1460_PASTE_RESCUE.md` §6).
//!
//! Three 100×24 panels:
//!
//! 1. a 12-line paste folded into `[paste #1 +12 lines]` — the composer stays
//!    three rows tall instead of fifteen;
//! 2. two pastes around typed text, `[paste #1 …] and [paste #2 …]`;
//! 3. the same draft submitted: the transcript's user message carries the 12
//!    lines, proving the marker is expanded on the way to the agent.
//!
//! The frames prove what was painted, not the key timing; the behaviour is
//! pinned by `tests/composer_paste.rs` (27 cases).

use std::sync::Arc;
use std::time::Duration;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::input::{InputEvent, Key, KeyCode, KeyModifiers};
use pi_tui::keybindings::reset_keybindings;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tokio::sync::Mutex as AsyncMutex;

const COLS: u16 = 100;
const ROWS: u16 = 24;

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

fn model() -> Agent {
    Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ))
}

fn app() -> App {
    let agent = model();
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1460-paste-frames".into(),
            ..AppConfig::default()
        },
    );
    app.info("Ready — paste a log with Ctrl+V and watch it collapse.");
    app
}

/// Twelve lines, so the paste folds into `[paste #1 +12 lines]`.
fn block(lines: usize) -> String {
    (1..=lines)
        .map(|n| format!("log line {n}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn enter() -> Key {
    Key::new(KeyCode::Enter, KeyModifiers::NONE)
}

/// One buffer row as plain text (wide glyphs collapse to one cell).
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

fn rows(app: &mut App) -> Vec<String> {
    let area = Rect::new(0, 0, COLS, ROWS);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    (0..ROWS).map(|y| row(&buf, y)).collect()
}

fn dump(lines: &[String], caption: &str) {
    println!("PANEL {caption}");
    println!("FRAME DUMP cols={COLS} rows={ROWS}");
    for line in lines {
        println!("|{}|", line);
    }
    println!("END FRAME DUMP");
}

#[test]
fn frame_dump_paste_folds_to_a_marker() {
    reset_keybindings();
    let mut app = app();
    app.step_paste(&block(12));
    let lines = rows(&mut app);
    assert!(
        lines.iter().any(|l| l.contains("[paste #1 +12 lines]")),
        "the composer draws the marker:\n{lines:#?}"
    );
    // The composer stayed compact: the draft is one row, not twelve.
    assert!(
        !lines.iter().any(|l| l.contains("log line 1")),
        "no pasted line is painted into the composer:\n{lines:#?}"
    );
    dump(
        &lines,
        "LUM-1460 a 12-line paste folds into `[paste #1 +12 lines]`",
    );
}

#[test]
fn frame_dump_two_markers_with_typed_text() {
    reset_keybindings();
    let mut app = app();
    app.step_paste(&block(11));
    for c in " and ".chars() {
        app.step(InputEvent::Key(Key::new(
            KeyCode::Char(c),
            KeyModifiers::NONE,
        )));
    }
    app.step_paste(&"x".repeat(1234));
    let lines = rows(&mut app);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("[paste #1 +11 lines] and [paste #2 1234 chars]")),
        "both markers keep their own summary:\n{lines:#?}"
    );
    dump(
        &lines,
        "LUM-1460 two pastes: `[paste #1 +11 lines] and [paste #2 1234 chars]`",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frame_dump_submitted_paste_reaches_the_transcript() {
    reset_keybindings();
    let agent = Arc::new(AsyncMutex::new(model()));
    let mut app = App::new(
        &*agent.lock().await,
        AppConfig {
            session_id: "lum1460-paste-frames".into(),
            ..AppConfig::default()
        },
    );
    app.info("Ready — paste a log with Ctrl+V and watch it collapse.");
    let paste = block(12);
    app.step_paste(&paste);
    let StepOutcome::Submitted(submission) = app.step(InputEvent::Key(enter())) else {
        panic!("expected a submission");
    };
    assert_eq!(submission.text, paste, "the model gets the 12 lines");
    app.submit(agent.clone(), submission);

    for _ in 0..400 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        app.drain_agent_events();
        if !app.is_busy() {
            break;
        }
    }
    let lines = rows(&mut app);
    assert!(
        lines.iter().any(|l| l.contains("log line 1")),
        "the transcript shows the expanded paste:\n{lines:#?}"
    );
    dump(
        &lines,
        "LUM-1460 the submitted marker reached the transcript expanded (12 lines)",
    );
}
