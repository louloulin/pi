//! Provider routing — turn a resolved [`Model`] into a streaming adapter.
//!
//! Before this module existed, `main.rs` hard-coded
//! [`FauxProvider`](pi_ai::providers::faux::FauxProvider) in print, RPC
//! and interactive mode, so the real `OpenAiProvider` /
//! `AnthropicProvider` / `GoogleProvider` adapters were unreachable from
//! the CLI: picking `--model anthropic/claude-sonnet-4-5` still streamed
//! from the faux provider. It also meant `/model` (TUI) and `setModel`
//! (RPC) swapped the model descriptor without swapping the transport.
//!
//! [`ProviderRouter`] fixes both: it is a single [`StreamFn`] that
//! dispatches on `model.provider` at stream time, so any model the
//! catalog can resolve is streamed by its own adapter. The router is
//! built once per process from the environment, mirroring the TS
//! upstream's `getEnvApiKey()` mapping:
//!
//! | provider    | credential env vars (priority order)                              |
//! |-------------|-------------------------------------------------------------------|
//! | `openai`    | `OPENAI_API_KEY`                                                   |
//! | `anthropic` | `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_OAUTH_TOKEN` |
//! | `google`    | `GEMINI_API_KEY`, `GOOGLE_API_KEY`                                 |
//! | `faux`      | *(none — always registered)*                                       |
//!
//! `GOOGLE_API_KEY` is a Rust-port convenience: the upstream only reads
//! `GEMINI_API_KEY`, but the Google SDKs document both names and users
//! routinely export the latter. `GEMINI_API_KEY` always wins.
//!
//! Each adapter also accepts an optional base-URL override
//! (`OPENAI_BASE_URL`, `ANTHROPIC_BASE_URL`, `GEMINI_BASE_URL` /
//! `GOOGLE_BASE_URL`) so a local gateway or proxy can be used without a
//! code change. The upstream catalog carries `baseUrl` per model instead;
//! the Rust `Model` descriptor has no such field yet, so the environment
//! is the extension point.

use std::collections::HashMap;
use std::env;
use std::sync::Arc;

use async_trait::async_trait;
use pi_ai::providers::anthropic::AnthropicProvider;
use pi_ai::providers::faux::FauxProvider;
use pi_ai::providers::google::GoogleProvider;
use pi_ai::providers::openai::OpenAiProvider;
use pi_ai::stream::AssistantMessageEventStream;
use pi_ai::{SharedStreamFn, SimpleStreamOptions, StreamError, StreamFn};
use pi_protocol::{Context, Model};
use thiserror::Error;

/// Providers this router knows how to construct, in catalog order.
const KNOWN_PROVIDERS: [&str; 3] = ["openai", "anthropic", "google"];

/// Credential environment variables for `provider`, in priority order.
///
/// Returns an empty slice for providers that need no credential (the
/// faux provider) or that this build has no adapter for.
pub fn api_key_env_vars(provider: &str) -> &'static [&'static str] {
    match provider {
        "anthropic" => &[
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_OAUTH_TOKEN",
        ],
        "openai" => &["OPENAI_API_KEY"],
        "google" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        _ => &[],
    }
}

/// Base-URL override environment variables for `provider`, in priority
/// order. Empty for providers without an override.
pub fn base_url_env_vars(provider: &str) -> &'static [&'static str] {
    match provider {
        "openai" => &["OPENAI_BASE_URL"],
        "anthropic" => &["ANTHROPIC_BASE_URL"],
        "google" => &["GEMINI_BASE_URL", "GOOGLE_BASE_URL"],
        _ => &[],
    }
}

/// Errors raised while resolving a provider for a model.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProviderError {
    /// The model's provider is known but no credential env var is set.
    #[error("provider `{provider}` needs an API key: set one of {vars}")]
    MissingApiKey {
        /// Provider id from the model descriptor.
        provider: String,
        /// Comma-joined env vars that would satisfy the provider, in
        /// priority order (see [`api_key_env_vars`]).
        vars: String,
    },
    /// The model's provider has no adapter in this build.
    #[error("no streaming adapter for provider `{provider}` (model `{model}`)")]
    UnsupportedProvider {
        /// Provider id from the model descriptor.
        provider: String,
        /// Model id, echoed to make the failing selection obvious.
        model: String,
    },
}

impl ProviderError {
    /// Process exit code for this error (sysexits `EX_CONFIG`).
    pub fn exit_code(&self) -> u8 {
        78
    }
}

