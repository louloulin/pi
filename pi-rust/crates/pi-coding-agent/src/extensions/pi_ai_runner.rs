//! Built-in pi-ai provider runner for the extension host (LUM-1180).
//!
//! `pi-extensions` serves the shim's `@earendil-works/pi-ai/compat` provider
//! factories (`anthropicMessagesApi` / `openAIResponsesApi`, plus
//! `openAICompletionsApi` / `googleGenerativeAIApi` /
//! `azureOpenAIResponsesApi` since LUM-1211) through an injected
//! [`PiAiStreamRunner`], because it cannot depend on `pi-ai` — the crate that
//! owns the concrete providers. This module is that runner: it turns the
//! upstream-shaped request the shim sends into a
//! [`Model`]/[`Context`]/[`SimpleStreamOptions`] triple, picks the provider
//! adapter and adapts the Rust event stream back into the
//! upstream-shaped JS events.
//!
//! Credential resolution mirrors [`ProviderRouter`](crate::provider):
//! `options.apiKey` wins, then the provider registry's env vars (through
//! [`pi_ai::get_env_api_key`], so `credentials.json`-less env-only builds and
//! the CLI agree). `options.baseUrl` / `model.baseUrl` win over the
//! provider's `*_BASE_URL` override, which wins over the registry default.
//!
//! Custom `options.headers` are **not** forwarded: the Anthropic and
//! OpenAI-Responses adapters take only a credential and a base URL. That is
//! the one documented divergence that can change what a caller sees (the
//! upstream `custom-provider-gitlab-duo` example passes an `Authorization`
//! header alongside `apiKey: "gitlab-duo"`).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use pi_ai::ext_bridge::{
    api_key_from_js, base_url_from_js, context_from_js, model_from_js, stream_options_from_js,
    unbridged_api_error, JsAssistantEventStream, JsEventEncoder,
};
use pi_ai::providers::registry;
use pi_extensions::{PiAiEventStream, PiAiStreamRequest, PiAiStreamRunner};
use tokio_util::sync::CancellationToken;

use crate::provider::{api_wire_name, build_adapter};

/// Env lookup used to resolve credentials and base-URL overrides.
///
/// Injectable so the runner can be exercised without touching the process
/// environment.
type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Runner behind the five bridged JS provider factories:
/// `anthropicMessagesApi` / `openAIResponsesApi` /
/// `openAICompletionsApi` / `googleGenerativeAIApi` /
/// `azureOpenAIResponsesApi`.
pub struct BuiltinPiAiStreamRunner {
    get_env: EnvLookup,
}

impl std::fmt::Debug for BuiltinPiAiStreamRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltinPiAiStreamRunner").finish()
    }
}

impl Default for BuiltinPiAiStreamRunner {
    fn default() -> Self {
        Self::from_env()
    }
}

impl BuiltinPiAiStreamRunner {
    /// Read credentials from the process environment.
    pub fn from_env() -> Self {
        Self::with_env(|name| std::env::var(name).ok())
    }

    /// Build a runner with a custom environment lookup.
    pub fn with_env(get_env: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Self {
        Self {
            get_env: Arc::new(get_env),
        }
    }

    fn env(&self, name: &str) -> Option<String> {
        (self.get_env)(name).filter(|value| !value.is_empty())
    }
}

impl PiAiStreamRunner for BuiltinPiAiStreamRunner {
    fn start<'a>(
        &'a self,
        request: PiAiStreamRequest,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<PiAiEventStream, String>> + Send + 'a>> {
        Box::pin(async move {
            let model = model_from_js(&request.model)?;
            let context = context_from_js(&request.context)?;
            let mut options = stream_options_from_js(&request.options);

            let provider_id = model.provider.0.as_str();
            let api_key = api_key_from_js(&request.options)
                .or_else(|| pi_ai::get_env_api_key(provider_id, None))
                .ok_or_else(|| {
                    format!(
                        "no API key for provider `{provider_id}`: pass `apiKey` in the stream \
                         options or set one of {:?}",
                        registry::api_key_env_vars(provider_id)
                    )
                })?;
            let base_url = base_url_from_js(&request.options)
                .or_else(|| {
                    request
                        .model
                        .get("baseUrl")
                        .and_then(serde_json::Value::as_str)
                        .filter(|url| !url.is_empty())
                        .map(str::to_string)
                })
                .or_else(|| {
                    registry::base_url_env_vars(provider_id)
                        .iter()
                        .find_map(|name| self.env(name))
                })
                .or_else(|| registry::default_base_url(provider_id).map(str::to_string))
                .unwrap_or_default();

            // Pick the adapter through the same table `ProviderRouter`
            // uses, so the bridge and the CLI cannot disagree on which
            // provider an api family gets — the three families LUM-1211
            // added (`openai-completions` / `google-generative-ai` /
            // `azure-openai-responses`) come for free that way. `None` is
            // the forward-compat guard for a family this build has no
            // adapter for; the message shares `model_from_js`'s gap list.
            let adapter = build_adapter(model.api, api_key, base_url)
                .ok_or_else(|| unbridged_api_error(api_wire_name(model.api)))?;

            // The provider is what actually stops the HTTP request; the host
            // additionally drops this stream when the shim cancels, so a
            // provider that ignores the token (Anthropic's `send_streaming`
            // does) still releases the connection.
            options.signal = Some(pi_ai::AbortSignal::native(cancel));
            let events = adapter
                .stream_simple(&model, &context, &options)
                .await
                .map_err(|error| error.to_string())?;
            let encoder = JsEventEncoder::new(
                request.api.clone(),
                provider_id.to_string(),
                model.id.clone(),
            );
            let stream: PiAiEventStream = Box::pin(JsAssistantEventStream::new(events, encoder));
            Ok(stream)
        })
    }
}
