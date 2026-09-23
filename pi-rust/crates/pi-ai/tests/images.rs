//! Offline integration tests for the `pi-ai` image-generation slice
//! (`packages/ai/src/images.ts` + friends).
//!
//! Everything here runs without network access: the OpenRouter HTTP path is
//! exercised against a one-shot `TcpListener` on loopback, and the rest are
//! pure functions. Tests that touch the process-global registry use
//! per-test api ids so they stay independent under the default parallel test
//! runner.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use async_trait::async_trait;
use pi_ai::images::builtins::register_builtin_images_api_providers;
use pi_ai::images::openrouter::{
    apply_response, parse_data_url, parse_usage, OpenRouterImagesProvider,
};
use pi_ai::images::{
    build_params, generate_images, get_images_api_provider, register_images_api_provider,
    registered_images_api_providers, AssistantImages, ImageModality, ImageModels, ImagesContext,
    ImagesError, ImagesInputContent, ImagesModel, ImagesOptions, ImagesStopReason, ImagesUsage,
    ProviderImages, UsageCost, OPENROUTER_IMAGES_API, OPENROUTER_IMAGES_BASE_URL,
    OPENROUTER_PROVIDER_ID,
};

fn unique(tag: &str) -> String {
    format!("lum-1164-{tag}")
}

#[tokio::test]
async fn builtin_registration_exposes_openrouter() {
    register_builtin_images_api_providers();
    assert!(get_images_api_provider(OPENROUTER_IMAGES_API).is_some());
    assert!(registered_images_api_providers()
        .iter()
        .any(|api| api == OPENROUTER_IMAGES_API));
}

#[tokio::test]
async fn facade_rejects_unknown_api() {
    let api = unique("unknown-api");
    let model = ImagesModel::new(
        "someprovider",
        "some-model",
        api.clone(),
        "http://localhost",
    );
    let error = generate_images(
        &model,
        &ImagesContext::text("hi"),
        &ImagesOptions::default(),
    )
    .await
    .expect_err("unknown api must not resolve");
    assert_eq!(error, ImagesError::NoApiProvider(api));
    assert_eq!(
        error.to_string(),
        format!(
            "No API provider registered for api: {}",
            unique("unknown-api")
        )
    );
}

struct EchoProvider;

#[async_trait]
impl ProviderImages for EchoProvider {
    async fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        _options: &ImagesOptions,
    ) -> AssistantImages {
        let mut result = AssistantImages::new(
            model.api.clone(),
            model.provider.to_string(),
            model.id.clone(),
            ImagesStopReason::Stop,
        );
        result.output = context.input.clone();
        result
    }
}

#[tokio::test]
async fn facade_routes_to_registered_adapter() {
    let api = unique("echo-api");
    register_images_api_provider(&api, Arc::new(EchoProvider), None);
    let model = ImagesModel::new("echoprov", "echo-1", api, "http://localhost");
    let context = ImagesContext::text("a red panda");

    let result = generate_images(&model, &context, &ImagesOptions::default())
        .await
        .expect("registered api must resolve");
    assert_eq!(result.stop_reason, ImagesStopReason::Stop);
    assert_eq!(result.output, context.input);
    assert_eq!(result.model, "echo-1");
    assert_eq!(result.provider.0, "echoprov");
}

#[tokio::test]
async fn registry_guard_rejects_mismatched_api() {
    let registered = unique("guard-a");
    register_images_api_provider(&registered, Arc::new(EchoProvider), Some("test".into()));
    let provider = get_images_api_provider(&registered).expect("registered above");

    let model = ImagesModel::new(
        "guardprov",
        "guard-1",
        unique("guard-b"),
        "http://localhost",
    );
    let result = provider
        .generate_images(
            &model,
            &ImagesContext::text("hi"),
            &ImagesOptions::default(),
        )
        .await;
    assert_eq!(result.stop_reason, ImagesStopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some(&*format!(
            "Mismatched api: {} expected {}",
            unique("guard-b"),
            registered
        ))
    );
    assert_eq!(
        pi_ai::images::image_api_provider_source_id(&registered).as_deref(),
        Some("test")
    );
}

