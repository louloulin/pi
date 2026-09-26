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
//! Credentials are not read straight from the environment: the router runs
//! [`pi_ai::resolve_api_key_for_provider`], so a credential held by the
//! router's [`CredentialStore`] **owns the provider** and the env vars above
//! are only the fallback when nothing is stored. With an empty store — the
//! default for every `from_env*` constructor — the outcome is the env-only one
//! described by the table, byte for byte. OAuth is not wired yet: the
//! providers that need it are absent from the registry, so their adapters are
//! reported as unsupported rather than misconfigured.
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
use futures::executor::block_on;
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
    default_provider_auth_context, resolve_api_key_for_provider, AuthContext,
    AuthResolutionOverrides, CredentialStore, InMemoryCredentialStore, ProviderEnv,
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
pub(crate) fn build_adapter(api: Api, api_key: String, base_url: String) -> Option<SharedStreamFn> {
    let adapter: SharedStreamFn = match api {
        Api::Faux => Arc::new(FauxProvider::default()),
        Api::OpenAiChatCompletions => Arc::new(OpenAiProvider::with_base_url(api_key, base_url)),
        Api::OpenAiResponses => Arc::new(OpenAiResponsesProvider::with_base_url(api_key, base_url)),
        Api::AzureOpenAiResponses => Arc::new(AzureOpenAiResponsesProvider::new(api_key, base_url)),
        Api::AnthropicMessages => Arc::new(AnthropicProvider::with_base_url(api_key, base_url)),
        Api::GoogleGenerativeAi => Arc::new(GoogleProvider::with_base_url(api_key, base_url)),
        Api::MistralConversations => Arc::new(MistralProvider::with_base_url(api_key, base_url)),
        Api::BedrockConverse | Api::CohereV2 => return None,
    };
    Some(adapter)
}

/// Default base URL for an API family, taken from the first registry
/// entry that speaks it. Used when an extension registers a provider
/// without a `baseUrl`: the adapter still needs a host, and "the same
/// host the first built-in provider of this family uses" is the least
/// surprising default. `None` for families with no built-in provider
/// (and for the keyless faux provider).
fn default_base_url_for_api(api: Api) -> Option<&'static str> {
    BUILTIN_PROVIDERS
        .iter()
        .find(|spec| spec.api == api && !spec.default_base_url.is_empty())
        .map(|spec| spec.default_base_url)
}

/// Wire name for an [`Api`], matching the extension-facing ids in
/// `pi_extensions::SUPPORTED_PROVIDER_APIS` where one exists. Used to
/// stamp a per-model `api` hint into the catalog and to phrase errors.
///
/// Thin wrapper over [`Api::api_id`] — the extension bridge
/// (`pi_ai::ext_bridge::model_from_js`) parses the same table backwards with
/// [`Api::from_api_id`], so there is exactly one match per direction.
pub fn api_wire_name(api: Api) -> &'static str {
    api.api_id()
}

