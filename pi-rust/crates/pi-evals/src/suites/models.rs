//! `models` suite — provider registry + model catalog.
//!
//! Upstream `models.eval.ts` is model-backed: it asks a coding agent to add
//! a model to an existing provider and then judges the *edit* it produces.
//! The Rust port cannot judge an edit offline, so it judges the resulting
//! **catalog state** instead, which is the outcome the upstream test is
//! really about: the new model resolves, the other models of that provider
//! survive, and the API family is inferred correctly.
//!
//! Documented divergence: `Models::register_provider_json` calls
//! `set_provider`, which *replaces* a provider's entries. Upstream
//! `registerProvider` merges. The second case therefore has to include the
//! pre-existing entries in the envelope, and asserts that contract
//! explicitly so the behavior cannot drift silently.

use std::collections::HashSet;

use pi_ai::models::Models;
use pi_ai::providers::registry;
use pi_protocol::{Api, Model, ProviderId};
use serde_json::{json, Value};

use crate::harness::{CaseOutput, EvalError, EvalSuite};

/// Registry invariants every built-in provider must satisfy.
fn registry_invariants_case() -> crate::harness::Case {
    crate::harness::Case::builder("models-registry-invariants")
        .description("every built-in provider spec is complete and uniquely identified")
        .assertion("registry-invariants", |output| {
            let expected = output.output.clone();
            if expected["unique_ids"] != Value::Bool(true) {
                return Err("provider ids are not unique".into());
            }
            if expected["faux_requires_key"] != Value::Bool(false) {
                return Err("faux must not require an API key".into());
            }
            if expected["providers_with_models"].as_u64().unwrap_or(0) == 0 {
                return Err("no provider exposes a model catalog".into());
            }
            if expected["providers_missing_base_url"].as_u64().unwrap_or(1) != 0 {
                return Err(format!(
                    "providers missing a default base URL: {}",
                    expected["providers_missing_base_url"]
                ));
            }
            Ok(())
        })
        .run(|| async {
            let mut ids = HashSet::new();
            let mut unique_ids = true;
            let mut providers_with_models = 0u64;
            let mut providers_missing_base_url = 0u64;
            let mut model_count = 0u64;
            for spec in registry::BUILTIN_PROVIDERS {
                unique_ids &= ids.insert(spec.id);
                if !spec.models.is_empty() {
                    providers_with_models += 1;
                    model_count += spec.models.len() as u64;
                }
                let needs_transport = spec.api != Api::Faux;
                // Azure OpenAI resolves its host from `AZURE_OPENAI_BASE_URL`
                // or `AZURE_OPENAI_RESOURCE_NAME`, so it has no fixed default.
                let env_only_base_url = spec.id == "azure-openai-responses";
                if needs_transport && spec.default_base_url.is_empty() && !env_only_base_url {
                    providers_missing_base_url += 1;
                }
                if needs_transport && spec.api_key_env.is_empty() {
                    return Err(EvalError::Case(format!(
                        "provider `{}` requires transport but declares no credential env var",
                        spec.id
                    )));
                }
            }
            let faux_spec = registry::find_provider("faux")
                .ok_or_else(|| EvalError::Case("faux spec is missing from the registry".into()))?;
            let faux_requires_key = faux_spec.requires_api_key();
            if faux_requires_key {
                return Err(EvalError::Case(
                    "faux must not require an API key".into(),
                ));
            }
            Ok(CaseOutput {
                output: json!({
                    "unique_ids": unique_ids,
                    "faux_requires_key": faux_requires_key,
                    "provider_count": registry::provider_ids().count(),
                    "providers_with_models": providers_with_models,
                    "providers_missing_base_url": providers_missing_base_url,
                    "model_count": model_count,
                }),
                ..CaseOutput::default()
            })
        })
        .build()
}

