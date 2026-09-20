//! Thinking-level policy — the pure core the interactive driver routes every
//! entry point through.
//!
//! Upstream keeps this logic in `AgentSession`
//! (`packages/coding-agent/src/core/agent-session.ts:1814-1862`) and the
//! `packages/ai` catalogue (`getSupportedThinkingLevels` /
//! `clampThinkingLevel`, `packages/ai/src/models.ts:915-946`). The Rust port
//! has the [`ThinkingLevel`] type and the border palette but no consumer, so
//! the parse / clamp / cycle rules live here once and the four entry points
//! (`app.thinking.cycle`, `/thinking <level>`, the `/thinking` selector and
//! `app.thinking.save`) all call into them.
//!
//! # The reasoning-capability equivalent flag
//!
//! Upstream gates thinking on `Model.reasoning` (a plain boolean on the model
//! descriptor, `packages/ai/src/types.ts:851`). The Rust descriptor
//! ([`pi_protocol::Model`]) carries no such field: the provider registry
//! deliberately drops it (`pi-ai/src/providers/registry.rs:585`), and
//! `pi-ai` is frozen for this stage. [`model_supports_thinking`] is therefore
//! the documented equivalent flag: it derives the capability from data the
//! descriptor *does* carry (its API family and id) instead of inventing a
//! field. The mapping is conservative — the families whose upstream adapters
//! accept a thinking / `reasoning_effort` parameter reason, `openai-completions`
//! only for ids that are known reasoning models, and the deterministic `faux`
//! double never reasons.

use pi_agent_core::ThinkingLevel;
use pi_protocol::{Api, Model};

/// Upstream's `DEFAULT_THINKING_LEVEL`
/// (`packages/coding-agent/src/core/defaults.ts`).
pub const DEFAULT_THINKING_LEVEL: ThinkingLevel = ThinkingLevel::Medium;

/// Every level, in upstream declaration order (`THINKING_LEVEL_OPTIONS`).
pub const THINKING_LEVEL_OPTIONS: [ThinkingLevel; 7] = ThinkingLevel::ALL;

/// The levels a model that does not reason supports — `off` only
/// (`getSupportedThinkingLevels`, `packages/ai/src/models.ts:915`).
pub const OFF_ONLY: [ThinkingLevel; 1] = [ThinkingLevel::Off];

/// Parse a `/thinking <level>` argument. Case-insensitive, accepts the
/// upstream spelling plus a couple of readable aliases for `xhigh`.
pub fn parse_thinking_level(text: &str) -> Option<ThinkingLevel> {
    text.parse().ok()
}

/// The level's canonical spelling (`off` … `max`).
pub fn level_label(level: ThinkingLevel) -> &'static str {
    level.as_str()
}

/// `off, minimal, low, medium, high, xhigh, max` — the `Available levels:`
/// list upstream prints for an unknown `/thinking` argument
/// (`interactive-mode.ts:4789`) and the settings loader warns with.
pub fn available_levels_list() -> String {
    THINKING_LEVEL_OPTIONS
        .iter()
        .map(|level| level.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// One-line description shown in the `/thinking` selector, copied from
/// upstream `LEVEL_DESCRIPTIONS`
/// (`components/thinking-selector.ts:17-25`).
pub fn level_description(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "No reasoning",
        ThinkingLevel::Minimal => "Very brief reasoning (~1k tokens)",
        ThinkingLevel::Low => "Light reasoning (~2k tokens)",
        ThinkingLevel::Medium => "Moderate reasoning (~8k tokens)",
        ThinkingLevel::High => "Deep reasoning (~16k tokens)",
        ThinkingLevel::Xhigh => "Extra-high reasoning (~32k tokens)",
        ThinkingLevel::Max => "Maximum reasoning",
    }
}

/// The levels a model supports.
///
/// A non-reasoning model offers `off` alone; a reasoning model offers every
/// level. Upstream additionally filters `xhigh` / `max` through
/// `thinkingLevelMap`, which the Rust descriptor does not carry, so they stay
/// available whenever the model reasons — the provider clamps what it cannot
/// honour.
pub fn available_thinking_levels(supports_reasoning: bool) -> &'static [ThinkingLevel] {
    if supports_reasoning {
        &THINKING_LEVEL_OPTIONS
    } else {
        &OFF_ONLY
    }
}

