//! Smoke test for `pi-agent-core` Stage 0 surface.

use std::sync::Arc;

use pi_agent_core::agent::Agent;
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Model, ProviderId};

#[tokio::test]
async fn agent_prompt_enqueues_message() {
    let model = Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: pi_protocol::Api::Faux,
        label: None,
        context_window: 1024,
        max_output_tokens: 256,
    };

    let mut agent = Agent::new(model, Arc::new(FauxProvider), "you are pi");
    agent.prompt("hello").await.expect("prompt enqueue");
}
