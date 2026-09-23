//! OpenRouter image generation — port of
//! `packages/ai/src/api/openrouter-images.ts` and the lazy wrapper in
//! `packages/ai/src/providers/images/register-builtins.ts`.
//!
//! OpenRouter serves image models through its **Chat Completions** endpoint:
//! the request is an ordinary chat request with `modalities: ["image", …]`,
//! and generated images come back as `data:` URLs inside
//! `choices[0].message.images[]`. There is therefore no dedicated image wire
//! protocol to port — only the request/response adaptation plus the same
//! retry, error-body and usage handling the chat adapters use.
//!
//! # Deliberate differences from upstream
//!
//! * Upstream registers this adapter behind a **dynamic `import()`** so the
//!   OpenAI SDK is only loaded if an image call actually happens. Rust has no
//!   lazy module loading, so the adapter is a unit struct compiled into the
//!   crate; the "lazy import failed" branch of `register-builtins.ts` has no
//!   counterpart.
//! * Upstream uses the `openai` SDK. The Rust port posts with `reqwest`
//!   directly and reuses [`crate::retry::retry_provider_request`], matching
//!   how every other provider in this crate talks HTTP.
//! * The `onPayload` / `onResponse` hooks are not modelled (see
//!   [`super::types::ImagesOptions`]).
//! * WASM builds cannot make the request (no `reqwest`); the adapter returns
//!   an `error` result explaining that, like the WASM chat adapters.

use async_trait::async_trait;
use serde_json::json;

use super::types::{
    AssistantImages, ImagesContext, ImagesInputContent, ImagesModel, ImagesOptions,
    ImagesStopReason, ImagesUsage, ProviderImages, UsageCost,
};
#[cfg(not(target_arch = "wasm32"))]
use crate::types::AbortSignal;

/// The image API id registered for OpenRouter (upstream
/// `KnownImagesApi`).
pub const OPENROUTER_IMAGES_API: &str = "openrouter-images";

/// OpenRouter provider id (matches the chat-side `ProviderId`).
pub const OPENROUTER_PROVIDER_ID: &str = "openrouter";

/// Default base URL for OpenRouter image models (upstream
/// `image-models.generated.ts`).
pub const OPENROUTER_IMAGES_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// OpenRouter image-generation adapter.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenRouterImagesProvider;

impl OpenRouterImagesProvider {
    /// Construct the adapter.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ProviderImages for OpenRouterImagesProvider {
    async fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: &ImagesOptions,
    ) -> AssistantImages {
        let mut result = AssistantImages::new(
            model.api.clone(),
            model.provider.to_string(),
            model.id.clone(),
            ImagesStopReason::Stop,
        );

        let Some(api_key) = options.api_key.as_deref() else {
            result.stop_reason = ImagesStopReason::Error;
            result.error_message = Some(format!("No API key for provider: {}", model.provider));
            return result;
        };

        let params = build_params(model, context);

        #[cfg(not(target_arch = "wasm32"))]
        {
            match send_request(model, api_key, &params, options).await {
                Ok(response) => apply_response(&mut result, &response, model),
                Err(error) => {
                    let aborted = options
                        .signal
                        .as_ref()
                        .is_some_and(AbortSignal::is_cancelled);
                    result.stop_reason = if aborted {
                        ImagesStopReason::Aborted
                    } else {
                        ImagesStopReason::Error
                    };
                    result.error_message = Some(describe_error(&error, aborted));
                }
            }
        }

        #[cfg(target_arch = "wasm32")]
        {
            let _ = (api_key, params);
            result.stop_reason = ImagesStopReason::Error;
            result.error_message =
                Some("OpenRouter image generation is unavailable on wasm32".to_string());
        }

        result
    }
}

