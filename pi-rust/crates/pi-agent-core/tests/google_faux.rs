//! Google-faux style integration test for `pi-agent-core`.
//!
//! Mirrors `anthropic_faux.rs`: an agent driven by a Google Gemini
//! shaped stream (SSE `data:` frames carrying
//! `candidates[].content.parts[]`, `usageMetadata`, `finishReason`)
//! must complete a full prompt → assistant turn cycle without leaving
//! anything on the wire. Recorded fixtures are replayed through
//! `pi_ai::providers::google::parse_sse` behind a custom `StreamFn`.
//!
//! This file is a *test* addition only — it does not touch the public
//! surface of `pi-agent-core`.

use std::sync::Arc;

use async_trait::async_trait;
use futures::{stream, StreamExt};
use pi_agent_core::{Agent, AgentHookAdapter, AgentOptions};
use pi_ai::providers::google::parse_sse;
use pi_ai::StreamFn;
use pi_protocol::{
    AssistantMessageEvent, Content, Context, Message, Model, ProviderId, Role, StopReason,
    ToolDefinition, Usage,
};
use serde_json::json;
use tokio_util::bytes::Bytes;

/// Google-faux `StreamFn` that pulls from a recorded fixture without
/// going over the network.
struct GoogleFixtureStreamFn {
    fixture: Vec<u8>,
    model_id: String,
}

impl GoogleFixtureStreamFn {
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
impl StreamFn for GoogleFixtureStreamFn {
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
        .join("google")
}

fn gemini_model() -> Model {
    Model {
        provider: ProviderId::new("google"),
        id: "gemini-2.5-flash".into(),
        api: pi_protocol::Api::GoogleGenerativeAi,
        label: None,
        context_window: 1_048_576,
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

async fn drain(provider: &GoogleFixtureStreamFn, ctx: &Context) -> Vec<AssistantMessageEvent> {
    let mut stream = provider
        .stream_simple(&gemini_model(), ctx, &Default::default())
        .await
        .expect("stream opens");
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.expect("event ok"));
    }
    events
}

/// A single agent turn with the text fixture must land in the agent's
/// message log — the same prompt → assistant_message handoff the
/// Anthropic adapter exercises.
#[tokio::test]
async fn google_faux_text_turn_round_trip() {
    let fixture = fixtures_root().join("text_response.sse");
    let provider = Arc::new(GoogleFixtureStreamFn::new(&fixture, "gemini-2.5-flash"));

    let mut agent = Agent::new(AgentOptions::new(gemini_model(), provider, "you are pi"));
    agent
        .prompt("hi")
        .await
        .expect("prompt resolves with text fixture");

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
    assert_eq!(last_text, "Hello there!");
}

/// The trailing `Done` event carries the wire contract
/// `pi-agent-core` forwards to subscribers: `StopReason::Stop` plus the
/// Google usage numbers (prompt / candidates / total).
#[tokio::test]
async fn google_faux_done_event_shape() {
    let fixture = fixtures_root().join("text_response.sse");
    let provider = GoogleFixtureStreamFn::new(&fixture, "gemini-2.5-flash");
    let events = drain(&provider, &weather_context()).await;

    let (stop_reason, content, usage) = events
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
    assert_eq!(stop_reason, StopReason::Stop);
    let text: String = content
        .iter()
        .filter_map(|c| match c {
            Content::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello there!");
    assert_eq!(usage.input, 12);
    assert_eq!(usage.output, 3);
    assert_eq!(usage.total, 15);
}

/// `thoughtsTokenCount` folds into `Usage::output` while the thought
/// text itself is delivered only as `ThinkingDelta` (the protocol has no
/// stored thinking block yet).
#[tokio::test]
async fn google_faux_thinking_and_usage() {
    let fixture = fixtures_root().join("thinking_response.sse");
    let provider = GoogleFixtureStreamFn::new(&fixture, "gemini-2.5-flash");
    let events = drain(&provider, &weather_context()).await;

    let thinking: String = events
        .iter()
        .filter_map(|e| match e {
            AssistantMessageEvent::ThinkingDelta { delta } => Some(delta.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(thinking, "Let me reason about this.");

    let (content, usage) = events
        .iter()
        .find_map(|e| match e {
            AssistantMessageEvent::Done { content, usage, .. } => Some((content.clone(), *usage)),
            _ => None,
        })
        .expect("done event");
    assert_eq!(usage.output, 17);
    let text: String = content
        .iter()
        .filter_map(|c| match c {
            Content::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "The answer is 4.");
}

/// Cache reads survive the parser's `Done` event: `input` is the
/// uncached prompt portion and `cache_read` carries the cached count.
#[tokio::test]
async fn google_faux_cache_fields_land_in_done() {
    let fixture = fixtures_root().join("cache_read.sse");
    let provider = GoogleFixtureStreamFn::new(&fixture, "gemini-2.5-flash");
    let events = drain(&provider, &weather_context()).await;

    let usage: Usage = events
        .iter()
        .find_map(|e| match e {
            AssistantMessageEvent::Done { usage, .. } => Some(*usage),
            _ => None,
        })
        .expect("done event");
    assert_eq!(usage.input, 100);
    assert_eq!(usage.cache_read, 900);
    assert_eq!(usage.cache_write, 0);
}

/// The tool-use fixture must land a `functionCall` as a
/// `Content::ToolCall` in the assistant message. The loop is stopped
/// after one turn, otherwise the replayed fixture would never end.
#[tokio::test]
async fn google_faux_tool_call_lands_in_assistant_message() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;

    let fixture = fixtures_root().join("tool_use_response.sse");
    let provider = Arc::new(GoogleFixtureStreamFn::new(&fixture, "gemini-2.5-flash"));

    let stop_counter = StdArc::new(AtomicUsize::new(0));
    let stop_counter_clone = stop_counter.clone();
    let hooks = AgentHookAdapter::default().with_should_stop_after_turn(StdArc::new(move |_| {
        let counter = stop_counter_clone.clone();
        Box::pin(async move { counter.fetch_add(1, Ordering::SeqCst) >= 1 })
    }));

    let mut agent = Agent::new(AgentOptions::new(gemini_model(), provider, "you are pi"));
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
    assert_eq!(tool_call.id, "call_test_01");
    assert_eq!(tool_call.arguments["city"], "San Francisco");
}
