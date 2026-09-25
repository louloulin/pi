//! Short viewports: the composer keeps its row, and the scrollbar keeps its
//! column instead of eating the transcript's.
//!
//! Two defects LUM-1262 recorded and LUM-1266 fixed here:
//!
//! 1. On a ≤23-row terminal the startup header was budgeted before the
//!    editor region, so the prompt row was clipped off the bottom of the
//!    screen and the user typed blind (`plan_chrome`).
//! 2. The chat-log scrollbar was painted over the transcript's last column,
//!    which is exactly where an exactly-full-width row — every `/help` row —
//!    puts a character (`App::viewport_for_render`).
//!
//! No test here installs a keybinding table: `pi-tui`'s own registry has no
//! `app.*` ids, so the built-in header is short and these assertions do not
//! depend on the driver's table (`startup_header.rs` covers the tall one).
//!
//! The message viewport is [`HEIGHT`] minus the status bar, the prompt row,
//! and the editor border row (Phase 2 / G3), i.e. `HEIGHT - 3`.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const WIDTH: u16 = 40;
/// Message viewport is this minus the status bar and the prompt row, i.e.
/// 8 rows — the canonical size the other App tests use.
const HEIGHT: u16 = 10;
/// Rightmost column of the viewport = the scrollbar's column.
const BAR: u16 = WIDTH - 1;
/// Characters a fully-filled row holds at [`BAR`] columns: the two-character
/// role prefix plus `text_width_for(width) = width - 2`.
const FULL_ROW_BODY: usize = BAR as usize - 2;

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
    App::new(
        &agent,
        AppConfig {
            session_id: "short-viewport".into(),
            ..AppConfig::default()
        },
    )
}

/// The interactive render path (scrollbar overlay on).
fn render(app: &mut App) -> Buffer {
    let area = Rect {
        x: 0,
        y: 0,
        width: WIDTH,
        height: HEIGHT,
    };
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    buf
}

fn symbol_at(buf: &Buffer, x: u16, y: u16) -> String {
    buf.cell((x, y))
        .expect("cell in bounds")
        .symbol()
        .to_string()
}

