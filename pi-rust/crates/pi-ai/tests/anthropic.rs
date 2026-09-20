//! End-to-end integration tests for `AnthropicProvider`.
//!
//! These tests exercise the full `StreamFn` trait surface (request
//! building + SSE parsing + `AssistantMessage` accumulation) against
//! recorded Anthropic fixture streams. They run offline — no real
//! HTTP calls.
//!
//! The flow mirrors what `pi-agent-core` consumes on each turn:
//!
//! 1. Build the request body from a `Context`.
//! 2. Stream a recorded fixture through `parse_sse`.
//! 3. Drive a `StreamFn` impl that reads from the fixture (no
//!    `reqwest` round-trip) and forwards events to the test harness.
//! 4. Assert the resulting `AssistantMessage` field-by-field against
//!    the TS port's expectations.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{stream, StreamExt};
use pi_ai::providers::anthropic::{
    builtin_claude_models, catalog_from_json, AnthropicCatalog, AnthropicModelEntry,
    AnthropicProvider, MessagesRequest, ANTHROPIC_VERSION, DEFAULT_BASE_URL,
};
use pi_ai::StreamFn;
use pi_protocol::{
    AssistantMessageEvent, Content, Context, Message, Model, ProviderId, Role, StopReason,
    ToolDefinition, Usage,
};
use serde_json::json;

/// Test stream function that replays a recorded SSE byte stream
/// through `AnthropicProvider::parse_sse` — no HTTP involved.
struct FixtureStreamFn {
    fixture: Vec<u8>,
    model_id: String,
}

impl FixtureStreamFn {
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
impl StreamFn for FixtureStreamFn {
    async fn stream_simple(
        &self,
        _model: &Model,
        _ctx: &Context,
        _options: &pi_ai::SimpleStreamOptions,
    ) -> Result<pi_ai::AssistantMessageEventStream, pi_ai::StreamError> {
        let model_id = self.model_id.clone();
        let bytes = self.fixture.clone();
        let chunks = stream::iter(vec![Ok::<Bytes, pi_ai::StreamError>(Bytes::from(bytes))]);
        Ok(pi_ai::providers::anthropic::parse_sse(
            Box::pin(chunks),
            model_id,
        ))
    }
}

fn fixtures_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/anthropic")
}

fn claude_model() -> Model {
    Model {
        provider: ProviderId::new("anthropic"),
        id: "claude-haiku-4-5".into(),
        api: pi_protocol::Api::AnthropicMessages,
        label: Some("Claude Haiku 4.5".into()),
        context_window: 200_000,
        max_output_tokens: 8_192,
    }
}

fn user_context(question: &str) -> Context {
    let mut ctx = Context::new("you are pi");
    ctx.messages.push(Message {
        role: Role::User,
        content: vec![Content::text(question)],
        model: None,
    });
    ctx
}

/// Replay `text_response.sse` end-to-end and assert the resulting
/// `AssistantMessage` matches the TS port's expectations.
#[tokio::test]
async fn text_response_round_trip() {
    let fixture = fixtures_root().join("text_response.sse");
    let provider = Arc::new(FixtureStreamFn::new(&fixture, "claude-haiku-4-5"));

    let mut events: Vec<AssistantMessageEvent> = Vec::new();
    let mut stream = provider
        .stream_simple(&claude_model(), &user_context("hi"), &Default::default())
        .await
        .expect("stream opens");
    while let Some(ev) = stream.next().await {
        events.push(ev.expect("event ok"));
    }

    // First event is Start; last is Done.
    assert!(matches!(events[0], AssistantMessageEvent::Start { .. }));
    let done = events
        .iter()
        .find_map(|e| match e {
            AssistantMessageEvent::Done {
                content,
                stop_reason,
                usage,
            } => Some((content.clone(), *stop_reason, *usage)),
            _ => None,
        })
        .expect("done event");
    let (content, stop_reason, usage) = done;

    assert_eq!(stop_reason, StopReason::Stop);
    let text: String = content
        .iter()
        .filter_map(|c| match c {
            Content::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello there");
    assert_eq!(usage.input, 17);
    assert_eq!(usage.output, 4);
}

/// Replay `tool_use_response.sse` and assert the tool call arguments
/// land in the final `AssistantMessage`.
#[tokio::test]
async fn tool_use_response_round_trip() {
    let fixture = fixtures_root().join("tool_use_response.sse");
    let provider = Arc::new(FixtureStreamFn::new(&fixture, "claude-haiku-4-5"));

    let mut ctx = user_context("what's the weather in SF?");
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

    let mut stream = provider
        .stream_simple(&claude_model(), &ctx, &Default::default())
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
                content,
                stop_reason,
                ..
            } => Some((content.clone(), *stop_reason)),
            _ => None,
        })
        .expect("done event");
    let (content, stop_reason) = done;

    assert_eq!(stop_reason, StopReason::ToolUse);
    let tc = content
        .iter()
        .find_map(|c| match c {
            Content::ToolCall(t) => Some(t),
            _ => None,
        })
        .expect("tool call in done");
    assert_eq!(tc.name, "get_weather");
    assert_eq!(tc.id, "toolu_test_01");
    assert_eq!(tc.arguments["city"], "San Francisco");
}

