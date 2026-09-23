//! LUM-1447 frame source — the collapsed-tool hint under three key tables.
//!
//! `cargo test -- --nocapture` prints the cell grid a real
//! [`pi_tui::App::render_to_buffer`] produced and `scripts/frame_to_png.py`
//! paints it (this Windows runner has no PTY — see `docs/LUM1426_POINTER_COLUMNS.md` §9).
//!
//! Three 80×20 panels, one per `app.tools.expand` state:
//!
//! 1. `ctrl+o` (the shipped default) — `Ctrl+O to expand`;
//! 2. `ctrl+u` (a `keybindings.json` override) — `Ctrl+U to expand`;
//! 3. unbound — the chord is gone, the affordance stays (`to expand`).
//!
//! Frames prove the painted text, not the key handling; the key handling is
//! pinned by `editor.rs`'s `history_chords_*` unit tests and
//! `tests/hint_bindings.rs`.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentEvent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Content, Model, ProviderId, ToolCall, ToolResult};
use pi_tui::app::{App, AppConfig};
use pi_tui::keybindings::{
    set_keybindings, tui_default_keybindings, KeybindingDefinition, KeybindingsConfig,
    KeybindingsManager,
};
use pi_tui::message::{ToolBlock, ToolBlockRenderer};
use pi_tui::styled::{SpanStyle, StyledSpan};
use pi_tui::theme::ThemeColor;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const COLS: u16 = 80;
const ROWS: u16 = 20;

/// `set_keybindings` mutates process state, so the three panels run one at a
/// time inside this binary.
static REGISTRY: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_registry() -> std::sync::MutexGuard<'static, ()> {
    REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct StubRenderer {
    lines: usize,
}

impl ToolBlockRenderer for StubRenderer {
    fn begin_tool(&mut self, _call: &ToolCall) {}

    fn finish_tool(&mut self, result: &ToolResult, _width: u16) -> Option<ToolBlock> {
        let header = vec![vec![StyledSpan::new(
            format!("bash run-{}", result.tool_call_id),
            SpanStyle::fg(ThemeColor::ToolTitle).bold(),
        )]];
        let body = (0..self.lines)
            .map(|i| {
                vec![StyledSpan::new(
                    format!("bash-line-{i}"),
                    SpanStyle::fg(ThemeColor::ToolOutput),
                )]
            })
            .collect();
        Some(ToolBlock::new(header, body))
    }
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

/// Install an `app.tools.expand` binding (empty = unbound) over the TUI table.
fn install(keys: Vec<&str>) {
    let mut definitions = tui_default_keybindings();
    definitions.push((
        "app.tools.expand".to_string(),
        KeybindingDefinition::new(keys),
    ));
    set_keybindings(KeybindingsManager::new(
        definitions,
        KeybindingsConfig::new(),
    ));
}

fn app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1447-binding-hints".into(),
            ..AppConfig::default()
        },
    );
    app.set_tool_block_renderer(Box::new(StubRenderer { lines: 10 }));
    app.apply_agent_event(AgentEvent::ToolExecutionStart {
        call: ToolCall {
            id: "call-1".into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": "echo hi" }),
        },
    });
    app.apply_agent_event(AgentEvent::ToolExecutionEnd {
        result: ToolResult {
            tool_call_id: "call-1".into(),
            content: Box::new(Content::text("hi")),
            is_error: false,
            details: None,
            added_tool_names: None,
        },
        duration_ms: 5,
    });
    let _ = app.render_snapshot(COLS, ROWS);
    app
}

/// One buffer row as plain text (wide glyphs collapse to one cell).
fn row(buf: &Buffer, y: u16) -> String {
    let mut text = String::new();
    let mut skip = 0usize;
    for x in 0..COLS {
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

fn dump(app: &mut App, caption: &str) {
    let area = Rect::new(0, 0, COLS, ROWS);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    assert!(
        (0..ROWS).any(|y| row(&buf, y).contains("lines,")),
        "the collapsed hint is on screen"
    );
    println!("PANEL {caption}");
    println!("FRAME DUMP cols={COLS} rows={ROWS}");
    for y in 0..ROWS {
        println!("|{}|", row(&buf, y));
    }
    println!("END FRAME DUMP");
}

#[test]
fn frame_dump_fold_hint_default_ctrl_o() {
    let _guard = lock_registry();
    install(vec!["ctrl+o"]);
    let mut app = app();
    let area = Rect::new(0, 0, COLS, ROWS);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    assert!(
        (0..ROWS).any(|y| row(&buf, y).contains("Ctrl+O to expand")),
        "the default table must still advertise Ctrl+O"
    );
    dump(
        &mut app,
        "LUM-1447 default `app.tools.expand` = ctrl+o: `Ctrl+O to expand`",
    );
}

#[test]
fn frame_dump_fold_hint_rebound_ctrl_u() {
    let _guard = lock_registry();
    install(vec!["ctrl+u"]);
    let mut app = app();
    let area = Rect::new(0, 0, COLS, ROWS);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    assert!(
        (0..ROWS).any(|y| row(&buf, y).contains("Ctrl+U to expand")),
        "the rebound chord must reach the transcript hint"
    );
    dump(
        &mut app,
        "LUM-1447 `app.tools.expand` overridden to ctrl+u: the hint follows the key",
    );
}

#[test]
fn frame_dump_fold_hint_unbound() {
    let _guard = lock_registry();
    install(vec![]);
    let mut app = app();
    let area = Rect::new(0, 0, COLS, ROWS);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    assert!(
        (0..ROWS).any(|y| row(&buf, y).contains("to expand")),
        "an unbound fold still advertises the affordance"
    );
    assert!(
        !(0..ROWS).any(|y| row(&buf, y).contains("Ctrl+")),
        "an unbound fold must not advertise any chord"
    );
    dump(
        &mut app,
        "LUM-1447 `app.tools.expand` unbound: no chord, the affordance stays",
    );
}
