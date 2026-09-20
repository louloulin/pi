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
