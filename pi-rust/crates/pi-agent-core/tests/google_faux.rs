//! Google-Gemini-faux-style integration test for `pi-agent-core`.
//!
//! Mirrors `anthropic_faux.rs`: it drives the real agent loop with a
//! Google-shaped `StreamFn` that replays recorded Gemini fixtures
//! through `parse_sse` — no network involved. The tool-call fixture is
//! followed by a text fixture on the second call, so the loop runs the
//! full prompt → assistant tool call → tool result → **second request**
//! → assistant text cycle. The second request's `Context` is captured
//! and converted with `GoogleProvider::build_request` to prove the
//! tool result reached the wire as a `functionResponse`.
//!
//! This file is a test addition only — it does not touch the public
//! surface of `pi-agent-core`, only its fixtures/inputs.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::{stream, StreamExt};
use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::google::{parse_sse, GoogleProvider};
use pi_ai::StreamFn;
use pi_protocol::{
    Api, AssistantMessageEvent, Content, Context, Message, Model, ProviderId, Role, StopReason,
    ToolDefinition, Usage,
};
use serde_json::json;
use tokio_util::bytes::Bytes;

/// Gemini-faux `StreamFn` that replays a scripted list of fixture byte
/// streams — one per `stream_simple` call — and records every `Context`
/// it was handed.
struct GoogleFixtureStreamFn {
    fixtures: Vec<Vec<u8>>,
    call_count: AtomicUsize,
    seen_contexts: Mutex<Vec<Context>>,
}

