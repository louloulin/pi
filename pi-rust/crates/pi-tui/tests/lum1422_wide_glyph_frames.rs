//! LUM-1422 — wide-glyph column accounting, rendered.
//!
//! This file is the **frame source** for
//! `docs/screenshots/lum1422-*.png`: `cargo test -- --nocapture` prints the
//! cell grid the real [`pi_tui::App::render_snapshot`] built (the same grid the
//! driver hands to `ratatui`), and `scripts/frame_to_png.py` paints it. It also
//! prints a machine-readable width report (`PANEL … max_row_columns=…`) that the
//! audit doc quotes, so "before" and "after" are compared with a number instead
//! of with an impression.
//!
//! Why not `scripts/pty_capture.py`: that is the repo's evidence of record and
//! needs a real PTY (`pty`, `fcntl`, `termios`) plus `pyte`; this round ran on
//! a Windows runner with neither. So these images show **frames**, not
//! keystroke interaction, and they carry no colour. Everything they claim is
//! also asserted by `tests/composer_wide_glyphs.rs` and the markdown suite,
//! which is where the width contract is actually pinned.
//!
//! The dump is a *sheet*: a label row between panels, because
//! `frame_to_png.py` renders one grid per file. The label rows are ordinary
//! grid rows, padded to the sheet width like every other row.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::input::{Key, KeyCode, KeyModifiers};
use pi_tui::MessageItem;

const SHEET_COLS: usize = 88;

/// One rendered panel: what it shows, the terminal it was rendered for, and
/// the rows the App produced.
struct Panel {
    label: &'static str,
    width: u16,
    rows: Vec<String>,
}

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

fn app(session: &str) -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    App::new(
        &agent,
        AppConfig {
            session_id: session.into(),
            ..AppConfig::default()
        },
    )
}

fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        app.step_key(Key::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
}

/// Pad `text` to `width` **columns** with `fill`.
///
/// Padded by display columns, not characters: this sheet mixes CJK and ASCII,
/// and a wide glyph already spends two of the sheet's columns.
fn pad(text: &str, width: usize, fill: char) -> String {
    let mut out = text.to_string();
    while ratatui::text::Line::from(out.clone()).width() < width {
        out.push(fill);
    }
    out
}

/// Display columns a row occupies on a terminal — measured with `ratatui`'s own
/// `Line::width`, the same `unicode-width` the buffer and the terminal use.
fn columns(row: &str) -> usize {
    ratatui::text::Line::from(row.to_string()).width()
}

/// The three width panels.
///
/// Panel 1 is the case the issue is about: a CJK draft in a narrow terminal.
/// Before the fix the draft wrapped after 42 *characters* (84 columns) and the
/// painted row was twice as wide as the terminal, so the terminal re-flowed it
/// and every row below it moved. Panel 2 shows the mixed CJK/ASCII draft and
/// the caret. Panel 3 is the transcript: a CJK prompt and a markdown reply with
/// a wide-glyph table, which is where the same defect made ratatui's
/// `Buffer::diff` skip the rest of the row.
fn panels() -> Vec<Panel> {
    let mut narrow = app("lum1422-narrow");
    type_text(&mut narrow, "把整个 tui 的宽度按终端列数计算，宽字符占两列");

    let mut mixed = app("lum1422-mixed");
    type_text(&mut mixed, "宽度 width 计算 by columns");

    let mut chat = app("lum1422-transcript");
    chat.messages_mut()
        .push(MessageItem::user("用中文解释一下表格对齐"));
    chat.messages_mut().push(MessageItem::assistant(
        "## 列宽对齐\n\n- 宽字符占两列\n- 表格边框按列数对齐\n\n| 名称 | 说明 |\n| --- | --- |\n| 宽度 | 按终端列数计算 |\n| 光标 | 落在宽字符之后 |\n",
    ));
    chat.messages_mut().push(MessageItem::tool(
        "read · pi-rust/crates/pi-tui/src/visual_text.rs",
    ));

    vec![
        Panel {
            label: "1. CJK draft, 44x9 — wraps at the column edge, not the character count",
            width: 44,
            rows: narrow.render_snapshot(44, 9).lines,
        },
        Panel {
            label: "2. mixed CJK/ASCII draft, 40x9 — the caret sits after the glyph it follows",
            width: 40,
            rows: mixed.render_snapshot(40, 9).lines,
        },
        Panel {
            label: "3. transcript with CJK markdown + a wide-glyph table, 88x20",
            width: 88,
            rows: chat.render_snapshot(88, 20).lines,
        },
    ]
}

