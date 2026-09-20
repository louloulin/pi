//! Azure OpenAI Responses provider — the deployment-scoped variant of
//! [`super::openai_responses`].
//!
//! Mirrors `packages/ai/src/api/azure-openai-responses.ts`. Azure speaks the
//! same wire protocol as OpenAI Responses (`POST /responses`, same request
//! body, same SSE events), so this adapter reuses
//! [`OpenAiResponsesProvider::build_request`] and [`parse_sse`] verbatim and
//! only overrides the three Azure-specific pieces:
//!
//! * **URL** — `{base}/deployments/{deployment}/responses?api-version={version}`
//!   instead of `{base}/responses`.
//! * **Auth** — the `api-key` header instead of `Authorization: Bearer`.
//! * **Deployment name** — the wire `model` field is the Azure *deployment*
//!   name, resolved from an explicit override, then
//!   `AZURE_OPENAI_DEPLOYMENT_NAME_MAP` (`modelId=deployment,modelId=deployment`),
//!   then the catalog model id.
//!
//! # Configuration
//!
//! The base URL is required. It is resolved from, in order:
//!
//! 1. the base URL passed to the constructor (the registry feeds
//!    `AZURE_OPENAI_BASE_URL` here),
//! 2. `AZURE_OPENAI_RESOURCE_NAME`, expanded to
//!    `https://{resource}.openai.azure.com/openai/v1`.
//!
//! The API version comes from `AZURE_OPENAI_API_VERSION`, defaulting to
//! [`DEFAULT_API_VERSION`] (`v1`). Azure hosts given without a path (or with
//! only `/openai`) are normalized to `/openai/v1`.
//!
//! # Deliberate differences from upstream
//!
//! * `onPayload` / `onResponse` hooks, `fetch` injection, `timeoutMs` and
//!   `samplingParams` are not modelled by the narrow Rust
//!   [`SimpleStreamOptions`]; the request body is exactly what
//!   [`OpenAiResponsesProvider::build_request`] produces.
//! * Reasoning-effort / reasoning-summary parameters are not part of
//!   [`SimpleStreamOptions`] yet, so they are not sent.
//! * The non-streaming 400 fallback the OpenAI Responses adapter has is
//!   omitted: upstream Azure always requests `stream: true`.
//! * wasm32 has no HTTP transport wired for this adapter yet.

use std::collections::BTreeMap;

use async_trait::async_trait;
#[cfg(not(target_arch = "wasm32"))]
use futures::TryStreamExt;

use pi_protocol::{Context, Model};

use super::openai_responses::{parse_sse, OpenAiResponsesProvider};
use crate::stream::AssistantMessageEventStream;
use crate::types::{SimpleStreamOptions, StreamError};
use crate::StreamFn;

/// Default `api-version` query value.
pub const DEFAULT_API_VERSION: &str = "v1";

/// Credential env var (the registry reads it too).
pub const API_KEY_ENV: &str = "AZURE_OPENAI_API_KEY";
/// Explicit base-URL env var.
pub const BASE_URL_ENV: &str = "AZURE_OPENAI_BASE_URL";
/// Resource-name env var, expanded to the default Azure host.
pub const RESOURCE_NAME_ENV: &str = "AZURE_OPENAI_RESOURCE_NAME";
/// `api-version` override env var.
pub const API_VERSION_ENV: &str = "AZURE_OPENAI_API_VERSION";
/// Deployment-name map env var (`modelId=deployment,modelId=deployment`).
pub const DEPLOYMENT_NAME_MAP_ENV: &str = "AZURE_OPENAI_DEPLOYMENT_NAME_MAP";

/// Azure OpenAI Responses provider.
///
/// Construct with [`AzureOpenAiResponsesProvider::new`] (env-driven API
/// version + deployment map) or the `with_*` builders for tests and
/// programmatic use.
#[derive(Debug, Clone)]
pub struct AzureOpenAiResponsesProvider {
    /// Key sent in the `api-key` header.
    pub api_key: String,
    /// Base URL, or empty to resolve from `AZURE_OPENAI_RESOURCE_NAME` at
    /// request time.
    pub base_url: String,
    /// `api-version` query value.
    pub api_version: String,
    /// `modelId -> deployment` map. Empty when unset.
    pub deployment_names: BTreeMap<String, String>,
    /// Explicit deployment override; wins over [`Self::deployment_names`].
    pub deployment_name: Option<String>,
}

