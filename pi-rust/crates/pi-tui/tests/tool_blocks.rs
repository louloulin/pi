//! Interactive tool-output folding in the fullscreen [`App`].
//!
//! The driver renders a tool block into styled lines and installs a
//! [`ToolBlockRenderer`] on the App; the App owns the folding policy:
//!
//! * a collapsed block shows a tail preview plus a
//!   `… (+M lines, Ctrl+O to expand)` hint,
//! * `app.tools.expand` (`Ctrl+O`) expands / collapses every block,
//! * a left click toggles exactly the block under the pointer and does not
//!   start a text selection.
//!
//! These tests use a stub renderer so they stay independent of
//! `pi-coding-agent` (which owns the real, syntax-highlighting renderers and
//! depends on this crate, not the other way round).

use std::sync::Arc;

use pi_agent_core::{Agent, AgentEvent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Content, Model, ProviderId, ToolCall, ToolResult};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::input::{
    InputEvent, Key, KeyCode, KeyModifiers, MouseButton, MouseGesture, MouseGestureKind,
};
use pi_tui::message::{ToolBlock, ToolBlockRenderer};
use pi_tui::styled::{SpanStyle, StyledSpan};
use pi_tui::theme::ThemeColor;

const WIDTH: u16 = 40;
const HEIGHT: u16 = 20;

/// A renderer that gives every call a fixed call header plus `lines` styled
/// result lines tagged with the `toolOutput` slot.
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

fn app_with_renderer(lines: usize) -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "tool-blocks".into(),
            ..AppConfig::default()
        },
    );
    app.set_tool_block_renderer(Box::new(StubRenderer { lines }));
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    app
}

fn call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments: serde_json::json!({ "command": "echo hi" }),
    }
}

fn result(id: &str) -> ToolResult {
    ToolResult {
        tool_call_id: id.to_string(),
        content: Box::new(Content::text("hi")),
        is_error: false,
        details: None,
        added_tool_names: None,
        images: Vec::new(),
    }
}

fn run_tool(app: &mut App, id: &str) {
    app.apply_agent_event(AgentEvent::ToolExecutionStart {
        call: call(id, "bash"),
    });
    app.apply_agent_event(AgentEvent::ToolExecutionEnd {
        result: result(id),
        duration_ms: 5,
    });
}

fn gesture(kind: MouseGestureKind, x: u16, y: u16) -> InputEvent {
    InputEvent::gesture(MouseGesture::new(kind, x, y, false))
}

fn click(app: &mut App, x: u16, y: u16) {
    assert!(matches!(
        app.step(gesture(MouseGestureKind::Press(MouseButton::Left), x, y)),
        StepOutcome::Redraw | StepOutcome::Idle
    ));
    assert!(matches!(
        app.step(gesture(MouseGestureKind::Release(MouseButton::Left), x, y)),
        StepOutcome::Redraw | StepOutcome::Idle
    ));
}

#[test]
fn interactive_snapshot_shows_the_folded_block_and_ctrl_o_expands_it() {
    let mut app = app_with_renderer(10);
    run_tool(&mut app, "call-1");

    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    let text = snapshot.lines.join("\n");
    // The call header stays visible; the result folds to a tail preview with
    // the expand hint.
    assert!(text.contains("bash run-call-1"), "{text}");
    assert!(text.contains("… (+6 lines, Ctrl+O to expand)"), "{text}");
    assert!(text.contains("bash-line-9"), "{text}");
    assert!(!text.contains("bash-line-0"), "{text}");

    // The rich styling reaches the interactive render, not just the plain
    // text: the header and the preview spans keep the renderer's slots.
    let styled = app.messages().render_styled_lines(WIDTH);
    assert!(styled.iter().flatten().any(|span| {
        span.text == "bash run-call-1" && span.style.fg == Some(ThemeColor::ToolTitle)
    }));
    assert!(styled.iter().flatten().any(|span| {
        span.text == "bash-line-9" && span.style.fg == Some(ThemeColor::ToolOutput)
    }));

    // `app.tools.expand` reveals every line and reports the new state.
    assert_eq!(
        app.step_key(Key::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
        StepOutcome::Redraw
    );
    assert!(app.tools_expanded());
    assert_eq!(
        app.status_flash(),
        Some("Tool output: expanded"),
        "the chord acknowledges itself in the status hint"
    );
    let text = app.render_snapshot(WIDTH, HEIGHT).lines.join("\n");
    assert!(text.contains("bash-line-0"), "{text}");
    assert!(!text.contains("Ctrl+O to expand"), "{text}");
}

#[test]
fn click_toggles_only_the_block_under_the_pointer() {
    let mut app = app_with_renderer(10);
    run_tool(&mut app, "call-a");
    run_tool(&mut app, "call-b");
    let _ = app.render_snapshot(WIDTH, HEIGHT);

    let ranges = app.messages().item_line_ranges(WIDTH);
    assert_eq!(ranges.len(), 2);
    // Collapsed: header + hint + 4 preview lines each.
    assert_eq!(ranges[0].1 - ranges[0].0, 6);
    assert_eq!(ranges[1].1 - ranges[1].0, 6);

    let (width, height) = app.viewport();
    let (origin_x, origin_y) = app.viewport_origin();
    let (start, _) = app.messages().visible_lines(width, height);
    let second_block_row = ranges[1].0 - start;

    click(&mut app, origin_x + 2, origin_y + second_block_row as u16);

    let ranges = app.messages().item_line_ranges(WIDTH);
    assert_eq!(ranges[0].1 - ranges[0].0, 6, "the first block stays folded");
    assert_eq!(ranges[1].1 - ranges[1].0, 11, "the clicked block expands");
    assert!(!app.has_selection(), "a toggle is not a selection");
    assert!(app.take_clipboard_request().is_none());
}
