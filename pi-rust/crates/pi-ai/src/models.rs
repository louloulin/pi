//! Model catalog loader — typed wrapper over the catalog JSON file.
//!
//! The catalog is data, not code. Three entry points feed [`Models`]:
//!
//! * The static registry (`pi_ai::providers::registry::BUILTIN_PROVIDERS`)
//!   is what `pi-coding-agent`'s `build_default_models` walks to seed the
//!   in-memory catalog at startup.
//! * [`Models::register_provider_json`] deserialises a provider-shaped
//!   JSON envelope (mirroring each provider's `*.models.ts` file). The
//!   extension host uses this to register models a plugin contributes.
//! * [`Models::load_models_json`] deserialises a top-level `models.json`
//!   envelope (`{"providers": {"<id>": {"models": [...]}}}`) and validates
//!   it against the static registry. It is what a user-facing `models.json`
//!   drop-in or the catalog refresh path reads at startup.
//!
//! The `register_provider_json` path stays loose (any provider id, default
//! context window) because extensions deliberately extend the catalog with
//! ids the static registry does not know about yet. The `load_models_json`
//! path is strict because it ships with the binary and a typo there has to
//! be caught before the first request goes out.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use pi_protocol::{Api, Model, ProviderId};

use crate::types::StreamError;

/// Errors emitted by [`Models::load_models_json`].
#[derive(Debug, thiserror::Error)]
pub enum LoadModelsError {
    /// The catalog file could not be read (permission, missing parent
    /// directory, …).
    #[error("read models.json at {path}: {message}")]
    Io {
        /// Path the loader attempted to read.
        path: PathBuf,
        /// Underlying error message.
        message: String,
    },
    /// The catalog file is not valid JSON.
    #[error("parse models.json at {path}: {message}")]
    Parse {
        /// Path the loader attempted to read.
        path: PathBuf,
        /// Underlying parser error message.
        message: String,
    },
    /// The catalog parsed but failed schema validation. `errors`
    /// carries one bullet per failed entry, in load order, with the
    /// full JSON path so the user can fix the file directly.
    #[error("invalid models.json schema at {path}:\n{errors}")]
    Schema {
        /// Path the loader attempted to read.
        path: PathBuf,
        /// Bullet-list of validation failures, in load order.
        errors: String,
    },
}

/// In-memory model catalog.
#[derive(Debug, Clone, Default)]
pub struct Models {
    by_provider: HashMap<ProviderId, Vec<Model>>,
}

impl Models {
    /// Construct an empty catalog.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a provider's models. Replaces any prior registration.
    pub fn set_provider(&mut self, provider: ProviderId, models: Vec<Model>) {
        self.by_provider.insert(provider, models);
    }

    /// Look up a model by `(provider, id)`.
    pub fn get_model(&self, provider: &ProviderId, id: &str) -> Option<&Model> {
        self.by_provider
            .get(provider)
            .and_then(|models| models.iter().find(|m| m.id == id))
    }

    /// Iterator over all registered models.
    pub fn iter(&self) -> impl Iterator<Item = (&ProviderId, &Model)> {
        self.by_provider
            .iter()
            .flat_map(|(p, ms)| ms.iter().map(move |m| (p, m)))
    }

    /// Whether `provider` is configured for use — at least one
    /// `api_key_env` variable is set, or the provider has an OAuth
    /// credential stored (upstream `Models.getAvailable()` mirror).
    ///
    /// `env` is the same scoped env the auth resolver sees (`pi-coding-agent`
    /// passes its injectable `get_env`). Providers whose `api_key_env` is
    /// empty (faux and the OAuth-first `github-copilot` / `openai-codex` /
    /// `kimi-coding`) are reported as available only when OAuth credentials
    /// are stored — today that is always false until OAuth flow lands; the
    /// shape leaves room for it without touching this module.
    pub fn is_provider_available(
        &self,
        provider: &ProviderId,
        env: Option<&crate::auth::types::ProviderEnv>,
    ) -> bool {
        if self.by_provider.contains_key(provider) {
            crate::env_api_keys::find_env_keys(&provider.0, env).is_some()
        } else {
            false
        }
    }

    /// Iterator over models whose provider is configured (`Models.getAvailable()`).
    ///
    /// Used by the model selector so unconfigured providers (Ling in the
    /// screenshot the user filed) do not appear as a choice — the user
    /// must `/login <provider>` first.
    pub fn iter_available(
        &self,
        env: Option<&crate::auth::types::ProviderEnv>,
    ) -> impl Iterator<Item = (&ProviderId, &Model)> {
        let env = env.cloned();
        self.by_provider
            .iter()
            .filter(move |(p, _)| {
                crate::env_api_keys::find_env_keys(&p.0, env.as_ref()).is_some()
            })
            .flat_map(|(p, ms)| ms.iter().map(move |m| (p, m)))
    }

