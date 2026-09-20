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
//! | `openai`    | `OPENAI_API_KEY`                                                   |
//! | `openai-responses` | `OPENAI_API_KEY` (same credential, `/v1/responses` API)      |
//! | `anthropic` | `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_OAUTH_TOKEN` |
//! | `google`    | `GEMINI_API_KEY`, `GOOGLE_API_KEY`                                 |
//! | `faux`      | *(none — always registered)*                                       |
//!
//! `GOOGLE_API_KEY` is a Rust-port convenience: the upstream only reads
//! `GEMINI_API_KEY`, but the Google SDKs document both names and users
//! routinely export the latter. `GEMINI_API_KEY` always wins.
//!
//! The concrete provider list, credential env vars, base URLs and model
//! catalogs all live in the data-driven
//! [`pi_ai::providers::registry`]. Besides the first-party providers
//! above (`openai` and `openai-responses` share a credential but speak
//! different wire protocols, so they are two registry entries), the
//! registry carries the OpenAI Chat
//! Completions–compatible family (DeepSeek, Groq, Cerebras, Moonshot AI,
//! Z.AI, OpenRouter, Together, Fireworks, Baseten, NVIDIA, Hugging Face,
//! Xiaomi, Ant Ling) and `xai`, which speaks the Responses API against
//! its own host (`XAI_API_KEY`). All of those reuse an existing adapter
//! and differ only by base URL and credential — no per-provider code path.
//!
//! Each adapter also accepts an optional base-URL override
//! (`OPENAI_BASE_URL`, `ANTHROPIC_BASE_URL`, `GEMINI_BASE_URL` /
//! `GOOGLE_BASE_URL`, and a `<PROVIDER>_BASE_URL` name for every registry
//! entry) so a local gateway or proxy can be used without a code change.
//! The upstream catalog carries `baseUrl` per model instead; the Rust
//! `Model` descriptor has no such field yet, so the environment is the
//! extension point.

use std::collections::HashMap;
use std::env;
use std::sync::Arc;

use async_trait::async_trait;
use pi_ai::providers::anthropic::AnthropicProvider;
use pi_ai::providers::azure_openai_responses::AzureOpenAiResponsesProvider;
use pi_ai::providers::faux::FauxProvider;
use pi_ai::providers::google::GoogleProvider;
use pi_ai::providers::mistral::MistralProvider;
use pi_ai::providers::openai::OpenAiProvider;
use pi_ai::providers::openai_responses::OpenAiResponsesProvider;
use pi_ai::providers::registry::{self, ProviderSpec, BUILTIN_PROVIDERS};
use pi_ai::stream::AssistantMessageEventStream;
use pi_ai::{
    ProviderRetryPolicy, RetryStreamFn, SharedStreamFn, SimpleStreamOptions, StreamError, StreamFn,
};
use pi_protocol::{Api, Context, Model};
use thiserror::Error;

/// Credential environment variables for `provider`, in priority order.
///
/// Returns an empty slice for providers that need no credential (the
/// faux provider) or that this build has no adapter for. Backed by the
/// provider registry so `pi-coding-agent` and `pi-ai` cannot disagree.
pub fn api_key_env_vars(provider: &str) -> &'static [&'static str] {
    registry::api_key_env_vars(provider)
}

/// Base-URL override environment variables for `provider`, in priority
/// order. Empty for providers without an override.
pub fn base_url_env_vars(provider: &str) -> &'static [&'static str] {
    registry::base_url_env_vars(provider)
}

/// Construct the streaming adapter for one registry entry.
///
/// `api_key` and `base_url` are ignored by adapters that do not use them
/// (the faux provider). Returns `None` for API families this build has no
/// adapter for; every family currently in
/// [`BUILTIN_PROVIDERS`](pi_ai::providers::registry::BUILTIN_PROVIDERS)
/// is implemented, so this is a forward-compatibility guard.
fn build_adapter(spec: &ProviderSpec, api_key: String, base_url: String) -> Option<SharedStreamFn> {
    let adapter: SharedStreamFn = match spec.api {
        Api::Faux => Arc::new(FauxProvider::default()),
        Api::OpenAiChatCompletions => Arc::new(OpenAiProvider::with_base_url(api_key, base_url)),
        Api::OpenAiResponses => {
            Arc::new(OpenAiResponsesProvider::with_base_url(api_key, base_url))
        }
        Api::AzureOpenAiResponses => Arc::new(AzureOpenAiResponsesProvider::new(api_key, base_url)),
        Api::AnthropicMessages => Arc::new(AnthropicProvider::with_base_url(api_key, base_url)),
        Api::GoogleGenerativeAi => Arc::new(GoogleProvider::with_base_url(api_key, base_url)),
        Api::MistralConversations => Arc::new(MistralProvider::with_base_url(api_key, base_url)),
        Api::BedrockConverse | Api::CohereV2 => return None,
    };
    Some(adapter)
}

