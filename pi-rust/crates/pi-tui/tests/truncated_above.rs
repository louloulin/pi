//! The "cut above" affordance: a block taller than the viewport must not be
//! indistinguishable from a block that simply starts at the top edge.
//!
//! LUM-1271 measured the defect on a real PTY: at 34 rows the expanded
//! startup header plus the composer/status rows leave ~11-13 transcript rows,
//! so the ~34-row wrapped `/help` body showed only its tail. The reader had
//! no way to tell that the first on-screen row was the middle of a block, and
//! `Home` was documented but never advertised at the moment it was needed.
//!
//! `App::truncated_above_lines` is the rule (top edge strictly inside an
//! item's row span, while the viewport is pinned to the tail) and
//! `App::paint_truncated_above` is the one-row hint it paints — see
//! `docs/TUI_TRUNCATION_AFFORDANCE_LUM1273.md`.
//!
//! The viewport in this file is `BAR`x7: `WIDTH - 1` while the transcript
//! overflows, `HEIGHT - 3` (status + prompt + Phase 2 / G3 editor border).

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::input::{InputEvent, MouseButton, MouseGesture, MouseGestureKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const WIDTH: u16 = 40;
const HEIGHT: u16 = 10;
/// Rightmost column of the frame = the scrollbar's column while the log
/// overflows.
const BAR: u16 = WIDTH - 1;

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
            session_id: "truncated-above".into(),
            ..AppConfig::default()
        },
    )
}

/// The interactive render path (scrollbar overlay on), exactly what the
/// alt-screen driver paints.
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

