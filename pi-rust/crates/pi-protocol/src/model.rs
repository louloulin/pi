//! Model descriptor + provider identifier. Mirrors `packages/ai/src/types.ts`.

use serde::{Deserialize, Serialize};

/// Provider identifier — matches the keys in the model catalog.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProviderId(pub String);

impl ProviderId {
    /// Convenience constructor.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// API family — drives provider-specific stream adapters in `pi-ai`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Api {
    /// OpenAI chat completions.
    OpenAiChatCompletions,
    /// OpenAI Responses.
    OpenAiResponses,
    /// Azure OpenAI Responses — the deployment-scoped variant of
    /// [`Api::OpenAiResponses`] (`POST /deployments/{name}/responses`,
    /// `api-key` header). Mirrors the upstream `azure-openai-responses`
    /// API family.
    #[serde(rename = "azure-openai-responses")]
    AzureOpenAiResponses,
    /// Anthropic Messages.
    AnthropicMessages,
    /// Google Generative AI.
    GoogleGenerativeAi,
    /// Bedrock Converse / ConverseStream.
    BedrockConverse,
    /// Cohere v2.
    CohereV2,
    /// Mistral native Chat Completions.
    MistralConversations,
    /// Stub used by the faux provider in tests.
    Faux,
}

impl Api {
    /// Extension-facing wire id for this family.
    ///
    /// These strings are the ids upstream's TS `Api` union uses and that the
    /// `@earendil-works/pi-ai/compat` registry hands across the QuickJS
    /// boundary — **not** the `snake_case` spelling the `Serialize` impl
    /// produces. [`Api::from_api_id`] is the inverse; the two are the single
    /// source of truth for the mapping (host runners and the extension bridge
    /// both go through them, so they cannot drift apart).
    pub const fn api_id(self) -> &'static str {
        match self {
            Api::AnthropicMessages => "anthropic-messages",
            Api::OpenAiResponses => "openai-responses",
            Api::OpenAiChatCompletions => "openai-completions",
            Api::GoogleGenerativeAi => "google-generative-ai",
            Api::AzureOpenAiResponses => "azure-openai-responses",
            Api::BedrockConverse => "bedrock-converse",
            Api::CohereV2 => "cohere-v2",
            Api::MistralConversations => "mistral-conversations",
            Api::Faux => "faux",
        }
    }

    /// Parse an extension-facing wire id back into an [`Api`].
    ///
    /// Inverse of [`Api::api_id`]. Returns `None` for an unknown id so the
    /// caller can decide whether the family is merely unbridged or genuinely
    /// unknown; callers that only accept a subset (the shim bridge) filter
    /// with their own allow-list on top.
    pub fn from_api_id(id: &str) -> Option<Self> {
        Some(match id {
            "anthropic-messages" => Api::AnthropicMessages,
            "openai-responses" => Api::OpenAiResponses,
            "openai-completions" => Api::OpenAiChatCompletions,
            "google-generative-ai" => Api::GoogleGenerativeAi,
            "azure-openai-responses" => Api::AzureOpenAiResponses,
            "bedrock-converse" => Api::BedrockConverse,
            "cohere-v2" => Api::CohereV2,
            "mistral-conversations" => Api::MistralConversations,
            "faux" => Api::Faux,
            _ => return None,
        })
    }
}

/// A registered model entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    /// Provider identifier.
    pub provider: ProviderId,
    /// Model identifier used in API requests.
    pub id: String,
    /// API family.
    pub api: Api,
    /// Human-readable label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Context window size in tokens.
    #[serde(default)]
    pub context_window: u32,
    /// Maximum output tokens.
    #[serde(default)]
    pub max_output_tokens: u32,
}

/// Token usage reported at the end of a stream.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens consumed.
    pub input: u32,
    /// Output tokens generated.
    pub output: u32,
    /// Cached input tokens (provider-specific; e.g. Anthropic cache reads).
    #[serde(default)]
    pub cache_read: u32,
    /// Cache write tokens (Anthropic cache creation).
    #[serde(default)]
    pub cache_write: u32,
    /// Total tokens (input + output) when the provider reports it.
    #[serde(default)]
    pub total: u32,
}

#[cfg(test)]
mod tests {
    use super::Api;
    use std::collections::HashSet;

    /// Every variant, so the round-trip check cannot silently skip one as the
    /// enum grows.
    const ALL: &[Api] = &[
        Api::OpenAiChatCompletions,
        Api::OpenAiResponses,
        Api::AzureOpenAiResponses,
        Api::AnthropicMessages,
        Api::GoogleGenerativeAi,
        Api::BedrockConverse,
        Api::CohereV2,
        Api::MistralConversations,
        Api::Faux,
    ];

    /// [`Api::api_id`] and [`Api::from_api_id`] are the two directions of one
    /// table. Round-tripping every variant and rejecting duplicate ids keeps
    /// them from drifting apart as variants are added.
    #[test]
    fn api_ids_round_trip_and_stay_unique() {
        let mut seen = HashSet::new();
        for api in ALL {
            let id = api.api_id();
            assert!(seen.insert(id), "duplicate api id `{id}`");
            assert_eq!(Api::from_api_id(id), Some(*api), "round trip for `{id}`");
        }
        assert_eq!(Api::from_api_id("not-a-real-api"), None);
    }
}
