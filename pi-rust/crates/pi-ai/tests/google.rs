//! End-to-end integration tests for `GoogleProvider` (Gemini).
//!
//! These tests exercise the full `StreamFn` trait surface (request
//! building + SSE parsing + `AssistantMessage` accumulation) against
//! recorded Gemini fixture streams. They run offline — no real HTTP
//! calls.
//!
//! The flow mirrors what `pi-agent-core` consumes on each turn:
//!
//! 1. Build the request body from a `Context`.
//! 2. Stream a recorded fixture through `parse_sse`.
//! 3. Drive a `StreamFn` impl that reads from the fixture (no `reqwest`
//!    round-trip) and forward events to the test harness.
//! 4. Assert the resulting `AssistantMessage` field-by-field against
//!    the TS port's expectations.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{stream, StreamExt};
use pi_ai::providers::google::{
    builtin_gemini_catalog, builtin_gemini_models, catalog_from_json, parse_sse,
    GenerateContentRequest, GoogleCatalog, GoogleModelEntry, GoogleProvider,
    DEFAULT_BASE_URL,
};
use pi_ai::models::Models;
use pi_ai::StreamFn;
use pi_protocol::{
    Api, AssistantMessageEvent, Content, Context, Message, Model, ProviderId, Role, StopReason,
    ToolDefinition, Usage,
};
use serde_json::json;

/// Test stream function that replays a recorded SSE byte stream
/// through `parse_sse` — no HTTP involved.
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

    fn from_bytes(bytes: &str, model_id: &str) -> Self {
        Self {
            fixture: bytes.as_bytes().to_vec(),
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
        let chunks = stream::iter(vec![Ok::<Bytes, pi_ai::StreamError>(Bytes::from(
            self.fixture.clone(),
        ))]);
        Ok(parse_sse(Box::pin(chunks), model_id))
    }
}

fn fixtures_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/google")
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
        .stream_simple(&gemini_model(), ctx, &Default::default())
        .await
        .expect("stream opens");
    while let Some(ev) = stream.next().await {
        events.push(ev.expect("event ok"));
    }
    events
}

