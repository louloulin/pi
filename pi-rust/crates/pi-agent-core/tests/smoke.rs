//! Smoke test for `pi-agent-core` Stage 0 surface.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Model, ProviderId};

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: pi_protocol::Api::Faux,
        label: None,
        context_window: 1024,
        max_output_tokens: 256,
    }
}

#[tokio::test]
async fn agent_prompt_enqueues_message() {
    let mut agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider),
        "you are pi",
    ));
    agent.prompt("hello").await.expect("prompt enqueue");
}

#[tokio::test]
async fn agent_new_without_hooks_runs_default_loop() {
    let mut agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider),
        "you are pi",
    ));
    // No hooks registered — the loop must fall back to the defaults
    // (`should_stop_after_turn` returns `false`, `prepare_next_turn`
    // returns `None`) without panicking.
    let outcome = agent
        .loop_mut()
        .run(
            vec![pi_protocol::Message {
                role: pi_protocol::Role::User,
                content: vec![pi_protocol::Content::text("hello")],
                model: None,
            }],
            |_turn| {},
        )
        .await
        .expect("loop runs without hooks");
    assert!(!outcome.tool_executed);
}