impl AzureOpenAiResponsesProvider {
    /// Build a provider from an API key and base URL, taking the API version
    /// and deployment map from the environment.
    pub fn new(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
            api_version: env_var(API_VERSION_ENV)
                .unwrap_or_else(|| DEFAULT_API_VERSION.to_string()),
            deployment_names: parse_deployment_name_map(
                env_var(DEPLOYMENT_NAME_MAP_ENV).as_deref(),
            ),
            deployment_name: None,
        }
    }

    /// Override the `api-version` query value.
    pub fn with_api_version(mut self, api_version: impl Into<String>) -> Self {
        self.api_version = api_version.into();
        self
    }

    /// Force every request to a single deployment name.
    pub fn with_deployment(mut self, deployment_name: impl Into<String>) -> Self {
        self.deployment_name = Some(deployment_name.into());
        self
    }

    /// Replace the deployment-name map.
    pub fn with_deployment_names(mut self, map: BTreeMap<String, String>) -> Self {
        self.deployment_names = map;
        self
    }

    /// Resolve the deployment name for `model_id`.
    ///
    /// Precedence mirrors upstream: explicit override, then the deployment
    /// map, then the catalog model id itself.
    pub fn resolve_deployment_name(&self, model_id: &str) -> String {
        if let Some(explicit) = &self.deployment_name {
            return explicit.clone();
        }
        self.deployment_names
            .get(model_id)
            .cloned()
            .unwrap_or_else(|| model_id.to_string())
    }

    /// Resolve the base URL for a request.
    ///
    /// `self.base_url` wins; otherwise `AZURE_OPENAI_RESOURCE_NAME` is
    /// expanded. The result is normalized (see [`normalize_base_url`]).
    pub fn resolve_base_url(&self) -> Result<String, StreamError> {
        let raw = if self.base_url.trim().is_empty() {
            match env_var(RESOURCE_NAME_ENV) {
                Some(resource) if !resource.trim().is_empty() => {
                    format!("https://{}.openai.azure.com/openai/v1", resource.trim())
                }
                _ => String::new(),
            }
        } else {
            self.base_url.clone()
        };

        if raw.trim().is_empty() {
            return Err(StreamError::Malformed(
                "Azure OpenAI base URL is required: set AZURE_OPENAI_BASE_URL \
                 or AZURE_OPENAI_RESOURCE_NAME"
                    .to_string(),
            ));
        }
        Ok(normalize_base_url(&raw))
    }

    /// The full request URL for a deployment.
    pub fn endpoint(&self, deployment: &str) -> Result<String, StreamError> {
        let base = self.resolve_base_url()?;
        Ok(format!(
            "{base}/deployments/{deployment}/responses?api-version={}",
            self.api_version
        ))
    }

    /// Reuse the OpenAI Responses request builder with the deployment name
    /// swapped into the wire `model` field.
    fn build_request(
        &self,
        model: &Model,
        ctx: &Context,
        options: &SimpleStreamOptions,
        deployment: &str,
    ) -> Result<super::openai_responses::ResponsesRequest, StreamError> {
        let mut effective = model.clone();
        effective.id = deployment.to_string();
        OpenAiResponsesProvider::build_request(&effective, ctx, options, true)
    }
}

#[async_trait]
impl StreamFn for AzureOpenAiResponsesProvider {
    async fn stream_simple(
        &self,
        model: &Model,
        ctx: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        let deployment = self.resolve_deployment_name(&model.id);
        let body = self.build_request(model, ctx, options, &deployment)?;

        #[cfg(not(target_arch = "wasm32"))]
        {
            let url = self.endpoint(&deployment)?;
            let client = reqwest::Client::new();
            let response = client
                .post(&url)
                .header("api-key", &self.api_key)
                .json(&body)
                .send()
                .await?;
            let status = response.status();
            if !status.is_success() {
                let hint = crate::retry::retry_hint_from_headers(response.headers());
                let text = response.text().await.unwrap_or_default();
                return Err(StreamError::provider_with_hint(
                    status.as_u16(),
                    crate::utils::error_body::truncate_provider_error_body(&text),
                    hint,
                ));
            }
            let model_id = model.id.clone();
            let byte_stream = response.bytes_stream();
            let mapped = byte_stream.map_err(StreamError::Transport);
            Ok(parse_sse(Box::pin(mapped), model_id))
        }

        #[cfg(target_arch = "wasm32")]
        {
            let _ = body;
            Err(StreamError::Malformed(
                "AzureOpenAiResponsesProvider is not yet implemented for wasm32-unknown-unknown"
                    .into(),
            ))
        }
    }
}

/// Parse `modelId=deployment,modelId=deployment`.
///
/// Blank segments and segments without both sides are skipped, matching
/// upstream `parseDeploymentNameMap`.
pub fn parse_deployment_name_map(value: Option<&str>) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Some(value) = value else {
        return map;
    };
    for entry in value.split(',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((model_id, deployment)) = trimmed.split_once('=') {
            let model_id = model_id.trim();
            let deployment = deployment.trim();
            if !model_id.is_empty() && !deployment.is_empty() {
                map.insert(model_id.to_string(), deployment.to_string());
            }
        }
    }
    map
}