fn done_event(events: &[AssistantMessageEvent]) -> (Vec<Content>, StopReason, Usage) {
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

/// Replay `text_response.sse` end-to-end and assert the resulting
/// `AssistantMessage` matches the TS port's expectations.
#[tokio::test]
async fn text_response_round_trip() {
    let fixture = fixtures_root().join("text_response.sse");
    let provider = Arc::new(FixtureStreamFn::new(&fixture, "gemini-2.5-flash"));

    let events = collect(&provider, &user_context("hi")).await;

    assert!(matches!(events[0], AssistantMessageEvent::Start { .. }));
    let deltas: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AssistantMessageEvent::TextDelta { delta } => Some(delta.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, vec!["Hello", " there", "!"]);

    let (content, stop_reason, usage) = done_event(&events);
    assert_eq!(stop_reason, StopReason::Stop);
    assert_eq!(text_of(&content), "Hello there!");
    assert_eq!(usage.input, 12);
    assert_eq!(usage.output, 3);
    assert_eq!(usage.total, 15);
}

/// Replay `function_call_response.sse` and assert the tool call lands in
/// the final `AssistantMessage` with `StopReason::ToolUse`.
#[tokio::test]
async fn tool_call_round_trip() {
    let fixture = fixtures_root().join("function_call_response.sse");
    let provider = Arc::new(FixtureStreamFn::new(&fixture, "gemini-2.5-flash"));

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

    let deltas: Vec<(u32, Option<String>, Option<String>)> = events
        .iter()
        .filter_map(|e| match e {
            AssistantMessageEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => Some((*index, id.clone(), name.clone().or_else(|| arguments_delta.clone()))),
            _ => None,
        })
        .collect();
    assert_eq!(deltas.len(), 1, "one functionCall part → one delta");
    assert_eq!(deltas[0].1.as_deref(), Some("get_weather_1"));
    assert_eq!(deltas[0].2.as_deref(), Some("get_weather"));

    let (content, stop_reason, _) = done_event(&events);
    assert_eq!(stop_reason, StopReason::ToolUse);
    let tc = content
        .iter()
        .find_map(|c| match c {
            Content::ToolCall(t) => Some(t),
            _ => None,
        })
        .expect("tool call in done");
    assert_eq!(tc.name, "get_weather");
    assert_eq!(tc.id, "get_weather_1");
    assert_eq!(tc.arguments["city"], "San Francisco");
}

/// Replay `thinking_response.sse`: thought parts stream as
/// `ThinkingDelta` and are dropped from the final content, while the
/// usable text is retained.
#[tokio::test]
async fn thinking_response_round_trip() {
    let fixture = fixtures_root().join("thinking_response.sse");
    let provider = Arc::new(FixtureStreamFn::new(&fixture, "gemini-2.5-flash"));

    let events = collect(&provider, &user_context("what is 2+2?")).await;

    let thinking: String = events
        .iter()
        .filter_map(|e| match e {
            AssistantMessageEvent::ThinkingDelta { delta } => Some(delta.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(thinking, "Let me think");

    let (content, stop_reason, usage) = done_event(&events);
    assert_eq!(stop_reason, StopReason::Stop);
    assert_eq!(text_of(&content), "The answer is 4");
    // input excludes the cached tokens; output folds in the thoughts.
    assert_eq!(usage.input, 6);
    assert_eq!(usage.cache_read, 4);
    assert_eq!(usage.output, 13);
    assert_eq!(usage.total, 23);
}

/// An error envelope on a `data:` line must surface as a stream error.
#[tokio::test]
async fn error_event_surfaces_as_stream_error() {
    let fixture = fixtures_root().join("error_event.sse");
    let provider = Arc::new(FixtureStreamFn::new(&fixture, "gemini-2.5-flash"));

    let mut stream = provider
        .stream_simple(&gemini_model(), &user_context("hi"), &Default::default())
        .await
        .expect("stream opens");
    let mut saw_error = false;
    while let Some(ev) = stream.next().await {
        if let Err(pi_ai::StreamError::Malformed(msg)) = ev {
            if msg.contains("quota") {
                saw_error = true;
            }
        }
    }
    assert!(saw_error, "expected the parser to surface the upstream error");
}

/// Feed the fixture 16 bytes at a time so the line-buffering path is
/// exercised across chunk boundaries.
#[tokio::test]
async fn sse_parser_handles_chunked_input() {
    let fixture = fixtures_root().join("text_response.sse");
    let bytes = std::fs::read(&fixture).expect("read fixture");
    let mut chunks: Vec<Result<Bytes, pi_ai::StreamError>> = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let end = (offset + 16).min(bytes.len());
        chunks.push(Ok(Bytes::copy_from_slice(&bytes[offset..end])));
        offset = end;
    }
    let mut stream = parse_sse(stream::iter(chunks), "gemini-2.5-flash".to_string());
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.expect("stream event"));
    }
    let (content, stop_reason, _) = done_event(&events);
    assert_eq!(stop_reason, StopReason::Stop);
    assert_eq!(text_of(&content), "Hello there!");
}

/// Build the request body directly and assert the Gemini wire shape
/// (contents / systemInstruction / generationConfig / tools).
#[test]
fn request_body_carries_system_tools_and_config() {
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

    let req: GenerateContentRequest = GoogleProvider::build_request(
        &gemini_model(),
        &ctx,
        &pi_ai::SimpleStreamOptions {
            temperature: Some(0.4),
            max_tokens: Some(256),
            ..Default::default()
        },
    )
    .expect("build request");
    let v = serde_json::to_value(&req).expect("serialize");

    assert_eq!(v["contents"][0]["role"], "user");
    assert_eq!(v["contents"][0]["parts"][0]["text"], "hi");
    assert_eq!(v["systemInstruction"]["parts"][0]["text"], "be terse");
    assert!((v["generationConfig"]["temperature"].as_f64().unwrap() - 0.4).abs() < 1e-6);
    assert_eq!(v["generationConfig"]["maxOutputTokens"], 256);

    let decls = v["tools"][0]["functionDeclarations"].as_array().unwrap();
    assert_eq!(decls.len(), 1);
    assert_eq!(decls[0]["name"], "get_weather");
    assert_eq!(decls[0]["parametersJsonSchema"]["required"][0], "city");
    // Gemini 2.5 does not require explicit function-call ids.
    assert!(v["tools"][0].get("id").is_none());
}

/// Tool results become `functionResponse` parts; the function name is
/// recovered from the matching assistant tool call.
#[test]
fn tool_results_become_function_responses() {
    let mut ctx = Context::new("");
    ctx.messages.push(Message {
        role: Role::Assistant,
        content: vec![Content::ToolCall(pi_protocol::ToolCall {
            id: "call_1".into(),
            name: "get_weather".into(),
            arguments: json!({"city": "Berlin"}),
        })],
        model: None,
    });
    ctx.messages.push(Message {
        role: Role::Tool,
        content: vec![Content::ToolResult(pi_protocol::ToolResult {
            tool_call_id: "call_1".into(),
            content: Box::new(Content::text("cold and rainy")),
            is_error: true,
            details: None,
        })],
        model: None,
    });
    let req = GoogleProvider::build_request(&gemini_model(), &ctx, &Default::default())
        .expect("build request");
    let v = serde_json::to_value(&req).expect("serialize");

    // Assistant turn → model role with a functionCall part.
    assert_eq!(v["contents"][0]["role"], "model");
    assert_eq!(
        v["contents"][0]["parts"][0]["functionCall"]["name"],
        "get_weather"
    );
    // Tool turn → user role with a functionResponse part using the
    // `error` key because the result is flagged as an error.
    assert_eq!(v["contents"][1]["role"], "user");
    let fr = &v["contents"][1]["parts"][0]["functionResponse"];
    assert_eq!(fr["name"], "get_weather");
    assert_eq!(fr["response"]["error"], "cold and rainy");
}

/// `MAX_TOKENS` maps to `StopReason::MaxTokens` and a tool-call turn that
/// ends in `STOP` is promoted to `ToolUse`.
#[tokio::test]
async fn finish_reason_mapping_drives_stop_reason() {
    let payload = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"partial\"}],\"role\":\"model\"},\"finishReason\":\"MAX_TOKENS\",\"index\":0}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":2},\"modelVersion\":\"gemini-2.5-flash\"}\n\n";
    let provider = Arc::new(FixtureStreamFn::from_bytes(payload, "gemini-2.5-flash"));
    let events = collect(&provider, &user_context("go")).await;
    let (_, stop_reason, _) = done_event(&events);
    assert_eq!(stop_reason, StopReason::MaxTokens);

    let tools_payload = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"get_weather\",\"args\":{\"city\":\"SF\"}}}],\"role\":\"model\"},\"finishReason\":\"STOP\",\"index\":0}],\"modelVersion\":\"gemini-2.5-flash\"}\n\n";
    let provider = Arc::new(FixtureStreamFn::from_bytes(tools_payload, "gemini-2.5-flash"));
    let events = collect(&provider, &user_context("weather")).await;
    let (_, stop_reason, _) = done_event(&events);
    assert_eq!(stop_reason, StopReason::ToolUse);
}