/// The sheet [`print_width_sheet`] dumps for `frame_to_png.py`.
fn width_sheet() -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for panel in panels() {
        lines.push(pad(&format!("-- {} ", panel.label), SHEET_COLS, '-'));
        for row in &panel.rows {
            lines.push(pad(row, SHEET_COLS, ' '));
        }
        lines.push(pad("", SHEET_COLS, ' '));
    }
    lines
}

#[test]
fn print_width_sheet() {
    let sheet = width_sheet();
    let width = sheet
        .iter()
        .map(|row| columns(row))
        .max()
        .unwrap_or(0)
        .max(SHEET_COLS);
    println!("FRAME DUMP cols={width} rows={}", sheet.len());
    for row in &sheet {
        println!("|{row}|");
    }
    println!("END FRAME DUMP");
}

/// The numeric half of the evidence: the widest row each panel painted, next to
/// the terminal it was painted for. `max_row_columns <= terminal` is the
/// invariant the fix establishes; a panel that breaks it is a row the terminal
/// would re-flow.
#[test]
fn print_width_report() {
    println!("WIDTH REPORT");
    for (index, panel) in panels().iter().enumerate() {
        let widest = panel.rows.iter().map(|row| columns(row)).max().unwrap_or(0);
        println!(
            "PANEL {} terminal={} max_row_columns={}",
            index + 1,
            panel.width,
            widest
        );
    }
    println!("END WIDTH REPORT");
}

/// One full interaction at 120x30 — the "does the TUI read like codex / pi"
/// frame: a CJK prompt, a streaming assistant body with markdown, a tool block,
/// and the footer. Rendered through the same `render_snapshot` path the driver
/// uses, so it is the current tip and not a hand-written mock.
#[test]
fn print_overview_sheet() {
    use pi_tui::styled::{SpanStyle, StyledSpan};
    use pi_tui::theme::ThemeColor;

    let mut app = app("lum1422-overview");
    app.messages_mut().push(MessageItem::user(
        "把 pi-tui 的宽度改成按终端列数计算（宽字符占两列）",
    ));
    let mut assistant = MessageItem::assistant(
        "## 改动\n\n- `visual_text` 按列数换行\n- 绘制时宽字符占两格，后面的格子清空\n- 表格、列表、对话框同步\n\n| 模块 | 状态 |\n| --- | --- |\n| 输入框 | 完成 |\n| 转录视图 | 完成 |\n",
    );
    assistant.thinking = "先量一次基准，再决定改哪一层。".into();
    assistant.streaming = true;
    app.messages_mut().push(assistant);
    app.messages_mut().push_tool_block(
        "bash · cargo test -p pi-tui",
        vec![vec![StyledSpan::new(
            "$ cargo test -p pi-tui",
            SpanStyle::fg(ThemeColor::Accent).bold(),
        )]],
        vec![
            vec![StyledSpan::new(
                "test result: ok. 939 passed; 0 failed",
                SpanStyle::fg(ThemeColor::Text),
            )],
            vec![StyledSpan::new(
                "(12 earlier lines, Ctrl+O to expand)",
                SpanStyle::fg(ThemeColor::Dim),
            )],
        ],
    );
    type_text(&mut app, "下一步把 markdown 的列宽协商也补上");

    let rows = app.render_snapshot(120, 30).lines;
    println!("FRAME DUMP cols=120 rows={}", rows.len() + 2);
    println!(
        "|{}|",
        pad("-- pi-tui current tip, 120x30 (frame buffer) ", 120, '-')
    );
    for row in &rows {
        println!("|{}|", pad(row, 120, ' '));
    }
    println!("|{}|", pad("", 120, ' '));
    println!("END FRAME DUMP");
}
