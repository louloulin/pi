//! Image-generation contract — the Rust counterpart of the image slice of
//! `packages/ai/src/types.ts` (`ImagesApi`, `ImagesContext`,
//! `AssistantImages`, `ImagesModel`, `ImagesOptions`, `ProviderImages`).
//!
//! Upstream keeps two request shapes: chat (`Provider` / `StreamFn`) and
//! images (`ProviderImages` / `ImagesFunction`). This module is the image
//! half — same lifecycle, different result type (`AssistantImages` instead
//! of a message event stream).
//!
//! # Deliberate differences from upstream
//!
//! * `ImagesApi` is a `String`, not a string-literal union: upstream's
//!   `ImagesApi` is deliberately open-ended (`KnownImagesApi | (string & {})`)
//!   so third-party adapters can register their own id.
//! * `ImagesOptions` keeps only the fields the Rust adapters honour. There is
//!   no `fetch` injection, no `onPayload` / `onResponse` hook and no
//!   `telemetryContext`; they can be added when an adapter needs them.
//! * `ImagesUsage` carries the request `cost` that upstream attaches to its
//!   `Usage`; [`pi_protocol::Usage`] has no cost field.
//! * `ProviderImages::generate_images` cannot reject: like upstream's image
//!   adapters, failures are returned as an [`AssistantImages`] with
//!   [`ImagesStopReason::Error`] / [`ImagesStopReason::Aborted`].

use std::collections::BTreeMap;

use async_trait::async_trait;
use pi_protocol::{ImageContent, ProviderId, TextContent};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::auth::ProviderHeaders;
use crate::types::AbortSignal;

/// Image API id. Upstream `KnownImagesApi` is `"openrouter-images"`; the
/// alias stays a `String` so adapters can register their own id.
pub type ImagesApi = String;

/// Whether an image model accepts or produces `text` / `image` parts
/// (upstream `ImagesModel["input" | "output"]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageModality {
    /// Plain text.
    Text,
    /// Raster image.
    Image,
}

/// One item of image-model input (upstream `ImagesInputContent`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImagesInputContent {
    /// Text prompt.
    Text(TextContent),
    /// Inline base64 image.
    Image(ImageContent),
}

impl ImagesInputContent {
    /// Convenience constructor for a text prompt.
    pub fn text<S: Into<String>>(text: S) -> Self {
        Self::Text(TextContent { text: text.into() })
    }

    /// Convenience constructor for an inline base64 image.
    pub fn image<S: Into<String>, D: Into<String>>(mime_type: S, data: D) -> Self {
        Self::Image(ImageContent {
            mime_type: mime_type.into(),
            data: data.into(),
        })
    }
}

/// One item of image-model output (upstream `ImagesOutputContent`). Same
/// shape as [`ImagesInputContent`].
pub type ImagesOutputContent = ImagesInputContent;

/// The request payload for one image-generation call.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ImagesContext {
    /// Inline prompt parts (text and/or reference images).
    pub input: Vec<ImagesInputContent>,
}

impl ImagesContext {
    /// A context holding a single text prompt.
    pub fn text<S: Into<String>>(prompt: S) -> Self {
        Self {
            input: vec![ImagesInputContent::text(prompt)],
        }
    }
}

/// Terminal state of an image-generation call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImagesStopReason {
    /// Generation completed.
    Stop,
    /// Provider or transport error; see [`AssistantImages::error_message`].
    Error,
    /// Caller cancelled through [`ImagesOptions::signal`].
    Aborted,
}

/// Per-request cost, in USD. Mirrors upstream `Usage["cost"]`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageCost {
    /// Input (prompt) cost.
    pub input: f64,
    /// Output (generated) cost.
    pub output: f64,
    /// Cached-input read cost.
    pub cache_read: f64,
    /// Cache-write cost.
    pub cache_write: f64,
    /// Sum of the four rates.
    pub total: f64,
}

/// Token accounting for one image request. The image-side counterpart of
/// [`pi_protocol::Usage`], plus the cost fields upstream attaches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ImagesUsage {
    /// Input tokens consumed.
    pub input: u32,
    /// Output tokens generated.
    pub output: u32,
    /// Cached input tokens.
    pub cache_read: u32,
    /// Cache-write tokens.
    pub cache_write: u32,
    /// Total tokens (input + output + cache read + cache write).
    pub total_tokens: u32,
    /// Cost derived from the model's per-million-token rates.
    pub cost: UsageCost,
}

/// The result of one image-generation call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantImages {
    /// API that produced the result.
    pub api: ImagesApi,
    /// Provider id.
    pub provider: ProviderId,
    /// Model id sent on the wire.
    pub model: String,
    /// Generated parts, in provider order.
    pub output: Vec<ImagesOutputContent>,
    /// Provider response id, when the wire format carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    /// Token / cost accounting, when the provider reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ImagesUsage>,
    /// Terminal state.
    pub stop_reason: ImagesStopReason,
    /// Human-readable failure text; only set for `error` / `aborted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
}