/// A provided, unique function-call id is preserved instead of being
/// synthesised.
#[tokio::test]
async fn provided_tool_call_id_is_preserved() {
    let payload = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"id\":\"call_abc\",\"name\":\"lookup\",\"args\":{}}}],\"role\":\"model\"},\"finishReason\":\"STOP\",\"index\":0}],\"modelVersion\":\"gemini-2.5-flash\"}\n\n";
    let provider = Arc::new(FixtureStreamFn::from_bytes(payload, "gemini-2.5-flash"));
    let events = collect(&provider, &user_context("x")).await;
    let (content, _, _) = done_event(&events);
    let tc = content
        .iter()
        .find_map(|c| match c {
            Content::ToolCall(t) => Some(t),
            _ => None,
        })
        .expect("tool call");
    assert_eq!(tc.id, "call_abc");
}

/// `catalog_from_json` round-trips a `GoogleCatalog` (including the
/// pricing metadata the shared `Model` type does not carry yet).
#[test]
fn catalog_from_json_round_trip() {
    let catalog = GoogleCatalog {
        provider: "google".into(),
        models: vec![GoogleModelEntry {
            id: "gemini-2.5-flash".into(),
            label: Some("Gemini 2.5 Flash".into()),
            context_window: 1_048_576,
            max_output_tokens: 65_536,
            reasoning: true,
            pricing: Some(pi_ai::providers::google::GooglePricing {
                input: 0.30,
                output: 2.50,
                cache_read: 0.075,
                cache_write: 0.0,
            }),
        }],
    };
    let json = serde_json::to_string(&catalog).expect("serialize");
    let models = catalog_from_json(&json).expect("parse");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "gemini-2.5-flash");
    assert_eq!(models[0].api, Api::GoogleGenerativeAi);
    assert_eq!(models[0].context_window, 1_048_576);
    assert_eq!(models[0].provider.0, "google");
}

