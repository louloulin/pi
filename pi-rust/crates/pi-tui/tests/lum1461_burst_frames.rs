//! LUM-1461 frame source — the paste-burst fallback and the stale-marker hint.
//!
//! `cargo test -- --nocapture` prints the cell grids a real
//! [`pi_tui::App::render_to_buffer`] produced and `scripts/frame_to_png.py`
//! paints them (this Windows runner has no PTY — see
//! `docs/LUM1461_PASTE_BURST.md` §6).
//!
//! Three panels, each in its own test so its dump can be captured on its own:
//!
//! 1. a 12-line paste arriving as *key events* (no bracketed paste) folds into
//!    `[paste #1 +12 lines]` once the burst goes quiet;
//! 2. two such bursts around typed text keep their own summaries;
//! 3. a marker recalled from the cross-session history file — no registry
//!    behind it — stays literal and the one-shot hint is on screen.

use std::sync::Arc;
use std::time::{Duration, Instant};

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::input::{Key, KeyCode, KeyModifiers, PASTE_BURST_ACTIVE_IDLE_TIMEOUT};
use pi_tui::keybindings::reset_keybindings;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const COLS: u16 = 120;
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

fn agent() -> Agent {
    Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ))
}

fn app() -> App {
    reset_keybindings();
    let mut app = App::new(
        &agent(),
        AppConfig {
            session_id: "burst".into(),
            paste_burst: true,
            ..AppConfig::default()
        },
    );
    app.info("Ready — paste a log into a terminal that sends no paste event.");
    app
}

fn block(lines: usize) -> String {
    (1..=lines)
        .map(|n| format!("log line {n}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Fifteen hundred characters, so the second burst folds by character count.
fn long_line(chars: usize) -> String {
    "x".repeat(chars)
}

fn key_for(c: char) -> Key {
    if c == '\n' {
        Key::new(KeyCode::Enter, KeyModifiers::NONE)
    } else {
        Key::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
}

/// Deliver `text` the way a terminal without bracketed paste does: one key
/// event per character, 1 ms apart. Returns the instant of the last one.
fn feed_burst(app: &mut App, text: &str, start: Instant) -> Instant {
    let mut now = start;
    for c in text.chars() {
        app.step_key_at(key_for(c), now);
        now += Duration::from_millis(1);
    }
    now
}

/// Deliver `text` as human typing: 120 ms between characters, far outside the
/// burst window.
fn type_slowly(app: &mut App, text: &str, start: Instant) -> Instant {
    let mut now = start;
    for c in text.chars() {
        app.step_key_at(key_for(c), now);
        now += Duration::from_millis(120);
    }
    now
}

fn due(after: Instant) -> Instant {
    after + PASTE_BURST_ACTIVE_IDLE_TIMEOUT + Duration::from_millis(1)
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
fn frame_dump_a_key_burst_folds_to_a_marker() {
    let mut app = app();
    let last = feed_burst(&mut app, &block(12), Instant::now());
    app.tick_paste_burst(due(last));
    let lines = rows(&mut app);
    assert!(
        lines.iter().any(|l| l.contains("[paste #1 +12 lines]")),
        "the composer draws the marker the burst folded into:\n{lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("log line 1")),
        "no pasted line is painted into the composer:\n{lines:#?}"
    );
    dump(
        &lines,
        "LUM-1461 a 12-line paste sent as key events folds into `[paste #1 +12 lines]`",
    );
}

#[test]
fn frame_dump_two_bursts_around_typed_text() {
    let mut app = app();
    let last = feed_burst(&mut app, &block(11), Instant::now());
    assert!(app.tick_paste_burst(due(last)));
    // Ordinary typing between the two bursts must not join either of them.
    let typed_end = type_slowly(&mut app, " and ", due(last) + Duration::from_millis(200));
    let last = feed_burst(&mut app, &long_line(1500), typed_end);
    assert!(app.tick_paste_burst(due(last)));
    let lines = rows(&mut app);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("[paste #1 +11 lines] and [paste #2 1500 chars]")),
        "both bursts keep their own summary:\n{lines:#?}"
    );
    dump(
        &lines,
        "LUM-1461 two bursts: `[paste #1 +11 lines] and [paste #2 1500 chars]`",
    );
}

#[test]
fn frame_dump_a_stale_recalled_marker_stays_literal() {
    let dir = std::env::temp_dir().join(format!(
        "pi-tui-lum1461-frames-{}-{}",
        std::process::id(),
        line!()
    ));
    let path = dir.join("history.jsonl");
    let _ = std::fs::remove_dir_all(&dir);
    pi_tui::history_store::append(&path, "[paste #1 +12 lines]").unwrap();

    reset_keybindings();
    let mut app = App::new(
        &agent(),
        AppConfig {
            session_id: "burst".into(),
            paste_burst: true,
            history_path: Some(path.clone()),
            ..AppConfig::default()
        },
    );
    app.info("Ready — recall a prompt from the previous session.");
    app.step_key(Key::new(KeyCode::Up, KeyModifiers::NONE));
    let lines = rows(&mut app);
    assert!(
        lines.iter().any(|l| l.contains("[paste #1 +12 lines]")),
        "the recalled marker is ordinary draft text:\n{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("no longer available")),
        "the one-shot hint is on screen:\n{lines:#?}"
    );
    dump(
        &lines,
        "LUM-1461 a marker recalled from history has no content — literal text plus one hint",
    );
    let _ = std::fs::remove_dir_all(&dir);
}
