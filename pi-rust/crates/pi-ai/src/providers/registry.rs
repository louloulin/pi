//! Static, data-driven provider registry.
//!
//! Before this module existed, everything that knew about a provider was
//! scattered across `pi-coding-agent`: the model catalog lived in
//! `main.rs::build_default_models`, and the credential / base-URL env
//! var mapping lived in `provider.rs`. Adding a provider meant editing
//! three files. The registry moves the *data* — id, display name, API
//! family, default base URL, credential env vars, base-URL override env
//! vars and the built-in model catalog — into one table, so a new
//! provider is a single [`ProviderSpec`] entry.
//!
//! The table mirrors the TypeScript upstream's `packages/ai/src/providers/*.ts`.
//! The upstream ships dozens of providers; the Rust port currently covers
//! the four first-party API families (`faux`, `openai`,
//! `anthropic`, `google`) plus the OpenAI Chat Completions–compatible
//! family, whose members only differ by base URL, credential and model
//! ids. Providers that speak a different wire protocol (OpenAI Responses,
//! Bedrock Converse, Cohere v2, Mistral conversations, Anthropic-native
//! such as `kimi-coding`) are intentionally absent until their adapters
//! land.
//!
//! All entries in [`BUILTIN_PROVIDERS`] use one of the four adapters in
//! [`super`]: [`pi_protocol::Api::OpenAiChatCompletions`] maps to
//! [`super::openai::OpenAiProvider`], and so on.

use pi_protocol::Api;

/// One model in a provider's built-in catalog.
///
/// The adapter only needs `id`; `label`, `context_window` and
/// `max_output_tokens` feed `pi list-models` and the TUI. They mirror the
/// upstream generated catalog, which is not checked into the repository,
/// so they are curated here and may drift from the vendor's live values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    /// Model id sent on the wire (e.g. `deepseek-v4-pro`).
    pub id: &'static str,
    /// Human-readable label shown by `pi list-models`.
    pub label: &'static str,
    /// Context window size in tokens.
    pub context_window: u32,
    /// Maximum output tokens.
    pub max_output_tokens: u32,
}

impl ModelSpec {
    /// Build a spec from its id and label, defaulting the token limits.
    pub const fn new(id: &'static str, label: &'static str) -> Self {
        Self {
            id,
            label,
            context_window: 128_000,
            max_output_tokens: 32_768,
        }
    }

    /// Override the token limits.
    pub const fn with_limits(mut self, context_window: u32, max_output_tokens: u32) -> Self {
        self.context_window = context_window;
        self.max_output_tokens = max_output_tokens;
        self
    }
}

/// Static description of one provider.
///
/// A `ProviderSpec` carries everything needed to build the provider's
/// streaming adapter and to populate the model catalog — no per-provider
/// code path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderSpec {
    /// Provider id, matching `Model.provider` and the upstream catalog
    /// key (e.g. `deepseek`).
    pub id: &'static str,
    /// Display name (e.g. `DeepSeek`).
    pub display_name: &'static str,
    /// API family the adapter speaks for this provider.
    pub api: Api,
    /// Base URL used when no base-URL override env var is set. Empty for
    /// providers that need no transport (faux).
    pub default_base_url: &'static str,
    /// Credential env vars in priority order. Empty when no credential
    /// is required (faux).
    pub api_key_env: &'static [&'static str],
    /// Base-URL override env vars in priority order. Empty when the
    /// provider has no override.
    pub base_url_env: &'static [&'static str],
    /// Built-in model catalog.
    pub models: &'static [ModelSpec],
}

impl ProviderSpec {
    /// Whether a credential is required to register an adapter.
    pub fn requires_api_key(&self) -> bool {
        !self.api_key_env.is_empty()
    }
}

/// DeepSeek — https://api.deepseek.com (`/chat/completions`).
const DEEPSEEK_MODELS: &[ModelSpec] = &[
    ModelSpec::new("deepseek-chat", "DeepSeek Chat"),
    ModelSpec::new("deepseek-r1", "DeepSeek R1").with_limits(128_000, 65_536),
    ModelSpec::new("deepseek-v3.2", "DeepSeek V3.2").with_limits(128_000, 65_536),
    ModelSpec::new("deepseek-v4-flash", "DeepSeek V4 Flash").with_limits(128_000, 65_536),
    ModelSpec::new("deepseek-v4-pro", "DeepSeek V4 Pro").with_limits(128_000, 65_536),
];

