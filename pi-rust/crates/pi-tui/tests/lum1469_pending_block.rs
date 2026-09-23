//! LUM-1469 — the queued-messages block (`Steering:` / `Follow-up:` rows plus
//! the `↳ <chord> to edit all queued messages` hint), rendered.
//!
//! Before this round the App accepted prompts typed while a turn was running
//! (`App::submit` → `MessageView::push_pending`) but painted **nothing** on
//! screen: `git grep -n 'pending()' -- pi-rust/crates/pi-tui/src` matched only
//! the queue bookkeeping, never a paint path. Upstream has painted it since the
//! first interactive mode (`updatePendingMessagesDisplay`,
//! `packages/coding-agent/src/modes/interactive/interactive-mode.ts:4366-4385`),
//! and codex shows the same state in its own preview
//! (`codex-rs/tui/src/bottom_pane/pending_input_preview.rs`).
//!
//! The frames are produced by a real [`pi_tui::App::render_to_buffer`];
//! `cargo test -- --nocapture` prints the cell grid and
//! `scripts/frame_to_png.py` paints it (this Windows runner has no PTY — see
//! `docs/LUM1469_PENDING_QUEUE.md` §5). They show what was painted, not key
//! timing.
//!
//! `set_keybindings` is not touched here: the hint's chord resolution is
//! covered by `lum1469_pending_chord.rs` and the `message.rs` unit tests, so
//! this file runs with the registry every other frame test sees.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, RenderSnapshot};
use pi_tui::message::{MessageItem, PendingMessageKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: Some("Faux".into()),
        context_window: 8192,
        max_output_tokens: 512,
    }
}

/// An App with a transcript long enough to overflow the 20-row viewport the
/// frame tests use, so the block is visibly carved out of the transcript's
/// bottom rather than floating in empty space.
fn app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1469-pending".into(),
            ..AppConfig::default()
        },
    );
    for turn in 0..12 {
        app.messages_mut()
            .push(MessageItem::user(format!("turn {turn}: explain the queue")));
        app.messages_mut().push(MessageItem::assistant(
            "Prompts typed while a turn is running wait in the queue until the turn ends.",
        ));
    }
    app.info("Ready.");
    app
}

