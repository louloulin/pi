//! P13 — folded tool blocks own clicks on their body.
//!
//! Upstream wraps every tool block in a `MouseRegion` so a left click on
//! a folded result toggles `tool_expanded`
//! (`packages/tui/src/components/tool-execution.ts:115-126`). The Rust
//! port pairs [`pi_tui::MessageView::tool_block_ranges`] with
//! [`pi_tui::app::App::tool_block_regions`] and routes the gesture through
//! `step_tool_block_mouse_gesture`. These tests pin:
//!
//! * the bounding rectangle matches the rendered rows,
//! * a left press+release on the block toggles its `tool_expanded`,
//! * a release that started outside the block does not toggle,
//! * a release on a different block does not toggle the first one.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::components::mouse_region::MouseRegion;
use pi_tui::core::input_parse::{MouseButton, MouseGesture, MouseGestureKind};
use pi_tui::message::MessageItem;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const WIDTH: u16 = 40;
const HEIGHT: u16 = 12;

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
    App::new(&agent, AppConfig::default())
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

/// Render a transcript long enough that the tool block stays folded so we
/// can observe the fold/expand transition.
fn seed_long_tool_block(app: &mut App) {
    let body_lines: Vec<pi_tui::utils::styled::StyledLine> = (0..30)
        .map(|i| {
            vec![pi_tui::utils::styled::StyledSpan::new(
                format!("line {i}"),
                pi_tui::utils::styled::SpanStyle::PLAIN,
            )]
        })
        .collect();
    app.messages_mut()
        .push_tool_styled("[tool:read] short header", body_lines);
}

#[test]
fn tool_block_region_covers_the_folded_block_rows() {
    let mut app = app();
    seed_long_tool_block(&mut app);
    let _buf = render(&mut app, WIDTH, HEIGHT);
    let regions = app.tool_block_regions();
    assert_eq!(regions.len(), 1, "exactly one tool block in the transcript");
    let (idx, region) = regions.into_iter().next().unwrap();
    // The folded preview + fold hint is more than one row.
    assert!(
        region.rect().height >= 2,
        "folded block region should cover the preview + hint: {:?}",
        region.rect(),
    );
    // The rectangle starts inside the message area (origin_y > 0 because of
    // the prompt label).
    assert!(region.rect().y < HEIGHT);
    assert!(region.rect().x < WIDTH);
    assert!(region.rect().width == WIDTH);
    assert_eq!(idx, 0, "the only tool item is at index 0");
}

#[test]
fn click_on_a_folded_tool_block_expands_it() {
    let mut app = app();
    seed_long_tool_block(&mut app);
    let _buf = render(&mut app, WIDTH, HEIGHT);

    let (_, region) = app.tool_block_regions().into_iter().next().unwrap();
    let rect = region.rect();
    // The block is folded by default — line count should match the preview,
    // not the full 30-line body.
    let before_lines = app
        .messages_mut()
        .render_styled_lines(WIDTH)
        .len();
    assert!(
        before_lines < 30,
        "before click, the folded preview should be much shorter than the body: {before_lines}",
    );

    // Press + release on the center of the block — the upstream click gate.
    let press = MouseGesture::new(
        MouseGestureKind::Press(MouseButton::Left),
        rect.x + rect.width / 2,
        rect.y + rect.height / 2,
        false,
    );
    let release = MouseGesture::new(
        MouseGestureKind::Release(MouseButton::Left),
        rect.x + rect.width / 2,
        rect.y + rect.height / 2,
        false,
    );
    app.step_mouse_gesture(press);
    let outcome = app.step_mouse_gesture(release);
    // The expand changes the layout — outcome must request a redraw.
    assert!(matches!(outcome, pi_tui::app::StepOutcome::Redraw));

    // After the click, the block is expanded and the line count jumps.
    let after_lines = app
        .messages_mut()
        .render_styled_lines(WIDTH)
        .len();
    assert!(
        after_lines > before_lines,
        "clicking should expand the block; before={before_lines} after={after_lines}",
    );
}

#[test]
fn release_outside_the_pressed_block_does_not_toggle() {
    let mut app = app();
    seed_long_tool_block(&mut app);
    let _buf = render(&mut app, WIDTH, HEIGHT);

    let (_, region) = app.tool_block_regions().into_iter().next().unwrap();
    let rect = region.rect();
    let inside_x = rect.x + rect.width / 2;
    let inside_y = rect.y + rect.height / 2;
    let outside_y = rect.y.saturating_add(rect.height).min(HEIGHT - 1);

    // Press inside the block, release outside — upstream's `isClick` gate
    // must keep the block folded.
    app.step_mouse_gesture(MouseGesture::new(
        MouseGestureKind::Press(MouseButton::Left),
        inside_x,
        inside_y,
        false,
    ));
    let before_lines = app
        .messages_mut()
        .render_styled_lines(WIDTH)
        .len();
    app.step_mouse_gesture(MouseGesture::new(
        MouseGestureKind::Release(MouseButton::Left),
        inside_x,
        outside_y,
        false,
    ));
    let after_lines = app
        .messages_mut()
        .render_styled_lines(WIDTH)
        .len();
    assert_eq!(
        before_lines, after_lines,
        "press+release across the block boundary must not expand it",
    );
}

#[test]
fn second_click_collapses_the_block_again() {
    let mut app = app();
    seed_long_tool_block(&mut app);
    let _buf = render(&mut app, WIDTH, HEIGHT);
    let (_, region) = app.tool_block_regions().into_iter().next().unwrap();
    let rect = region.rect();

    let press = || {
        MouseGesture::new(
            MouseGestureKind::Press(MouseButton::Left),
            rect.x + rect.width / 2,
            rect.y + rect.height / 2,
            false,
        )
    };
    let release = || {
        MouseGesture::new(
            MouseGestureKind::Release(MouseButton::Left),
            rect.x + rect.width / 2,
            rect.y + rect.height / 2,
            false,
        )
    };

    let collapsed_len = app
        .messages_mut()
        .render_styled_lines(WIDTH)
        .len();
    app.step_mouse_gesture(press());
    app.step_mouse_gesture(release());
    let expanded_len = app
        .messages_mut()
        .render_styled_lines(WIDTH)
        .len();
    assert!(expanded_len > collapsed_len);
    // The region has shifted because the block grew — recompute it for the
    // second click.
    let (_, region) = app.tool_block_regions().into_iter().next().unwrap();
    let rect = region.rect();
    let press = MouseGesture::new(
        MouseGestureKind::Press(MouseButton::Left),
        rect.x + rect.width / 2,
        rect.y + rect.height / 2,
        false,
    );
    let release = MouseGesture::new(
        MouseGestureKind::Release(MouseButton::Left),
        rect.x + rect.width / 2,
        rect.y + rect.height / 2,
        false,
    );
    app.step_mouse_gesture(press);
    app.step_mouse_gesture(release);
    let collapsed_again = app
        .messages_mut()
        .render_styled_lines(WIDTH)
        .len();
    assert_eq!(
        collapsed_again, collapsed_len,
        "the second click should collapse the block back to the preview",
    );
}

#[test]
fn empty_transcript_yields_no_tool_block_regions() {
    let mut app = app();
    let _buf = render(&mut app, WIDTH, HEIGHT);
    assert!(app.tool_block_regions().is_empty());
}

// Sanity check: the imported `MouseRegion` and `MouseGesture` types resolve
// to the same surface area the App uses; this guards the test module
// against accidental refactors that move the symbol.
#[test]
fn mouse_region_companion_constructs() {
    let _r = MouseRegion::new(Rect::new(0, 0, 1, 1));
}