/// Moonshot AI (global + mainland China). Same model ids on both hosts.
const MOONSHOT_MODELS: &[ModelSpec] = &[
    ModelSpec::new("kimi-k2.5", "Kimi K2.5").with_limits(256_000, 32_768),
    ModelSpec::new("kimi-k2.6", "Kimi K2.6").with_limits(256_000, 32_768),
];

/// NVIDIA NIM (model ids are vendor-namespaced).
const NVIDIA_MODELS: &[ModelSpec] =
    &[
        ModelSpec::new("nvidia/nemotron-3-super-120b-a12b", "Nemotron 3 Super 120B")
            .with_limits(131_072, 32_768),
    ];

/// OpenRouter (also exposes the `auto` router model).
const OPENROUTER_MODELS: &[ModelSpec] = &[
    ModelSpec::new("moonshotai/kimi-k2.6", "Kimi K2.6").with_limits(256_000, 32_768),
    ModelSpec::new("auto", "OpenRouter Auto"),
];

/// Z.AI Coding Plan (global).
const ZAI_MODELS: &[ModelSpec] = &[
    ModelSpec::new("glm-5", "GLM-5"),
    ModelSpec::new("glm-5.1", "GLM-5.1"),
    ModelSpec::new("glm-5.2", "GLM-5.2"),
    ModelSpec::new("glm-5.3", "GLM-5.3"),
];

/// Z.AI Coding Plan (mainland China, bigmodel.cn).
const ZAI_CODING_CN_MODELS: &[ModelSpec] = &[
    ModelSpec::new("glm-5.3", "GLM-5.3"),
    ModelSpec::new("glm-5.1", "GLM-5.1"),
    ModelSpec::new("glm-4.6v", "GLM-4.6V"),
    ModelSpec::new("glm-5v-turbo", "GLM-5V Turbo"),
];

/// Baseten Model APIs (model ids are vendor-namespaced).
const BASETEN_MODELS: &[ModelSpec] = &[
    ModelSpec::new("zai-org/GLM-5.2", "GLM-5.2").with_limits(200_000, 32_768),
    ModelSpec::new("moonshotai/Kimi-K2.6", "Kimi K2.6").with_limits(262_144, 32_768),
];

/// Groq.
const GROQ_MODELS: &[ModelSpec] =
    &[ModelSpec::new("openai/gpt-oss-120b", "GPT-OSS 120B").with_limits(131_072, 32_768)];

/// Cerebras.
const CEREBRAS_MODELS: &[ModelSpec] =
    &[ModelSpec::new("gpt-oss-120b", "GPT-OSS 120B").with_limits(131_072, 32_768)];

/// Hugging Face Inference Providers router.
const HUGGINGFACE_MODELS: &[ModelSpec] =
    &[ModelSpec::new("moonshotai/Kimi-K2.6", "Kimi K2.6").with_limits(256_000, 32_768)];

/// Together AI.
const TOGETHER_MODELS: &[ModelSpec] = &[
    ModelSpec::new("moonshotai/Kimi-K2.6", "Kimi K2.6").with_limits(256_000, 32_768),
    ModelSpec::new("deepseek-ai/DeepSeek-V4-Pro", "DeepSeek V4 Pro").with_limits(128_000, 65_536),
    ModelSpec::new("openai/gpt-oss-120b", "GPT-OSS 120B").with_limits(128_000, 32_768),
];

/// Fireworks AI (model ids are path-shaped).
const FIREWORKS_MODELS: &[ModelSpec] = &[
    ModelSpec::new("accounts/fireworks/models/kimi-k2p6", "Kimi K2.6").with_limits(256_000, 32_768),
    ModelSpec::new("accounts/fireworks/models/kimi-k3", "Kimi K3").with_limits(256_000, 32_768),
    ModelSpec::new("accounts/fireworks/models/glm-5p2", "GLM-5.2").with_limits(200_000, 32_768),
    ModelSpec::new(
        "accounts/fireworks/models/deepseek-v4-flash-0731",
        "DeepSeek V4 Flash (0731)",
    )
    .with_limits(128_000, 65_536),
];

