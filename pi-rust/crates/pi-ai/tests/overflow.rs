//! Port of `packages/ai/test/overflow.test.ts`.
//!
//! The vectors mirror upstream one-for-one; the only adaptation is the
//! `AssistantMessage` split (Rust has no `errorMessage`), so the error cases
//! pass the text to [`is_context_overflow`] directly.

use pi_ai::{is_context_overflow, is_context_overflow_error_text, is_recoverable_length};
use pi_protocol::{StopReason, Usage};

fn zero_usage() -> Usage {
    Usage::default()
}

/// `createErrorMessage(errorMessage)` — a failed turn with no usage.
fn is_error_overflow(error_message: &str, context_window: u32) -> bool {
    is_context_overflow(
        StopReason::Error,
        Some(error_message),
        &zero_usage(),
        Some(context_window),
    )
}

fn usage(input: u32, cache_read: u32, cache_write: u32, output: u32) -> Usage {
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        total: input + cache_read + cache_write + output,
    }
}

#[test]
fn detects_explicit_ollama_prompt_too_long_errors() {
    assert!(is_error_overflow(
        "400 `prompt too long; exceeded max context length by 100918 tokens`",
        32768,
    ));
}

#[test]
fn detects_together_ai_context_length_errors() {
    assert!(is_error_overflow(
        "400 The input (516368 tokens) is longer than the model's context length (262144 tokens).",
        262144,
    ));
}

#[test]
fn detects_litellm_wrapped_openai_maximum_context_length_errors() {
    assert!(is_error_overflow(
        "Error: 503 litellm.ServiceUnavailableError: litellm.MidStreamFallbackError: litellm.APIConnectionError: APIConnectionError: OpenAIException - Requested token count exceeds the model's maximum context length of 131072 tokens.",
        131072,
    ));
}

#[test]
fn detects_openai_compatible_parenthesized_maximum_context_length_errors() {
    assert!(is_error_overflow(
        "Error: 400 Input length (265330) exceeds model's maximum context length (262144).",
        262144,
    ));
}

#[test]
fn detects_openrouter_poolside_maximum_allowed_input_length_errors() {
    assert!(is_error_overflow(
        "Provider returned error: Input length 131393 exceeds the maximum allowed input length of 131040 tokens.",
        131072,
    ));
}

#[test]
fn detects_ds4_configured_context_size_errors() {
    assert!(is_error_overflow(
        "400 Prompt has 256468 tokens, but the configured context size is 256000 tokens",
        256000,
    ));
    assert!(is_error_overflow(
        "Prompt has 5,958,968 tokens, but the configured context size is 256,000 tokens",
        256000,
    ));
}

#[test]
fn does_not_treat_generic_non_overflow_ollama_errors_as_overflow() {
    assert!(!is_error_overflow(
        "500 `model runner crashed unexpectedly`",
        32768,
    ));
}

#[test]
fn does_not_treat_bedrock_throttling_too_many_tokens_as_overflow() {
    // Bedrock returns this for HTTP 429 rate limiting, NOT context overflow.
    // `formatBedrockError` uses a human-readable prefix for ThrottlingException.
    assert!(!is_error_overflow(
        "Throttling error: Too many tokens, please wait before trying again.",
        200000,
    ));
}

#[test]
fn does_not_treat_bedrock_service_unavailable_as_overflow() {
    assert!(!is_error_overflow(
        "Service unavailable: The service is temporarily unavailable.",
        200000,
    ));
}

#[test]
fn does_not_treat_generic_rate_limit_errors_as_overflow() {
    assert!(!is_error_overflow(
        "Rate limit exceeded, please retry after 30 seconds.",
        200000,
    ));
}

#[test]
fn does_not_treat_http_429_style_errors_as_overflow() {
    assert!(!is_error_overflow(
        "Too many requests. Please slow down.",
        200000,
    ));
}

#[test]
fn detects_xiaomi_style_overflow_length_stop_with_zero_output_and_filled_context() {
    // `createLengthStopMessage({ input: 58, cacheRead: 1048512, output: 0 })`.
    let message = usage(58, 1048512, 0, 0);
    assert!(is_context_overflow(
        StopReason::MaxTokens,
        None,
        &message,
        Some(1_048_576),
    ));
}

#[test]
fn treats_a_length_stop_below_the_desired_output_limit_as_recoverable() {
    // `createLengthStopMessage({ input: 3, cacheRead: 253584, cacheWrite:
    // 25554, output: 16 })`, desired limit 128000.
    let message = usage(3, 253584, 25554, 16);
    assert!(is_recoverable_length(
        StopReason::MaxTokens,
        &message,
        128000
    ));
}

#[test]
fn does_not_recover_a_length_stop_that_reached_the_desired_output_limit() {
    let message = usage(4062, 0, 0, 1024);
    assert!(!is_recoverable_length(
        StopReason::MaxTokens,
        &message,
        1024
    ));
}

#[test]
fn treats_zero_output_length_stops_as_recoverable_without_context_metadata() {
    let message = usage(100, 0, 0, 0);
    assert!(is_recoverable_length(
        StopReason::MaxTokens,
        &message,
        128000
    ));
}

#[test]
fn does_not_treat_normal_length_stops_with_output_as_context_overflow() {
    let message = usage(1000, 0, 0, 4096);
    assert!(!is_context_overflow(
        StopReason::MaxTokens,
        None,
        &message,
        Some(200000),
    ));
}

#[test]
fn does_not_treat_zero_output_length_stops_far_below_context_as_context_overflow() {
    let message = usage(100, 0, 0, 0);
    assert!(!is_context_overflow(
        StopReason::MaxTokens,
        None,
        &message,
        Some(200000),
    ));
}

#[test]
fn detects_silent_overflow_when_usage_exceeds_the_window() {
    // z.ai style: the turn succeeds, but the input already blew the window.
    let message = usage(210_000, 0, 0, 10);
    assert!(is_context_overflow(
        StopReason::Stop,
        None,
        &message,
        Some(200_000),
    ));
    // Exactly at the window is still fine.
    let at_limit = usage(200_000, 0, 0, 10);
    assert!(!is_context_overflow(
        StopReason::Stop,
        None,
        &at_limit,
        Some(200_000),
    ));
}

#[test]
fn a_missing_or_zero_context_window_disables_the_usage_cases() {
    let silent = usage(210_000, 0, 0, 10);
    assert!(!is_context_overflow(StopReason::Stop, None, &silent, None));
    assert!(!is_context_overflow(
        StopReason::Stop,
        None,
        &silent,
        Some(0),
    ));
}

#[test]
fn no_error_text_means_the_error_case_never_matches() {
    assert!(!is_context_overflow(
        StopReason::Error,
        None,
        &zero_usage(),
        Some(200_000),
    ));
    assert!(!is_context_overflow(
        StopReason::Error,
        Some(""),
        &zero_usage(),
        Some(200_000),
    ));
}

#[test]
fn the_text_classifier_is_the_non_overflow_first_case() {
    assert!(is_context_overflow_error_text(
        "prompt is too long: 213462 tokens > 200000 maximum"
    ));
    assert!(!is_context_overflow_error_text(
        "Throttling error: Too many tokens, please wait before trying again."
    ));
    assert!(!is_context_overflow_error_text(""));
}

#[test]
fn cerebras_bodiless_400_and_413_are_overflow() {
    assert!(is_context_overflow_error_text("400 (no body)"));
    assert!(is_context_overflow_error_text("413 status code (no body)"));
    // A leading status line is required — `400` in the middle is not a match.
    assert!(!is_context_overflow_error_text("Error: 400 (no body)"));
}