/// Resolve the `apiKey` string an extension handed to `pi.registerProvider`.
///
/// Three forms are recognized, mirroring upstream's `ProviderConfig.apiKey`:
/// a literal is used verbatim, `$VAR` / `${VAR}` is read from `get_env`, and
/// the leading-`!command` form is **rejected** — this build never executes a
/// shell command to produce a credential. `Ok(None)` means "no key" (the
/// field was absent or blank); `Err` carries a human-readable reason the
/// caller logs before skipping the provider.
///
/// `get_env` is injectable so tests stay hermetic (the same reason
/// [`ProviderRouter::from_env_with`] takes one).
pub fn resolve_extension_api_key(
    raw: Option<&str>,
    get_env: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<String>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    if raw.starts_with('!') {
        return Err(
            "apiKey `!command` form is not supported: the extension host never executes a \
             command to obtain a credential; use a literal or $VAR instead"
                .into(),
        );
    }
    let var = if let Some(inner) = raw.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        Some(inner)
    } else {
        raw.strip_prefix('$')
    };
    let Some(var) = var else {
        return Ok(Some(raw.to_string()));
    };
    if var.is_empty() {
        return Err("apiKey references an empty environment variable name".into());
    }
    match get_env(var).map(|value| value.trim().to_string()) {
        Some(value) if !value.is_empty() => Ok(Some(value)),
        _ => Err(format!(
            "apiKey references unset environment variable `{var}`"
        )),
    }
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
    /// An extension registered a provider for an API family this build
    /// cannot stream (native `streamSimple` APIs, Bedrock, …).
    #[error("no streaming adapter for api `{api}`")]
    UnsupportedApi {
        /// API family name the registration named.
        api: String,
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
/// the same router can back print, RPC and interactive mode, and the
/// credential store / auth context are shared the same way.
#[derive(Clone)]
pub struct ProviderRouter {
    adapters: HashMap<String, SharedStreamFn>,
    credentials: Arc<dyn CredentialStore>,
    auth_context: Arc<dyn AuthContext>,
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

/// Auth context with no ambient environment at all.
///
/// [`ProviderRouter::from_env_with`] and
/// [`ProviderRouter::from_env_with_policy`] take their environment as an
/// injectable closure, which they hand to the resolver as scoped
/// [`AuthResolutionOverrides::env`] and pair with this context. That keeps an
/// injected router hermetic: the fallback the resolver walks next contributes
/// nothing, so the host process environment can never turn an unconfigured
/// provider into a configured one (and the env-only regression tests do not
/// depend on the machine they run on). Only
/// [`ProviderRouter::from_env`] uses the ambient
/// [`default_provider_auth_context`].
struct EmptyAuthContext;

#[async_trait]
impl AuthContext for EmptyAuthContext {
    async fn env(&self, _name: &str) -> Option<String> {
        None
    }

    async fn file_exists(&self, _path: &str) -> bool {
        false
    }
}

/// The provider's credential env vars as a scoped [`ProviderEnv`].
///
/// This reproduces the pre-wiring `env_value` lookup — first set var in
/// registry priority order, trimmed, empty means unset — as the scoped
/// environment handed to the resolver, which applies the stored-credential
/// -first policy on top of it.
fn scoped_provider_env(get_env: &dyn Fn(&str) -> Option<String>, env_vars: &[&str]) -> ProviderEnv {
    env_vars
        .iter()
        .filter_map(|name| {
            let value = get_env(name)?.trim().to_string();
            if value.is_empty() {
                None
            } else {
                Some(((*name).to_string(), value))
            }
        })
        .collect()
}

/// Resolve `spec`'s api key through the auth subsystem.
///
/// The constructors are synchronous and the resolver is async, so this bridges
/// with [`block_on`]. Every step of the api-key path is synchronous
/// underneath — the in-memory store reads an `RwLock`, and both auth contexts
/// answer from a map or `std::env` — so the bridge never waits on a reactor and
/// is safe to call from inside an async context (`pi-evals` builds its router
/// that way). A store failure is returned, not swallowed, so the caller can
/// decide; the constructor treats it as unconfigured and logs.
fn resolve_provider_key(
    spec: &ProviderSpec,
    get_env: &dyn Fn(&str) -> Option<String>,
    credentials: &dyn CredentialStore,
    auth_context: &dyn AuthContext,
) -> Result<Option<String>, pi_ai::AuthError> {
    let overrides = AuthResolutionOverrides {
        env: Some(scoped_provider_env(get_env, spec.api_key_env)),
        ..AuthResolutionOverrides::default()
    };
    let resolved = block_on(resolve_api_key_for_provider(
        spec.id,
        credentials,
        auth_context,
        Some(overrides),
    ))?;
    Ok(resolved
        .and_then(|credential| credential.key)
        .filter(|key| !key.is_empty()))
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
    /// every other registry entry is registered only when a credential
    /// resolves for it — a stored credential first, then one of its
    /// credential env vars. Requesting a model whose provider is absent
    /// then fails [`require`](Self::require) with the exact env var to
    /// set instead of silently streaming from faux.
    pub fn from_env() -> Self {
        Self::from_env_with_credentials(
            |name| env::var(name).ok(),
            ProviderRetryPolicy::default(),
            Arc::new(InMemoryCredentialStore::new()),
            default_provider_auth_context(),
        )
    }

    /// [`from_env`](Self::from_env) with an injectable environment, so
    /// tests can exercise every branch without mutating process state.
    ///
    /// The closure is the only environment the router sees: it reaches the
    /// resolver as scoped overrides and its fallback context is empty.
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
        Self::from_env_with_credentials(
            get_env,
            retry_policy,
            Arc::new(InMemoryCredentialStore::new()),
            Arc::new(EmptyAuthContext),
        )
    }

    /// [`from_env_with_policy`](Self::from_env_with_policy) with an explicit
    /// credential store and auth context.
    ///
    /// Embedders wire a persistent [`CredentialStore`] (and the ambient
    /// [`AuthContext`] that goes with it) here; the plain `from_env*`
    /// constructors pass an empty [`InMemoryCredentialStore`], which is what
    /// keeps their behaviour identical to the pre-wiring env-only router.
    pub fn from_env_with_credentials(
        get_env: impl Fn(&str) -> Option<String>,
        retry_policy: ProviderRetryPolicy,
        credentials: Arc<dyn CredentialStore>,
        auth_context: Arc<dyn AuthContext>,
    ) -> Self {
        let mut adapters: HashMap<String, SharedStreamFn> = HashMap::new();

        for spec in BUILTIN_PROVIDERS {
            // OAuth-first providers (`github-copilot`, `openai-codex`,
            // `kimi-coding`) declare no `api_key_env` and no ambient path,
            // and the OAuth flow that would authenticate them is not wired
            // in the Rust port yet. Skip them so the router reports
            // "unconfigured" until P33 lands.
            if spec.requires_api_key() {
                let base_url = env_value(&get_env, spec.base_url_env)
                    .unwrap_or_else(|| spec.default_base_url.to_string());
                let api_key = match resolve_provider_key(
                    spec,
                    &get_env,
                    credentials.as_ref(),
                    auth_context.as_ref(),
                ) {
                    Ok(Some(api_key)) => api_key,
                    // Store and environment both empty: leave the provider
                    // unregistered so `require` names the env var to set,
                    // exactly as the env-only router did.
                    Ok(None) => continue,
                    Err(error) => {
                        tracing::warn!(
                            provider = spec.id,
                            %error,
                            "provider credential resolution failed; treating the provider as unconfigured"
                        );
                        continue;
                    }
                };
                if let Some(adapter) = build_adapter(spec.api, api_key, base_url) {
                    adapters.insert(spec.id.to_string(), adapt_retry(adapter, retry_policy));
                }
            } else if spec.oauth.is_none() && !spec.models.is_empty() {
                // Keyless provider with a built-in catalog (faux): always
                // register so tests and the offline binary can speak it.
                let base_url = env_value(&get_env, spec.base_url_env)
                    .unwrap_or_else(|| spec.default_base_url.to_string());
                if let Some(adapter) = build_adapter(spec.api, String::new(), base_url) {
                    adapters.insert(spec.id.to_string(), adapt_retry(adapter, retry_policy));
                }
            }
            // OAuth-first provider with no catalog: skip — see comment above.
        }

        Self {
            adapters,
            credentials,
            auth_context,
        }
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

    /// Register (or replace) the adapter for one extension-declared provider.
    ///
    /// This is the application-layer half of `pi.registerProvider`:
    /// [`crate::extensions::wiring`] resolves the extension's `apiKey` and
    /// passes the config here. `base_url` defaults to the API family's
    /// built-in host when absent or blank (see [`default_base_url_for_api`]),
    /// so a `registerProvider("my-proxy", { api, apiKey, models })` with no
    /// explicit URL still reaches a real endpoint.
    ///
    /// Re-registering a name replaces the adapter in place; an existing
    /// built-in entry (`anthropic`, `openai`, …) is overridden exactly as
    /// upstream's `registerProvider` overrides builtin providers.
    pub fn register_provider(
        &mut self,
        provider_id: &str,
        api: Api,
        base_url: Option<String>,
        api_key: Option<String>,
    ) -> Result<(), ProviderError> {
        let base_url = base_url
            .map(|url| url.trim().to_string())
            .filter(|url| !url.is_empty())
            .or_else(|| default_base_url_for_api(api).map(str::to_string))
            .unwrap_or_default();
        let adapter =
            build_adapter(api, api_key.unwrap_or_default(), base_url).ok_or_else(|| {
                ProviderError::UnsupportedApi {
                    api: api_wire_name(api).to_string(),
                }
            })?;
        self.adapters.insert(provider_id.to_string(), adapter);
        Ok(())
    }

    /// Register (or replace) the adapter for one extension-declared
    /// provider whose streaming is implemented by the extension itself.
    ///
    /// The `pi.registerProvider` `streamSimple` overload (and the native
    /// `Provider` object overload) does not name an API family this build
    /// owns: the extension speaks its own wire protocol, so there is nothing
    /// for [`build_adapter`] to do and no `apiKey` for the host to inject. The caller (see
    /// [`crate::extensions::wiring`]) builds the [`StreamFn`] that calls back
    /// into the JS host and hands it here.
    ///
    /// Re-registering a name replaces the adapter in place, so an extension
    /// provider overrides a built-in of the same name exactly like
    /// [`register_provider`](Self::register_provider) does.
    pub fn register_stream_fn(&mut self, provider_id: &str, adapter: SharedStreamFn) {
        self.adapters.insert(provider_id.to_string(), adapter);
    }

    /// Remove the adapter registered for `provider_id` (the router half of
    /// `pi.unregisterProvider`). Returns `true` when an entry was removed.
    pub fn unregister_provider(&mut self, provider_id: &str) -> bool {
        self.adapters.remove(provider_id).is_some()
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

    /// The credential store this router resolves provider credentials from.
    ///
    /// Shared with the caller, so a login / logout performed elsewhere is
    /// visible to the next router built from the same store.
    pub fn credential_store(&self) -> Arc<dyn CredentialStore> {
        self.credentials.clone()
    }

    /// The auth context this router resolves ambient credentials from.
    pub fn auth_context(&self) -> Arc<dyn AuthContext> {
        self.auth_context.clone()
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
                vars: "ANTHROPIC_API_KEY, ANTHROPIC_AUTH_TOKEN, ANTHROPIC_OAUTH_TOKEN".to_string(),
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
            .require(&model(
                "google",
                "gemini-2.5-flash",
                Api::GoogleGenerativeAi
            ))
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

    // -----------------------------------------------------------------
    // Auth wiring: a stored credential owns the provider, env is the
    // fallback. All of these stay offline — no API key is ever used on
    // the wire, only at adapter-construction time.
    // -----------------------------------------------------------------

    /// Seed an [`InMemoryCredentialStore`] with one api-key credential.
    fn store_with(provider_id: &str, key: &str) -> Arc<dyn CredentialStore> {
        let stored = pi_ai::Credential::ApiKey(pi_ai::ApiKeyCredential {
            key: Some(key.to_string()),
            env: None,
        });
        let store = Arc::new(InMemoryCredentialStore::new());
        block_on(store.modify(
            provider_id,
            Box::new(move |_current| {
                let stored = stored.clone();
                Box::pin(async move { Ok(Some(stored)) })
            }),
            None,
        ))
        .expect("seeding the in-memory store must not fail");
        store
    }

    #[test]
    fn a_stored_credential_wins_over_the_same_env_var() {
        let store = store_with("anthropic", "stored-key");
        let get_env = |name: &str| {
            if name == "ANTHROPIC_API_KEY" {
                Some("env-key".to_string())
            } else {
                None
            }
        };
        let spec = registry::find_provider("anthropic").expect("anthropic is registered");

        let resolved = resolve_provider_key(spec, &get_env, store.as_ref(), &EmptyAuthContext)
            .expect("resolution must not fail");
        assert_eq!(resolved.as_deref(), Some("stored-key"));

        // The resolved key is what registers the adapter.
        let router = ProviderRouter::from_env_with_credentials(
            get_env,
            ProviderRetryPolicy::default(),
            store,
            Arc::new(EmptyAuthContext),
        );
        assert!(router.has_provider("anthropic"));
    }

    #[test]
    fn without_a_stored_credential_the_env_var_is_the_fallback() {
        let store = InMemoryCredentialStore::new();
        let get_env = |name: &str| {
            if name == "ANTHROPIC_API_KEY" {
                Some("env-key".to_string())
            } else {
                None
            }
        };
        let spec = registry::find_provider("anthropic").expect("anthropic is registered");

        let resolved = resolve_provider_key(spec, &get_env, &store, &EmptyAuthContext)
            .expect("resolution must not fail");
        assert_eq!(resolved.as_deref(), Some("env-key"));
        assert_eq!(
            get_env("ANTHROPIC_API_KEY").as_deref(),
            Some("env-key"),
            "the injected environment is the only source"
        );
    }

    #[test]
    fn no_stored_credential_and_no_env_var_reports_a_missing_key() {
        let store = InMemoryCredentialStore::new();
        let spec = registry::find_provider("anthropic").expect("anthropic is registered");
        let resolved = resolve_provider_key(spec, &empty_env, &store, &EmptyAuthContext)
            .expect("resolution must not fail");
        assert_eq!(resolved, None);

        let router = ProviderRouter::from_env_with(empty_env);
        assert_eq!(
            require_err(
                &router,
                &model("anthropic", "claude-sonnet-4-5", Api::AnthropicMessages)
            ),
            ProviderError::MissingApiKey {
                provider: "anthropic".to_string(),
                vars: "ANTHROPIC_API_KEY, ANTHROPIC_AUTH_TOKEN, ANTHROPIC_OAUTH_TOKEN".to_string(),
            }
        );
    }

    /// Regression lock for the pre-wiring contract: with nothing stored,
    /// the router behaves exactly as the env-only one did — the first set
    /// env var registers the provider (trimmed), an empty/absent var does
    /// not, and the keyless faux provider is untouched.
    #[test]
    fn an_empty_store_keeps_the_env_only_behaviour() {
        let get_env = |name: &str| match name {
            "ANTHROPIC_API_KEY" => Some("  sk-anthropic  ".to_string()),
            "GROQ_API_KEY" => Some("   ".to_string()),
            _ => None,
        };
        let router = ProviderRouter::from_env_with(get_env);
        assert_eq!(router.provider_ids(), vec!["anthropic", "faux"]);

        let spec = registry::find_provider("anthropic").expect("anthropic is registered");
        let resolved = resolve_provider_key(
            spec,
            &get_env,
            &InMemoryCredentialStore::new(),
            &EmptyAuthContext,
        )
        .expect("resolution must not fail");
        assert_eq!(resolved.as_deref(), Some("sk-anthropic"));

        assert_eq!(
            require_err(
                &router,
                &model("groq", "llama-3.3-70b", Api::OpenAiChatCompletions)
            ),
            ProviderError::MissingApiKey {
                provider: "groq".to_string(),
                vars: "GROQ_API_KEY".to_string(),
            }
        );
        assert!(router.has_provider("faux"));
        // The default constructors wire an empty in-memory store, which is
        // what makes the env-only behaviour above hold.
        assert!(block_on(router.credential_store().read("anthropic", None))
            .expect("reading an empty store must not fail")
            .is_none());
    }

    #[test]
    fn the_keyless_faux_provider_is_not_in_the_auth_registry() {
        assert!(pi_ai::provider_auth_for("faux").is_none());
        assert!(pi_ai::provider_auth_for("not-a-provider").is_none());
        let router = ProviderRouter::from_env_with(empty_env);
        assert!(router.has_provider("faux"));
        assert!(router
            .require(&model("faux", "faux-model", Api::Faux))
            .is_ok());
    }

    /// The providers added to the registry by LUM-1170 are covered by the
    /// wiring without any per-provider code: one registers from its env
    /// var, the other from a stored credential only.
    #[test]
    fn newly_registered_providers_resolve_through_the_auth_subsystem() {
        let minimax_env = |name: &str| {
            if name == "MINIMAX_API_KEY" {
                Some("sk-minimax".to_string())
            } else {
                None
            }
        };
        let router = ProviderRouter::from_env_with(minimax_env);
        assert!(router.has_provider("minimax"));
        assert!(router
            .require(&model("minimax", "MiniMax-M2", Api::AnthropicMessages))
            .is_ok());

        let router = ProviderRouter::from_env_with_credentials(
            empty_env,
            ProviderRetryPolicy::default(),
            store_with("vercel-ai-gateway", "stored-gateway-key"),
            Arc::new(EmptyAuthContext),
        );
        assert!(router.has_provider("vercel-ai-gateway"));
        assert!(router
            .require(&model(
                "vercel-ai-gateway",
                "anthropic/claude-sonnet-4-5",
                Api::AnthropicMessages
            ))
            .is_ok());
    }
}