/// Clamp `level` to the nearest available level, preferring higher levels
/// first and then lower ones — upstream `clampThinkingLevel`
/// (`packages/ai/src/models.ts:926`).
pub fn clamp_thinking_level(supports_reasoning: bool, level: ThinkingLevel) -> ThinkingLevel {
    let available = available_thinking_levels(supports_reasoning);
    if available.contains(&level) {
        return level;
    }
    let requested = THINKING_LEVEL_OPTIONS
        .iter()
        .position(|candidate| *candidate == level)
        .unwrap_or(0);
    for candidate in &THINKING_LEVEL_OPTIONS[requested..] {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    for candidate in THINKING_LEVEL_OPTIONS[..requested].iter().rev() {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    available[0]
}

/// The next level `app.thinking.cycle` moves to, or `None` when the model does
/// not support thinking at all — upstream `AgentSession::cycleThinkingLevel`
/// (`agent-session.ts:1843`).
pub fn cycle_thinking_level(
    supports_reasoning: bool,
    current: ThinkingLevel,
) -> Option<ThinkingLevel> {
    if !supports_reasoning {
        return None;
    }
    let levels = available_thinking_levels(true);
    let index = levels
        .iter()
        .position(|candidate| *candidate == current)
        .unwrap_or(0);
    Some(levels[(index + 1) % levels.len()])
}

/// The equivalent flag for upstream's `Model.reasoning` — see the module
/// docs for why the descriptor cannot carry the boolean itself.
pub fn model_supports_thinking(model: &Model) -> bool {
    match model.api {
        // The deterministic test double never reasons (upstream
        // `providers/faux.ts` builds its models with `reasoning: false`).
        Api::Faux => false,
        // These families' upstream adapters all accept a thinking /
        // `reasoning_effort` parameter for their shipped catalogue.
        Api::AnthropicMessages
        | Api::GoogleGenerativeAi
        | Api::BedrockConverse
        | Api::MistralConversations => true,
        // Mixed families: only specific ids reason.
        Api::OpenAiChatCompletions | Api::OpenAiResponses | Api::AzureOpenAiResponses => {
            openai_id_reasons(&model.id)
        }
        // No thinking controls in the Rust port's Cohere adapter.
        Api::CohereV2 => false,
    }
}

/// Whether an OpenAI-shaped model id denotes a reasoning model. The catalogue
/// mixes reasoning and non-reasoning ids in one family, so the id is the only
/// signal left once `Model.reasoning` is dropped.
fn openai_id_reasons(id: &str) -> bool {
    let lower = id.to_ascii_lowercase();
    let tokens = lower
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .collect::<Vec<_>>();
    const REASONING_TOKENS: &[&str] = &["o1", "o3", "o4"];
    tokens.iter().any(|token| REASONING_TOKENS.contains(token))
        || lower.starts_with("gpt-5")
        || lower.contains("codex")
        || lower.contains("thinking")
        || lower.contains("reason")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_protocol::ProviderId;

    fn model(api: Api, id: &str) -> Model {
        Model {
            provider: ProviderId::new("test"),
            id: id.into(),
            api,
            label: None,
            context_window: 1024,
            max_output_tokens: 256,
        }
    }

    #[test]
    fn parses_every_level_and_is_case_insensitive() {
        for level in THINKING_LEVEL_OPTIONS {
            assert_eq!(parse_thinking_level(level.as_str()), Some(level));
            assert_eq!(
                parse_thinking_level(&level.as_str().to_uppercase()),
                Some(level)
            );
        }
        assert_eq!(parse_thinking_level("  HIGH "), Some(ThinkingLevel::High));
        assert_eq!(parse_thinking_level("x-high"), Some(ThinkingLevel::Xhigh));
        assert_eq!(parse_thinking_level("bogus"), None);
    }

    #[test]
    fn a_non_reasoning_model_only_offers_off() {
        assert_eq!(available_thinking_levels(false), &[ThinkingLevel::Off]);
        assert_eq!(available_thinking_levels(true).len(), 7);
        assert_eq!(
            clamp_thinking_level(false, ThinkingLevel::Max),
            ThinkingLevel::Off
        );
        assert_eq!(cycle_thinking_level(false, ThinkingLevel::Off), None);
    }

    #[test]
    fn cycling_visits_every_level_then_wraps() {
        let mut level = ThinkingLevel::Off;
        let mut seen = vec![level];
        for _ in 0..THINKING_LEVEL_OPTIONS.len() - 1 {
            level = cycle_thinking_level(true, level).expect("supported");
            seen.push(level);
        }
        assert_eq!(seen, THINKING_LEVEL_OPTIONS.to_vec());
        assert_eq!(
            cycle_thinking_level(true, ThinkingLevel::Max),
            Some(ThinkingLevel::Off),
            "the end of the ladder wraps"
        );
    }

    #[test]
    fn clamping_keeps_a_supported_level_untouched() {
        for level in THINKING_LEVEL_OPTIONS {
            assert_eq!(clamp_thinking_level(true, level), level);
        }
    }

    #[test]
    fn the_faux_double_never_reasons() {
        assert!(!model_supports_thinking(&model(Api::Faux, "faux-model")));
    }

    #[test]
    fn anthropic_and_google_models_reason() {
        assert!(model_supports_thinking(&model(
            Api::AnthropicMessages,
            "claude-sonnet-4-5"
        )));
        assert!(model_supports_thinking(&model(
            Api::GoogleGenerativeAi,
            "gemini-2.5-pro"
        )));
    }

    #[test]
    fn openai_completions_only_reasons_for_reasoning_ids() {
        assert!(model_supports_thinking(&model(
            Api::OpenAiChatCompletions,
            "o3-mini"
        )));
        assert!(model_supports_thinking(&model(
            Api::OpenAiChatCompletions,
            "gpt-5-codex"
        )));
        assert!(!model_supports_thinking(&model(
            Api::OpenAiChatCompletions,
            "gpt-4o"
        )));
        assert!(!model_supports_thinking(&model(
            Api::OpenAiChatCompletions,
            "gpt-4o-mini"
        )));
    }

    #[test]
    fn azure_responses_follows_the_same_id_rule() {
        assert!(model_supports_thinking(&model(
            Api::AzureOpenAiResponses,
            "gpt-5"
        )));
        assert!(!model_supports_thinking(&model(
            Api::AzureOpenAiResponses,
            "gpt-4.1"
        )));
    }
}