/// One buffer row as plain text (wide glyphs collapse to one cell).
///
/// Rows keep the frame's trailing padding — the "the cut is marked at the
/// region edge" assertion needs the real cell grid. Assertions that compare
/// copy compare [`text`].
fn row(buf: &Buffer, y: u16, cols: u16) -> String {
    let mut text = String::new();
    let mut skip = 0usize;
    for x in 0..cols {
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

fn rows(app: &mut App, cols: u16, height: u16) -> Vec<String> {
    let area = Rect::new(0, 0, cols, height);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    (0..height).map(|y| row(&buf, y, cols)).collect()
}

fn dump(lines: &[String], cols: u16, height: u16, caption: &str) {
    println!("PANEL {caption}");
    println!("FRAME DUMP cols={cols} rows={height}");
    for line in lines {
        println!("|{line}|");
    }
    println!("END FRAME DUMP");
}

/// The row the composer's draft is drawn on — the *last* `> ` row, since
/// transcript user messages carry the same marker.
fn composer_row(snapshot: &RenderSnapshot) -> usize {
    snapshot
        .lines
        .iter()
        .rposition(|line| line.starts_with("> "))
        .expect("the composer row is on screen")
}

/// The row count of the pending block on a rendered frame: the spacer + one
/// row per queued prompt + the hint row, exactly what `plan_chrome` reserved.
fn pending_rows(lines: &[String]) -> usize {
    lines
        .iter()
        .filter(|line| {
            let line = text(line);
            line.starts_with("Steering: ") || line.starts_with("Follow-up: ")
        })
        .count()
        + lines
            .iter()
            .filter(|line| text(line).starts_with('↳'))
            .count()
}

/// One frame row without its trailing padding.
fn text(line: &str) -> &str {
    line.trim_end()
}

#[test]
fn a_queued_prompt_is_painted_above_the_composer() {
    let mut app = app();
    app.messages_mut()
        .push_pending(PendingMessageKind::Steer, "second thought");
    let lines = rows(&mut app, 100, 24);

    let steer = lines
        .iter()
        .position(|line| text(line) == "Steering: second thought")
        .unwrap_or_else(|| panic!("the queued prompt is painted:\n{lines:#?}"));
    // The startup header advertises the same chord, so the contextual hint is
    // the row carrying upstream's `↳` lead-in.
    let hint = lines
        .iter()
        .position(|line| text(line).starts_with('↳'))
        .expect("the dequeue hint is painted");
    let composer = lines
        .iter()
        .rposition(|line| line.starts_with("> "))
        .expect("the composer is painted");
    assert_eq!(hint, steer + 1, "hint sits directly below the last prompt");
    assert!(composer > hint, "the block sits above the composer");
    assert!(
        lines[steer - 1].trim().is_empty(),
        "upstream's `Spacer(1)` separates the block from the transcript: {:?}",
        lines[steer - 1]
    );
}

#[test]
fn steering_and_follow_up_rows_render_in_delivery_order() {
    let mut app = app();
    // Insertion order is deliberately the reverse of delivery order: steering
    // is delivered first (`MessageView::pending`).
    app.messages_mut()
        .push_pending(PendingMessageKind::FollowUp, "after the turn");
    app.messages_mut()
        .push_pending(PendingMessageKind::Steer, "join this turn");
    let lines = rows(&mut app, 100, 24);
    let steer = lines
        .iter()
        .position(|line| text(line) == "Steering: join this turn")
        .expect("steering row");
    let follow = lines
        .iter()
        .position(|line| text(line) == "Follow-up: after the turn")
        .expect("follow-up row");
    assert_eq!(follow, steer + 1);
}

/// The block costs the transcript exactly the rows it occupies — the same
/// "the transcript gives up exactly one row" contract the footer row has
/// (LUM-1466) — and never moves the composer.
#[test]
fn the_block_costs_the_transcript_exactly_its_rows() {
    let flat = app();
    let flat_snapshot = flat.render_snapshot(80, 24);
    assert_eq!(flat.messages().pending_block_rows(), 0);
    let flat_viewport = flat.viewport();
    let flat_origin = flat.viewport_origin();

    let mut queued = app();
    queued
        .messages_mut()
        .push_pending(PendingMessageKind::Steer, "typed while running");
    let queued_snapshot = queued.render_snapshot(80, 24);

    assert_eq!(queued.messages().pending_len(), 1);
    // Spacer + one prompt + hint = 3 rows.
    assert_eq!(queued.messages().pending_block_rows(), 3);
    // The composer keeps its row: the block is carved out of the transcript,
    // not inserted under the composer.
    assert_eq!(
        composer_row(&flat_snapshot),
        composer_row(&queued_snapshot),
        "the composer does not move"
    );
    assert_eq!(
        queued.viewport().1,
        flat_viewport.1 - 3,
        "the transcript gives up exactly the block's rows"
    );
    assert_eq!(queued.viewport().0, flat_viewport.0, "width is untouched");
    assert_eq!(
        queued.viewport_origin(),
        flat_origin,
        "the transcript's top row does not move"
    );
}

/// With nothing queued the block is not planned at all: the frame is the
/// pre-LUM-1469 frame, with the composer on the row it always had.
#[test]
fn an_empty_queue_adds_no_row() {
    let mut app = app();
    let lines = rows(&mut app, 100, 24);
    assert_eq!(pending_rows(&lines), 0, "{lines:#?}");
    assert!(
        !lines.iter().any(|line| line.contains('↳')),
        "no hint row without a queue:\n{lines:#?}"
    );
    // The composer is where the two-row-footer geometry puts it: one row above
    // the single status row.
    assert!(
        lines[22].starts_with("> "),
        "composer row unchanged: {:?}",
        lines[22]
    );
}

/// A queued draft is one row and a cut is marked with `…` rather than
/// silently clipped (LUM-1412's rule for every region).
#[test]
fn a_long_queued_draft_is_one_marked_row() {
    let mut narrow = app();
    narrow.messages_mut().push_pending(
        PendingMessageKind::Steer,
        "a queued draft long enough that it cannot possibly fit on one narrow row",
    );
    let lines = rows(&mut narrow, 44, 20);
    let row = lines
        .iter()
        .position(|line| line.starts_with("Steering: a queued draft"))
        .expect("the queued row is painted");
    assert!(
        text(&lines[row]).ends_with('…'),
        "the cut is marked: {:?}",
        lines[row]
    );
    assert!(
        pi_tui::width::columns(&lines[row]) <= 44,
        "the row never exceeds the terminal width"
    );
    // Only the first line of a multi-line draft; `first\nsecond` below would
    // otherwise paint a second row.
    let mut multi = app();
    multi
        .messages_mut()
        .push_pending(PendingMessageKind::Steer, "first\nsecond");
    let lines = rows(&mut multi, 44, 20);
    assert_eq!(pending_rows(&lines), 2, "one prompt row + one hint row");
}

/// Even on a terminal too short for everything, the composer and the queued
/// block survive — the foldable header is what gives way (the rule
/// `plan_chrome` sets, and the reason the block is budgeted before the
/// header).
#[test]
fn a_short_terminal_keeps_the_composer_and_the_block() {
    let mut app = app();
    app.messages_mut()
        .push_pending(PendingMessageKind::Steer, "queued");
    let lines = rows(&mut app, 80, 8);
    assert!(
        lines.iter().any(|line| text(line) == "Steering: queued"),
        "the queued row survives:\n{lines:#?}"
    );
    assert!(
        lines.iter().any(|line| text(line).starts_with('↳')),
        "the hint row survives:\n{lines:#?}"
    );
    assert!(
        lines.iter().any(|line| line.starts_with("> ")),
        "the composer survives:\n{lines:#?}"
    );
}

#[test]
fn frame_dump_queued_prompts() {
    let mut app = app();
    app.messages_mut()
        .push_pending(PendingMessageKind::Steer, "also check the retry path");
    app.messages_mut()
        .push_pending(PendingMessageKind::FollowUp, "then summarise");
    let lines = rows(&mut app, 100, 24);
    // Two prompt rows + the hint row.
    assert_eq!(pending_rows(&lines), 3, "{lines:#?}");
    assert!(lines
        .iter()
        .any(|l| text(l) == "Steering: also check the retry path"));
    assert!(lines.iter().any(|l| text(l) == "Follow-up: then summarise"));
    dump(&lines, 100, 24, "LUM-1469 queued prompts (100×24)");
}

#[test]
fn frame_dump_cut_queued_draft() {
    let mut app = app();
    app.messages_mut().push_pending(
        PendingMessageKind::Steer,
        "a queued draft long enough that it cannot possibly fit on one narrow row",
    );
    let lines = rows(&mut app, 44, 16);
    let row = lines
        .iter()
        .position(|line| line.starts_with("Steering: a queued draft"))
        .expect("the queued row is painted");
    assert!(text(&lines[row]).ends_with('…'), "{:?}", lines[row]);
    dump(&lines, 44, 16, "LUM-1469 cut queued draft (44×16)");
}

#[test]
fn frame_dump_no_queue_baseline() {
    let mut app = app();
    let lines = rows(&mut app, 100, 24);
    assert_eq!(pending_rows(&lines), 0);
    dump(
        &lines,
        100,
        24,
        "LUM-1469 no queue — the pre-change frame (100×24)",
    );
}
