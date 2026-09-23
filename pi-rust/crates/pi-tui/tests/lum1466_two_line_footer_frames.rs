//! LUM-1466 frame source — the two-row footer (`pwd (branch) • name` above the
//! stats row), rendered.
//!
//! `cargo test -- --nocapture` prints the cell grid a real
//! [`pi_tui::App::render_to_buffer`] produced and `scripts/frame_to_png.py`
//! paints it (this Windows runner has no PTY — see
//! `docs/LUM1466_TWO_LINE_FOOTER.md` §5).
//!
//! Panels over one session with a small transcript:
//!
//! 1. `100×30` with a cwd + branch + `/name` — upstream's `[pwdLine, statsLine]`;
//! 2. `44×14` with a cwd long enough to be cut — the location row is marked
//!    with `…` rather than silently truncated (LUM-1412's rule);
//! 3. `100×30` with no cwd — the pre-LUM-1466 single stats row, unchanged.
//!
//! These frames prove what was painted, not key timing.
//!
//! `set_keybindings` is not used here: the footer's hint text comes from the
//! same registry the header reads, and this file only asserts footer rows, so
//! the App runs with `pi-tui`'s own `tui.*` table. The file is a separate
//! integration binary because it dumps frames for the screenshot script.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, RenderSnapshot};
use pi_tui::message::MessageItem;
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

/// An App with a two-message transcript and a footer carrying the values a
/// real driver hands it.
fn app(cwd: Option<&str>, branch: Option<&str>) -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1466-footer".into(),
            ..AppConfig::default()
        },
    );
    app.messages_mut()
        .push(MessageItem::user("where does the footer come from?"));
    app.info("Ready.");
    app.set_status_cwd(cwd.map(str::to_string));
    app.set_status_git_branch(branch.map(str::to_string));
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

/// The frame row the composer's draft is drawn on — the *last* `> ` row,
/// since transcript user messages are prefixed with the same marker.
fn composer_row(snapshot: &RenderSnapshot) -> usize {
    snapshot
        .lines
        .iter()
        .rposition(|line| line.starts_with("> "))
        .expect("the composer row is on screen")
}

#[test]
fn a_location_row_is_drawn_above_the_stats_row() {
    let mut app = app(Some("/srv/repo"), Some("main"));
    app.set_session_name(Some("demo".into()));
    let lines = rows(&mut app, 100, 30);

    let location = lines
        .iter()
        .position(|line| line.starts_with("/srv/repo (main) • demo"))
        .expect("the location row is painted");
    let stats = lines
        .iter()
        .position(|line| line.contains("?/8.2k") && line.contains("Faux"))
        .expect("the stats row is painted");
    assert_eq!(stats, location + 1, "stats sit directly below the location");
    // The name moved to the location row, so it is not repeated below it.
    assert!(!lines[stats].contains("demo"), "{:?}", lines[stats]);
    // Both rows are the last chrome rows: nothing but blank rows after them.
    assert_eq!(stats, 29, "the stats row is the last terminal row");
}

#[test]
fn the_transcript_gives_up_exactly_one_row_for_the_location_row() {
    let tall = app(Some("/srv/repo"), Some("main"));
    let tall_snapshot = tall.render_snapshot(80, 24);
    let flat = app(None, None);
    let flat_snapshot = flat.render_snapshot(80, 24);

    assert_eq!(
        composer_row(&flat_snapshot) - composer_row(&tall_snapshot),
        1,
        "the second footer row costs the transcript exactly one row"
    );
    assert_eq!(flat_snapshot.status.cwd, None);
    assert_eq!(tall_snapshot.status.cwd.as_deref(), Some("/srv/repo"));
}

#[test]
fn a_session_without_a_cwd_keeps_the_single_row_footer() {
    let mut app = app(None, None);
    let lines = rows(&mut app, 100, 30);
    // Exactly one row carries the stats; the row above it is transcript.
    let stats_rows = lines.iter().filter(|line| line.contains("?/8.2k")).count();
    assert_eq!(stats_rows, 1, "{lines:#?}");
    assert!(lines[29].contains("?/8.2k"), "{:?}", lines[29]);
    // The composer takes the row the location row would have used.
    assert!(lines[28].starts_with("> "), "{:?}", lines[28]);
}

#[test]
fn a_branch_without_a_cwd_adds_no_row() {
    let mut app = app(None, Some("main"));
    let lines = rows(&mut app, 100, 30);
    assert!(
        !lines.iter().any(|line| line.contains("(main)")),
        "a branch has nothing to hang on without a pwd:\n{lines:#?}"
    );
}

#[test]
fn a_long_location_row_is_marked_when_it_is_cut() {
    let mut app = app(
        Some("/srv/very/deep/inside/a/long/repository"),
        Some("feature/a-long-branch"),
    );
    let lines = rows(&mut app, 44, 14);
    let location = lines
        .iter()
        .position(|line| line.starts_with("/srv/very/deep"))
        .expect("the location row is painted");
    assert!(
        lines[location].ends_with('…'),
        "the cut is marked: {:?}",
        lines[location]
    );
    assert_eq!(
        pi_tui::width::columns(&lines[location]),
        44,
        "the location row is exactly the terminal width"
    );
}

#[test]
fn a_short_terminal_still_paints_the_composer_and_both_footer_rows() {
    let mut app = app(Some("/srv/repo"), Some("main"));
    let lines = rows(&mut app, 80, 8);
    assert!(
        lines.iter().any(|line| line.starts_with("> ")),
        "the composer survives:\n{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("/srv/repo (main)")),
        "the location row survives:\n{lines:#?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("?/8.2k")),
        "the stats row survives:\n{lines:#?}"
    );
}

#[test]
fn frame_dump_two_line_footer() {
    let mut app = app(Some("/srv/repo"), Some("main"));
    app.set_session_name(Some("demo".into()));
    let lines = rows(&mut app, 100, 30);
    assert!(lines
        .iter()
        .any(|l| l.starts_with("/srv/repo (main) • demo")));
    assert!(
        lines.iter().any(|l| l.contains("?/8.2k")),
        "the stats row is painted below it:
{lines:#?}"
    );
    dump(&lines, 100, 30, "LUM-1466 two-row footer (100×30)");
}

#[test]
fn frame_dump_cut_location_row() {
    let mut app = app(
        Some("/srv/very/deep/inside/a/long/repository"),
        Some("feature/a-long-branch"),
    );
    let lines = rows(&mut app, 44, 14);
    assert!(lines.iter().any(|l| l.ends_with('…')));
    assert!(
        lines.iter().any(|l| l.contains("?/8.2k")),
        "the stats row survives the cut:
{lines:#?}"
    );
    dump(&lines, 44, 14, "LUM-1466 cut location row (44×14)");
}

#[test]
fn frame_dump_single_row_footer_without_a_cwd() {
    let mut app = app(None, None);
    let lines = rows(&mut app, 100, 30);
    assert!(lines.iter().any(|l| l.contains("?/8.2k")));
    dump(
        &lines,
        100,
        30,
        "LUM-1466 single-row footer, no cwd (100×30)",
    );
}
