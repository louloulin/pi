//! End-to-end tests for `MistralProvider`'s SSE adapter.
//!
//! Mirrors `packages/ai/src/api/mistral-conversations.ts`: recorded
//! Mistral `chat.completion.chunk` frames are replayed through
//! `pi_ai::providers::mistral::parse_sse` and the resulting events are
//! asserted field-by-field. Everything runs offline — no HTTP.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{stream, StreamExt};
use pi_ai::providers::mistral::parse_sse;
use pi_ai::StreamFn;
use pi_protocol::{
    AssistantMessageEvent, Content, Context, Message, Model, ProviderId, Role, StopReason, Usage,
};
use serde_json::json;

/// Replays a recorded fixture through the Mistral SSE parser.
struct FixtureStreamFn {
    fixture: Vec<u8>,
    model_id: String,
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
        let mut fixture = Self::new(fixture_path, model_id);
        fixture.chunk_size = Some(chunk_size);
        fixture
    }

    /// Raw bytes, for cases a checked-in fixture cannot express: the
    /// repository normalises text files to LF (`.gitattributes`), so a CRLF
    /// payload has to be built at runtime.
    fn from_bytes(bytes: Vec<u8>, model_id: &str) -> Self {
        Self {
            fixture: bytes,
            model_id: model_id.to_string(),
            chunk_size: None,
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
        let chunks: Vec<Result<Bytes, pi_ai::StreamError>> = match self.chunk_size {
            Some(size) => self
                .fixture
                .chunks(size)
                .map(|c| Ok(Bytes::copy_from_slice(c)))
                .collect(),
            None => vec![Ok(Bytes::from(self.fixture.clone()))],
        };
        Ok(parse_sse(Box::pin(stream::iter(chunks)), model_id))
    }
}

fn fixtures_root() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/mistral")
}

