//! Tool block background slots.
//!
//! The Rust message renderer stamps the configured `ThemeBg` slot onto
//! every cell of every line of a tool block, mirroring upstream's
//! `Box(paddingX, 1, theme.bg("toolPendingBg" | "toolSuccessBg" |
//! "toolErrorBg", …))` pattern
//! (`packages/coding-agent/src/modes/interactive/components/tool-execution.ts`).
//!
//! These tests pin the slot for each [`ToolStatus`] so a future change
//! cannot silently fall back to the empty / Reset background.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::message::{MessageItem, ToolStatus};
use pi_tui::theme::{builtin_theme, ColorMode, ThemeBg};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const WIDTH: u16 = 40;
const HEIGHT: u16 = 8;

/// Dark palette values used by `assets/themes/dark.json`:
///   `"toolPendingBg": "#282832"` → (40, 40, 50)
///   `"toolSuccessBg": "#283228"` → (40, 50, 40)
///   `"toolErrorBg":   "#3c2828"` → (60, 40, 40)
const TOOL_PENDING: (u8, u8, u8) = (40, 40, 50);
const TOOL_SUCCESS: (u8, u8, u8) = (40, 50, 40);
const TOOL_ERROR: (u8, u8, u8) = (60, 40, 40);

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
            session_id: "tool-bg".into(),
            ..AppConfig::default()
        },
    )
}

fn render(app: &mut App, width: u16, height: u16) -> Buffer {
    let area = Rect {
        x: 0,
        y: 0,
        width,
        height,
    };
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    buf
}

fn bg_at(buf: &Buffer, x: u16, y: u16) -> Option<ThemeBg> {
    use ratatui::style::Color;
    let cell = buf.cell((x, y))?;
    let style = cell.style();
    match style.bg? {
        Color::Rgb(r, g, b) if (r, g, b) == TOOL_PENDING => Some(ThemeBg::ToolPendingBg),
        Color::Rgb(r, g, b) if (r, g, b) == TOOL_SUCCESS => Some(ThemeBg::ToolSuccessBg),
        Color::Rgb(r, g, b) if (r, g, b) == TOOL_ERROR => Some(ThemeBg::ToolErrorBg),
        _ => None,
    }
}

fn assert_cell_bg(buf: &Buffer, x: u16, y: u16, expected: ThemeBg, label: &str) {
    let got = bg_at(buf, x, y).unwrap_or_else(|| panic!("{label}: cell ({x},{y}) carries no tool bg"));
    assert_eq!(
        got, expected,
        "{label}: cell ({x},{y}) carries {got:?}, expected {expected:?}"
    );
}

#[test]
fn tool_pending_block_stamps_tool_pending_bg() {
    let mut app = app();
    app.messages_mut().push(MessageItem::tool_pending("echo hi"));
    let buf = render(&mut app, WIDTH, HEIGHT);

    // The first tool row is the body (no call header because the simple
    // constructor leaves `tool_header` None); the body starts at column 2
    // after the `[t] ` prefix. Stamp the configured pending bg on every
    // cell of the row, not just the prefix.
    assert_cell_bg(&buf, 2, 0, ThemeBg::ToolPendingBg, "pending body");
}

#[test]
fn tool_success_block_stamps_tool_success_bg() {
    let mut app = app();
    app.messages_mut().push(MessageItem::tool("echo hi"));
    let buf = render(&mut app, WIDTH, HEIGHT);
    assert_cell_bg(&buf, 2, 0, ThemeBg::ToolSuccessBg, "success body");
}

#[test]
fn tool_error_block_stamps_tool_error_bg() {
    let mut app = app();
    app.messages_mut().push(MessageItem::tool_error("echo hi"));
    let buf = render(&mut app, WIDTH, HEIGHT);
    assert_cell_bg(&buf, 2, 0, ThemeBg::ToolErrorBg, "error body");
}

#[test]
fn tool_status_setter_switches_the_bg_slot() {
    let mut app = app();
    let mut item = MessageItem::tool("echo hi");
    item.set_tool_status(ToolStatus::Pending);
    app.messages_mut().push(item);
    let buf = render(&mut app, WIDTH, HEIGHT);
    assert_cell_bg(&buf, 2, 0, ThemeBg::ToolPendingBg, "after set_tool_status(Pending)");

    let mut item = MessageItem::tool("echo hi");
    item.set_tool_status(ToolStatus::Error);
    app.messages_mut().replace_last(item);
    let buf = render(&mut app, WIDTH, HEIGHT);
    assert_cell_bg(&buf, 2, 0, ThemeBg::ToolErrorBg, "after set_tool_status(Error)");
}

#[test]
fn tool_block_bg_resolves_through_a_light_theme() {
    // The slot resolution must come from the live theme, not a hard-coded
    // RGB triple. Switch to the built-in light theme and confirm the
    // pending slot's RGB still produces the pending bg on a tool row.
    let mut app = app();
    app.messages_mut()
        .push(MessageItem::tool_pending("echo hi"));
    app.set_theme(builtin_theme("light", ColorMode::TrueColor).expect("light"));
    let buf = render(&mut app, WIDTH, HEIGHT);

    // The test does not assert a specific RGB triple under light (the
    // pending color is different from dark), but it must still be a
    // non-Reset bg on the tool body — i.e. the slot is wired through.
    use ratatui::style::Color;
    let cell = buf.cell((2, 0)).expect("cell in bounds");
    let bg = cell.style().bg.expect("tool body must carry a bg slot");
    assert_ne!(bg, Color::Reset, "tool body must carry a non-Reset bg");
}