/// Xiaomi MiMo.
const XIAOMI_MODELS: &[ModelSpec] = &[ModelSpec::new("mimo-v2.5-pro", "MiMo v2.5 Pro")];

/// The first-party faux provider used by tests and offline runs.
const FAUX_MODELS: &[ModelSpec] =
    &[ModelSpec::new("faux-model", "Faux test model").with_limits(8_192, 1_024)];

/// The four first-party providers plus the OpenAI-compatible family.
///
/// First-party providers (faux / openai / anthropic / google) come first
/// so `pi list-models` and error messages keep their historical ordering;
/// the family is appended alphabetically.
pub const BUILTIN_PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: "faux",
        display_name: "Faux",
        api: Api::Faux,
        default_base_url: "",
        api_key_env: &[],
        base_url_env: &[],
        models: FAUX_MODELS,
    },
    ProviderSpec {
        id: "openai",
        display_name: "OpenAI",
        api: Api::OpenAiChatCompletions,
        default_base_url: "https://api.openai.com/v1",
        api_key_env: &["OPENAI_API_KEY"],
        base_url_env: &["OPENAI_BASE_URL"],
        models: &[ModelSpec::new("gpt-4o-mini", "GPT-4o mini").with_limits(128_000, 16_384)],
    },
    ProviderSpec {
        id: "anthropic",
        display_name: "Anthropic",
        api: Api::AnthropicMessages,
        default_base_url: "https://api.anthropic.com",
        api_key_env: &[
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_OAUTH_TOKEN",
        ],
        base_url_env: &["ANTHROPIC_BASE_URL"],
        models: &[
            ModelSpec::new("claude-sonnet-4-5", "Claude Sonnet 4.5").with_limits(200_000, 8_192),
            ModelSpec::new("claude-opus-4-5", "Claude Opus 4.5").with_limits(200_000, 8_192),
            ModelSpec::new("claude-haiku-4-5", "Claude Haiku 4.5").with_limits(200_000, 8_192),
        ],
    },
    ProviderSpec {
        id: "google",
        display_name: "Google",
        api: Api::GoogleGenerativeAi,
        default_base_url: "https://generativelanguage.googleapis.com/v1beta",
        api_key_env: &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        base_url_env: &["GEMINI_BASE_URL", "GOOGLE_BASE_URL"],
        models: &[
            ModelSpec::new("gemini-2.5-pro", "Gemini 2.5 Pro").with_limits(1_048_576, 65_536),
            ModelSpec::new("gemini-2.5-flash", "Gemini 2.5 Flash").with_limits(1_048_576, 65_536),
            ModelSpec::new("gemini-2.5-flash-lite", "Gemini 2.5 Flash Lite")
                .with_limits(1_048_576, 65_536),
        ],
    },
    ProviderSpec {
        id: "baseten",
        display_name: "Baseten",
        api: Api::OpenAiChatCompletions,
        default_base_url: "https://inference.baseten.co/v1",
        api_key_env: &["BASETEN_API_KEY"],
        base_url_env: &["BASETEN_BASE_URL"],
        models: BASETEN_MODELS,
    },
    ProviderSpec {
        id: "cerebras",
        display_name: "Cerebras",
        api: Api::OpenAiChatCompletions,
        default_base_url: "https://api.cerebras.ai/v1",
        api_key_env: &["CEREBRAS_API_KEY"],
        base_url_env: &["CEREBRAS_BASE_URL"],
        models: CEREBRAS_MODELS,
    },
    ProviderSpec {
        id: "deepseek",
        display_name: "DeepSeek",
        api: Api::OpenAiChatCompletions,
        default_base_url: "https://api.deepseek.com",
        api_key_env: &["DEEPSEEK_API_KEY"],
        base_url_env: &["DEEPSEEK_BASE_URL"],
        models: DEEPSEEK_MODELS,
    },
    ProviderSpec {
        id: "fireworks",
        display_name: "Fireworks AI",
        api: Api::OpenAiChatCompletions,
        default_base_url: "https://api.fireworks.ai/inference",
        api_key_env: &["FIREWORKS_API_KEY"],
        base_url_env: &["FIREWORKS_BASE_URL"],
        models: FIREWORKS_MODELS,
    },
    ProviderSpec {
        id: "groq",
        display_name: "Groq",
        api: Api::OpenAiChatCompletions,
        default_base_url: "https://api.groq.com/openai/v1",
        api_key_env: &["GROQ_API_KEY"],
        base_url_env: &["GROQ_BASE_URL"],
        models: GROQ_MODELS,
    },
    ProviderSpec {
        id: "huggingface",
        display_name: "Hugging Face",
        api: Api::OpenAiChatCompletions,
        default_base_url: "https://router.huggingface.co/v1",
        api_key_env: &["HF_TOKEN"],
        base_url_env: &["HUGGINGFACE_BASE_URL"],
        models: HUGGINGFACE_MODELS,
    },
    ProviderSpec {
        id: "moonshotai",
        display_name: "Moonshot AI",
        api: Api::OpenAiChatCompletions,
        default_base_url: "https://api.moonshot.ai/v1",
        api_key_env: &["MOONSHOT_API_KEY"],
        base_url_env: &["MOONSHOT_BASE_URL"],
        models: MOONSHOT_MODELS,
    },
    ProviderSpec {
        id: "moonshotai-cn",
        display_name: "Moonshot AI (China)",
        api: Api::OpenAiChatCompletions,
        default_base_url: "https://api.moonshot.cn/v1",
        api_key_env: &["MOONSHOT_API_KEY"],
        base_url_env: &["MOONSHOT_CN_BASE_URL"],
        models: MOONSHOT_MODELS,
    },
    ProviderSpec {
        id: "nvidia",
        display_name: "NVIDIA",
        api: Api::OpenAiChatCompletions,
        default_base_url: "https://integrate.api.nvidia.com/v1",
        api_key_env: &["NVIDIA_API_KEY"],
        base_url_env: &["NVIDIA_BASE_URL"],
        models: NVIDIA_MODELS,
    },
    ProviderSpec {
        id: "openrouter",
        display_name: "OpenRouter",
        api: Api::OpenAiChatCompletions,
        default_base_url: "https://openrouter.ai/api/v1",
        api_key_env: &["OPENROUTER_API_KEY"],
        base_url_env: &["OPENROUTER_BASE_URL"],
        models: OPENROUTER_MODELS,
    },
    ProviderSpec {
        id: "together",
        display_name: "Together AI",
        api: Api::OpenAiChatCompletions,
        default_base_url: "https://api.together.ai/v1",
        api_key_env: &["TOGETHER_API_KEY"],
        base_url_env: &["TOGETHER_BASE_URL"],
        models: TOGETHER_MODELS,
    },
    ProviderSpec {
        id: "xiaomi",
        display_name: "Xiaomi MiMo",
        api: Api::OpenAiChatCompletions,
        default_base_url: "https://api.xiaomimimo.com/v1",
        api_key_env: &["XIAOMI_API_KEY"],
        base_url_env: &["XIAOMI_BASE_URL"],
        models: XIAOMI_MODELS,
    },
    ProviderSpec {
        id: "zai",
        display_name: "Z.AI",
        api: Api::OpenAiChatCompletions,
        default_base_url: "https://api.z.ai/api/coding/paas/v4",
        api_key_env: &["ZAI_API_KEY"],
        base_url_env: &["ZAI_BASE_URL"],
        models: ZAI_MODELS,
    },
    ProviderSpec {
        id: "zai-coding-cn",
        display_name: "Z.AI Coding (China)",
        api: Api::OpenAiChatCompletions,
        default_base_url: "https://open.bigmodel.cn/api/coding/paas/v4",
        api_key_env: &["ZAI_CODING_CN_API_KEY"],
        base_url_env: &["ZAI_CODING_CN_BASE_URL"],
        models: ZAI_CODING_CN_MODELS,
    },
];