#[test]
fn build_params_encodes_text_and_data_url_image() {
    let model = ImagesModel::new(
        OPENROUTER_PROVIDER_ID,
        "google/gemini-2.5-flash-image-preview",
        OPENROUTER_IMAGES_API,
        OPENROUTER_IMAGES_BASE_URL,
    );
    let context = ImagesContext {
        input: vec![
            ImagesInputContent::text("make it snow"),
            ImagesInputContent::image("image/png", "QUJD"),
        ],
    };

    let params = build_params(&model, &context);
    assert_eq!(params["model"], "google/gemini-2.5-flash-image-preview");
    assert_eq!(params["stream"], false);
    assert_eq!(params["messages"][0]["role"], "user");
    assert_eq!(params["messages"][0]["content"][0]["type"], "text");
    assert_eq!(params["messages"][0]["content"][0]["text"], "make it snow");
    assert_eq!(params["messages"][0]["content"][1]["type"], "image_url");
    assert_eq!(
        params["messages"][0]["content"][1]["image_url"]["url"],
        "data:image/png;base64,QUJD"
    );
}

#[test]
fn build_params_requests_text_modality_only_when_supported() {
    let mut model = ImagesModel::new("openrouter", "m", OPENROUTER_IMAGES_API, "http://x");
    model.output = vec![ImageModality::Image];
    assert_eq!(
        build_params(&model, &ImagesContext::text("hi"))["modalities"],
        serde_json::json!(["image"])
    );

    model.output = vec![ImageModality::Image, ImageModality::Text];
    assert_eq!(
        build_params(&model, &ImagesContext::text("hi"))["modalities"],
        serde_json::json!(["image", "text"])
    );
}

#[test]
fn parse_usage_computes_cost_from_model_rates() {
    let mut model = ImagesModel::new("openrouter", "m", OPENROUTER_IMAGES_API, "http://x");
    model.cost = pi_ai::images::ImageModelCost {
        input: 1.0,
        output: 2.0,
        cache_read: 0.5,
        cache_write: 4.0,
    };

    let raw = serde_json::json!({
        "prompt_tokens": 1_000_000,
        "completion_tokens": 500_000,
    });
    let usage = parse_usage(&raw, &model);
    assert_eq!(usage.input, 1_000_000);
    assert_eq!(usage.output, 500_000);
    assert_eq!(usage.cache_read, 0);
    assert_eq!(usage.cache_write, 0);
    assert_eq!(usage.total_tokens, 1_500_000);
    assert!((usage.cost.input - 1.0).abs() < 1e-9);
    assert!((usage.cost.output - 1.0).abs() < 1e-9);
    assert!((usage.cost.total - 2.0).abs() < 1e-9);
}

#[test]
fn parse_usage_splits_cache_read_from_cache_write() {
    let model = ImagesModel::new("openrouter", "m", OPENROUTER_IMAGES_API, "http://x");
    // Upstream: `cacheWrite > 0 ? max(0, reportedCached - cacheWrite) : reportedCached`.
    let raw = serde_json::json!({
        "prompt_tokens": 100,
        "completion_tokens": 10,
        "prompt_tokens_details": { "cached_tokens": 70, "cache_write_tokens": 30 },
    });
    let usage = parse_usage(&raw, &model);
    assert_eq!(usage.cache_write, 30);
    assert_eq!(usage.cache_read, 40);
    assert_eq!(usage.input, 30);
    assert_eq!(usage.output, 10);
    assert_eq!(usage.total_tokens, 110);

    // With no cache-write tokens the reported cached count is the read count.
    let raw = serde_json::json!({
        "prompt_tokens": 100,
        "completion_tokens": 10,
        "prompt_tokens_details": { "cached_tokens": 70 },
    });
    let usage = parse_usage(&raw, &model);
    assert_eq!(usage.cache_read, 70);
    assert_eq!(usage.cache_write, 0);
    assert_eq!(usage.input, 30);
}

