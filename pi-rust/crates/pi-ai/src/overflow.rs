//! Context-overflow detection — the Rust port of
//! `packages/ai/src/utils/overflow.ts`.
//!
//! Providers report "the prompt no longer fits the model" in three different
//! shapes, and the agent has to recognise all of them to decide between
//! retrying, compacting, or failing:
//!
//! 1. **Error-based overflow.** Most providers fail the request with a
//!    provider-specific message (`"prompt is too long: 213462 tokens >
//!    200000 maximum"`, `"Requested token count exceeds the model's maximum
//!    context length of 131072 tokens"`, …). [`is_context_overflow_error_text`]
//!    classifies that text; the non-overflow patterns (`rate limit` /
//!    `too many requests` / Bedrock's human-readable throttling prefix) win
//!    first so a throttling error that happens to mention `too many tokens`
//!    is not mistaken for an overflow.
//! 2. **Silent overflow.** A few deployments (z.ai) accept the oversized
//!    request and answer successfully; the only signal is that
//!    `usage.input + usage.cache_read` came back larger than the model's
//!    context window.
//! 3. **Length-stop overflow.** Xiaomi MiMo truncates the oversized input to
//!    fill the context window exactly and then returns `length` with zero
//!    output. A `length` stop with `output == 0` whose input fills at least
//!    99% of the window is treated as overflow.
//!
//! # Deliberate difference from TypeScript
//!
//! Upstream reads the error text from `AssistantMessage.errorMessage`.
//! `pi-protocol::AssistantMessage` now carries the equivalent
//! `error_message` field, but it is not threaded to this classifier yet: the
//! Rust agent loop surfaces a failed provider call as `Err`, and the only
//! finished-turn event the agent loop keeps (`AssistantMessageEvent::Done`)
//! has no error slot. [`is_context_overflow`] therefore still takes the error
//! text as an explicit `Option<&str>`. Passing
//! `Some(message.error_message.as_deref())` gives upstream's case-1
//! behaviour; callers that only have the `stop_reason` pass `None`, which
//! disables case 1 but keeps cases 2 and 3 — exactly the information the
//! message still carries.

use std::sync::OnceLock;

use pi_protocol::{StopReason, Usage};
use regex::Regex;
use regex::RegexBuilder;

/// Provider error texts that mean "the input exceeded the context window".
///
/// Order in the joined alternation is not significant (`is_match` is
/// unanchored), but each entry is kept verbatim from upstream so the two
/// files diff cleanly. See `packages/ai/src/utils/overflow.ts` for the
/// provider-by-provider examples.
const OVERFLOW_PATTERNS: &[&str] = &[
    "prompt is too long",                                     // Anthropic token overflow
    "request_too_large",                                      // Anthropic request byte-size overflow (HTTP 413)
    "input is too long for requested model",                  // Amazon Bedrock
    "exceeds the context window",                             // OpenAI (Completions & Responses API)
    "exceeds (?:the )?(?:model'?s )?maximum context length(?: of [0-9,]+ tokens?|\\s*\\([0-9,]+\\))", // OpenAI-compatible / LiteLLM
    "input token count.*exceeds the maximum",                 // Google (Gemini)
    "maximum prompt length is [0-9]+",                        // xAI (Grok)
    "reduce the length of the messages",                      // Groq
    "maximum context length is [0-9]+ tokens",                // OpenRouter (most backends)
    "exceeds (?:the )?maximum allowed input length of [0-9,]+ tokens?", // OpenRouter / Poolside
    "input \\([0-9]+ tokens\\) is longer than the model'?s context length \\([0-9]+ tokens\\)", // Together AI
    "exceeds the limit of [0-9]+",                            // GitHub Copilot
    "exceeds the available context size",                     // llama.cpp server
    "greater than the context length",                        // LM Studio
    "context window exceeds limit",                           // MiniMax
    "exceeded model token limit",                             // Kimi For Coding
    "too large for model with [0-9]+ maximum context length", // Mistral
    "prompt has [0-9,]+ tokens?, but the configured context size is [0-9,]+ tokens?", // DS4 server
    "model_context_window_exceeded",                          // z.ai non-standard finish_reason
    "prompt too long; exceeded (?:max )?context length",      // Ollama explicit overflow error
    "range of input length should be",                        // DashScope / Qwen Token Plan
    "context[_ ]length[_ ]exceeded",                          // Generic fallback
    "too many tokens",                                        // Generic fallback
    "token limit exceeded",                                   // Generic fallback
    "^4(?:00|13)\\s*(?:status code)?\\s*\\(no body\\)",       // Cerebras: 400/413 with no body
];