/// Reads one environment variable, if set and non-empty (after trim).
fn env_value(get_env: &dyn Fn(&str) -> Option<String>, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        get_env(name)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

/// Dispatch table from provider id to streaming adapter.
///
/// Cloneable and cheap to clone: the adapters live behind [`Arc`], so
/// the same router can back print, RPC and interactive mode.
#[derive(Clone)]
pub struct ProviderRouter {
    adapters: HashMap<String, SharedStreamFn>,
}

impl std::fmt::Debug for ProviderRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut ids: Vec<&str> = self.adapters.keys().map(String::as_str).collect();
        ids.sort_unstable();
        f.debug_struct("ProviderRouter")
            .field("providers", &ids)
            .finish()
    }
}

impl Default for ProviderRouter {
    fn default() -> Self {
        Self::from_env()
    }
}

impl ProviderRouter {
    /// Build a router from the process environment.
    ///
    /// The faux provider is always registered (offline / test runs);
    /// `openai` / `anthropic` / `google` are registered only when their
    /// credential env var is present. Requesting a model whose provider
    /// is absent then fails [`require`](Self::require) with the exact
    /// env var to set instead of silently streaming from faux.
    pub fn from_env() -> Self {
        Self::from_env_with(|name| env::var(name).ok())
    }

    /// [`from_env`](Self::from_env) with an injectable environment, so
    /// tests can exercise every branch without mutating process state.
    pub fn from_env_with(get_env: impl Fn(&str) -> Option<String>) -> Self {
        let mut adapters: HashMap<String, SharedStreamFn> = HashMap::new();
        adapters.insert(
            "faux".to_string(),
            Arc::new(FauxProvider::default()) as SharedStreamFn,
        );

        for provider in KNOWN_PROVIDERS {
            let Some(api_key) = env_value(&get_env, api_key_env_vars(provider)) else {
                continue;
            };
            let base_url = env_value(&get_env, base_url_env_vars(provider));
            let adapter: SharedStreamFn = match provider {
                "openai" => match base_url {
                    Some(url) => Arc::new(OpenAiProvider::with_base_url(api_key, url)),
                    None => Arc::new(OpenAiProvider::new(api_key)),
                },
                "anthropic" => match base_url {
                    Some(url) => Arc::new(AnthropicProvider::with_base_url(api_key, url)),
                    None => Arc::new(AnthropicProvider::new(api_key)),
                },
                "google" => match base_url {
                    Some(url) => Arc::new(GoogleProvider::with_base_url(api_key, url)),
                    None => Arc::new(GoogleProvider::new(api_key)),
                },
                _ => unreachable!("KNOWN_PROVIDERS is exhaustive"),
            };
            adapters.insert(provider.to_string(), adapter);
        }

        Self { adapters }
    }

    /// Resolve the adapter for `model`, or explain what is missing.
    ///
    /// Call this once for the model the CLI was asked to run so a missing
    /// credential fails at startup with a precise message, rather than
    /// mid-turn as a generic stream error.
    pub fn require(&self, model: &Model) -> Result<SharedStreamFn, ProviderError> {
        let provider = model.provider.0.as_str();
        match self.adapters.get(provider) {
            Some(adapter) => Ok(adapter.clone()),
            None if !api_key_env_vars(provider).is_empty() => Err(ProviderError::MissingApiKey {
                provider: provider.to_string(),
                vars: api_key_env_vars(provider).join(", "),
            }),
            None => Err(ProviderError::UnsupportedProvider {
                provider: provider.to_string(),
                model: model.id.clone(),
            }),
        }
    }

    /// Provider ids with a registered adapter, sorted.
    pub fn provider_ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.adapters.keys().map(String::as_str).collect();
        ids.sort_unstable();
        ids
    }

    /// Whether `provider` has a registered adapter.
    pub fn has_provider(&self, provider: &str) -> bool {
        self.adapters.contains_key(provider)
    }

    /// Human-readable hint appended to a runtime dispatch failure.
    fn hint(&self, provider: &str) -> String {
        let vars = api_key_env_vars(provider);
        if vars.is_empty() {
            format!("no adapter registered for provider `{provider}`")
        } else {
            format!(
                "provider `{provider}` is not configured; set one of {}",
                vars.join(", ")
            )
        }
    }
}