#[test]
fn apply_response_extracts_text_images_and_usage() {
    let mut model = ImagesModel::new(
        OPENROUTER_PROVIDER_ID,
        "m",
        OPENROUTER_IMAGES_API,
        "http://x",
    );
    model.cost = pi_ai::images::ImageModelCost {
        output: 1_000_000.0,
        ..Default::default()
    };

    let response = serde_json::json!({
        "id": "gen-123",
        "usage": { "prompt_tokens": 5, "completion_tokens": 2 },
        "choices": [{
            "message": {
                "content": "here you go",
                "images": [
                    { "image_url": "data:image/png;base64,QUJD" },
                    { "image_url": "https://example.com/not-inline.png" },
                    { "image_url": { "url": "data:image/webp;base64,REVG" } },
                    { "image_url": "data:image/png;base64," },
                ],
            },
        }],
    });

    let mut result = AssistantImages::new(
        OPENROUTER_IMAGES_API,
        OPENROUTER_PROVIDER_ID,
        "m",
        ImagesStopReason::Stop,
    );
    apply_response(&mut result, &response, &model);

    assert_eq!(result.response_id.as_deref(), Some("gen-123"));
    assert_eq!(
        result.usage,
        Some(ImagesUsage {
            input: 5,
            output: 2,
            cache_read: 0,
            cache_write: 0,
            total_tokens: 7,
            cost: UsageCost {
                output: 2.0,
                total: 2.0,
                ..Default::default()
            },
        })
    );
    assert_eq!(result.output.len(), 3);
    assert_eq!(result.output[0], ImagesInputContent::text("here you go"));
    assert_eq!(
        result.output[1],
        ImagesInputContent::image("image/png", "QUJD")
    );
    assert_eq!(
        result.output[2],
        ImagesInputContent::image("image/webp", "REVG")
    );
}

#[test]
fn apply_response_tolerates_truncated_bodies() {
    let model = ImagesModel::new(
        OPENROUTER_PROVIDER_ID,
        "m",
        OPENROUTER_IMAGES_API,
        "http://x",
    );
    let mut result = AssistantImages::new(
        OPENROUTER_IMAGES_API,
        OPENROUTER_PROVIDER_ID,
        "m",
        ImagesStopReason::Stop,
    );

    // No `choices` at all (upstream reads `response.choices[0]`).
    apply_response(&mut result, &serde_json::json!({ "id": "x" }), &model);
    assert!(result.output.is_empty());
    assert_eq!(result.response_id.as_deref(), Some("x"));
    assert!(result.usage.is_none());

    // A choice with neither content nor images.
    apply_response(
        &mut result,
        &serde_json::json!({ "choices": [{ "message": {} }] }),
        &model,
    );
    assert!(result.output.is_empty());
}

#[test]
fn parse_data_url_accepts_only_inline_base64() {
    assert_eq!(
        parse_data_url("data:image/png;base64,QUJD"),
        Some(("image/png".to_string(), "QUJD".to_string()))
    );
    // A missing `;base64` marker is rejected, matching upstream's regex.
    assert_eq!(parse_data_url("data:image/svg+xml,PHN2Zz4="), None);
    assert_eq!(
        parse_data_url("data:image/svg+xml;base64,PHN2Zz4="),
        Some(("image/svg+xml".to_string(), "PHN2Zz4=".to_string()))
    );
    assert_eq!(parse_data_url("https://example.com/x.png"), None);
    assert_eq!(parse_data_url("data:image/png;base64,"), None);
    assert_eq!(parse_data_url("data:,QUJD"), None);
    assert_eq!(parse_data_url("data:image/png;base64"), None);
}

#[tokio::test]
async fn openrouter_reports_missing_api_key() {
    let model = ImagesModel::new(
        OPENROUTER_PROVIDER_ID,
        "google/gemini-2.5-flash-image-preview",
        OPENROUTER_IMAGES_API,
        OPENROUTER_IMAGES_BASE_URL,
    );
    let result = OpenRouterImagesProvider::new()
        .generate_images(
            &model,
            &ImagesContext::text("hi"),
            &ImagesOptions::default(),
        )
        .await;
    assert_eq!(result.stop_reason, ImagesStopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("No API key for provider: openrouter")
    );
    assert!(result.output.is_empty());
}

/// A one-shot loopback HTTP server that answers with `body` and returns the
/// raw request it received.
fn spawn_http_server(
    status_line: &'static str,
    body: String,
) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .expect("read timeout");
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        // Read headers, then the declared body.
        loop {
            let read = stream.read(&mut buffer).expect("read request");
            if read == 0 {
                break;
            }
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
            "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
        stream.flush().expect("flush response");
        String::from_utf8_lossy(&request).to_string()
    });
    (format!("http://{addr}"), handle)
}