/// Build the OpenRouter Chat Completions body for an image request
/// (upstream `buildParams`).
///
/// Text parts become `{"type":"text","text":…}`; images become
/// `{"type":"image_url","image_url":{"url":"data:<mime>;base64,<data>"}}`.
/// `modalities` requests text output as well only when the model advertises
/// it.
///
/// Upstream also runs text through `sanitizeSurrogates`; a Rust `String`
/// cannot hold an unpaired surrogate, so that step has no counterpart here.
pub fn build_params(model: &ImagesModel, context: &ImagesContext) -> serde_json::Value {
    let content: Vec<serde_json::Value> = context
        .input
        .iter()
        .map(|item| match item {
            ImagesInputContent::Text(text) => json!({
                "type": "text",
                "text": text.text,
            }),
            ImagesInputContent::Image(image) => json!({
                "type": "image_url",
                "image_url": {
                    "url": format!("data:{};base64,{}", image.mime_type, image.data),
                },
            }),
        })
        .collect();

    let modalities = if model.output.contains(&super::types::ImageModality::Text) {
        json!(["image", "text"])
    } else {
        json!(["image"])
    };

    json!({
        "model": model.id,
        "messages": [{ "role": "user", "content": content }],
        "stream": false,
        "modalities": modalities,
    })
}

/// Parse the `usage` block of an OpenRouter response (upstream `parseUsage`).
pub fn parse_usage(raw: &serde_json::Value, model: &ImagesModel) -> ImagesUsage {
    let prompt_tokens = raw["prompt_tokens"].as_u64().unwrap_or(0) as u32;
    let reported_cached = raw["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or(0) as u32;
    let cache_write = raw["prompt_tokens_details"]["cache_write_tokens"]
        .as_u64()
        .unwrap_or(0) as u32;
    let cache_read = if cache_write > 0 {
        reported_cached.saturating_sub(cache_write)
    } else {
        reported_cached
    };
    let input = prompt_tokens.saturating_sub(cache_read + cache_write);
    let output = raw["completion_tokens"].as_u64().unwrap_or(0) as u32;

    let rate = |per_million: f64, tokens: u32| per_million / 1_000_000.0 * f64::from(tokens);
    let cost = UsageCost {
        input: rate(model.cost.input, input),
        output: rate(model.cost.output, output),
        cache_read: rate(model.cost.cache_read, cache_read),
        cache_write: rate(model.cost.cache_write, cache_write),
        total: 0.0,
    };
    let cost = UsageCost {
        total: cost.input + cost.output + cost.cache_read + cost.cache_write,
        ..cost
    };

    ImagesUsage {
        input,
        output,
        cache_read,
        cache_write,
        total_tokens: input + output + cache_read + cache_write,
        cost,
    }
}

/// Fold an OpenRouter response body into `result` (upstream in-line parsing
/// in `generateImages`).
///
/// Generated images are only accepted as `data:<mime>;base64,<payload>`
/// URLs; anything else (an `https:` link, a malformed payload) is skipped, as
/// upstream does.
pub fn apply_response(
    result: &mut AssistantImages,
    response: &serde_json::Value,
    model: &ImagesModel,
) {
    if let Some(id) = response["id"].as_str() {
        result.response_id = Some(id.to_string());
    }
    if response["usage"].is_object() {
        result.usage = Some(parse_usage(&response["usage"], model));
    }

    let Some(choice) = response["choices"].get(0) else {
        return;
    };
    let message = &choice["message"];

    if let Some(text) = message["content"].as_str() {
        if !text.is_empty() {
            result
                .output
                .push(ImagesInputContent::text(text.to_string()));
        }
    }

    let Some(images) = message["images"].as_array() else {
        return;
    };
    for image in images {
        let url = image["image_url"]
            .as_str()
            .or_else(|| image["image_url"]["url"].as_str());
        let Some(url) = url else { continue };
        let Some((mime_type, data)) = parse_data_url(url) else {
            continue;
        };
        result
            .output
            .push(ImagesInputContent::image(mime_type, data));
    }
}

/// Split a `data:<mime>;base64,<payload>` URL. Returns `None` for any other
/// scheme, a missing `;base64` marker, or an empty mime / payload — upstream
/// only accepts URLs matching `/^data:([^;]+);base64,(.+)$/`.
pub fn parse_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    let mime_type = meta.strip_suffix(";base64")?;
    if mime_type.is_empty() || mime_type.contains(';') || payload.is_empty() {
        return None;
    }
    Some((mime_type.to_string(), payload.to_string()))
}

