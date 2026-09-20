//! Offline integration tests for [`BuiltinPiAiStreamRunner`] — LUM-1180.
//!
//! The runner is what backs `@earendil-works/pi-ai/compat`'s
//! `anthropicMessagesApi` / `openAIResponsesApi` factories in the
//! extension host. `crates/pi-extensions/tests/pi_ai_provider.rs` covers the
//! host bridge with a fake runner; these tests cover the *real* provider
//! path, driven end to end against a one-shot loopback HTTP server that
//! replays a recorded Anthropic SSE stream — no network, no API key.
//!
//! The checked contract is the one the shim depends on:
//!
//! * the runner resolves the credential / base URL from the request and
//!   POSTs to `{baseUrl}/v1/messages` with `x-api-key`;
//! * the returned stream yields the **upstream-shaped** JS events in order
//!   (`text_delta` … then a terminal `done` carrying a whole
//!   `AssistantMessage`), which is exactly what the shim's
//!   `AssistantMessageEventStream` collects;
//! * a transport failure is reported as an `Err` from `start`, which the
//!   shim turns into a terminal `error` event (never a thrown exception).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use futures::StreamExt;
use pi_coding_agent::extensions::pi_ai_runner::BuiltinPiAiStreamRunner;
use pi_extensions::{PiAiStreamRequest, PiAiStreamRunner};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

/// A minimal Anthropic Messages SSE stream: one text block, then `done`.
const SSE_BODY: &str = concat!(
    "event: message_start\n",
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-haiku-4-5\",",
    "\"usage\":{\"input_tokens\":3,\"output_tokens\":0,\"cache_read_input_tokens\":1,",
    "\"cache_creation_input_tokens\":2}}}\n\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},",
    "\"usage\":{\"input_tokens\":3,\"output_tokens\":2}}\n\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

/// A one-shot loopback HTTP server: answers with `body` and returns the raw
/// request it received.
fn spawn_server(
    status_line: &'static str,
    content_type: &'static str,
    body: String,
) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("read timeout");
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let read = match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => break,
            };
            request.extend_from_slice(&buffer[..read]);
            let text = String::from_utf8_lossy(&request);
            if let Some(headers_end) = text.find("\r\n\r\n") {
                let content_length = text
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        if name.eq_ignore_ascii_case("content-length") {
                            value.trim().parse::<usize>().ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
                if request.len() >= headers_end + 4 + content_length {
                    break;
                }
            }
        }
        let response = format!(
            "{status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).expect("write");
        stream.flush().expect("flush");
        String::from_utf8_lossy(&request).to_string()
    });
    (format!("http://{addr}"), handle)
}

/// A request shaped exactly like the shim's `host_pi_ai_stream_start`
/// payload for `anthropicMessagesApi().streamSimple(model, context, options)`.
fn anthropic_request(base_url: &str) -> PiAiStreamRequest {
    PiAiStreamRequest {
        api: "anthropic-messages".to_string(),
        model: json!({
            "id": "claude-haiku-4-5",
            "provider": "anthropic",
            "api": "anthropic-messages",
            "contextWindow": 200_000,
            "maxTokens": 8192,
        }),
        context: json!({
            "systemPrompt": "be brief",
            "messages": [{ "role": "user", "content": "hi" }],
        }),
        options: json!({ "apiKey": "test-key", "baseUrl": base_url, "maxTokens": 64 }),
    }
}

fn kinds(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .map(|event| event["type"].as_str().unwrap_or("<missing>"))
        .collect()
}