/// A frame whose string literal holds a raw control character (or a stray
/// backslash) is repaired instead of aborting the stream — upstream's
/// `parseJsonWithRepair`.
#[tokio::test]
async fn malformed_string_literals_are_repaired() {
    let fixture = fixtures_root().join("repair_required.sse");
    let provider = Arc::new(FixtureStreamFn::new(&fixture, "claude-haiku-4-5"));

    let mut stream = provider
        .stream_simple(&claude_model(), &user_context("hi"), &Default::default())
        .await
        .expect("stream opens");
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.expect("event ok"));
    }

    let (content, stop_reason) = events
        .iter()
        .find_map(|e| match e {
            AssistantMessageEvent::Done {
                content,
                stop_reason,
                ..
            } => Some((content.clone(), *stop_reason)),
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
    assert_eq!(text, "raw\ttabpath C:\\Users");
}

/// Tool-call arguments accumulated from a malformed `partial_json` payload are
/// recovered by `parseStreamingJson` rather than surfacing as a raw string.
#[tokio::test]
async fn malformed_tool_arguments_are_repaired() {
    let fixture = fixtures_root().join("tool_use_repair_required.sse");
    let provider = Arc::new(FixtureStreamFn::new(&fixture, "claude-haiku-4-5"));

    let mut stream = provider
        .stream_simple(
            &claude_model(),
            &user_context("weather?"),
            &Default::default(),
        )
        .await
        .expect("stream opens");
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.expect("event ok"));
    }

    let content = events
        .iter()
        .find_map(|e| match e {
            AssistantMessageEvent::Done { content, .. } => Some(content.clone()),
            _ => None,
        })
        .expect("done event");
    let tc = content
        .iter()
        .find_map(|c| match c {
            Content::ToolCall(t) => Some(t),
            _ => None,
        })
        .expect("tool call in done");
    assert_eq!(tc.name, "get_weather");
    assert_eq!(tc.arguments["city"], "San\u{1}Francisco");
}

/// Replay `cache_read_response.sse` and assert the cache fields
/// populate the `Usage` payload on the trailing `Done`.
#[tokio::test]
async fn cache_read_response_round_trip() {
    let fixture = fixtures_root().join("cache_read_response.sse");
    let provider = Arc::new(FixtureStreamFn::new(&fixture, "claude-haiku-4-5"));

    let mut stream = provider
        .stream_simple(
            &claude_model(),
            &user_context("cached prompt"),
            &Default::default(),
        )
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
    assert_eq!(usage.input, 5);
    assert_eq!(usage.output, 3);
}

/// Replay `error_event.sse` and assert the parser surfaces the
/// upstream error as a `StreamError::Malformed`.
#[tokio::test]
async fn upstream_error_surfaces_as_stream_error() {
    let fixture = fixtures_root().join("error_event.sse");
    let provider = Arc::new(FixtureStreamFn::new(&fixture, "claude-haiku-4-5"));

    let mut stream = provider
        .stream_simple(&claude_model(), &user_context("hello"), &Default::default())
        .await
        .expect("stream opens");
    let mut saw_error = false;
    while let Some(ev) = stream.next().await {
        if let Err(pi_ai::StreamError::Malformed(msg)) = ev {
            if msg.contains("overloaded") {
                saw_error = true;
            }
        }
    }
    assert!(
        saw_error,
        "expected the parser to surface the upstream error"
    );
}