    /// Register a provider's models from a JSON envelope.
    ///
    /// `provider` is the [`ProviderId`] the entries will be registered
    /// under. The JSON shape depends on the provider; today we ship the
    /// Anthropic envelope (`{"provider": "...", "models": [...]}`) and
    /// a generic shape used by the OpenAI / faux providers:
    ///
    /// ```json
    /// {"models": [{"id": "...", "context_window": 128000, "max_output_tokens": 4096}]}
    /// ```
    ///
    /// The API enum is taken from `api` if the entry includes it,
    /// otherwise it is inferred from the provider name. Unknown
    /// providers fall back to [`Api::OpenAiChatCompletions`] (the
    /// common case for OpenAI-compatible mirrors).
    pub fn register_provider_json(
        &mut self,
        provider: &ProviderId,
        json_str: &str,
    ) -> Result<(), StreamError> {
        // First try the canonical provider-envelope shape.
        if let Ok(parsed) = serde_json::from_str::<GenericProviderEnvelope>(json_str) {
            let mut models = Vec::with_capacity(parsed.models.len());
            for entry in parsed.models {
                models.push(entry.into_model(provider.clone()));
            }
            self.set_provider(provider.clone(), models);
            return Ok(());
        }
        // Fall back to the bare-models shape used by the faux/OpenAI
        // registration helpers.
        if let Ok(entries) = serde_json::from_str::<Vec<GenericModelEntry>>(json_str) {
            let models = entries
                .into_iter()
                .map(|e| e.into_model(provider.clone()))
                .collect();
            self.set_provider(provider.clone(), models);
            return Ok(());
        }
        Err(StreamError::Malformed(format!(
            "{provider}: catalog JSON does not match either envelope"
        )))
    }

    /// Build a stub catalog with the faux provider registered.
    ///
    /// Lets `pi-agent-core` tests compile against `pi-ai` before Stage 1
    /// finishes the real provider ports.
    pub fn with_faux() -> Self {
        let mut models = Self::new();
        models.set_provider(
            ProviderId::new("faux"),
            vec![Model {
                provider: ProviderId::new("faux"),
                id: "faux-model".into(),
                api: Api::Faux,
                label: Some("Faux test model".into()),
                context_window: 8192,
                max_output_tokens: 1024,
            }],
        );
        models
    }