/// Merge model headers with caller headers; a `None` caller value suppresses
/// the model default (upstream `providerHeadersToRecord({...model.headers,
/// ...optionsHeaders})`).
fn merged_headers(model: &ImagesModel, options: &ImagesOptions) -> Vec<(String, String)> {
    let mut merged: std::collections::BTreeMap<String, String> = model.headers.clone();
    for (key, value) in &options.headers {
        match value {
            Some(value) => {
                merged.insert(key.clone(), value.clone());
            }
            None => {
                merged.remove(key);
            }
        }
    }
    merged.into_iter().collect()
}

/// Describe a [`StreamError`] the way upstream's catch block does
/// (`formatProviderError(normalizeProviderError(error))`).
#[cfg(not(target_arch = "wasm32"))]
fn describe_error(error: &crate::types::StreamError, aborted: bool) -> String {
    use crate::utils::error_body::{format_provider_error, normalize_provider_error};

    if aborted {
        return "aborted".to_string();
    }
    match error {
        crate::types::StreamError::Provider { status, body, .. } => {
            let normalized = normalize_provider_error(Some(*status), "", body);
            let text = format_provider_error(&normalized, None);
            if text.is_empty() {
                format!("{status} status code (no body)")
            } else {
                text
            }
        }
        other => other.to_string(),
    }
}

/// POST `{base_url}/chat/completions` with the provider retry loop.
#[cfg(not(target_arch = "wasm32"))]
async fn send_request(
    model: &ImagesModel,
    api_key: &str,
    params: &serde_json::Value,
    options: &ImagesOptions,
) -> Result<serde_json::Value, crate::types::StreamError> {
    use std::time::Duration;

    use crate::retry::{
        retry_provider_request, ProviderRetryPolicy, DEFAULT_MAX_PROVIDER_RETRY_DELAY_MS,
    };

    let url = format!("{}/chat/completions", model.base_url.trim_end_matches('/'));
    let mut builder = reqwest::Client::builder();
    if let Some(timeout_ms) = options.timeout_ms {
        builder = builder.timeout(Duration::from_millis(timeout_ms));
    }
    let client = builder.build()?;

    let policy = ProviderRetryPolicy::with_max_retry_delay_ms(
        options.max_retries.unwrap_or(0),
        options
            .max_retry_delay_ms
            .unwrap_or(DEFAULT_MAX_PROVIDER_RETRY_DELAY_MS),
    );

    let headers = merged_headers(model, options);
    let api_key = api_key.to_string();
    let params = params.clone();
    let signal = options.signal.as_ref();

    retry_provider_request(&policy, signal, || {
        let client = client.clone();
        let url = url.clone();
        let api_key = api_key.clone();
        let params = params.clone();
        let headers = headers.clone();
        async move { send_once(&client, &url, &api_key, &headers, &params).await }
    })
    .await
}

/// One HTTP attempt.
#[cfg(not(target_arch = "wasm32"))]
async fn send_once(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    headers: &[(String, String)],
    params: &serde_json::Value,
) -> Result<serde_json::Value, crate::types::StreamError> {
    use crate::types::StreamError;
    use crate::utils::error_body::truncate_provider_error_body;

    let mut request = client.post(url).bearer_auth(api_key).json(params);
    for (key, value) in headers {
        request = request.header(key, value);
    }

    let response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        let hint = crate::retry::retry_hint_from_headers(response.headers());
        let body = response.text().await.unwrap_or_default();
        return Err(StreamError::provider_with_hint(
            status.as_u16(),
            truncate_provider_error_body(&body),
            hint,
        ));
    }
    Ok(response.json::<serde_json::Value>().await?)
}
