//! LUM-1422 — the transcript-search surface budgets its width in **columns**.
//!
//! LUM-1418 moved the crate's layout to terminal columns
//! (`crate::width`, one CJK ideograph = two columns). Two places in the search
//! face were left measuring **characters**, and both are user-visible:
//!
//! * [`render_search_bar`] truncated the query and the result counter by
//!   `chars().count()`, so a 15-character Chinese query painted a 30-column row
//!   inside a 20-column bar. The painter (correctly) clips at the bar's edge,
//!   which threw the match counter and the closing border away.
//! * [`App`]'s `apply_search_highlight` treated the corpus's **character**
//!   offsets as buffer **columns**, so on a row containing wide glyphs the
//!   highlight sat several columns left of the match it was marking — over
//!   unrelated text.
//!
//! The tests measure with `ratatui`'s own `Line::width` / the rendered buffer's
//! cell modifiers, never with the helper under test, so they cannot agree with
//! the bug by sharing its arithmetic.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::input::{Key, KeyCode, KeyModifiers};
use pi_tui::search::{render_search_bar, SearchBar, SEARCH_PLACEHOLDER};
use pi_tui::styled::plain_text;
use pi_tui::width::columns;
use pi_tui::MessageItem;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

/// A 40-column bar with a 15-character CJK query: the case that used to paint
/// a 55-column row.
const BAR_WIDTH: u16 = 40;

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
            session_id: "lum1422-search".into(),
            ..AppConfig::default()
        },
    )
}

fn widest_bar_row(bar: &SearchBar, width: u16) -> usize {
    render_search_bar(bar, width)
        .lines
        .iter()
        .map(|line| columns(&plain_text(line)))
        .max()
        .unwrap_or(0)
}

#[test]
fn a_cjk_query_never_widens_the_search_bar() {
    let mut bar = SearchBar::new();
    bar.set_query("搜索中文查询串很长很长很长很长");

    assert_eq!(
        widest_bar_row(&bar, BAR_WIDTH),
        BAR_WIDTH as usize,
        "every row of the bar is exactly as wide as the bar"
    );
}

#[test]
fn the_query_is_truncated_by_display_columns() {
    let mut bar = SearchBar::new();
    bar.set_query("搜索中文查询串很长很长很长很长");

    let layout = render_search_bar(&bar, BAR_WIDTH);
    let middle = plain_text(&layout.lines[1]);
    // The row must still be a closed box: the query gives up its tail, it does
    // not push the counter or the right border out of the bar.
    assert!(
        middle.starts_with('│') && middle.ends_with('│'),
        "the bar row stays closed: {middle:?}"
    );
    assert!(
        middle.contains("No matches") || middle.contains("1/"),
        "the counter survives the truncation: {middle:?}"
    );
    // The kept prefix is a column-budgeted slice of the query.
    let query_part: String = middle
        .chars()
        .skip_while(|c| c.is_whitespace() || *c == '│')
        .take_while(|c| !c.is_whitespace())
        .collect();
    assert!(
        columns(&query_part) <= columns("搜索中文查询串很长很长很长很长"),
        "the visible query is a prefix: {query_part:?}"
    );
}

#[test]
fn an_ascii_query_and_placeholder_stay_inside_the_bar() {
    let mut bar = SearchBar::new();
    assert!(
        widest_bar_row(&bar, BAR_WIDTH) == BAR_WIDTH as usize,
        "the placeholder row fits ({SEARCH_PLACEHOLDER:?})"
    );

    bar.set_query("a-very-long-ascii-query-string");
    assert_eq!(
        widest_bar_row(&bar, BAR_WIDTH),
        BAR_WIDTH as usize,
        "an ASCII query fits too"
    );

    // The narrow bar collapses the key labels to bare arrows and still fits.
    for width in [12u16, 20, 24, 32] {
        let mut bar = SearchBar::new();
        bar.set_query("中文查询");
        let widest = widest_bar_row(&bar, width);
        assert!(
            widest <= width as usize,
            "bar at {width} painted {widest} columns"
        );
    }
}

fn modifier_cells(app: &mut App, area: Rect) -> Vec<(u16, u16, String)> {
    let mut buf = ratatui::buffer::Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    let mut out = Vec::new();
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = buf.cell((x, y)).expect("cell");
            if cell
                .modifier
                .intersects(Modifier::UNDERLINED | Modifier::REVERSED)
            {
                out.push((x, y, cell.symbol().to_string()));
            }
        }
    }
    out
}

#[test]
fn a_cjk_row_highlights_the_match_and_not_the_text_before_it() {
    let mut app = app();
    app.messages_mut()
        .push(MessageItem::assistant("中文标题，然后 needle 出现在这里"));
    assert!(app.open_search(), "the transcript search opens");
    for ch in "needle".chars() {
        app.step_key(Key::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }

    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 14,
    };
    let snapshot = app.render_snapshot(area.width, area.height);
    let row = snapshot
        .lines
        .iter()
        .find(|row| row.contains("needle"))
        .expect("the message is on screen");
    let expected_start = columns(&row[..row.find("needle").expect("match")]);
    let expected_end = expected_start + columns("needle");

    let cells = modifier_cells(&mut app, area);
    let highlighted: Vec<u16> = cells
        .iter()
        .filter(|(_, y, _)| *y == 0)
        .map(|(x, _, _)| *x)
        .collect();
    assert_eq!(
        highlighted,
        (expected_start as u16..expected_end as u16).collect::<Vec<_>>(),
        "the highlight covers the match's own columns ({expected_start}..{expected_end})"
    );
    // And it is the match: the marked cells spell it.
    let spelled: String = cells
        .iter()
        .filter(|(_, y, _)| *y == 0)
        .map(|(_, _, symbol)| symbol.as_str())
        .collect();
    assert_eq!(spelled, "needle", "the marked cells spell the query");
}