/// Exercise the `AnthropicProvider::build_request` builder directly,
/// asserting the wire payload matches the TS port's serialisation.
#[test]
fn request_body_carries_system_and_tools() {
    let mut ctx = Context::new("be terse");
    ctx.messages.push(Message {
        role: Role::User,
        content: vec![Content::text("hi")],
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

    let req: MessagesRequest = AnthropicProvider::build_request(
        &claude_model(),
        &ctx,
        &pi_ai::SimpleStreamOptions {
            temperature: Some(0.5),
            max_tokens: Some(256),
            ..Default::default()
        },
    )
    .expect("build request");

    let v = serde_json::to_value(&req).expect("serialize");
    assert_eq!(v["model"], "claude-haiku-4-5");
    assert_eq!(v["system"], "be terse");
    assert_eq!(v["max_tokens"], 256);
    assert!((v["temperature"].as_f64().unwrap() - 0.5).abs() < 1e-6);
    assert_eq!(v["stream"], true);
    let tools = v["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "get_weather");
    assert_eq!(tools[0]["input_schema"]["type"], "object");
    assert_eq!(
        tools[0]["input_schema"]["required"][0], "city",
        "schema carries through unchanged"
    );
}

/// The constant surface should not change without bumping the
/// spec'd `anthropic-version` header.
#[test]
fn version_constants_pin_upstream() {
    assert_eq!(DEFAULT_BASE_URL, "https://api.anthropic.com");
    assert_eq!(ANTHROPIC_VERSION, "2023-06-15");
}

/// The built-in Claude catalog must contain at least three models
/// and they must all advertise the `AnthropicMessages` API.
#[test]
fn builtin_catalog_ships_three_claude_models() {
    let models = builtin_claude_models();
    assert!(models.len() >= 3, "got {}", models.len());
    for m in &models {
        assert_eq!(m.provider.0, "anthropic");
        assert_eq!(m.api, pi_protocol::Api::AnthropicMessages);
        assert!(m.context_window > 0);
        assert!(m.max_output_tokens > 0);
    }
    // Spot-check the model ids the spec calls out by name.
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert!(
        ids.contains(&"claude-haiku-4-5"),
        "claude-haiku-4-5 missing from {ids:?}"
    );
}

/// `catalog_from_json` round-trips an `AnthropicCatalog` into the
/// in-memory `Model` form `Models::set_provider` expects.
#[test]
fn catalog_from_json_round_trip() {
    let catalog = AnthropicCatalog {
        provider: "anthropic".into(),
        models: vec![AnthropicModelEntry {
            id: "claude-haiku-4-5".into(),
            label: Some("Claude Haiku 4.5".into()),
            context_window: 200_000,
            max_output_tokens: 8_192,
            reasoning: false,
        }],
    };
    let json = serde_json::to_string(&catalog).expect("serialize");
    let models = catalog_from_json(&json).expect("parse");
    assert_eq!(models.len(), 1);
    let m = &models[0];
    assert_eq!(m.provider.0, "anthropic");
    assert_eq!(m.api, pi_protocol::Api::AnthropicMessages);
    assert_eq!(m.context_window, 200_000);
    assert_eq!(m.label.as_deref(), Some("Claude Haiku 4.5"));
}

/// `AnthropicProvider::with_base_url` should swap the endpoint and
/// preserve the api key.
#[test]
fn with_base_url_overrides_endpoint() {
    let provider = AnthropicProvider::with_base_url("sk-test", "https://example.test");
    assert_eq!(provider.base_url, "https://example.test");
    assert_eq!(provider.api_key, "sk-test");
    assert_eq!(provider.api_version, ANTHROPIC_VERSION);
    let custom = AnthropicProvider::new("sk-test").with_version("2024-01-01");
    assert_eq!(custom.api_version, "2024-01-01");
}

/// Round-trip the request body through serde — guards against future
/// refactors breaking the wire shape.
#[test]
fn request_body_round_trip_through_json() {
    let mut ctx = Context::new("");
    ctx.messages.push(Message {
        role: Role::User,
        content: vec![Content::text("hello")],
        model: None,
    });
    let req = AnthropicProvider::build_request(
        &claude_model(),
        &ctx,
        &pi_ai::SimpleStreamOptions::default(),
    )
    .expect("build request");
    let value = serde_json::to_value(&req).expect("serialize");
    assert_eq!(value["model"], "claude-haiku-4-5");
    assert_eq!(value["stream"], true);
    // `system` is skipped when empty so the request body stays minimal.
    assert!(value.get("system").is_none() || value["system"].is_null());
    assert!(value["max_tokens"].is_number());
    let messages = value["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
}
