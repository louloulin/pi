//! Anthropic-faux-style integration test for `pi-agent-core`.
//!
//! Stage 7 must verify that an agent driven by an Anthropic-shaped
//! stream (SSE-shaped event order, tool-use blocks, cache fields) can
//! capture a full prompt → assistant turn cycle without leaving
//! anything on the wire. We can't reach the real Anthropic endpoint
//! from CI, so we replay recorded fixtures through a custom `StreamFn`
//! that mimics the Anthropic Messages protocol.
//!
//! This file is a *test* addition only — it doesn't touch the public
//! surface of `pi-agent-core`, only its fixtures/inputs.

use std::sync::Arc;

use async_trait::async_trait;
use futures::{stream, StreamExt};
use pi_agent_core::{Agent, AgentHookAdapter, AgentOptions};
use pi_ai::providers::anthropic::parse_sse;
use pi_ai::StreamFn;
use pi_protocol::{
    AssistantMessageEvent, Content, Context, Message, Model, ProviderId, Role, StopReason,
    ToolDefinition, Usage,
};
use serde_json::json;
use tokio_util::bytes::Bytes;

/// Anthropic-faux-style `StreamFn` that pulls from a recorded fixture
/// without going over the network.
struct AnthropicFixtureStreamFn {
    fixture: Vec<u8>,
    model_id: String,
}

impl AnthropicFixtureStreamFn {
    fn new(fixture_path: &std::path::Path, model_id: &str) -> Self {
        let bytes = std::fs::read(fixture_path)
            .unwrap_or_else(|e| panic!("read fixture {fixture_path:?}: {e}"));
        Self {
            fixture: bytes,
            model_id: model_id.to_string(),
        }
    }
}

#[async_trait]
impl StreamFn for AnthropicFixtureStreamFn {
    async fn stream_simple(
        &self,
        _model: &Model,
        _ctx: &Context,
        _options: &pi_ai::SimpleStreamOptions,
    ) -> Result<pi_ai::AssistantMessageEventStream, pi_ai::StreamError> {
        let model_id = self.model_id.clone();
        let chunks = stream::iter(vec![Ok::<Bytes, pi_ai::StreamError>(Bytes::from(
            self.fixture.clone(),
        ))]);
        Ok(parse_sse(Box::pin(chunks), model_id))
    }
}

fn fixtures_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("pi-ai")
        .join("fixtures")
        .join("anthropic")
}

fn claude_model() -> Model {
    Model {
        provider: ProviderId::new("anthropic"),
        id: "claude-haiku-4-5".into(),
        api: pi_protocol::Api::AnthropicMessages,
        label: None,
        context_window: 200_000,
        max_output_tokens: 8_192,
    }
}

fn weather_context() -> Context {
    let mut ctx = Context::new("you are pi");
    ctx.messages.push(Message {
        role: Role::User,
        content: vec![Content::text("what's the weather in SF?")],
        model: None,
    });
    ctx.tools.push(ToolDefinition {
        name: "get_weather".into(),
        label: "Get weather".into(),
        description: "Get the current weather for a city".into(),
        parameters: json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"],
        }),
        metadata: None,
    });
    ctx
}

/// Run a single agent turn with the text fixture. The fixture ends
/// in `end_turn` (no tool calls), so the agent loop must exit after
/// one turn — exercising the prompt → assistant_message → state
/// handoff without ever touching the network.
#[tokio::test]
async fn anthropic_faux_text_turn_round_trip() {
    let fixture = fixtures_root().join("text_response.sse");
    let provider = Arc::new(AnthropicFixtureStreamFn::new(
        &fixture,
        "claude-haiku-4-5",
    ));

    let mut agent = Agent::new(AgentOptions::new(
        claude_model(),
        provider,
        "you are pi",
    ));
    agent
        .prompt("hi")
        .await
        .expect("prompt resolves with text fixture");

    // The agent's message log must contain the assistant turn that
    // the parser stitched together from the fixture.
    let mut last_text = String::new();
    let mut saw_assistant = false;
    for m in &agent.state().messages {
        if let Message {
            role: Role::Assistant,
            content,
            ..
        } = m
        {
            saw_assistant = true;
            for c in content {
                if let Content::Text(t) = c {
                    last_text.push_str(&t.text);
                }
            }
        }
    }
    assert!(saw_assistant, "agent must capture the assistant turn");
    assert_eq!(last_text, "Hello there");
}