impl GoogleFixtureStreamFn {
    fn new(fixtures: &[&std::path::Path]) -> Self {
        let fixtures = fixtures
            .iter()
            .map(|p| std::fs::read(p).unwrap_or_else(|e| panic!("read fixture {p:?}: {e}")))
            .collect();
        Self {
            fixtures,
            call_count: AtomicUsize::new(0),
            seen_contexts: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }

    fn context_for(&self, call: usize) -> Context {
        self.seen_contexts.lock().unwrap()[call].clone()
    }
}

#[async_trait]
impl StreamFn for GoogleFixtureStreamFn {
    async fn stream_simple(
        &self,
        _model: &Model,
        ctx: &Context,
        _options: &pi_ai::SimpleStreamOptions,
    ) -> Result<pi_ai::AssistantMessageEventStream, pi_ai::StreamError> {
        let call = self.call_count.fetch_add(1, Ordering::SeqCst);
        self.seen_contexts.lock().unwrap().push(ctx.clone());
        let fixture = self
            .fixtures
            .get(call)
            // Past the script, replay the final fixture so a runaway
            // loop still terminates instead of panicking.
            .or_else(|| self.fixtures.last())
            .cloned()
            .unwrap_or_default();
        let chunks = stream::iter(vec![Ok::<Bytes, pi_ai::StreamError>(Bytes::from(
            fixture,
        ))]);
        Ok(parse_sse(Box::pin(chunks), "gemini-2.5-flash".to_string()))
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
        api: Api::GoogleGenerativeAi,
        label: Some("Gemini 2.5 Flash".into()),
        context_window: 1_048_576,
        max_output_tokens: 65_536,
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

/// Run a single agent turn with the text fixture. The fixture ends in
/// `STOP` with no tool calls, so the loop exits after one turn.
#[tokio::test]
async fn google_faux_text_turn_round_trip() {
    let fixture = fixtures_root().join("text_response.sse");
    let provider = Arc::new(GoogleFixtureStreamFn::new(&[&fixture]));

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

/// The `Done` event shape `pi-agent-core` forwards to subscribers:
/// `StopReason::Stop` plus the Gemini usage numbers.
#[tokio::test]
async fn google_faux_stream_done_event_shape() {
    let fixture = fixtures_root().join("text_response.sse");
    let provider = Arc::new(GoogleFixtureStreamFn::new(&[&fixture]));

    let mut stream = provider
        .stream_simple(&gemini_model(), &weather_context(), &Default::default())
        .await
        .expect("stream opens");
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.expect("event ok"));
    }

    let (stop_reason, content, usage): (StopReason, Vec<Content>, Usage) = events
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

/// Full two-turn loop: the tool-call fixture drives the executor, then
/// the loop issues a **second request** with the tool result in the
/// context; the text fixture answers it and the loop exits cleanly.
#[tokio::test]
async fn google_faux_tool_call_then_second_request() {
    let tool_fixture = fixtures_root().join("function_call_response.sse");
    let text_fixture = fixtures_root().join("text_response.sse");
    let provider = Arc::new(GoogleFixtureStreamFn::new(&[&tool_fixture, &text_fixture]));

    let mut agent = Agent::new(AgentOptions::new(gemini_model(), provider.clone(), "you are pi"));
    agent
        .prompt("what's the weather in SF?")
        .await
        .expect("agent loop completes after the second turn");

    assert_eq!(
        provider.calls(),
        2,
        "agent must issue a second request after the tool result"
    );

    // Turn 1 produced a tool call and the stub executor appended the
    // matching tool result.
    let tool_call = agent
        .state()
        .messages
        .iter()
        .find_map(|m| match m.role {
            Role::Assistant => m.content.iter().find_map(|c| match c {
                Content::ToolCall(t) => Some(t.clone()),
                _ => None,
            }),
            _ => None,
        })
        .expect("tool call landed in the assistant message");
    assert_eq!(tool_call.name, "get_weather");
    assert_eq!(tool_call.arguments["city"], "San Francisco");

    let tool_messages: Vec<&Message> = agent
        .state()
        .messages
        .iter()
        .filter(|m| matches!(m.role, Role::Tool))
        .collect();
    assert_eq!(tool_messages.len(), 1, "stub executor appended one result");

    // Turn 2's context must carry the tool result back to Gemini as a
    // `functionResponse`, with the function name resolved from the
    // earlier assistant tool call.
    let second_ctx = provider.context_for(1);
    assert!(
        second_ctx
            .messages
            .iter()
            .any(|m| matches!(m.role, Role::Tool)),
        "second request context must include the tool result message"
    );
    let req = GoogleProvider::build_request(&gemini_model(), &second_ctx, &Default::default())
        .expect("second request builds");
    let v = serde_json::to_value(&req).expect("serialize second request");
    let fr = v["contents"]
        .as_array()
        .expect("contents array")
        .iter()
        .find_map(|turn| {
            turn["parts"]
                .as_array()?
                .iter()
                .find(|p| p.get("functionResponse").is_some())
        })
        .expect("functionResponse part in the second request");
    assert_eq!(fr["functionResponse"]["name"], "get_weather");
    assert_eq!(
        fr["functionResponse"]["response"]["output"],
        "(stub) executed get_weather"
    );

    // The final assistant turn is the text fixture's answer.
    let last_text: String = agent
        .state()
        .messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, Role::Assistant))
        .map(|m| {
            m.content
                .iter()
                .filter_map(|c| match c {
                    Content::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(last_text, "Hello there!");
}

/// A provider error event must be surfaced to the caller as a stream
/// error rather than silently producing an empty assistant turn.
#[tokio::test]
async fn google_faux_error_event_propagates() {
    let fixture = fixtures_root().join("error_event.sse");
    let provider = Arc::new(GoogleFixtureStreamFn::new(&[&fixture]));

    let mut stream = provider
        .stream_simple(&gemini_model(), &weather_context(), &Default::default())
        .await
        .expect("stream opens");
    let mut saw_error = false;
    while let Some(ev) = stream.next().await {
        if matches!(ev, Err(pi_ai::StreamError::Malformed(_))) {
            saw_error = true;
        }
    }
    assert!(saw_error, "error event must propagate as StreamError");
}
