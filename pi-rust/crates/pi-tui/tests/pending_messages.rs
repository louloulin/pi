//! LUM-1216: prompts typed while a turn is streaming must never be
//! dropped. The App queues them (steering first, then follow-up) and the
//! driver delivers them at the next turn boundary; `app.message.dequeue`
//! pulls them back into the editor.
//!
//! These tests run on a single-threaded runtime on purpose: `submit`
//! flips the busy flag synchronously, so the spawned turn does not get to
//! run until the test awaits, which makes "submit while busy"
//! deterministic.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, FollowUpOutcome};
use pi_tui::message::PendingMessageKind;
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

fn new_agent() -> Arc<AsyncMutex<Agent>> {
    Arc::new(AsyncMutex::new(Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ))))
}

async fn new_app(agent: &Arc<AsyncMutex<Agent>>) -> App {
    let config = AppConfig {
        session_id: "pending".into(),
        ..AppConfig::default()
    };
    App::new(&*agent.lock().await, config)
}

#[tokio::test]
async fn busy_submit_queues_instead_of_dropping() {
    let agent = new_agent();
    let mut app = new_app(&agent).await;

    // The first submit starts a turn and marks the App busy right away.
    app.submit(agent.clone(), "first".to_string());
    assert!(app.is_busy(), "a submitted prompt is in flight");
    assert_eq!(app.pending_len(), 0);

    for text in ["second", "third", "fourth"] {
        app.submit(agent.clone(), text.to_string());
    }

    // Nothing was dropped: all three are queued, in the order they were
    // typed, and the render shows them so the reader knows they landed.
    assert_eq!(app.pending_len(), 3);
    let rendered = app.render_snapshot(60, 20).lines.join("\n");
    assert!(rendered.contains("Steering: second"), "{rendered}");
    assert!(rendered.contains("Steering: third"), "{rendered}");
    assert!(rendered.contains("Steering: fourth"), "{rendered}");

    let delivered: Vec<String> = (0..3).filter_map(|_| app.take_next_pending()).collect();
    assert_eq!(delivered, vec!["second", "third", "fourth"]);
    assert_eq!(app.pending_len(), 0);
}

#[tokio::test]
async fn steering_is_delivered_before_follow_up() {
    let agent = new_agent();
    let mut app = new_app(&agent).await;

    app.submit(agent.clone(), "first".to_string());
    assert!(app.is_busy());

    app.submit(agent.clone(), "steer-1".to_string());
    app.set_editor_text("follow-up-1");
    assert_eq!(app.follow_up_from_editor(), FollowUpOutcome::Queued);
    app.submit(agent.clone(), "steer-2".to_string());

    assert_eq!(
        app.messages().pending(),
        vec![
            (PendingMessageKind::Steer, "steer-1"),
            (PendingMessageKind::Steer, "steer-2"),
            (PendingMessageKind::FollowUp, "follow-up-1"),
        ]
    );

    let delivered: Vec<String> = (0..3).filter_map(|_| app.take_next_pending()).collect();
    assert_eq!(delivered, vec!["steer-1", "steer-2", "follow-up-1"]);
}

#[tokio::test]
async fn dequeue_restores_queued_messages_to_the_editor() {
    let agent = new_agent();
    let mut app = new_app(&agent).await;

    app.submit(agent.clone(), "first".to_string());
    app.submit(agent.clone(), "second".to_string());
    app.set_editor_text("follow-up");
    assert_eq!(app.follow_up_from_editor(), FollowUpOutcome::Queued);
    assert_eq!(app.pending_len(), 2);

    // Text already in the buffer is kept, after the restored prompts.
    app.set_editor_text("draft");
    assert_eq!(app.restore_pending_to_editor(), 2);
    assert_eq!(app.editor_text(), "second\n\nfollow-up\n\ndraft");
    assert_eq!(app.pending_len(), 0);
    // Removing the queued lines from the render must not resurrect them.
    let rendered = app.render_snapshot(60, 20).lines.join("\n");
    assert!(!rendered.contains("Steering: second"), "{rendered}");

    // Nothing queued: the editor is left alone and the count is zero.
    app.set_editor_text("");
    assert_eq!(app.restore_pending_to_editor(), 0);
}

#[tokio::test]
async fn idle_follow_up_behaves_like_enter() {
    let agent = new_agent();
    let mut app = new_app(&agent).await;

    assert!(!app.is_busy());
    app.set_editor_text("hello");
    match app.follow_up_from_editor() {
        FollowUpOutcome::Submitted(submission) => assert_eq!(submission.text, "hello"),
        other => panic!("idle followUp must submit like Enter, got {other:?}"),
    }
    // The caller runs the normal Enter path, so the buffer is clear and
    // nothing was queued.
    assert_eq!(app.editor_text(), "");
    assert_eq!(app.pending_len(), 0);

    // An empty buffer is a no-op.
    assert_eq!(app.follow_up_from_editor(), FollowUpOutcome::Empty);
}
