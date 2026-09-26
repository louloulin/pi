//! N3 — Tool block "Took Xs" footer + truncation marker.
//!
//! nanopi's tool panels end with a `Took Xs` line so the reader can tell
//! at a glance how long the call ran, and a collapsed block shows
//! `… (+M lines, <chord> to expand)` so the reader knows how many lines
//! are hidden (`docs/NANOPI_VS_PI_RUST_GAP_ANALYSIS.md`, §2.4 / N3).
//!
//! The Rust port already had the fold hint (`tool_fold_hint`, LUM-1447
//! — the chord is resolved through the live keybindings registry, not
//! hardcoded). N3 wires the `Took Xs` footer:
//!
//!   * `MessageItem::elapsed_ms: Option<u64>` — driver writes the measured
//!     `Duration` once `ToolExecutionEnd` lands.
//!   * `MessageItem::took_footer_line(elapsed_ms)` formats the row in the
//!     style nanopi uses (millisecond, sub-second, minute, hour scales).
//!   * `MessageView::tool_body_lines` appends the footer *after* the fold
//!     hint so the row sits at the bottom of both expanded and collapsed
//!     blocks.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::components::message::{took_footer_line, MessageItem, ToolStatus};

const WIDTH: u16 = 60;
const HEIGHT: u16 = 12;

/// `took_footer_line` formats the row at four scales:
///   * `<1s`     → `Took 532ms`
///   * `<60s`    → `Took 12.4s`
///   * `<60min`  → `Took 4m 32s`
///   * otherwise → `Took 1h 12m`
#[test]
fn took_footer_line_uses_millisecond_scale_below_one_second() {
    let spans = took_footer_line(532);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].text, "Took 532ms");
}

#[test]
fn took_footer_line_uses_one_decimal_below_sixty_seconds() {
    let spans = took_footer_line(12_400);
    assert_eq!(spans[0].text, "Took 12.4s");
}

#[test]
fn took_footer_line_rounds_to_minutes_below_an_hour() {
    let spans = took_footer_line(4 * 60_000 + 32_000);
    assert_eq!(spans[0].text, "Took 4m 32s");
}

#[test]
fn took_footer_line_rounds_to_hours_above_an_hour() {
    let spans = took_footer_line((3600 + 12 * 60) * 1000);
    assert_eq!(spans[0].text, "Took 1h 12m");
}

#[test]
fn took_footer_line_carries_dim_italic_style() {
    let spans = took_footer_line(2_500);
    assert_eq!(spans[0].text, "Took 2.5s");
    assert!(
        spans[0].style.italic,
        "took footer must be italic, got {:?}",
        spans[0].style
    );
    assert_eq!(
        spans[0].style.fg,
        Some(pi_tui::theme::ThemeColor::Dim),
        "took footer must use the Dim slot"
    );
}

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
            session_id: "tool-took".into(),
            ..AppConfig::default()
        },
    )
}

fn render(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width,
        height,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    buf
}

/// Helper: dump the message viewport (everything except the bottom chrome)
/// as plain text so the tests can assert on the visible rows.
fn visible_text(buf: &ratatui::buffer::Buffer, width: u16, height: u16) -> String {
    let mut out = String::new();
    for y in 0..height {
        let row: String = (0..width)
            .map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()).unwrap_or_default())
            .collect();
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

/// A finished tool block (`tool_status = Success`) carrying an
/// `elapsed_ms` stamps the `Took Xs` row at the bottom of the visible
/// block. The header / body rows still appear above.
#[test]
fn tool_block_appends_took_line_with_elapsed() {
    let mut app = app();
    let mut item = MessageItem::tool("[bash] ls");
    item.text = "[bash] ls\nfile_a\nfile_b\nfile_c".to_string();
    item.tool_header = Some(vec![vec![pi_tui::utils::styled::StyledSpan::new(
        "[bash] ls".to_string(),
        pi_tui::utils::styled::SpanStyle::PLAIN,
    )]]);
    item.tool_lines = Some(vec![
        vec![pi_tui::utils::styled::StyledSpan::new(
            "file_a".to_string(),
            pi_tui::utils::styled::SpanStyle::PLAIN,
        )],
        vec![pi_tui::utils::styled::StyledSpan::new(
            "file_b".to_string(),
            pi_tui::utils::styled::SpanStyle::PLAIN,
        )],
        vec![pi_tui::utils::styled::StyledSpan::new(
            "file_c".to_string(),
            pi_tui::utils::styled::SpanStyle::PLAIN,
        )],
    ]);
    item.tool_expanded = Some(true);
    item.set_elapsed_ms(2_500);
    app.messages_mut().push(item);

    let buf = render(&mut app, WIDTH, HEIGHT);
    let text = visible_text(&buf, WIDTH, HEIGHT);
    assert!(
        text.contains("file_a") && text.contains("file_b") && text.contains("file_c"),
        "tool body must remain visible above the footer, got:\n{text}"
    );
    assert!(
        text.contains("Took 2.5s"),
        "the elapsed footer must show Took 2.5s, got:\n{text}"
    );
}