/// Texts that disqualify an otherwise-matching error from overflow detection.
///
/// Bedrock formats throttling as `"Throttling error: Too many tokens, please
/// wait before trying again."`, which would match the `/too many tokens/`
/// overflow pattern; the human-readable prefixes and the generic rate-limit
/// wording are checked first.
const NON_OVERFLOW_PATTERNS: &[&str] = &[
    "^(?:Throttling error|Service unavailable):", // AWS Bedrock non-overflow errors
    "rate limit",                                 // Generic rate limiting
    "too many requests",                          // Generic HTTP 429 style
];

/// Case-insensitive matcher for [`OVERFLOW_PATTERNS`].
fn overflow_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        RegexBuilder::new(&OVERFLOW_PATTERNS.join("|"))
            .case_insensitive(true)
            .build()
            .expect("overflow pattern compiles")
    })
}

/// Case-insensitive matcher for [`NON_OVERFLOW_PATTERNS`].
fn non_overflow_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        RegexBuilder::new(&NON_OVERFLOW_PATTERNS.join("|"))
            .case_insensitive(true)
            .build()
            .expect("non-overflow pattern compiles")
    })
}

/// Whether error **text** names a context-window overflow.
///
/// This is only case 1 of [`is_context_overflow`] — the classifier the agent
/// loop uses to keep an overflow out of the retry budget (upstream
/// `_isRetryableError` checks `isContextOverflow` before
/// `isRetryableAssistantError`). The non-overflow patterns are consulted
/// first.
pub fn is_context_overflow_error_text(error_message: &str) -> bool {
    if error_message.is_empty() {
        return false;
    }
    if non_overflow_pattern().is_match(error_message) {
        return false;
    }
    overflow_pattern().is_match(error_message)
}

/// Whether a finished assistant message indicates a context overflow.
///
/// `error_message` is the provider's error text when the turn failed — for a
/// finished [`pi_protocol::AssistantMessage`] that is
/// `message.error_message.as_deref()`, which the Rust call sites do not thread
/// through yet (see the module docs) — and `context_window` is the model's
/// window size, used for the silent and length-stop cases. A `None` or zero
/// window disables those two cases, and a `None` error text disables the
/// error-message case.
///
/// Mirrors `isContextOverflow(message, contextWindow?)`.
pub fn is_context_overflow(
    stop_reason: StopReason,
    error_message: Option<&str>,
    usage: &Usage,
    context_window: Option<u32>,
) -> bool {
    // Case 1: provider-reported overflow error text.
    if stop_reason == StopReason::Error {
        if let Some(text) = error_message {
            if is_context_overflow_error_text(text) {
                return true;
            }
        }
    }

    let Some(window) = context_window.filter(|window| *window > 0) else {
        return false;
    };
    // `usage.input + usage.cache_read`, in u64 so the comparison cannot wrap.
    let input_tokens = u64::from(usage.input) + u64::from(usage.cache_read);

    // Case 2: silent overflow (z.ai style) — the request succeeded but the
    // reported input already exceeds the window.
    if stop_reason == StopReason::Stop && input_tokens > u64::from(window) {
        return true;
    }

    // Case 3: length-stop overflow (Xiaomi MiMo style) — the server truncated
    // the input to fill the window, leaving no room to generate. `length`
    // maps to `StopReason::MaxTokens`.
    if stop_reason == StopReason::MaxTokens && usage.output == 0 {
        // `inputTokens >= contextWindow * 0.99`, integer-exact.
        if input_tokens * 100 >= u64::from(window) * 99 {
            return true;
        }
    }

    false
}

/// Whether a `length` stop ended below the caller's intended output limit.
///
/// Such a response may be caused by context pressure or provider-side
/// truncation, so the caller can make one bounded compact-and-retry attempt.
/// `desired_max_output` must be the original limit before any
/// context-based clamping.
///
/// Mirrors `isRecoverableLength(message, desiredMaxOutput)`.
pub fn is_recoverable_length(
    stop_reason: StopReason,
    usage: &Usage,
    desired_max_output: u32,
) -> bool {
    stop_reason == StopReason::MaxTokens
        && desired_max_output > 0
        && usage.output < desired_max_output
}

/// The overflow patterns, for callers that want to build their own matcher or
/// assert against them in tests.
///
/// Mirrors `getOverflowPatterns()`.
pub fn get_overflow_patterns() -> &'static [&'static str] {
    OVERFLOW_PATTERNS
}

/// The non-overflow patterns, for tests — mirrors upstream's internal list.
pub fn get_non_overflow_patterns() -> &'static [&'static str] {
    NON_OVERFLOW_PATTERNS
}
