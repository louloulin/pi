//! LUM-1467 frame source — the footer's stats row with every upstream field
//! (`↑/↓`, `R/W`, `CH%`, `$cost`, `(auto)`, `(provider) model • thinking`),
//! rendered.
//!
//! `cargo test -- --nocapture` prints the cell grid a real
//! [`pi_tui::App::render_to_buffer`] produced and `scripts/frame_to_png.py`
//! paints it (this Windows runner has no PTY — see
//! `docs/LUM1467_FOOTER_STATS_FIELDS.md` §5).
//!
//! Panels over one session with a small transcript:
//!
//! 1. `100×30` with a cwd + branch + every stat field — the row upstream's
//!    `footer.ts:130-200` builds, with the model as the accent right side;
//! 2. `44×14` with the same fields — the LUM-1367 whole-part drop order
//!    (`docs/LUM1367_FOOTER_BUDGET.md`) applied to the new parts;
//! 3. `100×30` with the new fields at their defaults — no `CH`, no `$`, no
//!    `(auto)`, no `(provider)`, and the same frame height as panel 1.
//!
//! These frames prove what was painted, not key timing.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions, ThinkingLevel};
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

/// An App whose footer carries every LUM-1467 field, so the stats row is the
/// one the task is about.
fn app(all_fields: bool) -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1467-stats".into(),
            ..AppConfig::default()
        },
    );
    app.messages_mut()
        .push(MessageItem::user("which stats does the footer show?"));
    app.info("Ready.");
    app.set_status_cwd(Some("/srv/repo".into()));
    app.set_status_git_branch(Some("main".into()));
    app.set_session_name(Some("demo".into()));
    // Upstream's right side is `(provider) model • thinking` for a reasoning
    // model (`footer.ts:182-197`).
    app.set_thinking_supported(true);
    app.set_thinking_level(ThinkingLevel::High);
    if all_fields {
        let status = app.status_data_mut();
        status.input_tokens = 12_000;
        status.output_tokens = 3_000;
        status.cache_read = 12_000;
        status.cache_write = 300;
        status.context_used = 64_000;
        status.latest_cache_hit_rate = Some(65.0);
        status.cost_micros = Some(123_000);
        status.auto_compact = true;
        status.provider_count = 2;
        status.provider_label = Some("anthropic".into());
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
fn the_stats_row_carries_every_field_in_upstream_order() {
    let mut app = app(true);
    let lines = rows(&mut app, 100, 30);
    let stats = lines
        .iter()
        .position(|line| line.contains("CH65.0%"))
        .expect("the stats row is painted");
    // Upstream's `statsParts.join(" ")` order, then the accent right side.
    assert!(
        lines[stats].starts_with("↑12k ↓3.0k R12k W300 CH65.0% $0.123 32.0%/200k (auto)"),
        "{:?}",
        lines[stats]
    );
    assert!(
        lines[stats].ends_with("(anthropic) claude-sonnet-4 • high"),
        "{:?}",
        lines[stats]
    );
    // The location row carries the name, so the stats row does not repeat it
    // (LUM-1466's `session_segment` rule).
    assert!(!lines[stats].contains("demo"), "{:?}", lines[stats]);
    assert_eq!(stats, 29, "the stats row is the last terminal row");
}

#[test]
fn the_new_fields_do_not_add_a_row() {
    // The hard gate: the same App configuration renders the same number of
    // rows with the LUM-1467 fields set and at their defaults.
    let populated = app(true);
    let defaulted = app(false);
    let with_fields = populated.render_snapshot(100, 30);
    let without_fields = defaulted.render_snapshot(100, 30);
    assert_eq!(
        with_fields.lines.len(),
        without_fields.lines.len(),
        "the stats fields must not change the frame height"
    );
    assert_eq!(with_fields.lines.len(), 30);
    // One stats row plus the location row — unchanged from LUM-1466.
    assert_eq!(with_fields.status.cwd.as_deref(), Some("/srv/repo"));
    // The defaults keep the new parts hidden.
    let row = without_fields.lines.last().expect("a stats row is painted");
    assert!(!row.contains("CH"), "{row:?}");
    assert!(!row.contains('$'), "{row:?}");
    assert!(!row.contains("(auto)"), "{row:?}");
    assert!(!row.contains("(anthropic)"), "{row:?}");
    // And the stats row still sits directly under the location row.
    assert!(
        without_fields.lines[29].contains("?/200k"),
        "{:?}",
        without_fields.lines[29]
    );
    assert!(without_fields.lines[29].ends_with("claude-sonnet-4 • high"));
    assert!(without_fields.lines[28].starts_with("/srv/repo (main) • demo"));
}

#[test]
fn a_44_column_frame_sheds_the_new_parts_in_the_lum1367_order() {
    let mut app = app(true);
    let lines = rows(&mut app, 44, 14);
    let stats = lines[13].clone();
    // The hint and the derived cache-hit rate are the first things to go; the
    // model keeps the right edge and the marker says a subset is on screen.
    assert!(!stats.contains("? for help"), "{stats:?}");
    assert!(!stats.contains("CH65.0%"), "{stats:?}");
    assert!(stats.trim_end().ends_with('…'), "{stats:?}");
    assert_eq!(pi_tui::width::columns(&stats), 44, "{stats:?}");
}

#[test]
fn frame_dump_all_stats_fields_100x30() {
    let mut app = app(true);
    let lines = rows(&mut app, 100, 30);
    assert!(lines[29].contains("CH65.0% $0.123"));
    dump(&lines, 100, 30, "LUM-1467 stats fields (100×30)");
}

#[test]
fn frame_dump_all_stats_fields_narrow_44x14() {
    let mut app = app(true);
    let lines = rows(&mut app, 44, 14);
    assert!(lines[13].trim_end().ends_with('…'));
    dump(&lines, 44, 14, "LUM-1467 stats fields, narrow (44×14)");
}

#[test]
fn frame_dump_default_fields_100x30() {
    let mut app = app(false);
    let lines = rows(&mut app, 100, 30);
    assert!(!lines[29].contains("CH"), "{:?}", lines[29]);
    dump(
        &lines,
        100,
        30,
        "LUM-1467 stats fields at their defaults (100×30)",
    );
}