/// Pending tool blocks never show `Took Xs` — the call has not finished,
/// no duration is known, and a footer that overlapped the pending-bg slot
/// would be visually inconsistent.
#[test]
fn tool_block_omits_took_line_while_pending() {
    let mut app = app();
    let item = MessageItem::tool_pending("[bash] sleeping")
        .with_tool_status(ToolStatus::Pending);
    app.messages_mut().push(item);

    let buf = render(&mut app, WIDTH, HEIGHT);
    let text = visible_text(&buf, WIDTH, HEIGHT);
    assert!(
        !text.contains("Took "),
        "pending blocks must not show a Took footer, got:\n{text}"
    );
}

/// `elapsed_ms = None` (the historical default) keeps the historical
/// "no footer" rendering — drivers that never observed `ToolExecutionStart`
/// produce the same byte-identical output they did before N3.
#[test]
fn tool_block_omits_took_line_when_elapsed_ms_is_none() {
    let mut app = app();
    let item = MessageItem::tool("[bash] no-elapsed");
    app.messages_mut().push(item);

    let buf = render(&mut app, WIDTH, HEIGHT);
    let text = visible_text(&buf, WIDTH, HEIGHT);
    assert!(
        !text.contains("Took "),
        "blocks without elapsed_ms must not show a Took footer, got:\n{text}"
    );
}

/// When the body exceeds [`TOOL_PREVIEW_LINES`] and the block is
/// collapsed, the fold hint `… (+M lines, <chord> to expand)` is the
/// *last* non-fold line above the `Took Xs` row. The fold count is
/// total − previewed, and the chord is resolved through the live
/// keybindings registry (default `Ctrl+O`, LUM-1447).
#[test]
fn tool_block_truncation_marker_appears_when_output_exceeds_lines() {
    let mut app = app();
    let body_lines: Vec<pi_tui::utils::styled::StyledLine> = (0..10)
        .map(|i| {
            vec![pi_tui::utils::styled::StyledSpan::new(
                format!("line_{i}"),
                pi_tui::utils::styled::SpanStyle::PLAIN,
            )]
        })
        .collect();
    let header = vec![vec![pi_tui::utils::styled::StyledSpan::new(
        "[bash] ten".to_string(),
        pi_tui::utils::styled::SpanStyle::PLAIN,
    )]];
    let mut item = MessageItem::tool("[bash] ten");
    item.tool_header = Some(header);
    item.tool_lines = Some(body_lines);
    item.tool_expanded = Some(false);
    item.set_elapsed_ms(1_800);
    app.messages_mut().push(item);

    let buf = render(&mut app, WIDTH, HEIGHT);
    let text = visible_text(&buf, WIDTH, HEIGHT);
    assert!(
        text.contains("line_6") || text.contains("line_9"),
        "collapsed preview must keep the bottom lines, got:\n{text}"
    );
    assert!(
        text.contains("(+"),
        "truncation marker must show the fold hint, got:\n{text}"
    );
    assert!(
        text.contains("Ctrl+O") || text.contains("ctrl+o") || text.contains("expand"),
        "truncation marker must advertise a chord (default Ctrl+O / expand), got:\n{text}"
    );
    assert!(
        text.contains("Took 1.8s"),
        "truncation marker must not displace the Took footer — both must be visible, got:\n{text}"
    );
}

/// `MessageItem::with_elapsed_ms` / `set_elapsed_ms` are the builder
/// form drivers use. Both must produce a value that round-trips.
#[test]
fn with_elapsed_ms_sets_the_field() {
    let item = MessageItem::tool("[bash] ls").with_elapsed_ms(4_200);
    assert_eq!(item.elapsed_ms, Some(4_200));
    let mut item = MessageItem::tool("[bash] ls");
    item.set_elapsed_ms(800);
    assert_eq!(item.elapsed_ms, Some(800));
}