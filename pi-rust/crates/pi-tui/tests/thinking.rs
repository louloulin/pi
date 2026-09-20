//! Thinking / reasoning block tests for `pi-tui`.
//!
//! The Rust port used to drop `AssistantMessageUpdate::ThinkingDelta` on the
//! floor (`app.rs`'s Stage 4 comment "Collapsed thinking — not rendered"), so
//! extended-thinking models showed no reasoning at all. These tests pin the
//! new behaviour end to end:
//!
//! * streamed thinking deltas accumulate on the trailing assistant item;
//! * the reasoning renders above the body in the `thinkingText` slot (italic),
//!   like upstream's thinking `Markdown` component;
//! * `app.thinking.toggle` (`Ctrl+T`) collapses it to `Thinking...` and back,
//!   reporting the new state through the transient status hint;
//! * `App::apply_agent_event` routes a real `ThinkingDelta` into the view.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentEvent, AgentOptions, AssistantMessageUpdate};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::input::{InputEvent, Key, KeyCode, KeyModifiers};
use pi_tui::message::{MessageItem, MessageView, HIDDEN_THINKING_LABEL};
use pi_tui::styled::{plain_text, StyledLine};
use pi_tui::theme::ThemeColor;

const WIDTH: u16 = 40;
const HEIGHT: u16 = 10;

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
            session_id: "thinking".into(),
            ..AppConfig::default()
        },
    )
}

fn ctrl(ch: char) -> InputEvent {
    InputEvent::Key(Key::new(KeyCode::Char(ch), KeyModifiers::CONTROL))
}

fn rendered(app: &App) -> String {
    app.render_snapshot(WIDTH, HEIGHT).lines.join("\n")
}

/// A view holding one assistant item with `thinking` above `body`.
fn view_with_thinking(thinking: &str, body: &str) -> MessageView {
    let mut view = MessageView::new().with_markdown(true);
    view.begin_assistant_stream("m");
    view.append_thinking_delta(thinking);
    view.append_assistant_delta(body);
    view.end_assistant_stream();
    view
}

fn find<'a>(line: &'a StyledLine, text: &str) -> Option<&'a pi_tui::styled::StyledSpan> {
    line.iter().find(|span| span.text == text)
}

// ---------------------------------------------------------------------------
// MessageView
// ---------------------------------------------------------------------------

#[test]
fn thinking_deltas_join_the_trailing_assistant_item() {
    let mut view = MessageView::new();
    view.begin_assistant_stream("gpt");
    view.append_thinking_delta("let me ");
    view.append_thinking_delta("reason");
    view.append_assistant_delta("answer");
    view.end_assistant_stream();

    assert_eq!(view.len(), 1, "one message, not one item per delta");
    let item = &view.items()[0];
    assert_eq!(item.thinking, "let me reason");
    assert_eq!(item.text, "[gpt]answer");
}

#[test]
fn thinking_renders_above_the_body() {
    let view = view_with_thinking("weigh the options", "the answer");
    let lines = view.render_lines(WIDTH);

    let thinking_at = lines
        .iter()
        .position(|l| l.contains("weigh the options"))
        .expect("thinking text is rendered");
    let body_at = lines
        .iter()
        .position(|l| l.contains("the answer"))
        .expect("body is rendered");
    assert!(
        thinking_at < body_at,
        "thinking must precede the body: {lines:?}"
    );
}

#[test]
fn visible_thinking_uses_the_thinking_text_slot_italic() {
    let view = view_with_thinking("weigh the options", "the answer");
    let lines = view.render_styled_lines(WIDTH);
    let line = lines
        .iter()
        .find(|line| plain_text(line).contains("weigh the options"))
        .expect("thinking line");
    let span = find(line, "weigh the options").expect("thinking span");
    assert_eq!(span.style.fg, Some(ThemeColor::ThinkingText));
    assert!(span.style.italic, "thinking renders italic, like upstream");
    // The role prefix is not recoloured.
    assert_eq!(line[0].text, "  ");
    assert_ne!(line[0].style.fg, Some(ThemeColor::ThinkingText));
}