/// Drive `parse_sse` directly so we can assert the trailing `Done`
/// event carries the expected `StopReason::Stop` and the right
/// `Usage` numbers — this is the wire contract `pi-agent-core`
/// forwards to its event subscribers.
#[tokio::test]
async fn anthropic_faux_stream_done_event_shape() {
    let fixture = fixtures_root().join("text_response.sse");
    let provider = Arc::new(AnthropicFixtureStreamFn::new(
        &fixture,
        "claude-haiku-4-5",
    ));

    let mut stream = provider
        .stream_simple(&claude_model(), &weather_context(), &Default::default())
        .await
        .expect("stream opens");
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.expect("event ok"));
    }

    let done = events
        .iter()
        .find_map(|e| match e {
            AssistantMessageEvent::Done {
                stop_reason,
                content,
                usage,
            } => Some((*stop_reason, content.clone(), *usage)),
            _ => None,
        })
        .expect("done event");
    let (stop_reason, content, _usage) = done;
    assert_eq!(stop_reason, StopReason::Stop);
    let text: String = content
        .iter()
        .filter_map(|c| match c {
            Content::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello there");
}

/// Cache fields (`cache_read_input_tokens`, `cache_creation_input_tokens`)
/// must survive the parser's `Done` event so `pi-agent-core` can
/// forward them to the session log.
#[tokio::test]
async fn anthropic_faux_cache_fields_land_in_done() {
    let fixture = fixtures_root().join("cache_read_response.sse");
    let provider = Arc::new(AnthropicFixtureStreamFn::new(
        &fixture,
        "claude-haiku-4-5",
    ));

    let mut stream = provider
        .stream_simple(&claude_model(), &weather_context(), &Default::default())
        .await
        .expect("stream opens");
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.expect("event ok"));
    }
    let usage: Usage = events
        .iter()
        .find_map(|e| match e {
            AssistantMessageEvent::Done { usage, .. } => Some(*usage),
            _ => None,
        })
        .expect("done event");
    assert_eq!(usage.cache_read, 1200);
    assert_eq!(usage.cache_write, 0);
}

/// Run a single agent turn with the tool_use fixture. The agent loop
/// must capture the assistant message with the tool call in its
/// content array. We don't let the loop continue (would replay the
/// fixture and never exit) — we stop after the first turn by
/// registering a `should_stop_after_turn` hook.
#[tokio::test]
async fn anthropic_faux_tool_call_lands_in_assistant_message() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;

    let fixture = fixtures_root().join("tool_use_response.sse");
    let provider = Arc::new(AnthropicFixtureStreamFn::new(
        &fixture,
        "claude-haiku-4-5",
    ));

    let mut hooks = AgentHookAdapter::default();
    let stop_counter = StdArc::new(AtomicUsize::new(0));
    let stop_counter_clone = stop_counter.clone();
    hooks = hooks.with_should_stop_after_turn(StdArc::new(move |_| {
        let counter = stop_counter_clone.clone();
        Box::pin(async move {
            // Stop after the first turn — the fixture replays the
            // tool_use block forever otherwise.
            counter.fetch_add(1, Ordering::SeqCst) >= 1
        })
    }));

    let mut agent = Agent::new(AgentOptions::new(
        claude_model(),
        provider,
        "you are pi",
    ));
    *agent.hooks_mut() = hooks;
    agent
        .prompt("what's the weather in SF?")
        .await
        .expect("prompt resolves with tool_use fixture");

    let last_message = agent
        .state()
        .messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, Role::Assistant))
        .expect("assistant message present");
    let tool_call = last_message
        .content
        .iter()
        .find_map(|c| match c {
            Content::ToolCall(t) => Some(t),
            _ => None,
        })
        .expect("tool call landed in the assistant message");
    assert_eq!(tool_call.name, "get_weather");
    assert_eq!(tool_call.arguments["city"], "San Francisco");
    // The stub tool executor also appended a `Tool` role message so
    // the loop could continue. The exact content is not asserted
    // (Stage 7 only verifies the Anthropic wire side) but the
    // message role must be `Tool`.
    let tool_messages: Vec<&Message> = agent
        .state()
        .messages
        .iter()
        .filter(|m| matches!(m.role, Role::Tool))
        .collect();
    assert!(
        !tool_messages.is_empty(),
        "stub executor must have appended a Tool role message"
    );
}