#[tokio::test]
async fn real_provider_stream_reaches_the_upstream_event_shape() {
    let (base_url, server) =
        spawn_server("HTTP/1.1 200 OK", "text/event-stream", SSE_BODY.to_string());
    let runner = BuiltinPiAiStreamRunner::with_env(|_name| None);

    let stream = runner
        .start(anthropic_request(&base_url), CancellationToken::new())
        .await
        .expect("the provider stream starts");
    let events: Vec<Value> = stream.collect().await;

    let head = server.join().expect("server thread");
    assert!(
        head.starts_with("POST /v1/messages HTTP/1.1"),
        "posts to the Messages endpoint: {head}"
    );
    assert!(
        head.to_ascii_lowercase().contains("x-api-key: test-key"),
        "sends the credential: {head}"
    );
    assert!(
        head.to_ascii_lowercase()
            .contains("anthropic-version: 2023-06-15"),
        "pins the wire version: {head}"
    );
    assert!(
        head.contains("\"system\":\"be brief\""),
        "carries the system prompt: {head}"
    );
    assert!(
        head.contains("\"max_tokens\":64"),
        "carries maxTokens from the options: {head}"
    );

    assert_eq!(
        kinds(&events),
        vec![
            "start",
            "text_start",
            "text_delta",
            "text_delta",
            "text_end",
            "done"
        ],
        "the Rust events are encoded as upstream AssistantMessageEvents: {events:?}"
    );
    assert_eq!(events[2]["delta"], "Hello");
    assert_eq!(events[3]["delta"], " world");
    assert!(
        events[2].get("partial").is_some(),
        "every event carries the accumulated partial: {:?}",
        events[2]
    );

    let done = &events[5];
    assert_eq!(done["reason"], "stop", "end_turn maps to stop");
    let message = &done["message"];
    assert_eq!(message["role"], "assistant");
    assert_eq!(message["provider"], "anthropic");
    assert_eq!(message["model"], "claude-haiku-4-5");
    assert_eq!(message["stopReason"], "stop");
    assert_eq!(message["content"][0]["type"], "text");
    assert_eq!(message["content"][0]["text"], "Hello world");
    assert_eq!(message["usage"]["input"], 3);
    assert_eq!(message["usage"]["output"], 2);
    assert_eq!(message["usage"]["cacheRead"], 1);
    assert_eq!(message["usage"]["cacheWrite"], 2);
    assert_eq!(message["usage"]["totalTokens"], 5);
}

#[tokio::test]
async fn a_provider_error_fails_the_start_not_the_events() {
    let (base_url, server) = spawn_server(
        "HTTP/1.1 401 Unauthorized",
        "application/json",
        json!({"error": {"message": "invalid api key"}}).to_string(),
    );
    let runner = BuiltinPiAiStreamRunner::with_env(|_name| None);

    let error = match runner
        .start(anthropic_request(&base_url), CancellationToken::new())
        .await
    {
        Ok(_) => panic!("a 401 must not produce a stream"),
        Err(error) => error,
    };
    let head = server.join().expect("server thread");
    assert!(head.starts_with("POST /v1/messages"), "{head}");
    assert!(
        error.contains("invalid api key"),
        "the provider message survives the bridge: {error}"
    );
}

#[tokio::test]
async fn a_missing_credential_is_reported_without_dialing() {
    let runner = BuiltinPiAiStreamRunner::with_env(|_name| None);
    // An unregistered provider id keeps this deterministic: a registered one
    // would consult the process environment through `get_env_api_key`.
    let mut request = anthropic_request("http://127.0.0.1:1");
    request.model["provider"] = json!("lum-1180-unregistered");
    request.options = json!({ "baseUrl": "http://127.0.0.1:1" });

    let error = match runner.start(request, CancellationToken::new()).await {
        Ok(_) => panic!("no credential must not produce a stream"),
        Err(error) => error,
    };
    assert!(
        error.contains("no API key for provider `lum-1180-unregistered`"),
        "{error}"
    );
}

