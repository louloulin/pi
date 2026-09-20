//! Port of `packages/ai/test/error-body.test.ts` against the Rust
//! `(status, message, raw_body)` surface.
//!
//! The four SDK-shape vectors keep their assertions; only the extraction path
//! differs. Upstream probes `statusCode` / `status` / `$metadata.httpStatusCode`
//! and `body` / `error` / `$response.body`; the Rust providers already hold the
//! status and the raw body, so those two values are passed in directly. The
//! class-instance / stream / non-Error vectors are covered by upstream's own
//! probe layer, which has no Rust counterpart — see
//! `pi_ai::utils::error_body` module docs.

use pi_ai::utils::error_body::{
    format_provider_error, normalize_provider_error, truncate_error_text,
    truncate_provider_error_body, NormalizedProviderError, MAX_PROVIDER_ERROR_BODY_CHARS,
};

#[test]
fn extracts_status_and_body_from_a_mistral_shaped_error() {
    let norm = normalize_provider_error(
        Some(403),
        "Mistral request failed",
        r#"{"error":"blocked by gateway WAF"}"#,
    );

    assert_eq!(norm.status, Some(403));
    assert_eq!(
        norm.body.as_deref(),
        Some(r#"{"error":"blocked by gateway WAF"}"#)
    );
    assert!(!norm.message_carries_body);
}

#[test]
fn reads_the_parsed_body_when_the_message_is_opaque() {
    // openai's APIError yields "403 status code (no body)" while the parsed
    // body stays on the error object; in Rust the raw body is that object's
    // wire form.
    let norm = normalize_provider_error(
        Some(403),
        "403 status code (no body)",
        r#"{"error":"blocked by gateway WAF"}"#,
    );

    assert_eq!(norm.status, Some(403));
    assert_eq!(
        norm.body.as_deref(),
        Some(r#"{"error":"blocked by gateway WAF"}"#)
    );
    assert!(!norm.message_carries_body);
}

#[test]
fn preserves_the_message_when_it_already_folds_in_the_body() {
    // @google/genai's happy path: `error.message` is the serialized body.
    let body = r#"{"error":{"code":403,"message":"Permission denied"}}"#;
    let norm = normalize_provider_error(Some(403), body, body);

    assert_eq!(norm.status, Some(403));
    assert!(norm.message_carries_body);
    assert_eq!(norm.message, body);
    assert_eq!(
        format_provider_error(&norm, Some("OpenAI API error")),
        format!("OpenAI API error (403): {body}")
    );
}

#[test]
fn extracts_status_and_body_from_a_bedrock_shaped_exception() {
    let norm = normalize_provider_error(
        Some(403),
        "UnknownError",
        r#"{"message":"blocked by gateway WAF"}"#,
    );

    assert_eq!(norm.status, Some(403));
    assert_eq!(
        norm.body.as_deref(),
        Some(r#"{"message":"blocked by gateway WAF"}"#)
    );
    assert!(!norm.message_carries_body);
    assert!(!format_provider_error(&norm, None).contains("Unknown: UnknownError"));
}

#[test]
fn treats_an_empty_or_whitespace_body_as_no_body() {
    for raw in ["", "   ", "\n\t"] {
        let norm = normalize_provider_error(Some(403), "403 status code (no body)", raw);
        assert_eq!(norm.body, None, "raw body {raw:?}");
        assert!(norm.message_carries_body);
        assert_eq!(norm.message, "403 status code (no body)");
    }
}

#[test]
fn trims_surrounding_whitespace_from_the_body() {
    let norm = normalize_provider_error(Some(500), "failed", "\n  upstream exploded \n");

    assert_eq!(norm.body.as_deref(), Some("upstream exploded"));
}

#[test]
fn sets_message_carries_body_when_the_message_contains_the_body() {
    let norm = normalize_provider_error(Some(500), "500: upstream exploded", "upstream exploded");

    assert!(norm.message_carries_body);
    assert_eq!(format_provider_error(&norm, None), "500: upstream exploded");
}

#[test]
fn truncates_the_body_at_the_cap() {
    let long_body = "x".repeat(MAX_PROVIDER_ERROR_BODY_CHARS + 50);
    let norm = normalize_provider_error(Some(500), "failed", &long_body);

    assert_eq!(MAX_PROVIDER_ERROR_BODY_CHARS, 4000);
    let body = norm.body.expect("body kept");
    assert!(body.contains("... [truncated 50 chars]"), "{body}");
    assert!(body.len() < long_body.len());
    assert_eq!(
        truncate_error_text(&long_body, MAX_PROVIDER_ERROR_BODY_CHARS),
        body
    );
}

#[test]
fn leaves_a_body_at_exactly_the_cap_untouched() {
    let exact = "x".repeat(MAX_PROVIDER_ERROR_BODY_CHARS);
    assert_eq!(truncate_provider_error_body(&exact), exact);
    assert_eq!(truncate_error_text("abcd", 4), "abcd");
}

#[test]
fn truncates_on_character_boundaries_for_multibyte_text() {
    // Two-byte characters: a byte-based cut would split the cap'th char.
    let body = "é".repeat(MAX_PROVIDER_ERROR_BODY_CHARS + 10);
    let truncated = truncate_provider_error_body(&body);

    assert!(
        truncated.ends_with("... [truncated 10 chars]"),
        "{truncated}"
    );
    let kept = truncated.trim_end_matches("... [truncated 10 chars]");
    assert_eq!(kept.chars().count(), MAX_PROVIDER_ERROR_BODY_CHARS);
    assert!(kept.chars().all(|c| c == 'é'));

    // Four-byte characters (emoji) must not be split either.
    let emoji_body = format!(
        "{}{}",
        "a".repeat(MAX_PROVIDER_ERROR_BODY_CHARS - 1),
        "😀😀😀"
    );
    let emoji_truncated = truncate_provider_error_body(&emoji_body);
    assert!(
        emoji_truncated.ends_with("... [truncated 2 chars]"),
        "{emoji_truncated}"
    );
    let emoji_kept = emoji_truncated.trim_end_matches("... [truncated 2 chars]");
    assert_eq!(emoji_kept.chars().count(), MAX_PROVIDER_ERROR_BODY_CHARS);
    assert!(emoji_kept.ends_with('😀'));
}

#[test]
fn formats_status_and_body_without_a_prefix() {
    let norm = normalize_provider_error(
        Some(403),
        "403 status code (no body)",
        r#"{"error":"blocked by gateway WAF"}"#,
    );

    let formatted = format_provider_error(&norm, None);

    assert!(formatted.contains("403"));
    assert!(formatted.contains("blocked by gateway WAF"));
    assert_ne!(formatted, "403 status code (no body)");
}

#[test]
fn formats_status_and_body_with_a_provider_prefix() {
    let norm = normalize_provider_error(
        Some(403),
        "403 status code (no body)",
        r#"{"error":"blocked by gateway WAF"}"#,
    );

    assert_eq!(
        format_provider_error(&norm, Some("OpenAI API error")),
        r#"OpenAI API error (403): {"error":"blocked by gateway WAF"}"#
    );
}

#[test]
fn returns_the_bare_message_when_there_is_no_status_or_no_body() {
    let no_status = NormalizedProviderError {
        status: None,
        body: Some("upstream exploded".into()),
        message: "transport error".into(),
        message_carries_body: false,
    };
    assert_eq!(format_provider_error(&no_status, None), "transport error");
    assert_eq!(
        format_provider_error(&no_status, Some("Mistral")),
        "transport error"
    );

    let no_body = NormalizedProviderError {
        status: Some(502),
        body: None,
        message: "TLS handshake failed".into(),
        message_carries_body: true,
    };
    assert_eq!(
        format_provider_error(&no_body, Some("Mistral")),
        "Mistral (502): TLS handshake failed"
    );
}
