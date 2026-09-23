//! LUM-1481 frame source — the footer's **third row**, the one upstream
//! reserves for `ctx.ui.setStatus(key, text)` statuses
//! (`packages/coding-agent/src/modes/interactive/components/footer.ts:243-251`).
//!
//! `cargo test -- --nocapture` prints the cell grid a real
//! [`pi_tui::App::render_to_buffer`] produced and `scripts/frame_to_png.py`
//! paints it (this Windows runner has no PTY — see
//! `docs/LUM1481_EXTENSION_STATUS.md` §5).
//!
//! Panels over one session:
//!
//! 1. `100×30` with two extension statuses installed — `location` row, stats
//!    row, status row, in that order, sorted by key;
//! 2. the same App with the statuses cleared — the row is **gone**, not blank,
//!    and the footer is two rows again (the regression frame);
//! 3. `44×16` with one status too wide for the terminal — the row is cut and
//!    marked, never wrapped into a second row.
//!
//! These frames prove what was painted, not key timing.

use std::sync::Arc;

use pi_agent_core::Agent;
use pi_agent_core::AgentOptions;
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::message::MessageItem;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("anthropic"),
        id: "claude-sonnet-4".into(),
        api: Api::Faux,
        label: Some("claude-sonnet-4".into()),
        context_window: 200_000,
        max_output_tokens: 8_192,
    }
}

/// An App with a working directory (so the footer already has a location row)
/// and, when `statuses` is true, two extension statuses installed out of order.
fn new_app(statuses: bool) -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1481-status".into(),
            ..AppConfig::default()
        },
    );
    app.messages_mut()
        .push(MessageItem::user("what is the extension status row?"));
    app.info("Ready.");
    app.set_status_cwd(Some("/srv/repo".into()));
    app.set_status_git_branch(Some("main".into()));
    if statuses {
        // Installed out of key order on purpose: the row is sorted by key.
        app.set_extension_status("zz-build", Some("compiling"));
        app.set_extension_status("aa-agent", Some("thinking"));
    }
    app
}

/// One buffer row as plain text (wide glyphs collapse to one cell).
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

#[test]
fn an_extension_status_is_the_footers_third_row() {
    let mut app = new_app(true);
    let lines = rows(&mut app, 100, 30);
    // Upstream pushes the status row *after* `[pwdLine, statsLine]`, so it is
    // the bottom row of the frame (`footer.ts:236-251`).
    assert!(lines[27].starts_with("/srv/repo (main)"), "{:?}", lines[27]);
    assert!(lines[28].ends_with("claude-sonnet-4"), "{:?}", lines[28]);
    assert_eq!(lines[29].trim_end(), "thinking compiling");
    // The status row is padded to the full width like every other footer row.
    assert_eq!(pi_tui::width::columns(&lines[29]), 100, "{:?}", lines[29]);
}

#[test]
fn the_status_row_costs_the_transcript_exactly_one_row() {
    let mut with_statuses = new_app(true);
    let mut without = new_app(false);
    let with_lines = rows(&mut with_statuses, 100, 30);
    let without_lines = rows(&mut without, 100, 30);
    // The frame height never changes — only the footer/transcript split does.
    assert_eq!(with_lines.len(), without_lines.len());
    let composer_with = with_lines
        .iter()
        .position(|line| line.contains("type a prompt"))
        .expect("the composer");
    let composer_without = without_lines
        .iter()
        .position(|line| line.contains("type a prompt"))
        .expect("the composer");
    assert_eq!(composer_without, 27);
    assert_eq!(composer_with, 26, "the composer moved up one row");
    // Everything above the composer (header + transcript) is byte-identical:
    // the extra status row came off the transcript viewport, not off what the
    // transcript shows at these rows.
    for row in 0..composer_with {
        assert_eq!(
            with_lines[row], without_lines[row],
            "row {row} above the composer changed"
        );
    }
    // The composer and the two existing footer rows moved up by exactly one,
    // and the new row is the last row of the frame.
    assert_eq!(with_lines[composer_with], without_lines[composer_without]);
    assert_eq!(
        with_lines[composer_with + 1],
        without_lines[composer_without + 1]
    );
    assert_eq!(
        with_lines[composer_with + 2],
        without_lines[composer_without + 2]
    );
    assert_eq!(with_lines[29].trim_end(), "thinking compiling");
}

#[test]
fn clearing_the_last_status_gives_the_row_back() {
    let mut app = new_app(true);
    app.clear_extension_statuses();
    let snapshot = app.render_snapshot(100, 30);
    assert!(snapshot.status.extension_statuses.is_empty());
    // Two footer rows again (location + stats) and the same frame height.
    assert_eq!(snapshot.lines.len(), 30);
    assert!(snapshot.lines[28].starts_with("/srv/repo (main)"));
    assert!(snapshot.lines[29].ends_with("claude-sonnet-4"));
    assert!(
        !snapshot.lines.iter().any(|line| line.contains("thinking")),
        "{:?}",
        snapshot.lines
    );
    // Per-key clearing is upstream's contract: the other key survives.
    let mut second = new_app(true);
    second.set_extension_status("aa-agent", None);
    let snapshot = second.render_snapshot(100, 30);
    assert_eq!(snapshot.status.extension_statuses.len(), 1);
    assert!(snapshot.lines[27].starts_with("/srv/repo (main)"));
    assert_eq!(snapshot.lines[29].trim_end(), "compiling");
}

#[test]
fn a_status_too_wide_for_the_terminal_is_marked_not_wrapped() {
    let mut app = new_app(false);
    app.set_extension_status(
        "aa-agent",
        Some("a status that is far too wide for a forty-four column terminal"),
    );
    let lines = rows(&mut app, 44, 16);
    let status_row = lines[15].clone();
    assert_eq!(pi_tui::width::columns(&status_row), 44, "{status_row:?}");
    assert!(status_row.trim_end().ends_with('…'), "{status_row:?}");
    // The row above it is the stats row, not more status text: the status row
    // is exactly one row no matter how long the text is.
    assert!(lines[14].contains("?/200k"), "{:?}", lines[14]);
    assert!(!lines[14].contains("far too wide"), "{:?}", lines[14]);
}

#[test]
fn frame_dump_extension_status_100x30() {
    let mut app = new_app(true);
    let lines = rows(&mut app, 100, 30);
    assert_eq!(lines[29].trim_end(), "thinking compiling");
    dump(&lines, 100, 30, "LUM-1481 extension status row (100x30)");
}

#[test]
fn frame_dump_extension_status_cleared_100x30() {
    let mut app = new_app(false);
    let lines = rows(&mut app, 100, 30);
    assert!(lines[28].starts_with("/srv/repo (main)"));
    dump(
        &lines,
        100,
        30,
        "LUM-1481 no extension statuses (regression, 100x30)",
    );
}

#[test]
fn frame_dump_extension_status_cut_44x16() {
    let mut app = new_app(false);
    app.set_extension_status(
        "aa-agent",
        Some("a status that is far too wide for a forty-four column terminal"),
    );
    let lines = rows(&mut app, 44, 16);
    assert!(lines[15].trim_end().ends_with('…'));
    dump(&lines, 44, 16, "LUM-1481 extension status cut (44x16)");
}