fn mistral_model() -> Model {
    Model {
        provider: ProviderId::new("mistral"),
        id: "mistral-large-latest".into(),
        api: pi_protocol::Api::MistralConversations,
        label: Some("Mistral Large (latest)".into()),
        context_window: 262_144,
        max_output_tokens: 262_144,
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

async fn collect(provider: &Arc<FixtureStreamFn>, ctx: &Context) -> Vec<AssistantMessageEvent> {
    let mut events = Vec::new();
    let mut stream = provider
        .stream_simple(&mistral_model(), ctx, &Default::default())
        .await
        .expect("stream opens");
    while let Some(event) = stream.next().await {
        events.push(event.expect("event ok"));
    }
    events
}

fn done_of(events: &[AssistantMessageEvent]) -> (Vec<Content>, StopReason, Usage) {
    events
        .iter()
        .find_map(|event| match event {
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

fn thinking_of(events: &[AssistantMessageEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            AssistantMessageEvent::ThinkingDelta { delta } => Some(delta.clone()),
            _ => None,
        })
        .collect()
}

/// `text_response.sse`: bare-string content deltas fold into one text
/// block, `usage` is mapped field-by-field, and `finish_reason: "stop"`
/// becomes [`StopReason::Stop`].
#[tokio::test]
async fn text_response_round_trip() {
    let fixture = fixtures_root().join("text_response.sse");
    let provider = Arc::new(FixtureStreamFn::new(&fixture, "mistral-large-latest"));
    let events = collect(&provider, &user_context("hi")).await;

    assert!(
        matches!(events[0], AssistantMessageEvent::Start { ref model } if model == "mistral-large-latest")
    );
    assert!(matches!(
        events.last(),
        Some(AssistantMessageEvent::Done { .. })
    ));
    let (content, stop_reason, usage) = done_of(&events);
    assert_eq!(stop_reason, StopReason::Stop);
    assert_eq!(text_of(&content), "Hello there");
    assert_eq!(content.len(), 1, "consecutive deltas extend one block");
    assert_eq!(usage.input, 17);
    assert_eq!(usage.output, 4);
    assert_eq!(usage.total, 21);
    assert_eq!(usage.cache_read, 0);
    assert_eq!(usage.cache_write, 0);
}

/// `tool_call_response.sse`: the id/name arrive on the first fragment and
/// the JSON arguments are split across chunks; the final call is parsed,
/// not left as a raw string.
#[tokio::test]
async fn tool_call_response_round_trip() {
    let fixture = fixtures_root().join("tool_call_response.sse");
    let provider = Arc::new(FixtureStreamFn::new(&fixture, "mistral-large-latest"));
    let events = collect(&provider, &user_context("read a.txt")).await;

    let deltas: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AssistantMessageEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => Some((*index, id.clone(), name.clone(), arguments_delta.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        deltas,
        vec![
            (
                0,
                Some("call_abc".to_string()),
                Some("read".to_string()),
                Some("{\"pa".to_string())
            ),
            (
                0,
                Some("call_abc".to_string()),
                Some("read".to_string()),
                Some("th\": \"a.txt\"}".to_string())
            ),
        ]
    );

    let (content, stop_reason, usage) = done_of(&events);
    assert_eq!(stop_reason, StopReason::ToolUse);
    assert_eq!(text_of(&content), "Let me check. ");
    let call = content
        .iter()
        .find_map(|c| match c {
            Content::ToolCall(call) => Some(call.clone()),
            _ => None,
        })
        .expect("tool call block");
    assert_eq!(call.id, "call_abc");
    assert_eq!(call.name, "read");
    assert_eq!(call.arguments, json!({"path": "a.txt"}));
    assert_eq!(usage.input, 40);
    assert_eq!(usage.output, 12);
}

/// `thinking_response.sse`: chunk-array content is understood, thinking is
/// streamed as its own event type, and the following text opens a new
/// block.
#[tokio::test]
async fn thinking_is_streamed_but_not_stored() {
    let fixture = fixtures_root().join("thinking_response.sse");
    let provider = Arc::new(FixtureStreamFn::new(&fixture, "mistral-medium-latest"));
    let events = collect(&provider, &user_context("2+2?")).await;

    assert_eq!(thinking_of(&events), "Let me think.");
    let (content, stop_reason, _) = done_of(&events);
    assert_eq!(stop_reason, StopReason::Stop);
    assert_eq!(text_of(&content), "The answer is 4.");
    assert_eq!(content.len(), 1);
}

/// `cache_read_response.sse`: cached prompt tokens are subtracted from the
/// reported prompt count and surface as `cache_read`; `model_length` is a
/// max-tokens stop.
#[tokio::test]
async fn cache_read_and_model_length_round_trip() {
    let fixture = fixtures_root().join("cache_read_response.sse");
    let provider = Arc::new(FixtureStreamFn::new(&fixture, "mistral-large-latest"));
    let events = collect(&provider, &user_context("cached prompt")).await;

    let (content, stop_reason, usage) = done_of(&events);
    assert_eq!(stop_reason, StopReason::MaxTokens);
    assert_eq!(text_of(&content), "cached reply truncated");
    assert_eq!(usage.input, 100);
    assert_eq!(usage.output, 9);
    assert_eq!(usage.cache_read, 900);
    assert_eq!(usage.total, 1009);
}

/// `error_stop.sse`: a provider-side `finish_reason: "error"` becomes an
/// `Error` event carrying upstream's message, followed by a terminal
/// `Done` whose stop reason is `Error`.
#[tokio::test]
async fn provider_stop_error_emits_error_event_then_done() {
    let fixture = fixtures_root().join("error_stop.sse");
    let provider = Arc::new(FixtureStreamFn::new(&fixture, "mistral-large-latest"));
    let events = collect(&provider, &user_context("boom")).await;

    let message = events
        .iter()
        .find_map(|event| match event {
            AssistantMessageEvent::Error { message } => Some(message.clone()),
            _ => None,
        })
        .expect("error event");
    assert_eq!(message, "Provider stopped with: error");
    let (content, stop_reason, _) = done_of(&events);
    assert_eq!(stop_reason, StopReason::Error);
    assert_eq!(text_of(&content), "partial answer");
    assert!(matches!(
        events.last(),
        Some(AssistantMessageEvent::Done { .. })
    ));
}

/// A stream that ends without a `finish_reason` is an error, exactly like
/// upstream's `Mistral stream ended without a finish reason`.
#[tokio::test]
async fn missing_finish_reason_is_an_error() {
    let fixture = fixtures_root().join("no_finish_reason.sse");
    let provider = Arc::new(FixtureStreamFn::new(&fixture, "mistral-large-latest"));
    let events = collect(&provider, &user_context("cut")).await;

    let message = events
        .iter()
        .find_map(|event| match event {
            AssistantMessageEvent::Error { message } => Some(message.clone()),
            _ => None,
        })
        .expect("error event");
    assert_eq!(message, "Mistral stream ended without a finish reason");
    let (content, stop_reason, _) = done_of(&events);
    assert_eq!(stop_reason, StopReason::Error);
    assert_eq!(text_of(&content), "cut off");
}

/// An event without a `choices` array is rejected rather than silently
/// ignored (upstream's `Invalid Mistral streaming event`).
#[tokio::test]
async fn malformed_event_is_a_stream_error() {
    let fixture = fixtures_root().join("malformed_event.sse");
    let provider = Arc::new(FixtureStreamFn::new(&fixture, "mistral-large-latest"));

    let mut stream = provider
        .stream_simple(&mistral_model(), &user_context("bad"), &Default::default())
        .await
        .expect("stream opens");
    let mut error = None;
    while let Some(event) = stream.next().await {
        if let Err(e) = event {
            error = Some(e.to_string());
            break;
        }
    }
    let error = error.expect("malformed event surfaces as a stream error");
    assert!(error.contains("mistral SSE JSON"), "{error}");
}

/// CRLF line endings and a JSON payload split across several `data:`
/// lines both parse (upstream joins `data:` lines with `\n`).
#[tokio::test]
async fn crlf_and_multi_line_data_are_parsed() {
    // `data:` lines split mid-JSON and CRLF line breaks.
    let payload = concat!(
        "data: {\"id\":\"cmpl-crlf\",\"choices\":[{\"index\":0,\r\n",
        "data: \"delta\":{\"content\":\"split\"},\"finish_reason\":null}]}\r\n",
        "\r\n",
        "data: {\"id\":\"cmpl-crlf\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"\"},\"finish_reason\":\"stop\"}]}\r\n",
        "\r\n",
        "data: [DONE]\r\n",
        "\r\n",
    );
    let fixture_bytes = payload.as_bytes().to_vec();
    for chunk_size in [None, Some(1)] {
        let mut input = FixtureStreamFn::from_bytes(fixture_bytes.clone(), "mistral-large-latest");
        input.chunk_size = chunk_size;
        let provider = Arc::new(input);
        let events = collect(&provider, &user_context("split")).await;
        let (content, stop_reason, _) = done_of(&events);
        assert_eq!(stop_reason, StopReason::Stop);
        assert_eq!(text_of(&content), "split");
    }
}

/// Feeding the stream one byte at a time must produce the same result —
/// the parser buffers partial lines and partial JSON.
#[tokio::test]
async fn chunked_bytes_across_boundaries() {
    let fixture = fixtures_root().join("text_response.sse");
    let provider = Arc::new(FixtureStreamFn::chunked(
        &fixture,
        "mistral-large-latest",
        1,
    ));
    let events = collect(&provider, &user_context("hi")).await;

    let (content, stop_reason, usage) = done_of(&events);
    assert_eq!(stop_reason, StopReason::Stop);
    assert_eq!(text_of(&content), "Hello there");
    assert_eq!(usage.input, 17);
    assert_eq!(usage.total, 21);
}