impl AssistantImages {
    /// An empty result in the given terminal state.
    pub fn new(
        api: impl Into<ImagesApi>,
        provider: impl Into<String>,
        model: impl Into<String>,
        stop_reason: ImagesStopReason,
    ) -> Self {
        Self {
            api: api.into(),
            provider: ProviderId::new(provider),
            model: model.into(),
            output: Vec::new(),
            response_id: None,
            usage: None,
            stop_reason,
            error_message: None,
            timestamp: now_millis(),
        }
    }

    /// A failed result with `stop_reason = error`.
    pub fn error(
        api: impl Into<ImagesApi>,
        provider: impl Into<String>,
        model: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        let mut result = Self::new(api, provider, model, ImagesStopReason::Error);
        result.error_message = Some(message.into());
        result
    }

    /// A cancelled result with `stop_reason = aborted`.
    pub fn aborted(
        api: impl Into<ImagesApi>,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self::new(api, provider, model, ImagesStopReason::Aborted)
    }
}

/// Per-million-token pricing for one image model, in USD. Mirrors upstream
/// `ModelCost`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ImageModelCost {
    /// Price per 1M input tokens.
    pub input: f64,
    /// Price per 1M output tokens.
    pub output: f64,
    /// Price per 1M cached input tokens.
    pub cache_read: f64,
    /// Price per 1M cache-write tokens.
    pub cache_write: f64,
}

/// A registered image-generation model. Mirrors upstream `ImagesModel`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImagesModel {
    /// Model identifier sent on the wire.
    pub id: String,
    /// Human-readable label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// API family that serves this model.
    pub api: ImagesApi,
    /// Provider id.
    pub provider: ProviderId,
    /// API base URL (no trailing `/chat/completions`).
    pub base_url: String,
    /// Accepted input modalities.
    #[serde(default)]
    pub input: Vec<ImageModality>,
    /// Produced output modalities.
    #[serde(default)]
    pub output: Vec<ImageModality>,
    /// Per-million-token pricing.
    #[serde(default)]
    pub cost: ImageModelCost,
    /// Extra request headers merged under the caller's headers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

impl ImagesModel {
    /// A model entry with sane defaults (`input = [text, image]`,
    /// `output = [image]`, zero cost).
    pub fn new(
        provider: impl Into<String>,
        id: impl Into<String>,
        api: impl Into<ImagesApi>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: None,
            api: api.into(),
            provider: ProviderId::new(provider),
            base_url: base_url.into(),
            input: vec![ImageModality::Text, ImageModality::Image],
            output: vec![ImageModality::Image],
            cost: ImageModelCost::default(),
            headers: BTreeMap::new(),
        }
    }
}

/// Request options for one image call. A narrowed port of upstream
/// `ImagesOptions` (see the module docs).
#[derive(Debug, Clone, Default)]
pub struct ImagesOptions {
    /// Bearer key. When `None` the adapter reports a configuration error
    /// instead of attempting the request.
    pub api_key: Option<String>,
    /// Extra request headers. A `None` value suppresses a same-named
    /// provider default (upstream `ProviderHeaders`).
    pub headers: ProviderHeaders,
    /// Provider-scoped environment values.
    pub env: BTreeMap<String, String>,
    /// Cancellation signal.
    pub signal: Option<AbortSignal>,
    /// Per-request timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Provider-request retry budget.
    pub max_retries: Option<u32>,
    /// Cap on a server-requested retry delay, in milliseconds.
    pub max_retry_delay_ms: Option<u64>,
    /// Opaque metadata forwarded to adapters that understand it.
    pub metadata: Option<serde_json::Value>,
}

/// The uniform contract every image adapter implements (upstream
/// `ProviderImages`). Implementations must not reject; they return an
/// [`AssistantImages`] carrying the error instead.
#[async_trait]
pub trait ProviderImages: Send + Sync {
    /// Generate images for `context` against `model`.
    async fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: &ImagesOptions,
    ) -> AssistantImages;
}

/// Errors the module-level [`crate::images::generate_images`] facade can
/// return. Adapter-level failures are not errors here — they are
/// `AssistantImages` values, matching upstream's image contract.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ImagesError {
    /// No adapter was registered for the model's `api`.
    #[error("No API provider registered for api: {0}")]
    NoApiProvider(ImagesApi),
}

/// Current wall-clock time in milliseconds since the Unix epoch.
///
/// `SystemTime::now()` is unavailable on `wasm32-unknown-unknown`, so the
/// WASM build reads the JS clock through `js_sys`.
fn now_millis() -> i64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or(0)
    }
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now() as i64
    }
}