/// Wrap `adapter` in [`RetryStreamFn`] when `policy` retries at all,
/// otherwise return it unchanged so the default build has no extra layer.
fn adapt_retry(adapter: SharedStreamFn, policy: ProviderRetryPolicy) -> SharedStreamFn {
    if !policy.is_enabled() {
        return adapter;
    }
    RetryStreamFn::shared(adapter, policy)
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
    /// every other registry entry is registered only when one of its
    /// credential env vars is present. Requesting a model whose provider
    /// is absent then fails [`require`](Self::require) with the exact
    /// env var to set instead of silently streaming from faux.
    pub fn from_env() -> Self {
        Self::from_env_with(|name| env::var(name).ok())
    }

    /// [`from_env`](Self::from_env) with an injectable environment, so
    /// tests can exercise every branch without mutating process state.
    pub fn from_env_with(get_env: impl Fn(&str) -> Option<String>) -> Self {
        Self::from_env_with_policy(get_env, ProviderRetryPolicy::default())
    }

    /// [`from_env_with`](Self::from_env_with) with an explicit
    /// provider-request retry policy.
    ///
    /// The policy is applied to every registered adapter through
    /// [`RetryStreamFn`], so `settings.retry.provider` reaches every
    /// provider without a per-adapter retry loop. The default,
    /// [`ProviderRetryPolicy::DEFAULT`], retries nothing — upstream's
    /// `settings.retry.provider.maxRetries` also starts undefined.
    pub fn from_env_with_policy(
        get_env: impl Fn(&str) -> Option<String>,
        retry_policy: ProviderRetryPolicy,
    ) -> Self {
        let mut adapters: HashMap<String, SharedStreamFn> = HashMap::new();

        for spec in BUILTIN_PROVIDERS {
            let api_key = env_value(&get_env, spec.api_key_env).unwrap_or_default();
            if spec.requires_api_key() && api_key.is_empty() {
                continue;
            }
            let base_url = env_value(&get_env, spec.base_url_env)
                .unwrap_or_else(|| spec.default_base_url.to_string());
            if let Some(adapter) = build_adapter(spec, api_key, base_url) {
                adapters.insert(spec.id.to_string(), adapt_retry(adapter, retry_policy));
            }
        }

        Self { adapters }
    }

    /// Re-wrap every adapter with a new provider-request retry policy.
    ///
    /// `pi-coding-agent` reads the `settings.retry.provider` slice and
    /// applies it here after building the router from the environment.
    /// Calling it twice wraps twice, so callers apply it once.
    pub fn with_provider_retry(mut self, policy: ProviderRetryPolicy) -> Self {
        if !policy.is_enabled() {
            return self;
        }
        for adapter in self.adapters.values_mut() {
            *adapter = adapt_retry(adapter.clone(), policy);
        }
        self
    }

    /// Replace the adapter registered for `provider_id`.
    ///
    /// Test-only: [`build_adapter`] covers every registry entry, so only
    /// the router's own tests need to install a scripted adapter.
    #[cfg(test)]
    fn set_adapter(&mut self, provider_id: &str, adapter: SharedStreamFn) {
        self.adapters.insert(provider_id.to_string(), adapter);
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

    /// Fails `remaining_failures` times with a retryable 503 (whose
    /// server-requested delay is zero, so retrying is instant), then
    /// delegates to the faux provider.
    struct FlakyAdapter {
        remaining_failures: std::sync::atomic::AtomicUsize,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl FlakyAdapter {
        fn new(remaining_failures: usize) -> Self {
            Self {
                remaining_failures: std::sync::atomic::AtomicUsize::new(remaining_failures),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl StreamFn for FlakyAdapter {
        async fn stream_simple(
            &self,
            model: &Model,
            ctx: &Context,
            options: &SimpleStreamOptions,
        ) -> Result<AssistantMessageEventStream, StreamError> {
            use std::sync::atomic::Ordering;

            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.remaining_failures.load(Ordering::SeqCst) > 0 {
                self.remaining_failures.fetch_sub(1, Ordering::SeqCst);
                return Err(StreamError::provider_with_hint(
                    503,
                    "overloaded".to_string(),
                    pi_ai::ProviderRetryHint {
                        retry_after_ms: Some(0),
                        should_retry: None,
                    },
                ));
            }
            FauxProvider::default()
                .stream_simple(model, ctx, options)
                .await
        }
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
    fn family_credentials_register_the_shared_adapter() {
        // The OpenAI-compatible family reuses `OpenAiProvider`; only the
        // credential, base URL and model ids differ, and all of that now
        // comes from the registry.
        let router = ProviderRouter::from_env_with(|name| match name {
            "DEEPSEEK_API_KEY" | "GROQ_API_KEY" => Some("test-key".to_string()),
            _ => None,
        });
        assert!(router.has_provider("deepseek"));
        assert!(router.has_provider("groq"));
        assert!(!router.has_provider("openai"));
        assert!(!router.has_provider("anthropic"));
        assert_eq!(router.provider_ids(), vec!["deepseek", "faux", "groq"]);
        assert!(router
            .require(&model(
                "deepseek",
                "deepseek-v4-pro",
                Api::OpenAiChatCompletions
            ))
            .is_ok());
    }

    #[test]
    fn xai_key_registers_the_responses_adapter_for_its_own_host() {
        // `xai` rides the Responses adapter but keeps its own credential,
        // so an XAI key must not register (or unregister) `openai-responses`.
        let router = ProviderRouter::from_env_with(|name| match name {
            "XAI_API_KEY" => Some("test-key".to_string()),
            _ => None,
        });
        assert!(router.has_provider("xai"));
        assert!(!router.has_provider("openai-responses"));
        assert!(!router.has_provider("openai"));
        assert_eq!(router.provider_ids(), vec!["faux", "xai"]);
        assert!(router
            .require(&model("xai", "grok-4.6", Api::OpenAiResponses))
            .is_ok());
        assert_eq!(api_key_env_vars("xai"), &["XAI_API_KEY"]);
        assert_eq!(base_url_env_vars("xai"), &["XAI_BASE_URL"]);
    }

    #[test]
    fn mistral_key_registers_the_native_adapter() {
        // Mistral has its own API family, so it must not be served by the
        // OpenAI Chat Completions adapter the `openai` entry uses.
        let router = ProviderRouter::from_env_with(|name| match name {
            "MISTRAL_API_KEY" => Some("test-key".to_string()),
            _ => None,
        });
        assert!(router.has_provider("mistral"));
        assert!(!router.has_provider("openai"));
        assert_eq!(router.provider_ids(), vec!["faux", "mistral"]);
        assert!(router
            .require(&model(
                "mistral",
                "mistral-large-latest",
                Api::MistralConversations
            ))
            .is_ok());
        assert_eq!(api_key_env_vars("mistral"), &["MISTRAL_API_KEY"]);
        assert_eq!(base_url_env_vars("mistral"), &["MISTRAL_BASE_URL"]);
    }

    #[test]
    fn family_missing_key_names_the_env_var_to_set() {
        let router = ProviderRouter::from_env_with(empty_env);
        let err = require_err(
            &router,
            &model("zai-coding-cn", "glm-5.3", Api::OpenAiChatCompletions),
        );
        assert_eq!(
            err,
            ProviderError::MissingApiKey {
                provider: "zai-coding-cn".to_string(),
                vars: "ZAI_CODING_CN_API_KEY".to_string(),
            }
        );
    }

    #[test]
    fn openai_responses_shares_the_openai_credential_but_not_the_adapter() {
        let router = ProviderRouter::from_env_with(|name| match name {
            "OPENAI_API_KEY" => Some("sk-test".to_string()),
            _ => None,
        });
        // One credential, both OpenAI API families registered.
        assert!(router.has_provider("openai"));
        assert!(router.has_provider("openai-responses"));
        assert_eq!(
            router.provider_ids(),
            vec!["faux", "openai", "openai-responses"]
        );
        assert!(router
            .require(&model("openai-responses", "gpt-5", Api::OpenAiResponses))
            .is_ok());
        assert_eq!(api_key_env_vars("openai-responses"), &["OPENAI_API_KEY"]);
        assert_eq!(base_url_env_vars("openai-responses"), &["OPENAI_BASE_URL"]);
    }

    #[test]
    fn openai_responses_without_a_key_names_the_shared_env_var() {
        let router = ProviderRouter::from_env_with(empty_env);
        let err = require_err(
            &router,
            &model("openai-responses", "gpt-5", Api::OpenAiResponses),
        );
        assert_eq!(
            err,
            ProviderError::MissingApiKey {
                provider: "openai-responses".to_string(),
                vars: "OPENAI_API_KEY".to_string(),
            }
        );
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

    #[tokio::test(flavor = "current_thread")]
    async fn provider_retry_policy_retries_a_transient_stream_error() {
        use futures::StreamExt;

        let flaky = Arc::new(FlakyAdapter::new(1));
        let mut router = ProviderRouter::from_env_with(empty_env);
        router.set_adapter("faux", flaky.clone());
        let router = router.with_provider_retry(ProviderRetryPolicy::new(2));

        let stream = router
            .stream_simple(
                &model("faux", "faux-model", Api::Faux),
                &Context::new("test"),
                &SimpleStreamOptions::default(),
            )
            .await
            .expect("the retry loop must recover from the 503");
        let events: Vec<_> = stream.collect().await;
        assert!(!events.is_empty());
        assert_eq!(flaky.calls(), 2, "one failure plus one retry");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_default_policy_never_retries() {
        let flaky = Arc::new(FlakyAdapter::new(1));
        let mut router = ProviderRouter::from_env_with(empty_env);
        router.set_adapter("faux", flaky.clone());
        let router = router.with_provider_retry(ProviderRetryPolicy::default());

        match router
            .stream_simple(
                &model("faux", "faux-model", Api::Faux),
                &Context::new("test"),
                &SimpleStreamOptions::default(),
            )
            .await
        {
            Ok(_) => panic!("a disabled policy must surface the provider error"),
            Err(err) => assert!(matches!(err, StreamError::Provider { status: 503, .. })),
        }
        assert_eq!(flaky.calls(), 1);
    }
}