/// Normalize an Azure base URL.
///
/// Trailing slashes are stripped. Azure hosts given with no path (or just
/// `/openai`) gain the `/openai/v1` prefix so the deployment path appends
/// correctly, mirroring upstream `normalizeAzureBaseUrl`.
pub fn normalize_base_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/').to_string();
    if let Some(rest) = trimmed.strip_prefix("https://") {
        let (host, path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, ""),
        };
        let is_azure_host = host.ends_with(".openai.azure.com")
            || host.ends_with(".cognitiveservices.azure.com")
            || host.ends_with(".ai.azure.com");
        let path = path.trim_end_matches('/');
        if is_azure_host && (path.is_empty() || path == "/openai") {
            return format!("https://{host}/openai/v1");
        }
    }
    trimmed
}

/// Read an environment variable, treating empty as absent.
fn env_var(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_protocol::{Api, Context, ProviderId};

    fn model() -> Model {
        Model {
            provider: ProviderId::new("azure-openai-responses"),
            id: "gpt-4o".into(),
            api: Api::AzureOpenAiResponses,
            label: None,
            context_window: 128_000,
            max_output_tokens: 16_384,
        }
    }

    #[test]
    fn api_family_serializes_with_the_upstream_kebab_case_string() {
        let json = serde_json::to_string(&Api::AzureOpenAiResponses).unwrap();
        assert_eq!(json, "\"azure-openai-responses\"");
        let back: Api = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Api::AzureOpenAiResponses);
    }

    #[test]
    fn parse_deployment_name_map_skips_malformed_segments() {
        let map =
            parse_deployment_name_map(Some(" gpt-4o=prod-gpt4o , broken, =x, y= ,o3=o3-pro "));
        assert_eq!(map.get("gpt-4o").map(String::as_str), Some("prod-gpt4o"));
        assert_eq!(map.get("o3").map(String::as_str), Some("o3-pro"));
        assert!(!map.contains_key("broken"));
        assert_eq!(map.len(), 2);
        assert!(parse_deployment_name_map(None).is_empty());
        assert!(parse_deployment_name_map(Some("")).is_empty());
    }

    #[test]
    fn deployment_name_precedence_is_override_then_map_then_model_id() {
        let mut map = BTreeMap::new();
        map.insert("gpt-4o".to_string(), "prod-gpt4o".to_string());

        let mapped = AzureOpenAiResponsesProvider::new("k", "https://r.openai.azure.com")
            .with_deployment_names(map.clone());
        assert_eq!(mapped.resolve_deployment_name("gpt-4o"), "prod-gpt4o");
        assert_eq!(mapped.resolve_deployment_name("o3"), "o3");

        let forced = mapped.clone().with_deployment("pinned");
        assert_eq!(forced.resolve_deployment_name("gpt-4o"), "pinned");
    }

    #[test]
    fn normalize_base_url_adds_the_openai_v1_prefix_for_bare_azure_hosts() {
        assert_eq!(
            normalize_base_url("https://res.openai.azure.com"),
            "https://res.openai.azure.com/openai/v1"
        );
        assert_eq!(
            normalize_base_url("https://res.openai.azure.com/openai/"),
            "https://res.openai.azure.com/openai/v1"
        );
        assert_eq!(
            normalize_base_url("https://res.openai.azure.com/openai/v1/"),
            "https://res.openai.azure.com/openai/v1"
        );
        // A custom gateway path is preserved.
        assert_eq!(
            normalize_base_url("https://gateway.example.com/azure/v1/"),
            "https://gateway.example.com/azure/v1"
        );
        // Non-Azure hosts with an empty path are left alone.
        assert_eq!(
            normalize_base_url("https://example.com/"),
            "https://example.com"
        );
    }

    #[test]
    fn base_url_prefers_the_explicit_value_and_otherwise_requires_configuration() {
        let explicit = AzureOpenAiResponsesProvider::new("k", "https://res.openai.azure.com");
        assert_eq!(
            explicit.resolve_base_url().unwrap(),
            "https://res.openai.azure.com/openai/v1"
        );

        // Empty base URL, and the test process has no resource-name env var.
        let unset = AzureOpenAiResponsesProvider::new("k", "");
        if std::env::var(RESOURCE_NAME_ENV).is_err() {
            assert!(unset.resolve_base_url().is_err());
        }
    }

    #[test]
    fn endpoint_uses_the_azure_deployment_path_and_api_version() {
        let provider = AzureOpenAiResponsesProvider::new("k", "https://res.openai.azure.com")
            .with_api_version("2024-10-21");
        assert_eq!(
            provider.endpoint("prod-gpt4o").unwrap(),
            "https://res.openai.azure.com/openai/v1/deployments/prod-gpt4o/responses?api-version=2024-10-21"
        );
    }

    #[test]
    fn request_body_reuses_the_responses_shape_with_the_deployment_as_model() {
        let provider = AzureOpenAiResponsesProvider::new("k", "https://res.openai.azure.com");
        let ctx = Context::new("system");
        let body = provider
            .build_request(
                &model(),
                &ctx,
                &SimpleStreamOptions::default(),
                "prod-gpt4o",
            )
            .unwrap();
        assert_eq!(body.model, "prod-gpt4o");
        assert!(body.stream);
    }
}
