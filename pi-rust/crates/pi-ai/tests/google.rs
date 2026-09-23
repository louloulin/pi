//! End-to-end integration tests for `GoogleProvider`.
//!
//! These tests replay recorded Google Generative Language API SSE
//! fixtures through `GoogleProvider::parse_sse` — no HTTP round-trip.
//! The flow mirrors `tests/anthropic.rs`:
//!
//! 1. Build the request body from a `Context` (asserted separately).
//! 2. Stream a recorded fixture through `parse_sse`.
//! 3. Assert the resulting `AssistantMessageEvent` sequence and the
//!    trailing `Done` payload field-by-field.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{stream, StreamExt};
use pi_ai::providers::google::{builtin_google_models, GoogleProvider, DEFAULT_BASE_URL};
use pi_ai::StreamFn;
use pi_protocol::{
    AssistantMessageEvent, Content, Context, Message, Model, ProviderId, Role, StopReason,
    ToolDefinition, Usage,
};
use serde_json::json;

/// Test stream function that replays a recorded SSE byte stream through
/// `GoogleProvider::parse_sse` — no HTTP involved.
struct FixtureStreamFn {
    fixture: Vec<u8>,
    model_id: String,
    /// When set, the fixture is delivered in chunks of this many bytes so
    /// line buffering across chunk boundaries is exercised.
    chunk_size: Option<usize>,
}

impl FixtureStreamFn {
    fn new(fixture_path: &std::path::Path, model_id: &str) -> Self {
        let bytes = std::fs::read(fixture_path)
            .unwrap_or_else(|e| panic!("read fixture {fixture_path:?}: {e}"));
        Self {
            fixture: bytes,
            model_id: model_id.to_string(),
            chunk_size: None,
        }
    }

    fn chunked(fixture_path: &std::path::Path, model_id: &str, chunk_size: usize) -> Self {
        let mut s = Self::new(fixture_path, model_id);
        s.chunk_size = Some(chunk_size);
        s
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
        let chunks: Vec<Result<Bytes, pi_ai::StreamError>> = match self.chunk_size {
            Some(size) => self
                .fixture
                .chunks(size)
                .map(|c| Ok(Bytes::copy_from_slice(c)))
                .collect(),
            None => vec![Ok(Bytes::from(self.fixture.clone()))],
        };
        Ok(pi_ai::providers::google::parse_sse(
            Box::pin(stream::iter(chunks)),
            model_id,
        ))
    }
}

fn fixtures_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/google")
}

