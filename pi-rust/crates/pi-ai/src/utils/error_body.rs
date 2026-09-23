//! Provider HTTP error normalization — port of
//! `packages/ai/src/utils/error-body.ts`.
//!
//! Endpoints behind a proxy / gateway may return a non-2xx response whose body
//! the provider SDK cannot fold into `error.message`, so provider catch blocks
//! that read only the message drop the body and surface opaque text like
//! `"403 status code (no body)"`. Upstream probes the known SDK error shapes
//! (Mistral, `openai`, `@google/genai`, AWS Bedrock) and returns a struct each
//! provider composes into its display string.
//!
//! # Rust mapping of the SDK probes
//!
//! The Rust providers use `reqwest` and have no SDK error object, so the
//! counterpart of the four probe branches is simply **the raw response body
//! plus the HTTP status**:
//!
//! | TypeScript probe | Rust |
//! | --- | --- |
//! | `error.statusCode` / `error.status` / `$metadata.httpStatusCode` / `$response.statusCode` | the `reqwest::StatusCode` the caller already has |
//! | `error.body` / `error.error` / `$response.body` | [`normalize_provider_error`]'s `raw_body` |
//! | `error.message` | [`normalize_provider_error`]'s `message` (the transport's own text, e.g. `reqwest::Error`'s display, when there is one) |
//!
//! `messageCarriesBody` is kept because its purpose survives the translation:
//! when the text a provider would display *already contains* the body, printing
//! the body again produces `"403: <body>: <body>"`. Upstream hits that on the
//! Anthropic / `@google/genai` happy path, where the SDK folded the body into
//! `error.message`; the Rust equivalent is a caller that passes the body as
//! its message (or whose transport message already embeds it).
//!
//! The cap is upstream's single [`MAX_PROVIDER_ERROR_BODY_CHARS`] (4000). The
//! three provider copies this replaced used their own 4096-**byte** cap with
//! an `…(truncated)` marker; truncation is now 4000 **characters** with
//! upstream's `... [truncated N chars]` marker (see the LUM-1157 status
//! chapter for the rationale and the deviation record).

/// Maximum characters kept from a provider error body
/// (`MAX_PROVIDER_ERROR_BODY_CHARS`).
pub const MAX_PROVIDER_ERROR_BODY_CHARS: usize = 4000;

/// `NormalizedProviderError` — the display fields a provider composes from a
/// non-2xx response.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NormalizedProviderError {
    /// HTTP status code, when one was available.
    pub status: Option<u16>,
    /// Raw HTTP body reason, already trimmed and truncated to the cap.
    pub body: Option<String>,
    /// The provider's own error text, or the raw body when there is none.
    pub message: String,
    /// True when `message` already contains the body, so providers must not
    /// print the body a second time.
    pub message_carries_body: bool,
}

/// Normalize a non-2xx provider response into the fields a provider displays.
///
/// * `status` — the HTTP status, when the caller has one (transport-level
///   failures have none).
/// * `message` — the provider's or transport's own error text.
/// * `raw_body` — the response body as read from the wire.
///
/// The body is trimmed, dropped when empty (so an empty body never surfaces as
/// `""`), and truncated to [`MAX_PROVIDER_ERROR_BODY_CHARS`].
pub fn normalize_provider_error(
    status: Option<u16>,
    message: &str,
    raw_body: &str,
) -> NormalizedProviderError {
    let trimmed = raw_body.trim();
    let body = if trimmed.is_empty() {
        None
    } else {
        Some(truncate_provider_error_body(trimmed))
    };
    // Upstream: `body === undefined || error.message.includes(body)`. The
    // no-body case is "nothing separate to print", the contains case is the
    // SDK-happy path where the body is already part of the message.
    let message_carries_body = match body.as_deref() {
        None => true,
        Some(body) => message.contains(body),
    };
    NormalizedProviderError {
        status,
        body,
        message: message.to_string(),
        message_carries_body,
    }
}

/// Compose a display string from a normalized error.
///
/// When the message already carries the body, or no body/status was extracted,
/// the message is returned unchanged (with the provider prefix when one is
/// given and a status is known). Otherwise the status and body are surfaced:
///
/// * no prefix: `"<status>: <body>"`
/// * prefix:    `"<prefix> (<status>): <body>"`
pub fn format_provider_error(norm: &NormalizedProviderError, prefix: Option<&str>) -> String {
    if norm.message_carries_body || norm.status.is_none() || norm.body.is_none() {
        return match (prefix, norm.status) {
            (Some(prefix), Some(status)) => format!("{prefix} ({status}): {}", norm.message),
            _ => norm.message.clone(),
        };
    }
    // Both are `Some` here (checked above).
    let status = norm.status.unwrap_or_default();
    let body = norm.body.as_deref().unwrap_or_default();
    match prefix {
        Some(prefix) => format!("{prefix} ({status}): {body}"),
        None => format!("{status}: {body}"),
    }
}

/// Truncate a provider error body to [`MAX_PROVIDER_ERROR_BODY_CHARS`].
///
/// The shared replacement for the per-provider `truncate_body` copies in
/// `pi-ai`'s anthropic / openai / openai_responses / google providers.
pub fn truncate_provider_error_body(body: &str) -> String {
    truncate_error_text(body, MAX_PROVIDER_ERROR_BODY_CHARS)
}

/// `truncateErrorText(text, maxChars)` — keep the first `max_chars`
/// **characters**, never splitting a UTF-8 sequence.
///
/// The cut walks `char_indices`, so the returned prefix always ends on a
/// character boundary; multi-byte text is truncated by character count exactly
/// like the TypeScript `String.prototype.slice`.
pub fn truncate_error_text(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let end = text
        .char_indices()
        .nth(max_chars)
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    format!(
        "{}... [truncated {} chars]",
        &text[..end],
        total - max_chars
    )
}