/// A base URL the request leaves out is resolved through the runner's
/// environment lookup — the same path `ProviderRouter` uses for
/// `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL`.
#[tokio::test]
async fn the_env_lookup_provides_the_base_url() {
    let (base_url, server) =
        spawn_server("HTTP/1.1 200 OK", "text/event-stream", SSE_BODY.to_string());
    let env_base_url = base_url.clone();
    let runner = BuiltinPiAiStreamRunner::with_env(move |name| {
        (name == "ANTHROPIC_BASE_URL").then(|| env_base_url.clone())
    });

    let mut request = anthropic_request(&base_url);
    request.options = json!({ "apiKey": "test-key" });
    let stream = runner
        .start(request, CancellationToken::new())
        .await
        .expect("the provider stream starts");
    let events: Vec<Value> = stream.collect().await;
    assert_eq!(
        events.last().expect("terminal event")["type"],
        "done",
        "{events:?}"
    );

    let head = server.join().expect("server thread");
    assert!(head.starts_with("POST /v1/messages"), "{head}");
}

/// A minimal OpenAI Responses SSE stream: one text item, then `completed`.
const RESPONSES_SSE_BODY: &str = concat!(
    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\",\"model\":\"gpt-4o-mini\"}}\n\n",
    "data: {\"type\":\"response.output_item.added\",\"output_index\":0,",
    "\"item\":{\"type\":\"message\",\"id\":\"m1\",\"role\":\"assistant\",\"content\":[]}}\n\n",
    "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"Hi\"}\n\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"model\":\"gpt-4o-mini\",",
    "\"usage\":{\"input_tokens\":5,\"output_tokens\":1,\"total_tokens\":6},\"output\":[]}}\n\n",
);

/// The `openAIResponsesApi` half of the bridge: same contract, the other
/// provider adapter (POST to `/responses`, `Authorization: Bearer`).
#[tokio::test]
async fn the_openai_responses_factory_streams_from_its_own_adapter() {
    let (base_url, server) = spawn_server(
        "HTTP/1.1 200 OK",
        "text/event-stream",
        RESPONSES_SSE_BODY.to_string(),
    );
    let runner = BuiltinPiAiStreamRunner::with_env(|_name| None);

    let mut request = anthropic_request(&base_url);
    request.api = "openai-responses".to_string();
    request.model = json!({
        "id": "gpt-4o-mini",
        "provider": "openai",
        "api": "openai-responses",
        "contextWindow": 128_000,
        "maxTokens": 4096,
    });
    request.options = json!({ "apiKey": "test-key", "baseUrl": base_url });

    let stream = runner
        .start(request, CancellationToken::new())
        .await
        .expect("the provider stream starts");
    let events: Vec<Value> = stream.collect().await;

    let head = server.join().expect("server thread");
    assert!(
        head.starts_with("POST /responses HTTP/1.1"),
        "posts to the Responses endpoint: {head}"
    );
    assert!(
        head.to_ascii_lowercase()
            .contains("authorization: bearer test-key"),
        "sends the bearer credential: {head}"
    );

    assert_eq!(
        kinds(&events),
        vec!["start", "text_start", "text_delta", "text_end", "done"],
        "{events:?}"
    );
    let done = events.last().expect("terminal event");
    assert_eq!(done["reason"], "stop");
    assert_eq!(done["message"]["provider"], "openai");
    assert_eq!(done["message"]["model"], "gpt-4o-mini");
    assert_eq!(done["message"]["content"][0]["text"], "Hi");
    assert_eq!(done["message"]["usage"]["input"], 5);
    assert_eq!(done["message"]["usage"]["output"], 1);
}

/// The runner refuses an api family it has no adapter for instead of
/// silently streaming from the wrong provider.
#[tokio::test]
async fn an_unbridged_api_is_rejected() {
    let runner = BuiltinPiAiStreamRunner::with_env(|_name| None);
    let mut request = anthropic_request("http://127.0.0.1:1");
    request.model = json!({ "id": "gpt-4o", "provider": "openai", "api": "openai-completions" });

    let error = match runner.start(request, CancellationToken::new()).await {
        Ok(_) => panic!("openai-completions must not produce a stream"),
        Err(error) => error,
    };
    assert!(
        error.contains("only serves `anthropic-messages` and `openai-responses`"),
        "{error}"
    );
}