#[tokio::test]
async fn openrouter_posts_chat_completions_and_parses_response() {
    let body = serde_json::json!({
        "id": "gen-loopback",
        "usage": { "prompt_tokens": 4, "completion_tokens": 1 },
        "choices": [{ "message": { "images": [{ "image_url": "data:image/png;base64,QUJD" }] } }],
    })
    .to_string();
    let (base_url, server) = spawn_http_server("HTTP/1.1 200 OK", body);

    let model = ImagesModel::new(
        OPENROUTER_PROVIDER_ID,
        "test/image-model",
        OPENROUTER_IMAGES_API,
        base_url,
    );
    let mut options = ImagesOptions {
        api_key: Some("test-key".to_string()),
        ..Default::default()
    };
    options
        .headers
        .insert("X-Custom".to_string(), Some("yes".to_string()));

    let result = OpenRouterImagesProvider::new()
        .generate_images(&model, &ImagesContext::text("a cat"), &options)
        .await;

    let request = server.join().expect("server thread");
    assert_eq!(result.stop_reason, ImagesStopReason::Stop);
    assert_eq!(result.response_id.as_deref(), Some("gen-loopback"));
    assert_eq!(
        result.output,
        vec![ImagesInputContent::image("image/png", "QUJD")]
    );

    assert!(request.starts_with("POST /chat/completions HTTP/1.1"));
    assert!(request
        .to_lowercase()
        .contains("authorization: bearer test-key"));
    assert!(request.to_lowercase().contains("x-custom: yes"));
    let payload: serde_json::Value =
        serde_json::from_str(request.split("\r\n\r\n").nth(1).expect("body")).expect("json body");
    assert_eq!(payload["model"], "test/image-model");
    assert_eq!(payload["messages"][0]["content"][0]["text"], "a cat");
}

#[tokio::test]
async fn openrouter_surfaces_http_error_body() {
    let body = serde_json::json!({ "error": { "message": "rate limited" } }).to_string();
    let (base_url, server) = spawn_http_server("HTTP/1.1 429 Too Many Requests", body);

    let model = ImagesModel::new(
        OPENROUTER_PROVIDER_ID,
        "test/image-model",
        OPENROUTER_IMAGES_API,
        base_url,
    );
    let options = ImagesOptions {
        api_key: Some("test-key".to_string()),
        ..Default::default()
    };

    let result = OpenRouterImagesProvider::new()
        .generate_images(&model, &ImagesContext::text("a cat"), &options)
        .await;
    let _ = server.join().expect("server thread");

    assert_eq!(result.stop_reason, ImagesStopReason::Error);
    let message = result.error_message.expect("error message");
    assert!(message.contains("429"), "message was {message}");
    assert!(message.contains("rate limited"), "message was {message}");
}

#[test]
fn image_model_catalog_lookup() {
    let mut catalog = ImageModels::new();
    assert!(catalog.is_empty());
    assert!(catalog.get_image_model("openrouter", "missing").is_none());
    assert_eq!(catalog.get_image_models("openrouter").len(), 0);

    catalog.register_provider(
        "openrouter",
        vec![
            ImagesModel::new(
                OPENROUTER_PROVIDER_ID,
                "google/gemini-2.5-flash-image-preview",
                OPENROUTER_IMAGES_API,
                OPENROUTER_IMAGES_BASE_URL,
            ),
            ImagesModel::new(
                OPENROUTER_PROVIDER_ID,
                "openai/gpt-image-1",
                OPENROUTER_IMAGES_API,
                OPENROUTER_IMAGES_BASE_URL,
            ),
        ],
    );
    assert_eq!(catalog.len(), 2);
    assert_eq!(
        catalog.get_image_providers(),
        vec!["openrouter".to_string()]
    );
    assert!(catalog
        .get_image_model("openrouter", "openai/gpt-image-1")
        .is_some());

    let mut json_catalog = ImageModels::new();
    json_catalog
        .register_provider_json(
            "openrouter",
            r#"[{"id":"a/b","api":"openrouter-images","base_url":"https://openrouter.ai/api/v1","name":"A/B"}]"#,
        )
        .expect("parse catalog");
    assert_eq!(json_catalog.len(), 1);
    let entry = json_catalog
        .get_image_model("openrouter", "a/b")
        .expect("entry");
    assert_eq!(entry.label.as_deref(), Some("A/B"));
    assert_eq!(entry.api, OPENROUTER_IMAGES_API);
}
