//! End-to-end tests that drive the full App / Agent stack with the
//! faux provider. They assert the App subscribes to `AgentEvent`s,
//! the App renders the resulting messages, and slash commands short
//! circuit before reaching the agent.

use std::sync::Arc;
use std::time::Duration;

use pi_agent_core::{Agent, AgentEvent, AgentOptions, AssistantMessageUpdate};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId, Role, StopReason, Usage};
use pi_tui::app::{App, AppConfig};
use pi_tui::input::{InputEvent, KeyCode, KeyModifiers};
use tokio::sync::Mutex as AsyncMutex;

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
    let config = AppConfig {
        session_id: "test".into(),
        ..AppConfig::default()
    };
    App::new(&agent, config)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submit_then_drain_renders_assistant_message() {
    let agent = Arc::new(AsyncMutex::new(Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ))));
    let config = AppConfig {
        session_id: "e2e".into(),
        ..AppConfig::default()
    };
    let mut app = App::new(&*agent.lock().await, config);

    app.submit(agent.clone(), "hello".to_string());

    // Wait for the spawned task to finish + emit the events.
    for _ in 0..200 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        app.drain_agent_events();
        if app.messages().len() >= 2 && !app.is_busy() {
            break;
        }
    }

    let items = app.messages().items().to_vec();
    // First item: the user prompt we submitted.
    // Second item: the assistant text from the faux provider.
    assert!(
        items.len() >= 2,
        "expected at least 2 items, got {:?}",
        items.iter().map(|i| (&i.role, &i.text)).collect::<Vec<_>>()
    );
    assert!(items[0].text.contains("hello"));
    let last = items.last().expect("at least one message");
    assert!(
        last.text.contains("(faux) hello"),
        "expected faux reply, got {:?}",
        last.text
    );

    // Render a snapshot and verify the rendered lines include the
    // user prompt and the assistant reply.
    let snapshot = app.render_snapshot(60, 12);
    let joined = snapshot.lines.join("\n");
    assert!(joined.contains("hello"));
    assert!(joined.contains("(faux) hello"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slash_command_does_not_reach_agent() {
    let agent = Arc::new(AsyncMutex::new(Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ))));
    let config = AppConfig {
        session_id: "slash".into(),
        ..AppConfig::default()
    };
    let mut app = App::new(&*agent.lock().await, config);

    // Type "/help" then submit.
    for c in "/help".chars() {
        app.step(InputEvent::Key(pi_tui::input::Key::char(c)));
    }
    let outcome = app.step(InputEvent::Key(pi_tui::input::Key::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    match outcome {
        pi_tui::app::StepOutcome::Submitted(text) => assert_eq!(text, "/help"),
        other => panic!("expected Submitted, got {other:?}"),
    }

    // The slash command dispatcher lives in pi-coding-agent; here we
    // just verify the App handed off the text instead of running an
    // agent turn.
    assert!(!app.is_busy());
    // The user message did not make it into the message view either
    // — the App only adds user messages when `submit()` is invoked.
    assert_eq!(app.messages().len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_short_circuits_via_ctrl_d_on_empty_buffer() {
    let mut app = app();
    let outcome = app.step(InputEvent::Key(pi_tui::input::Key::new(
        KeyCode::Char('d'),
        KeyModifiers::CONTROL,
    )));
    assert_eq!(outcome, pi_tui::app::StepOutcome::Exit);
    assert!(app.is_exit_requested());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_c_on_idle_exits() {
    let mut app = app();
    let outcome = app.step(InputEvent::Key(pi_tui::input::Key::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    )));
    assert_eq!(outcome, pi_tui::app::StepOutcome::Exit);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_subscriber_receives_text_delta_and_turn_end() {
    // Direct subscriber contract: the Agent emits the full event
    // sequence the TUI consumes.
    let mut agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut rx = agent.subscribe();
    agent.prompt("hello").await.expect("prompt");
    let mut saw_text_delta = false;
    let mut saw_turn_end = false;
    let mut collected = Vec::new();
    while let Ok(event) = rx.try_recv() {
        collected.push(event.clone());
        match event {
            AgentEvent::MessageUpdate(AssistantMessageUpdate::TextDelta { delta }) => {
                if delta.contains("(faux) hello") {
                    saw_text_delta = true;
                }
            }
            AgentEvent::TurnEnd { message, .. } => {
                assert_eq!(message.stop_reason, StopReason::Stop);
                saw_turn_end = true;
            }
            _ => {}
        }
    }
    assert!(
        saw_text_delta,
        "expected a text delta containing the faux reply (events: {:?})",
        collected
    );
    assert!(saw_turn_end, "expected TurnEnd event");
    // The faux provider runs a single turn with no tool calls, so the
    // last message in the agent log is the assistant message (the
    // user prompt is pushed first, the assistant reply second).
    let last_role = agent.state().messages.last().map(|m| m.role);
    assert_eq!(last_role, Some(Role::Assistant));
    let usage = Usage::default();
    let _ = usage;
}
