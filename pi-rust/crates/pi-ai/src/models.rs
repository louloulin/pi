//! Model catalog loader — typed wrapper over the catalog JSON file.
//!
//! Stage 0 ships an in-memory stub populated from a JSON literal. Stage 1
//! replaces it with a deserialised loader that reads from
//! `models.generated.json` (parity with `packages/ai/src/models.generated.ts`).
//! Stage 7 adds `register_provider_json`, which deserialises a
//! provider-shaped JSON envelope (mirroring each provider's
//! `*.models.ts` file) and registers it under a [`ProviderId`].

use std::collections::HashMap;

use pi_protocol::{Api, Model, ProviderId};

use crate::types::StreamError;

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

fn infer_api(provider: &ProviderId, hint: Option<&str>) -> Api {
    if let Some(name) = hint {
        match name {
            "anthropic-messages" => return Api::AnthropicMessages,
            "openai-chat-completions" => return Api::OpenAiChatCompletions,
            "openai-responses" => return Api::OpenAiResponses,
            "google-generative-ai" => return Api::GoogleGenerativeAi,
            "bedrock-converse" => return Api::BedrockConverse,
            "cohere-v2" => return Api::CohereV2,
            "faux" => return Api::Faux,
            _ => {}
        }
    }
    match provider.0.as_str() {
        "anthropic" => Api::AnthropicMessages,
        "openai" => Api::OpenAiChatCompletions,
        "google" | "google-vertex" => Api::GoogleGenerativeAi,
        "bedrock" | "amazon-bedrock" => Api::BedrockConverse,
        "cohere" => Api::CohereV2,
        "faux" => Api::Faux,
        // Default fallback for OpenAI-compatible mirrors (Ollama,
        // vllm, OpenRouter, …).
        _ => Api::OpenAiChatCompletions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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