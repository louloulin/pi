//! Phase 6 / G7 — User/Info/Tool messages render through
//! [`BoxLayout::with_bg`] (the upstream `Box(paddingX, paddingY,
//! theme.bg(slot, …))` pattern), not the legacy per-span stamp.
//!
//! Pre-Phase-6 the bg was applied by [`crate::components::message`]
//! walking each span and setting `style.bg` directly. After Phase 6 the
//! same end-state cell paint must come from [`BoxLayout`] so the
//! padding + bg + bgFn ownership is in one place. These tests pin that
//! delegation: a user message, an info message, and a tool message
//! must all paint their cells with the expected `ThemeBg` slot — the
//! exact behaviour the tool-block bg test already locked in for
//! `toolStatus`, applied across all three block kinds.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::message::{MessageItem, Role, ToolStatus};
use pi_tui::theme::{builtin_theme, ColorMode, ThemeBg};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const WIDTH: u16 = 60;
const HEIGHT: u16 = 8;

// `assets/themes/dark.json`:
//   userMessageBg    → userMsgBg      #343541 → (52, 53, 65)
//   customMessageBg  → customMsgBg    #2d2838 → (45, 40, 56)
//   toolSuccessBg                      #283228 → (40, 50, 40)
const USER_BG: (u8, u8, u8) = (52, 53, 65);
const INFO_BG: (u8, u8, u8) = (45, 40, 56);
const TOOL_SUCCESS_BG: (u8, u8, u8) = (40, 50, 40);

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
            session_id: "block-bg".into(),
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

fn bg_matches(cell_bg: Option<ratatui::style::Color>, expected: (u8, u8, u8)) -> bool {
    use ratatui::style::Color;
    matches!(cell_bg, Some(Color::Rgb(r, g, b)) if (r, g, b) == expected)
}

fn assert_cell_bg(buf: &Buffer, x: u16, y: u16, expected: (u8, u8, u8), label: &str) {
    let cell = buf.cell((x, y)).expect("cell in frame");
    let bg = cell.style().bg;
    assert!(
        bg_matches(bg, expected),
        "{label}: cell ({x},{y}) bg = {bg:?}, expected rgb{expected:?}"
    );
}

#[test]
fn user_block_paints_user_message_bg_through_box_layout() {
    let mut app = app();
    app.messages_mut().push(MessageItem::user("hi"));

    // The user block no longer carries a `> ` prefix (matches TS
    // pi-tui — see `user-message.ts`), so the body text now starts at
    // column 0. The first body cell must still carry the user bg,
    // proving the bg travels through `BoxLayout::with_bg` rather than
    // the per-span stamp.
    let buf = render(&mut app, WIDTH, HEIGHT);

    assert_cell_bg(&buf, 0, 0, USER_BG, "user body[0,0]");
}

#[test]
fn info_block_paints_custom_message_bg_through_box_layout() {
    let mut app = app();
    // `MessageItem::info(...)` does not exist; build one inline.
    app.messages_mut().push(MessageItem {
        role: Role::Info,
        text: "transcript line".into(),
        thinking: String::new(),
        streaming: false,
        tool_status: ToolStatus::Success,
        tool_header: None,
        tool_lines: None,
        tool_expanded: None,
        notice_lines: None,
        stop_reason: None,
        elapsed_ms: None,
    });

    // Info blocks have no `·` (TS parity — `custom-message.ts` adds
    // none), so the body starts at column 0.
    let buf = render(&mut app, WIDTH, HEIGHT);

    assert_cell_bg(&buf, 0, 0, INFO_BG, "info body[0,0]");
}

#[test]
fn tool_block_success_paints_tool_success_bg_through_box_layout() {
    let mut app = app();
    // The default `tool(...)` constructor already uses `ToolStatus::Success`,
    // so no need for a separate `tool_success` constructor.
    app.messages_mut().push(MessageItem::tool("echo hi"));

    let buf = render(&mut app, WIDTH, HEIGHT);

    // Pin the success slot at the first body cell of the tool row.
    assert_cell_bg(&buf, 2, 0, TOOL_SUCCESS_BG, "tool body[0,0]");
}

#[test]
fn box_layout_with_padding_xy_zero_preserves_the_legacy_paint_outline() {
    // The Phase-6 migration sets `padding_xy(0, 0)` on the BoxLayout
    // wrapping message blocks, so the painted region must remain
    // identical to the pre-Phase-6 cell-stamp: every body cell carries
    // the same slot, and there is no stray padding row above or below.
    use pi_tui::component::{Component, TextComponent};
    use pi_tui::components::BoxLayout;
    use pi_tui::utils::styled::{SpanStyle, StyledLine, StyledSpan};

    let inner_lines: Vec<StyledLine> = vec![vec![StyledSpan::new("hi", SpanStyle::PLAIN)]];
    let child: std::boxed::Box<dyn Component> = std::boxed::Box::new(
        TextComponent::from_lines(inner_lines),
    );
    let boxed = BoxLayout::new(vec![child])
        .with_padding_xy(0, 0)
        .with_bg(ThemeBg::UserMessageBg);
    let rendered = boxed.render(WIDTH);

    assert_eq!(rendered.len(), 1, "no padding rows added: {rendered:?}");
    assert_eq!(rendered[0].len(), 1, "one body span preserved");
    assert_eq!(
        rendered[0][0].text, "hi",
        "the body text passes through unchanged"
    );
    assert_eq!(
        rendered[0][0].style.bg,
        Some(ThemeBg::UserMessageBg),
        "BoxLayout::with_bg carries the slot onto the inner span"
    );
}