/// Look up a provider spec by id.
pub fn find_provider(id: &str) -> Option<&'static ProviderSpec> {
    BUILTIN_PROVIDERS.iter().find(|spec| spec.id == id)
}

/// Credential env vars for `provider`, in priority order.
///
/// Empty for providers that need no credential (faux) or that are unknown.
pub fn api_key_env_vars(provider: &str) -> &'static [&'static str] {
    find_provider(provider)
        .map(|spec| spec.api_key_env)
        .unwrap_or(&[])
}

/// Base-URL override env vars for `provider`, in priority order.
pub fn base_url_env_vars(provider: &str) -> &'static [&'static str] {
    find_provider(provider)
        .map(|spec| spec.base_url_env)
        .unwrap_or(&[])
}

/// Default base URL for `provider`, or `None` when unknown.
pub fn default_base_url(provider: &str) -> Option<&'static str> {
    find_provider(provider).map(|spec| spec.default_base_url)
}

/// All provider ids in catalog order.
pub fn provider_ids() -> impl Iterator<Item = &'static str> {
    BUILTIN_PROVIDERS.iter().map(|spec| spec.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn provider_ids_are_unique() {
        let mut seen = HashSet::new();
        for spec in BUILTIN_PROVIDERS {
            assert!(seen.insert(spec.id), "duplicate provider id `{}`", spec.id);
        }
    }

    #[test]
    fn model_ids_are_unique_per_provider() {
        for spec in BUILTIN_PROVIDERS {
            let mut seen = HashSet::new();
            for model in spec.models {
                assert!(
                    seen.insert(model.id),
                    "duplicate model `{}` on provider `{}`",
                    model.id,
                    spec.id
                );
            }
        }
    }

    #[test]
    fn api_key_env_vars_are_unique_per_provider() {
        for spec in BUILTIN_PROVIDERS {
            let mut seen = HashSet::new();
            for var in spec.api_key_env {
                assert!(
                    seen.insert(*var),
                    "duplicate credential env var `{var}` on provider `{}`",
                    spec.id
                );
            }
        }
    }

    #[test]
    fn base_url_env_vars_are_unique_per_provider() {
        for spec in BUILTIN_PROVIDERS {
            let mut seen = HashSet::new();
            for var in spec.base_url_env {
                assert!(
                    seen.insert(*var),
                    "duplicate base-URL env var `{var}` on provider `{}`",
                    spec.id
                );
            }
        }
    }

    #[test]
    fn credentialed_providers_have_a_default_base_url() {
        for spec in BUILTIN_PROVIDERS {
            if spec.requires_api_key() {
                assert!(
                    !spec.default_base_url.is_empty(),
                    "provider `{}` needs a default base URL",
                    spec.id
                );
            }
        }
    }

    #[test]
    fn faux_needs_no_credential_and_no_base_url() {
        let faux = find_provider("faux").expect("faux is built in");
        assert!(!faux.requires_api_key());
        assert!(faux.default_base_url.is_empty());
        assert!(faux.base_url_env.is_empty());
        assert!(!faux.models.is_empty());
    }

    #[test]
    fn openai_compatible_family_is_present() {
        for id in [
            "baseten",
            "cerebras",
            "deepseek",
            "fireworks",
            "groq",
            "huggingface",
            "moonshotai",
            "moonshotai-cn",
            "nvidia",
            "openrouter",
            "together",
            "xiaomi",
            "zai",
            "zai-coding-cn",
        ] {
            let spec = find_provider(id).unwrap_or_else(|| panic!("missing provider `{id}`"));
            assert_eq!(spec.api, Api::OpenAiChatCompletions, "provider `{id}`");
            assert!(spec.requires_api_key(), "provider `{id}`");
            assert!(!spec.models.is_empty(), "provider `{id}`");
        }
    }

    #[test]
    fn lookup_helpers_fall_back_to_empty() {
        assert!(api_key_env_vars("nope").is_empty());
        assert!(base_url_env_vars("nope").is_empty());
        assert_eq!(default_base_url("nope"), None);
        assert_eq!(api_key_env_vars("deepseek"), &["DEEPSEEK_API_KEY"]);
        assert_eq!(
            default_base_url("deepseek"),
            Some("https://api.deepseek.com")
        );
    }

    #[test]
    fn provider_ids_iterates_the_table() {
        let ids: Vec<&str> = provider_ids().collect();
        assert_eq!(ids.len(), BUILTIN_PROVIDERS.len());
        assert_eq!(ids.first().copied(), Some("faux"));
    }
}