#[async_trait]
impl StreamFn for ProviderRouter {
    async fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        // Dispatch per call, not per process: `/model` (TUI) and
        // `setModel` (RPC) can switch a live session between providers.
        match self.adapters.get(model.provider.0.as_str()) {
            Some(adapter) => adapter.stream_simple(model, context, options).await,
            None => Err(StreamError::Malformed(format!(
                "cannot stream model `{}`: {}",
                model.id,
                self.hint(&model.provider.0)
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_protocol::{Api, ProviderId};

    fn model(provider: &str, id: &str, api: Api) -> Model {
        Model {
            provider: ProviderId::new(provider),
            id: id.to_string(),
            api,
            label: None,
            context_window: 0,
            max_output_tokens: 0,
        }
    }

    fn empty_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn env_var_names_match_the_typescript_upstream() {
        assert_eq!(api_key_env_vars("openai"), &["OPENAI_API_KEY"]);
        assert_eq!(
            api_key_env_vars("anthropic"),
            &[
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "ANTHROPIC_OAUTH_TOKEN"
            ]
        );
        assert_eq!(
            api_key_env_vars("google"),
            &["GEMINI_API_KEY", "GOOGLE_API_KEY"]
        );
        assert!(api_key_env_vars("faux").is_empty());
    }

    #[test]
    fn faux_is_always_available_without_credentials() {
        let router = ProviderRouter::from_env_with(empty_env);
        assert!(router.has_provider("faux"));
        let faux = model("faux", "faux-model", Api::Faux);
        assert!(router.require(&faux).is_ok());
    }

    /// `require` returns an `Arc<dyn StreamFn>` on success, which is not
    /// `Debug`, so tests extract the error by matching.
    fn require_err(router: &ProviderRouter, model: &Model) -> ProviderError {
        match router.require(model) {
            Ok(_) => panic!("expected provider resolution to fail for {model:?}"),
            Err(err) => err,
        }
    }

    #[test]
    fn missing_key_names_the_env_var_to_set() {
        let router = ProviderRouter::from_env_with(empty_env);
        let err = require_err(
            &router,
            &model("anthropic", "claude-sonnet-4-5", Api::AnthropicMessages),
        );
        assert_eq!(
            err,
            ProviderError::MissingApiKey {
                provider: "anthropic".to_string(),
                vars: "ANTHROPIC_API_KEY, ANTHROPIC_AUTH_TOKEN, ANTHROPIC_OAUTH_TOKEN"
                    .to_string(),
            }
        );
        assert!(err.to_string().contains("ANTHROPIC_API_KEY"));
        assert_eq!(err.exit_code(), 78);
    }

    #[test]
    fn known_key_registers_the_adapter() {
        let router = ProviderRouter::from_env_with(|name| match name {
            "OPENAI_API_KEY" => Some("sk-test".to_string()),
            _ => None,
        });
        assert!(router.has_provider("openai"));
        assert!(!router.has_provider("anthropic"));
        assert!(router
            .require(&model("openai", "gpt-4o-mini", Api::OpenAiChatCompletions))
            .is_ok());
    }

    #[test]
    fn anthropic_auth_token_and_oauth_token_are_accepted() {
        for var in ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_OAUTH_TOKEN"] {
            let router = ProviderRouter::from_env_with(move |name| {
                (name == var).then(|| "token".to_string())
            });
            assert!(
                router
                    .require(&model(
                        "anthropic",
                        "claude-sonnet-4-5",
                        Api::AnthropicMessages
                    ))
                    .is_ok(),
                "{var} should satisfy the anthropic provider"
            );
        }
    }

    #[test]
    fn gemini_key_registers_google() {
        let router = ProviderRouter::from_env_with(|name| match name {
            "GEMINI_API_KEY" => Some("gm-test".to_string()),
            _ => None,
        });
        assert!(router
            .require(&model("google", "gemini-2.5-flash", Api::GoogleGenerativeAi))
            .is_ok());
        assert_eq!(router.provider_ids(), vec!["faux", "google"]);
    }

    #[test]
    fn unknown_provider_is_reported_as_unsupported() {
        let router = ProviderRouter::from_env_with(empty_env);
        let err = require_err(&router, &model("bedrock", "claude-3", Api::BedrockConverse));
        assert!(matches!(err, ProviderError::UnsupportedProvider { .. }));
        assert!(err.to_string().contains("bedrock"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn router_streams_faux_models_through_the_stream_fn_trait() {
        use futures::StreamExt;

        let router = ProviderRouter::from_env_with(empty_env);
        let faux = model("faux", "faux-model", Api::Faux);
        let context = Context::new("test");
        let stream = router
            .stream_simple(&faux, &context, &SimpleStreamOptions::default())
            .await
            .expect("faux dispatch");
        let events: Vec<_> = stream.collect().await;
        assert!(!events.is_empty());
        assert!(events.iter().all(|event| event.is_ok()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn router_rejects_unconfigured_provider_at_stream_time() {
        let router = ProviderRouter::from_env_with(empty_env);
        let remote = model("openai", "gpt-4o-mini", Api::OpenAiChatCompletions);
        let context = Context::new("test");
        // `AssistantMessageEventStream` is not `Debug`, so match instead of
        // `expect_err`.
        match router
            .stream_simple(&remote, &context, &SimpleStreamOptions::default())
            .await
        {
            Ok(_) => panic!("openai is unconfigured and must not stream"),
            Err(err) => assert!(err.to_string().contains("OPENAI_API_KEY"), "{err}"),
        }
    }
}