/// One item whose body is `rows` hard lines — the `/help` shape: a single
/// block far taller than the viewport.
fn long_block(app: &mut App, rows: usize) {
    let body = (0..rows)
        .map(|i| format!("row-{i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.info(body);
}

fn hidden_rows(app: &App) -> usize {
    let (width, height) = app.viewport();
    app.messages().line_count(width) - height as usize
}

// ---------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------

#[test]
fn a_pinned_viewport_reports_how_much_of_the_top_block_is_above_it() {
    let mut app = app();
    long_block(&mut app, 12);
    let _ = render(&mut app);

    assert_eq!(app.viewport(), (BAR, HEIGHT - 3));
    let hidden = hidden_rows(&app);
    assert!(hidden > 0, "the block must overflow the viewport");
    assert_eq!(
        app.truncated_above_lines(),
        Some(hidden),
        "a single-item log hides exactly the lines above the viewport"
    );
}

#[test]
fn a_block_boundary_at_the_top_edge_reports_nothing() {
    // Twenty separate one-line items: wherever the top edge lands, it lands
    // on a boundary, so nothing is cut and there is nothing to disclose.
    let mut app = app();
    for i in 0..20 {
        app.info(format!("line {i}"));
    }
    let _ = render(&mut app);

    assert!(hidden_rows(&app) > 0, "the log must overflow");
    assert_eq!(app.truncated_above_lines(), None);
}

#[test]
fn a_detached_viewport_reports_nothing() {
    // Scrolling away from the tail already has a signal — the jump-to-latest
    // pill — and spending a second transcript row to repeat it would leave
    // the reader with no transcript left.
    let mut app = app();
    long_block(&mut app, 24);
    let _ = render(&mut app);
    assert!(app.truncated_above_lines().is_some());

    app.scroll_viewport_up(3);
    let _ = render(&mut app);
    assert_eq!(
        app.truncated_above_lines(),
        None,
        "the pill owns the detached case"
    );
    assert!(app.scroll_to_end_rect().is_some(), "the pill is up");
}

#[test]
fn a_transcript_that_fits_reports_nothing() {
    let mut app = app();
    app.info("short");
    let _ = render(&mut app);
    assert_eq!(app.truncated_above_lines(), None);
}

// ---------------------------------------------------------------------------
// The painted hint
// ---------------------------------------------------------------------------

#[test]
fn the_hint_names_the_hidden_line_count_and_the_key_that_reaches_it() {
    let mut app = app();
    long_block(&mut app, 12);
    let buf = render(&mut app);

    let rect = app
        .truncated_above_rect()
        .expect("a cut block paints the hint");
    assert_eq!(rect.y, app.viewport_origin().1, "the hint owns the top row");
    assert!(rect.x + rect.width <= BAR, "the hint stops left of the bar");
    let label = row_text(&buf, rect.y);
    assert!(label.starts_with(" ⋯"), "found {label:?}");
    assert!(
        label.contains(&format!("{} lines above", hidden_rows(&app))),
        "found {label:?}"
    );
    assert!(label.contains("· Home"), "found {label:?}");

    // The head of the block really is off-screen, i.e. the hint is not
    // reporting a cut that is not there.
    assert!(
        !(0..HEIGHT).any(|y| row_text(&buf, y).contains("row-00")),
        "row-00 must be above the top edge"
    );
    // And the hint costs exactly one row: the next row is transcript again.
    assert!(
        row_text(&buf, rect.y + 1).contains("row-"),
        "found {:?}",
        row_text(&buf, rect.y + 1)
    );
}

#[test]
fn exactly_one_hidden_line_is_singular() {
    let mut app = app();
    // The viewport is 7 rows (Phase 2 / G3 reserves a row for the editor
    // border); an 8-row block leaves exactly one line above.
    long_block(&mut app, HEIGHT as usize - 3 + 1);
    let buf = render(&mut app);
    assert_eq!(app.truncated_above_lines(), Some(1));
    let label = row_text(&buf, app.viewport_origin().1);
    assert!(label.contains("1 line above"), "found {label:?}");
    assert!(!label.contains("1 lines above"), "found {label:?}");
}

#[test]
fn the_transcript_snapshot_carries_no_hint() {
    // `render_snapshot` backs `/transcript`; screen furniture must not leak
    // into the exported content.
    let mut app = app();
    long_block(&mut app, 12);
    let _ = render(&mut app);
    assert!(app.truncated_above_rect().is_some());

    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    assert!(
        !snapshot
            .lines
            .iter()
            .any(|line| line.contains("lines above")),
        "found the hint in {:?}",
        snapshot.lines
    );
}

// ---------------------------------------------------------------------------
// The pointer
// ---------------------------------------------------------------------------

#[test]
fn clicking_the_hint_does_the_key_it_advertises() {
    let mut app = app();
    long_block(&mut app, 24);
    let _ = render(&mut app);
    let rect = app.truncated_above_rect().expect("hint painted");

    app.step(InputEvent::gesture(MouseGesture::new(
        MouseGestureKind::Press(MouseButton::Left),
        rect.x + 1,
        rect.y,
        false,
    )));

    // The advertised key is `tui.altScreen.top` (Home), which
    // `App::scroll_viewport_to_top` resolves against the live line count
    // (it pins the viewport to the head). A 24-row body in a height-8
    // viewport leaves 16 hidden above, so the resulting offset is the
    // max-scroll value, not the `usize::MAX` sentinel that
    // `MessageView::scroll_to_top` uses internally.
    let (width, height) = app.viewport();
    let expected_offset = app
        .messages()
        .line_count(width)
        .saturating_sub(height as usize);
    assert_eq!(
        app.messages().scroll_offset(),
        expected_offset,
        "the advertised key is `tui.altScreen.top`, so the click is Home"
    );
    assert!(
        app.messages().scroll_offset() > 0,
        "the click scrolled the viewport up; the head is now in view"
    );
    let buf = render(&mut app);
    let pill = app
        .scroll_to_end_rect()
        .expect("detached view paints the pill");
    let label: String = (pill.x..pill.x + pill.width)
        .map(|x| symbol_at(&buf, x, pill.y))
        .collect();
    assert!(label.contains("Jump to latest"), "found {label:?}");
}