    /// Load a `models.json` catalog from disk.
    ///
    /// Returns the populated [`Models`] on success and an
    /// [`LoadModelsError`] with the full validation path on failure so
    /// the user can fix the file. The loader is strict: provider keys
    /// must match a known [`crate::providers::registry`] entry (so a
    /// typo never silently disables a provider), every model entry
    /// requires a non-empty `id`, a positive `context_window` and a
    /// positive `max_output_tokens`. Optional `api` hints are validated
    /// against [`infer_api`]'s accepted names.
    pub fn load_models_json(path: &Path) -> Result<Self, LoadModelsError> {
        let raw = std::fs::read_to_string(path).map_err(|err| LoadModelsError::Io {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
        Self::load_models_json_str(&raw, path)
    }

    /// Parse and validate a `models.json` envelope from a string.
    ///
    /// `origin` is recorded in error messages so the loader can be
    /// reused from sources other than disk (an embedded string in
    /// tests, an HTTP response body, …).
    pub fn load_models_json_str(json: &str, origin: &Path) -> Result<Self, LoadModelsError> {
        let parsed: ModelsJsonFile = serde_json::from_str(json).map_err(|err| {
            LoadModelsError::Parse {
                path: origin.to_path_buf(),
                message: err.to_string(),
            }
        })?;
        let mut models = Models::new();
        let mut errors: Vec<String> = Vec::new();
        for (raw_id, body) in parsed.providers {
            let path = format!("providers.{raw_id}");
            match Self::validate_provider_body(&raw_id, &path, body, &mut errors) {
                Some(entries) => {
                    models.set_provider(ProviderId::new(raw_id), entries);
                }
                None => {}
            }
        }
        if !errors.is_empty() {
            return Err(LoadModelsError::Schema {
                path: origin.to_path_buf(),
                errors: errors
                    .iter()
                    .map(|e| format!("  - {e}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            });
        }
        Ok(models)
    }

    fn validate_provider_body(
        provider_id: &str,
        path: &str,
        body: ProviderBody,
        errors: &mut Vec<String>,
    ) -> Option<Vec<Model>> {
        // Unknown provider ids must fail loud. A typo in `anhtropic`
        // should never silently disappear; the same load has to read
        // back what it wrote.
        let Some(spec) = crate::providers::registry::find_provider(provider_id) else {
            errors.push(format!("{path}: unknown provider (not in BUILTIN_PROVIDERS)"));
            return None;
        };
        // Optional per-provider `api` hint. If absent we fall back to
        // `infer_api` with `None`, which uses the provider id to pick
        // the adapter; if present, validate against the known names.
        let api_hint = match body.api.as_deref() {
            Some(name) => match api_from_name(name) {
                Some(api) => Some(api),
                None => {
                    errors.push(format!(
                        "{path}.api: unknown api hint {name:?} (expected one of anthropic-messages, openai-chat-completions, openai-responses, azure-openai-responses, google-generative-ai, bedrock-converse, cohere-v2, faux)"
                    ));
                    return None;
                }
            },
            None => None,
        };
        let default_api = api_hint.unwrap_or_else(|| spec.api);
        let mut entries: Vec<Model> = Vec::with_capacity(body.models.len());
        for (index, entry) in body.models.into_iter().enumerate() {
            let entry_path = format!("{path}.models[{index}]");
            match validate_model_entry(&entry, &entry_path, provider_id, default_api, errors) {
                Some(model) => entries.push(model),
                None => {}
            }
        }
        Some(entries)
    }
}

/// One provider's worth of models in a `models.json` envelope.
#[derive(Debug, serde::Deserialize)]
struct ProviderBody {
    /// Optional API family override for the whole provider. When
    /// absent the loader falls back to the provider id's default
    /// adapter (see [`infer_api`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api: Option<String>,
    /// Models shipped under this provider. An empty array is allowed
    /// (the provider shows up in the catalog with no entries).
    #[serde(default)]
    models: Vec<GenericModelEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct ModelsJsonFile {
    /// Provider id → models. The map is required so a misnamed
    /// provider surfaces as a clear "unknown provider" error rather
    /// than a bare deserialisation failure.
    #[serde(default)]
    providers: HashMap<String, ProviderBody>,
}

#[derive(Debug, serde::Deserialize)]
struct GenericProviderEnvelope {
    #[serde(default)]
    models: Vec<GenericModelEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct GenericModelEntry {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(default)]
    context_window: u32,
    #[serde(default)]
    max_output_tokens: u32,
    /// Optional API family — taken when present so a provider can mix
    /// `OpenAiChatCompletions` and `OpenAiResponses` entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api: Option<String>,
}

impl GenericModelEntry {
    fn into_model(self, provider: ProviderId) -> Model {
        Model {
            provider: provider.clone(),
            id: self.id,
            api: infer_api(&provider, self.api.as_deref()),
            label: self.label,
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
        }
    }
}

fn api_from_name(name: &str) -> Option<Api> {
    match name {
        "anthropic-messages" => Some(Api::AnthropicMessages),
        "openai-chat-completions" => Some(Api::OpenAiChatCompletions),
        "openai-responses" => Some(Api::OpenAiResponses),
        "azure-openai-responses" => Some(Api::AzureOpenAiResponses),
        "google-generative-ai" => Some(Api::GoogleGenerativeAi),
        "bedrock-converse" => Some(Api::BedrockConverse),
        "cohere-v2" => Some(Api::CohereV2),
        "faux" => Some(Api::Faux),
        _ => None,
    }
}

fn infer_api(provider: &ProviderId, hint: Option<&str>) -> Api {
    if let Some(api) = hint.and_then(api_from_name) {
        return api;
    }
    match provider.0.as_str() {
        "anthropic" => Api::AnthropicMessages,
        "openai" => Api::OpenAiChatCompletions,
        // Same credential as `openai`, different wire protocol — the
        // registry ships it as its own provider id for that reason.
        "openai-responses" => Api::OpenAiResponses,
        // Azure reuses the Responses wire protocol but has its own adapter
        // (deployment URL + `api-key` header).
        "azure-openai-responses" => Api::AzureOpenAiResponses,
        "google" | "google-vertex" => Api::GoogleGenerativeAi,
        "bedrock" | "amazon-bedrock" => Api::BedrockConverse,
        "cohere" => Api::CohereV2,
        "faux" => Api::Faux,
        // Default fallback for OpenAI-compatible mirrors (Ollama,
        // vllm, OpenRouter, …).
        _ => Api::OpenAiChatCompletions,
    }
}

/// Validate one model entry. Returns the [`Model`] on success and pushes
/// a fully-pathed error message into `errors` on every failed check.
/// Returns `None` when the entry failed validation; the caller skips it
/// but keeps loading the rest of the catalog.
fn validate_model_entry(
    entry: &GenericModelEntry,
    path: &str,
    provider_id: &str,
    default_api: Api,
    errors: &mut Vec<String>,
) -> Option<Model> {
    if entry.id.is_empty() {
        errors.push(format!("{path}.id: must be a non-empty string"));
        return None;
    }
    if entry.context_window == 0 {
        errors.push(format!(
            "{path}.context_window: must be > 0 (got 0); a zero context would silently truncate the first turn"
        ));
        return None;
    }
    if entry.max_output_tokens == 0 {
        errors.push(format!(
            "{path}.max_output_tokens: must be > 0 (got 0); a zero cap would silently truncate every response"
        ));
        return None;
    }
    let api = match entry.api.as_deref() {
        Some(name) => match api_from_name(name) {
            Some(api) => api,
            None => {
                errors.push(format!("{path}.api: unknown api hint {name:?}"));
                return None;
            }
        },
        None => default_api,
    };
    Some(Model {
        provider: ProviderId::new(provider_id),
        id: entry.id.clone(),
        api,
        label: entry.label.clone(),
        context_window: entry.context_window,
        max_output_tokens: entry.max_output_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_api_routes_openai_responses_providers_to_the_responses_api() {
        // A model id under the `openai` provider stays on Chat Completions…
        assert_eq!(
            infer_api(&ProviderId::new("openai"), None),
            Api::OpenAiChatCompletions
        );
        // …while the dedicated provider id takes the Responses adapter, with
        // no per-model `api` hint required.
        assert_eq!(
            infer_api(&ProviderId::new("openai-responses"), None),
            Api::OpenAiResponses
        );
        // Azure has its own adapter (deployment URL + `api-key` header).
        assert_eq!(
            infer_api(&ProviderId::new("azure-openai-responses"), None),
            Api::AzureOpenAiResponses
        );
        // An explicit hint still wins over provider-id inference.
        assert_eq!(
            infer_api(&ProviderId::new("openai"), Some("openai-responses")),
            Api::OpenAiResponses
        );
    }

    #[test]
    fn register_provider_json_loads_anthropic_envelope() {
        let json = r#"{
            "provider": "anthropic",
            "models": [
                {
                    "id": "claude-haiku-4-5",
                    "label": "Claude Haiku 4.5",
                    "context_window": 200000,
                    "max_output_tokens": 8192
                }
            ]
        }"#;
        let mut catalog = Models::new();
        catalog
            .register_provider_json(&ProviderId::new("anthropic"), json)
            .expect("register anthropic");
        let m = catalog
            .get_model(&ProviderId::new("anthropic"), "claude-haiku-4-5")
            .expect("lookup");
        assert_eq!(m.api, Api::AnthropicMessages);
        assert_eq!(m.context_window, 200_000);
    }

    #[test]
    fn register_provider_json_handles_bare_models_array() {
        let json = r#"[
            {"id": "gpt-4o-mini", "context_window": 128000, "max_output_tokens": 4096}
        ]"#;
        let mut catalog = Models::new();
        catalog
            .register_provider_json(&ProviderId::new("openai"), json)
            .expect("register openai");
        let m = catalog
            .get_model(&ProviderId::new("openai"), "gpt-4o-mini")
            .expect("lookup");
        assert_eq!(m.api, Api::OpenAiChatCompletions);
    }

    #[test]
    fn register_provider_json_rejects_malformed_envelope() {
        let mut catalog = Models::new();
        let err = catalog
            .register_provider_json(&ProviderId::new("anthropic"), "this is not json")
            .expect_err("should fail");
        assert!(matches!(err, StreamError::Malformed(_)));
    }
}

    #[test]
    fn load_models_json_str_loads_known_providers_with_validation() {
        let json = r#"{
            "providers": {
                "anthropic": {
                    "models": [
                        {"id": "claude-haiku-4-5", "context_window": 200000, "max_output_tokens": 8192}
                    ]
                },
                "openai": {
                    "models": [
                        {"id": "gpt-4o-mini", "context_window": 128000, "max_output_tokens": 4096}
                    ]
                }
            }
        }"#;
        let catalog = Models::load_models_json_str(json, Path::new("models.json"))
            .expect("load");
        let anthropic = catalog
            .get_model(&ProviderId::new("anthropic"), "claude-haiku-4-5")
            .expect("anthropic entry");
        assert_eq!(anthropic.api, Api::AnthropicMessages);
        assert_eq!(anthropic.context_window, 200_000);
        let openai = catalog
            .get_model(&ProviderId::new("openai"), "gpt-4o-mini")
            .expect("openai entry");
        assert_eq!(openai.api, Api::OpenAiChatCompletions);
        assert_eq!(openai.max_output_tokens, 4096);
    }

    #[test]
    fn load_models_json_str_rejects_unknown_provider_id() {
        let json = r#"{
            "providers": {
                "anhtropic": {
                    "models": [
                        {"id": "claude-haiku-4-5", "context_window": 200000, "max_output_tokens": 8192}
                    ]
                }
            }
        }"#;
        let err = Models::load_models_json_str(json, Path::new("models.json"))
            .expect_err("unknown provider must fail");
        match err {
            LoadModelsError::Schema { errors, .. } => {
                assert!(errors.contains("anhtropic"), "got: {errors}");
                assert!(errors.contains("unknown provider"));
            }
            other => panic!("expected Schema error, got {other:?}"),
        }
    }

    #[test]
    fn load_models_json_str_reports_every_invalid_model_with_full_path() {
        let json = r#"{
            "providers": {
                "anthropic": {
                    "models": [
                        {"id": "", "context_window": 200000, "max_output_tokens": 8192},
                        {"id": "claude-haiku-4-5", "context_window": 0, "max_output_tokens": 8192},
                        {"id": "claude-haiku-4-5", "context_window": 200000, "max_output_tokens": 0}
                    ]
                }
            }
        }"#;
        let err = Models::load_models_json_str(json, Path::new("models.json"))
            .expect_err("zero limits must fail");
        match err {
            LoadModelsError::Schema { errors, .. } => {
                assert!(errors.contains("providers.anthropic.models[0].id"), "got: {errors}");
                assert!(errors.contains("providers.anthropic.models[1].context_window"), "got: {errors}");
                assert!(errors.contains("providers.anthropic.models[2].max_output_tokens"), "got: {errors}");
            }
            other => panic!("expected Schema error, got {other:?}"),
        }
    }

    #[test]
    fn load_models_json_str_rejects_unknown_api_hint() {
        let json = r#"{
            "providers": {
                "anthropic": {
                    "api": "not-a-real-api",
                    "models": [
                        {"id": "claude-haiku-4-5", "context_window": 200000, "max_output_tokens": 8192}
                    ]
                }
            }
        }"#;
        let err = Models::load_models_json_str(json, Path::new("models.json"))
            .expect_err("unknown api hint must fail");
        match err {
            LoadModelsError::Schema { errors, .. } => {
                assert!(errors.contains("providers.anthropic.api"), "got: {errors}");
            }
            other => panic!("expected Schema error, got {other:?}"),
        }
    }

    #[test]
    fn load_models_json_str_per_entry_api_hint_overrides_provider_default() {
        let json = r#"{
            "providers": {
                "openai": {
                    "models": [
                        {"id": "gpt-4o-mini", "context_window": 128000, "max_output_tokens": 4096},
                        {"id": "o3", "context_window": 200000, "max_output_tokens": 100000, "api": "openai-responses"}
                    ]
                }
            }
        }"#;
        let catalog = Models::load_models_json_str(json, Path::new("models.json"))
            .expect("load");
        assert_eq!(
            catalog
                .get_model(&ProviderId::new("openai"), "gpt-4o-mini")
                .unwrap()
                .api,
            Api::OpenAiChatCompletions,
        );
        assert_eq!(
            catalog
                .get_model(&ProviderId::new("openai"), "o3")
                .unwrap()
                .api,
            Api::OpenAiResponses,
        );
    }

    #[test]
    fn load_models_json_str_returns_parse_error_on_invalid_json() {
        let err = Models::load_models_json_str("not json", Path::new("models.json"))
            .expect_err("parse must fail");
        assert!(matches!(err, LoadModelsError::Parse { .. }));
    }

    #[test]
    fn load_models_json_reads_from_disk() {
        let json = r#"{
            "providers": {
                "anthropic": {
                    "models": [
                        {"id": "claude-haiku-4-5", "context_window": 200000, "max_output_tokens": 8192}
                    ]
                }
            }
        }"#;
        let tmp = std::env::temp_dir().join("pi-ai-models-loader-test.json");
        std::fs::write(&tmp, json).expect("write tmp");
        let catalog = Models::load_models_json(&tmp).expect("load from disk");
        assert!(catalog
            .get_model(&ProviderId::new("anthropic"), "claude-haiku-4-5")
            .is_some());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn load_models_json_returns_io_error_on_missing_file() {
        let err = Models::load_models_json(Path::new("/no/such/file/models.json"))
            .expect_err("missing file must fail");
        assert!(matches!(err, LoadModelsError::Io { .. }));
    }