fn row_text(buf: &Buffer, y: u16) -> String {
    (0..WIDTH)
        .map(|x| symbol_at(buf, x, y))
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// The row carrying an info block, plus that row's index.
fn find_row(buf: &Buffer, needle: char) -> (u16, String) {
    for y in 0..HEIGHT {
        let text = row_text(buf, y);
        if text.contains(needle) {
            return (y, text);
        }
    }
    panic!("no row contains {needle:?}");
}

// ---------------------------------------------------------------------------
// The scrollbar keeps its own column
// ---------------------------------------------------------------------------

#[test]
fn a_full_width_row_keeps_its_last_column_next_to_the_bar() {
    // 20 rows so the transcript overflows and the bar is drawn, then the row
    // under test last so it is inside the pinned-to-tail viewport. `Z` is not
    // in the status bar's copy, so the row search below is unambiguous.
    let mut app = app();
    for i in 0..20 {
        app.info(format!("filler {i}"));
    }
    app.info_block("Z".repeat(FULL_ROW_BODY));
    let buf = render(&mut app);

    let (y, text) = find_row(&buf, 'Z');
    assert_eq!(
        text.chars().filter(|c| *c == 'Z').count(),
        FULL_ROW_BODY,
        "every character of a full-width row must be on screen: {text:?}"
    );
    assert_eq!(
        symbol_at(&buf, BAR - 1, y),
        "Z",
        "the text owns the last column inside the viewport"
    );
    let bar = symbol_at(&buf, BAR, y);
    assert!(
        matches!(bar.as_str(), "┃" | "│" | "█"),
        "the scrollbar owns the frame's last column, found {bar:?} in {text:?}"
    );
}

#[test]
fn the_viewport_gives_up_one_column_only_while_the_bar_is_drawn() {
    // One short line fits: no bar, no reserved column.
    let mut fitting = app();
    fitting.info("short");
    let _ = render(&mut fitting);
    assert_eq!(fitting.viewport(), (WIDTH, HEIGHT - 3));
    assert_eq!(fitting.scrollbar_geometry(), None);

    // Twenty lines overflow: the bar takes a column out of the text.
    let mut overflowing = app();
    for i in 0..20 {
        overflowing.info(format!("line {i}"));
    }
    let _ = render(&mut overflowing);
    assert_eq!(overflowing.viewport(), (WIDTH - 1, HEIGHT - 3));
    let geometry = overflowing
        .scrollbar_geometry()
        .expect("content overflows the viewport");
    assert_eq!(
        geometry.column, BAR,
        "the bar stays on the frame's right edge"
    );
}

#[test]
fn the_jump_to_latest_pill_stays_left_of_the_bar() {
    let mut app = app();
    for i in 0..40 {
        app.info(format!("line {i}"));
    }
    let _ = render(&mut app);
    // Scroll away from the tail so the pill is painted.
    app.scroll_viewport_up(5);
    let buf = render(&mut app);
    let pill = app
        .scroll_to_end_rect()
        .expect("detached view paints a pill");
    assert!(
        pill.x + pill.width <= BAR,
        "the pill must not reach the bar's column: {pill:?}"
    );
    // And it really is on screen, not just recorded.
    let label: String = (pill.x..pill.x + pill.width)
        .map(|x| symbol_at(&buf, x, pill.y))
        .collect();
    assert!(label.contains("Jump to latest"), "found {label:?}");
}

// ---------------------------------------------------------------------------
// The composer survives a short terminal
// ---------------------------------------------------------------------------

#[test]
fn a_short_terminal_folds_the_header_and_still_paints_the_composer() {
    for height in [10u16, 12, 23] {
        let agent = Agent::new(AgentOptions::new(
            faux_model(),
            Arc::new(FauxProvider::default()),
            "you are pi",
        ));
        let app = App::new(
            &agent,
            AppConfig {
                session_id: "short-viewport".into(),
                startup_header: true,
                ..AppConfig::default()
            },
        );
        let snapshot = app.render_snapshot(WIDTH, height);
        let text = snapshot.lines.join("\n");
        // 10 rows cannot hold the built-in hint list (7 rows here) plus the
        // composer and the status bar, so the header folds itself for that
        // frame and nothing is dropped from the bottom of the screen.
        if height == 10 {
            assert!(!text.contains("to delete to end"), "folded:\n{text}");
        }
        let prompt_row = snapshot
            .lines
            .iter()
            .position(|line| line.contains("type a prompt"))
            .unwrap_or_else(|| {
                panic!(
                    "no composer on a {WIDTH}x{height} terminal:\n{}",
                    snapshot.lines.join("\n")
                )
            });
        assert_eq!(
            prompt_row,
            height as usize - 2,
            "the composer sits directly above the status row at {WIDTH}x{height}"
        );
        let status = &snapshot.lines[height as usize - 1];
        assert!(
            !status.trim().is_empty(),
            "the status bar is painted below the composer at {WIDTH}x{height}"
        );
    }
}

#[test]
fn a_tall_terminal_keeps_the_expanded_header_and_the_composer() {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let app = App::new(
        &agent,
        AppConfig {
            session_id: "short-viewport".into(),
            startup_header: true,
            ..AppConfig::default()
        },
    );
    let snapshot = app.render_snapshot(WIDTH, 24);
    let text = snapshot.lines.join("\n");
    assert!(text.contains("pi v"), "{text}");
    assert!(
        text.contains("to delete to end"),
        "a terminal that fits the hint list keeps it:\n{text}"
    );
    let prompt_row = snapshot
        .lines
        .iter()
        .position(|line| line.contains("type a prompt"))
        .expect("composer painted");
    assert_eq!(prompt_row, 22);
}