fn gemini_model() -> Model {
    Model {
        provider: ProviderId::new("google"),
        id: "gemini-2.5-flash".into(),
        api: pi_protocol::Api::GoogleGenerativeAi,
        label: Some("Gemini 2.5 Flash".into()),
        context_window: 1_048_576,
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

async fn collect(provider: &FixtureStreamFn, ctx: &Context) -> Vec<AssistantMessageEvent> {
    let mut events = Vec::new();
    let mut stream = provider
        .stream_simple(&gemini_model(), ctx, &Default::default())
        .await
        .expect("stream opens");
    while let Some(ev) = stream.next().await {
        events.push(ev.expect("event ok"));
    }
    events
}

fn done_of(events: &[AssistantMessageEvent]) -> (Vec<Content>, StopReason, Usage) {
    events
        .iter()
        .find_map(|e| match e {
            AssistantMessageEvent::Done {
                content,
                stop_reason,
                usage,
            } => Some((content.clone(), *stop_reason, *usage)),
            _ => None,
        })
        .expect("done event")
}

fn text_of(content: &[Content]) -> String {
    content
        .iter()
        .filter_map(|c| match c {
            Content::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect()
}

/// Replay `text_response.sse` end-to-end.
#[tokio::test]
async fn text_response_round_trip() {
    let fixture = fixtures_root().join("text_response.sse");
    let provider = FixtureStreamFn::new(&fixture, "gemini-2.5-flash");
    let events = collect(&provider, &user_context("hi")).await;

    assert!(matches!(events[0], AssistantMessageEvent::Start { .. }));
    assert!(matches!(
        events.last(),
        Some(AssistantMessageEvent::Done { .. })
    ));

    let (content, stop_reason, usage) = done_of(&events);
    assert_eq!(stop_reason, StopReason::Stop);
    assert_eq!(text_of(&content), "Hello there!");
    assert_eq!(usage.input, 12);
    assert_eq!(usage.output, 3);
    assert_eq!(usage.total, 15);
}

/// The same fixture, delivered one byte at a time — proves the SSE line
/// buffer reassembles frames that straddle chunk boundaries.
#[tokio::test]
async fn text_response_survives_byte_by_byte_delivery() {
    let fixture = fixtures_root().join("text_response.sse");
    let provider = FixtureStreamFn::chunked(&fixture, "gemini-2.5-flash", 1);
    let events = collect(&provider, &user_context("hi")).await;

    let (content, stop_reason, usage) = done_of(&events);
    assert_eq!(stop_reason, StopReason::Stop);
    assert_eq!(text_of(&content), "Hello there!");
    assert_eq!(usage.total, 15);

    // Three text chunks in, three deltas out — chunking must not merge or
    // duplicate them.
    let deltas = events
        .iter()
        .filter(|e| matches!(e, AssistantMessageEvent::TextDelta { .. }))
        .count();
    assert_eq!(deltas, 3);
}

/// Replay `tool_use_response.sse`; the model text and the tool call both
/// land in the final content, and the stop reason flips to `ToolUse`.
#[tokio::test]
async fn tool_use_response_round_trip() {
    let fixture = fixtures_root().join("tool_use_response.sse");
    let provider = FixtureStreamFn::new(&fixture, "gemini-2.5-flash");

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

    let events = collect(&provider, &ctx).await;
    let (content, stop_reason, usage) = done_of(&events);

    assert_eq!(stop_reason, StopReason::ToolUse);
    assert_eq!(text_of(&content), "I'll look that up.");
    let tc = content
        .iter()
        .find_map(|c| match c {
            Content::ToolCall(t) => Some(t),
            _ => None,
        })
        .expect("tool call in done");
    assert_eq!(tc.name, "get_weather");
    assert_eq!(tc.id, "call_test_01");
    assert_eq!(tc.arguments["city"], "San Francisco");
    assert_eq!(usage.total, 60);
}

/// Replay `thinking_response.sse`: thought parts become
/// `ThinkingDelta` events and are not stored in the final content, while
/// `thoughtsTokenCount` folds into `Usage::output`.
#[tokio::test]
async fn thinking_response_is_streamed_but_not_stored() {
    let fixture = fixtures_root().join("thinking_response.sse");
    let provider = FixtureStreamFn::new(&fixture, "gemini-2.5-flash");
    let events = collect(&provider, &user_context("2+2?")).await;

    let thinking: String = events
        .iter()
        .filter_map(|e| match e {
            AssistantMessageEvent::ThinkingDelta { delta } => Some(delta.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(thinking, "Let me reason about this.");

    let (content, stop_reason, usage) = done_of(&events);
    assert_eq!(stop_reason, StopReason::Stop);
    assert_eq!(text_of(&content), "The answer is 4.");
    // output = candidatesTokenCount (7) + thoughtsTokenCount (10)
    assert_eq!(usage.output, 17);
    assert_eq!(usage.total, 22);
}

/// Replay `cache_read.sse`: cache reads are subtracted from the reported
/// prompt count, and the last frame's `MAX_TOKENS` wins the stop reason.
#[tokio::test]
async fn cache_read_and_max_tokens_round_trip() {
    let fixture = fixtures_root().join("cache_read.sse");
    let provider = FixtureStreamFn::new(&fixture, "gemini-2.5-flash");
    let events = collect(&provider, &user_context("cached prompt")).await;

    let (content, stop_reason, usage) = done_of(&events);
    assert_eq!(stop_reason, StopReason::MaxTokens);
    assert_eq!(text_of(&content), "cached reply truncated by max tokens");
    assert_eq!(usage.input, 100);
    assert_eq!(usage.output, 9);
    assert_eq!(usage.cache_read, 900);
    assert_eq!(usage.cache_write, 0);
    assert_eq!(usage.total, 1009);
}

/// Replay `error_frame.sse`: a top-level `error` object in a frame is
/// surfaced as a stream error rather than silently dropped.
#[tokio::test]
async fn error_frame_surfaces_as_stream_error() {
    let fixture = fixtures_root().join("error_frame.sse");
    let provider = FixtureStreamFn::new(&fixture, "gemini-2.5-flash");

    let mut stream = provider
        .stream_simple(&gemini_model(), &user_context("hi"), &Default::default())
        .await
        .expect("stream opens");
    let mut saw_error = false;
    while let Some(ev) = stream.next().await {
        if let Err(e) = ev {
            let msg = e.to_string();
            assert!(msg.contains("API key not valid"), "unexpected: {msg}");
            saw_error = true;
        }
    }
    assert!(saw_error, "error frame must surface as a stream error");
}

/// An empty byte stream still honours the Start → Done contract with an
/// empty `Stop` turn (same shape as the Anthropic adapter).
#[tokio::test]
async fn empty_stream_yields_start_then_done() {
    let provider = FixtureStreamFn {
        fixture: Vec::new(),
        model_id: "gemini-2.5-flash".into(),
        chunk_size: None,
    };
    let events = collect(&provider, &user_context("hi")).await;
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0], AssistantMessageEvent::Start { .. }));
    let (content, stop_reason, _usage) = done_of(&events);
    assert!(content.is_empty());
    assert_eq!(stop_reason, StopReason::Stop);
}

/// A malformed JSON frame is reported, not panicked on.
#[tokio::test]
async fn malformed_frame_surfaces_as_stream_error() {
    let provider = FixtureStreamFn {
        fixture: b"data: {not json}\n\n".to_vec(),
        model_id: "gemini-2.5-flash".into(),
        chunk_size: None,
    };
    let mut stream = provider
        .stream_simple(&gemini_model(), &user_context("hi"), &Default::default())
        .await
        .expect("stream opens");
    let mut error: Option<pi_ai::StreamError> = None;
    while let Some(ev) = stream.next().await {
        if let Err(e) = ev {
            error = Some(e);
            break;
        }
    }
    let message = error.expect("stream error").to_string();
    assert!(message.contains("Google stream chunk JSON"), "{message}");
}

/// The provider constructs its URL from `base_url`, so callers can point
/// it at a gateway or local mock.
#[test]
fn provider_url_and_catalog_smoke() {
    let provider = GoogleProvider::new("test-key");
    assert_eq!(provider.base_url, DEFAULT_BASE_URL);
    assert_eq!(provider.api_key, "test-key");

    let custom = GoogleProvider::with_base_url("k", "http://localhost:8080/v1beta/");
    assert_eq!(custom.base_url, "http://localhost:8080/v1beta/");

    let models = builtin_google_models();
    assert_eq!(models.len(), 3);
    assert!(models
        .iter()
        .all(|m| m.api == pi_protocol::Api::GoogleGenerativeAi));
}

/// `Arc<GoogleProvider>` satisfies the object-safe `StreamFn` bound used
/// by `pi-agent-core`.
#[test]
fn provider_is_object_safe() {
    let _provider: Arc<dyn StreamFn> = Arc::new(GoogleProvider::new("test-key"));
}