/// The built-in catalog covers the models the issue names and carries
/// positive pricing / context metadata.
#[test]
fn builtin_catalog_lists_named_gemini_models() {
    let catalog = builtin_gemini_catalog();
    let ids: Vec<&str> = catalog.models.iter().map(|m| m.id.as_str()).collect();
    assert!(ids.contains(&"gemini-2.5-pro"), "ids: {ids:?}");
    assert!(ids.contains(&"gemini-2.5-flash"), "ids: {ids:?}");
    for entry in &catalog.models {
        assert!(entry.context_window > 0);
        assert!(entry.max_output_tokens > 0);
        let pricing = entry.pricing.as_ref().expect("pricing present");
        assert!(pricing.input > 0.0);
        assert!(pricing.output > 0.0);
    }
    let models = builtin_gemini_models();
    assert_eq!(models.len(), catalog.models.len());
    for m in &models {
        assert_eq!(m.api, Api::GoogleGenerativeAi);
        assert_eq!(m.provider.0, "google");
    }
}

/// The existing generic catalog loader resolves
/// `google/gemini-2.5-flash` — the `--model` acceptance path.
#[test]
fn register_provider_json_resolves_google_slash_gemini() {
    let json = serde_json::to_string(&builtin_gemini_catalog()).expect("serialize");
    let mut models = Models::new();
    models
        .register_provider_json(&ProviderId::new("google"), &json)
        .expect("register google");
    let m = models
        .get_model(&ProviderId::new("google"), "gemini-2.5-flash")
        .expect("google/gemini-2.5-flash resolves");
    assert_eq!(m.api, Api::GoogleGenerativeAi);

    // The bare-models-array envelope works too.
    let bare = serde_json::to_string(&builtin_gemini_models()).expect("serialize");
    let mut models = Models::new();
    models
        .register_provider_json(&ProviderId::new("google"), &bare)
        .expect("register bare google models");
    assert!(models
        .get_model(&ProviderId::new("google"), "gemini-2.5-pro")
        .is_some());
}

/// `with_base_url` swaps the endpoint while preserving the API key.
#[test]
fn with_base_url_overrides_endpoint() {
    let provider = GoogleProvider::with_base_url("key-test", "https://example.test/v1beta");
    assert_eq!(provider.base_url, "https://example.test/v1beta");
    assert_eq!(provider.api_key, "key-test");
    assert_eq!(GoogleProvider::new("key-test").base_url, DEFAULT_BASE_URL);
}

/// A batched JSON-array payload (some proxies) is parsed chunk-by-chunk.
#[tokio::test]
async fn batched_array_payload_is_supported() {
    let payload = "data: [{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello\"}],\"role\":\"model\"},\"index\":0}]},{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\" world\"}],\"role\":\"model\"},\"finishReason\":\"STOP\",\"index\":0}],\"usageMetadata\":{\"promptTokenCount\":2,\"candidatesTokenCount\":2},\"modelVersion\":\"gemini-2.5-flash\"}]\n\n";
    let provider = Arc::new(FixtureStreamFn::from_bytes(payload, "gemini-2.5-flash"));
    let events = collect(&provider, &user_context("hi")).await;
    let (content, stop_reason, usage) = done_event(&events);
    assert_eq!(stop_reason, StopReason::Stop);
    assert_eq!(text_of(&content), "Hello world");
    assert_eq!(usage.input, 2);
}