#[test]
fn hidden_thinking_collapses_to_the_label_and_restores() {
    let mut view = view_with_thinking("weigh the options", "the answer");
    view.set_hide_thinking(true);

    let hidden = view.render_lines(WIDTH).join("\n");
    assert!(
        hidden.contains(HIDDEN_THINKING_LABEL),
        "collapsed label missing: {hidden:?}"
    );
    assert!(
        !hidden.contains("weigh the options"),
        "collapsed thinking must not leak its text: {hidden:?}"
    );
    assert!(hidden.contains("the answer"), "body stays visible");

    // The flag is display-only: the reasoning is still on the item.
    assert_eq!(view.items()[0].thinking, "weigh the options");
    view.set_hide_thinking(false);
    assert!(view
        .render_lines(WIDTH)
        .join("\n")
        .contains("weigh the options"));
}

#[test]
fn whitespace_only_thinking_renders_nothing() {
    let view = view_with_thinking("   \n  ", "the answer");
    let joined = view.render_lines(WIDTH).join("\n");
    assert!(!joined.contains(HIDDEN_THINKING_LABEL));
    assert!(joined.contains("the answer"));
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

#[test]
fn thinking_is_visible_by_default() {
    let mut app = app();
    assert!(
        app.thinking_visible(),
        "hideThinkingBlock defaults to false"
    );

    app.messages_mut().push(MessageItem {
        role: pi_tui::message::Role::Assistant,
        text: "the answer".into(),
        thinking: "weigh the options".into(),
        streaming: false,
        tool_header: None,
        tool_lines: None,
        tool_expanded: None,
    });
    assert!(rendered(&app).contains("weigh the options"));
}

#[test]
fn ctrl_t_toggles_thinking_and_flashes_the_status_hint() {
    let mut app = app();
    app.messages_mut().push(MessageItem {
        role: pi_tui::message::Role::Assistant,
        text: "the answer".into(),
        thinking: "weigh the options".into(),
        streaming: false,
        tool_header: None,
        tool_lines: None,
        tool_expanded: None,
    });

    assert_eq!(app.step(ctrl('t')), StepOutcome::Redraw);
    assert!(!app.thinking_visible(), "Ctrl+T hides thinking");
    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    assert!(snapshot.lines.join("\n").contains(HIDDEN_THINKING_LABEL));
    assert_eq!(
        snapshot.status.hint.as_deref(),
        Some("Thinking blocks: hidden")
    );
    assert_eq!(app.status_flash(), Some("Thinking blocks: hidden"));

    assert_eq!(app.step(ctrl('t')), StepOutcome::Redraw);
    assert!(app.thinking_visible(), "Ctrl+T again shows thinking");
    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    assert!(snapshot.lines.join("\n").contains("weigh the options"));
    assert_eq!(
        snapshot.status.hint.as_deref(),
        Some("Thinking blocks: visible")
    );
}

#[test]
fn next_key_press_clears_the_status_flash() {
    let mut app = app();
    assert_eq!(app.step(ctrl('t')), StepOutcome::Redraw);
    assert!(app.status_flash().is_some());

    // Any key press retires the transient message; the persistent help hint
    // is back on the following frame.
    app.step(InputEvent::Key(Key::new(
        KeyCode::Char('x'),
        KeyModifiers::NONE,
    )));
    assert_eq!(app.status_flash(), None);
    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    assert_eq!(snapshot.status.hint.as_deref(), Some("? for help"));
}

#[test]
fn clear_keeps_the_thinking_switch() {
    let mut app = app();
    app.step(ctrl('t'));
    assert!(!app.thinking_visible());

    assert_eq!(app.step(ctrl('l')), StepOutcome::Redraw);
    assert!(app.messages().is_empty(), "Ctrl+L clears the transcript");
    assert!(
        !app.thinking_visible(),
        "Ctrl+L must not reset the thinking switch"
    );
}

#[test]
fn apply_agent_event_routes_a_thinking_delta_into_the_view() {
    let mut app = app();
    app.apply_agent_event(AgentEvent::MessageStart { model: "m".into() });
    app.apply_agent_event(AgentEvent::MessageUpdate(
        AssistantMessageUpdate::ThinkingDelta {
            delta: "weigh ".into(),
        },
    ));
    app.apply_agent_event(AgentEvent::MessageUpdate(
        AssistantMessageUpdate::ThinkingDelta {
            delta: "the options".into(),
        },
    ));
    app.apply_agent_event(AgentEvent::MessageUpdate(
        AssistantMessageUpdate::TextDelta {
            delta: "the answer".into(),
        },
    ));

    assert_eq!(app.messages().len(), 1, "thinking joins the message item");
    assert_eq!(app.messages().items()[0].thinking, "weigh the options");
    assert!(rendered(&app).contains("weigh the options"));
}