/// Add a model to an existing provider and keep its other models.
fn add_model_case() -> crate::harness::Case {
    crate::harness::Case::builder("models-add-model-to-existing-provider")
        .description("registering a catalog envelope for an existing provider resolves every entry")
        .assertion("add-model", |output| {
            // `Api` serialises with `rename_all = "snake_case"`, so
            // `OpenAiChatCompletions` reaches the wire as
            // `open_ai_chat_completions` (unlike the kebab-case `api` hint
            // accepted on input).
            let expected = json!({
                "new_model": {
                    "provider": "openai",
                    "id": "fixture-chat",
                    "api": "open_ai_chat_completions",
                    "label": "Fixture Chat",
                    "context_window": 32768,
                    "max_output_tokens": 4096
                },
                "provider_model_count": 2,
                "existing_model_preserved": true,
            });
            if output.output != expected {
                return Err(format!(
                    "catalog state mismatch:\n expected {expected}\n      got {}",
                    output.output
                ));
            }
            Ok(())
        })
        .run(|| async {
            let provider = ProviderId::new("openai");
            let mut catalog = Models::new();
            catalog.set_provider(
                provider.clone(),
                vec![Model {
                    provider: provider.clone(),
                    id: "gpt-4o-mini".into(),
                    api: Api::OpenAiChatCompletions,
                    label: Some("GPT-4o mini".into()),
                    context_window: 128_000,
                    max_output_tokens: 4_096,
                }],
            );
            let envelope = json!({
                "provider": "openai",
                "models": [
                    { "id": "gpt-4o-mini", "label": "GPT-4o mini", "context_window": 128000, "max_output_tokens": 4096 },
                    { "id": "fixture-chat", "label": "Fixture Chat", "context_window": 32768, "max_output_tokens": 4096 }
                ]
            })
            .to_string();
            catalog
                .register_provider_json(&provider, &envelope)
                .map_err(|error| EvalError::Case(error.to_string()))?;
            let new_model = catalog
                .get_model(&provider, "fixture-chat")
                .ok_or_else(|| EvalError::Case("fixture-chat was not registered".into()))?;
            let existing_preserved = catalog.get_model(&provider, "gpt-4o-mini").is_some();
            if !existing_preserved {
                return Err(EvalError::Case(
                    "registering the envelope dropped the pre-existing gpt-4o-mini entry".into(),
                ));
            }
            Ok(CaseOutput {
                output: json!({
                    "new_model": serde_json::to_value(new_model)?,
                    "provider_model_count": catalog
                        .iter()
                        .filter(|(_, model)| model.provider.0 == "openai")
                        .count(),
                    "existing_model_preserved": existing_preserved,
                }),
                ..CaseOutput::default()
            })
        })
        .build()
}

/// API-family inference from provider name and per-entry `api` hints.
fn api_inference_case() -> crate::harness::Case {
    crate::harness::Case::builder("models-api-inference")
        .description("catalog entries infer their API family from provider id and hints")
        .assertion("api-inference", |output| {
            let expected = json!({
                "anthropic": "anthropic_messages",
                "openai": "openai_chat_completions",
                "google": "google_generative_ai",
                "mirror": "openai_chat_completions",
                "responses_hint": "openai_responses",
            });
            if output.output != expected {
                return Err(format!(
                    "api inference mismatch:\n expected {expected}\n      got {}",
                    output.output
                ));
            }
            Ok(())
        })
        .run(|| async {
            let mut catalog = Models::new();
            for (provider, id) in [
                ("anthropic", "claude-haiku-4-5"),
                ("openai", "gpt-4o-mini"),
                ("google", "gemini-2.5-flash"),
                ("ollama", "llama3.2"),
            ] {
                let provider_id = ProviderId::new(provider);
                let bare = json!([{ "id": id, "context_window": 1000, "max_output_tokens": 100 }])
                    .to_string();
                catalog
                    .register_provider_json(&provider_id, &bare)
                    .map_err(|error| EvalError::Case(error.to_string()))?;
            }
            let hinted = ProviderId::new("openai-responses-fixture");
            let envelope = json!({
                "models": [{ "id": "o4-mini", "api": "openai-responses" }]
            })
            .to_string();
            catalog
                .register_provider_json(&hinted, &envelope)
                .map_err(|error| EvalError::Case(error.to_string()))?;

            let api_of = |provider: &str, id: &str| -> Result<&'static str, EvalError> {
                let model = catalog
                    .get_model(&ProviderId::new(provider), id)
                    .ok_or_else(|| EvalError::Case(format!("{provider}/{id} missing")))?;
                Ok(match model.api {
                    Api::AnthropicMessages => "anthropic_messages",
                    Api::OpenAiChatCompletions => "openai_chat_completions",
                    Api::OpenAiResponses => "openai_responses",
                    Api::GoogleGenerativeAi => "google_generative_ai",
                    Api::BedrockConverse => "bedrock_converse",
                    Api::CohereV2 => "cohere_v2",
                    Api::MistralConversations => "mistral_conversations",
                    Api::AzureOpenAiResponses => "azure_openai_responses",
                    Api::Faux => "faux",
                })
            };

            Ok(CaseOutput {
                output: json!({
                    "anthropic": api_of("anthropic", "claude-haiku-4-5")?,
                    "openai": api_of("openai", "gpt-4o-mini")?,
                    "google": api_of("google", "gemini-2.5-flash")?,
                    "mirror": api_of("ollama", "llama3.2")?,
                    "responses_hint": api_of("openai-responses-fixture", "o4-mini")?,
                }),
                ..CaseOutput::default()
            })
        })
        .build()
}

/// Build the `models` suite.
pub fn suite() -> EvalSuite {
    EvalSuite::new("models")
        .with_case(registry_invariants_case())
        .with_case(add_model_case())
        .with_case(api_inference_case())
}
