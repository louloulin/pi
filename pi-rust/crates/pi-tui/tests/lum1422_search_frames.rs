//! LUM-1422 — the search surface's column budget, rendered.
//!
//! Frame source for `docs/screenshots/lum1422-search-*.png`: `cargo test --
//! --nocapture` prints the cell grid the real [`pi_tui::App::render_snapshot`] /
//! `ratatui::Buffer` produced, and `scripts/frame_to_png.py` paints it. Two
//! panels:
//!
//! 1. the transcript search bar (`render_search_bar`) with a CJK query at 40
//!    columns — before the fix the query row was 55 columns wide and the
//!    renderer clipped the counter and the closing border away;
//! 2. a CJK transcript with the search open, plus a `~~~~` marker row that
//!    shows **which cells carry the highlight** — before the fix the marker
//!    sat over the text *before* the match.
//!
//! Why not `scripts/pty_capture.py`: no PTY on this Windows runner (see
//! `docs/LUM1422_SEARCH_COLUMNS.md` §"证据分级"). These are frames, not
//! keystroke interaction, and they carry no colour.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::input::{Key, KeyCode, KeyModifiers};
use pi_tui::search::{render_search_bar, SearchBar};
use pi_tui::styled::plain_text;
use pi_tui::width::columns;
use pi_tui::MessageItem;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

const SHEET_COLS: usize = 78;

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
    App::new(
        &agent,
        AppConfig {
            session_id: "lum1422-search-frames".into(),
            ..AppConfig::default()
        },
    )
}

fn pad(text: &str, width: usize, fill: char) -> String {
    let mut out = text.to_string();
    while columns(&out) < width {
        out.push(fill);
    }
    out
}

/// A row of `~~~~` under the cells that carry the search highlight.
fn marker_row(buf: &Buffer, y: u16, width: u16) -> String {
    let mut out = String::new();
    for x in 0..width {
        let cell = buf.cell((x, y)).expect("in-bounds cell");
        let marked = cell
            .modifier
            .intersects(Modifier::UNDERLINED | Modifier::REVERSED);
        out.push(if marked { '~' } else { ' ' });
    }
    out
}

fn rows_with_markers(buf: &Buffer, area: Rect) -> Vec<String> {
    let mut lines = Vec::new();
    for y in 0..area.height {
        // Join the row the way a terminal reads it: a wide glyph owns its cell
        // and the cell it hides, so the hidden cell contributes nothing (the
        // same rule `App`'s own text dump uses).
        let mut text = String::new();
        let mut hidden = 0usize;
        for x in 0..area.width {
            if hidden > 0 {
                hidden -= 1;
                continue;
            }
            let symbol = buf.cell((x, y)).expect("in-bounds cell").symbol();
            text.push_str(symbol);
            let glyph = columns(symbol);
            if glyph > 1 {
                hidden = glyph - 1;
            }
        }
        lines.push(text.trim_end().to_string());
        let markers = marker_row(buf, y, area.width).trim_end().to_string();
        if !markers.is_empty() {
            lines.push(markers);
        }
    }
    lines
}

/// The two evidence panels, as `(label, rows)` pairs.
fn panels() -> Vec<(&'static str, Vec<String>)> {
    let mut bar = SearchBar::new();
    bar.set_query("搜索中文查询串很长很长很长很长");
    let bar_rows = render_search_bar(&bar, 40)
        .lines
        .iter()
        .map(|line| plain_text(line))
        .collect();

    let mut app = app();
    app.messages_mut()
        .push(MessageItem::assistant("中文标题，然后 needle 出现在这里"));
    app.open_search();
    for ch in "needle".chars() {
        app.step_key(Key::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 8,
    };
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    let search_rows = rows_with_markers(&buf, area);

    vec![
        (
            "1. CJK query in a 40-column bar — the query is truncated by columns",
            bar_rows,
        ),
        (
            "2. CJK transcript + Ctrl+F `needle` — `~` marks the highlighted cells",
            search_rows,
        ),
    ]
}

#[test]
fn print_search_sheet() {
    let mut sheet: Vec<String> = Vec::new();
    for (label, rows) in panels() {
        sheet.push(pad(&format!("-- {label} "), SHEET_COLS, '-'));
        for row in &rows {
            sheet.push(pad(row, SHEET_COLS, ' '));
        }
        sheet.push(pad("", SHEET_COLS, ' '));
    }
    let width = SHEET_COLS;
    println!("FRAME DUMP cols={width} rows={}", sheet.len());
    for row in &sheet {
        println!("|{row}|");
    }
    println!("END FRAME DUMP");
}

/// The numeric half: the bar's widest row and the highlighted columns.
#[test]
fn print_search_report() {
    let mut bar = SearchBar::new();
    bar.set_query("搜索中文查询串很长很长很长很长");
    let widest = render_search_bar(&bar, 40)
        .lines
        .iter()
        .map(|line| columns(&plain_text(line)))
        .max()
        .unwrap_or(0);
    println!("SEARCH REPORT");
    println!("BAR terminal=40 max_row_columns={widest}");

    let mut app = app();
    app.messages_mut()
        .push(MessageItem::assistant("中文标题，然后 needle 出现在这里"));
    app.open_search();
    for ch in "needle".chars() {
        app.step_key(Key::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 8,
    };
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    let marked: Vec<u16> = (0..area.width)
        .filter(|x| {
            buf.cell((*x, 0))
                .expect("cell")
                .modifier
                .intersects(Modifier::UNDERLINED | Modifier::REVERSED)
        })
        .collect();
    let snapshot = app.render_snapshot(area.width, area.height);
    let row = snapshot
        .lines
        .iter()
        .find(|row| row.contains("needle"))
        .expect("message on screen");
    let expected = columns(&row[..row.find("needle").expect("match")]);
    println!(
        "HIGHLIGHT marked_columns={:?} expected_start={expected} expected_end={}",
        marked,
        expected + columns("needle")
    );
    println!("END SEARCH REPORT");
}
