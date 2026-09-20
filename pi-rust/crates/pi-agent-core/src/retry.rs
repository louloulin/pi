//! Agent-level retry of the assistant call — Rust port of
//! `packages/ai/src/utils/retry.ts`.
//!
//! Upstream keeps this module in `@earendil-works/pi-ai` and drives it from
//! two layers: `completeSimpleWithRetries` (compaction) and `AgentSession`,
//! which restarts a *finished* turn whose last assistant message carries a
//! retryable `errorMessage`. The Rust agent loop does not turn a provider
//! failure into an assistant message at all — it returns [`AgentError`] and
//! truncates the event sequence (see `stream_assistant_events`) — so
//! [`AssistantMessage::error_message`] is never consulted here. This port
//! therefore hangs the same policy off the assistant call itself:
//! `stream_assistant_response` wraps one provider attempt in
//! [`retry_assistant_call`], which classifies the [`AgentError`] the attempt
//! returned. The classifier, the attempt numbering, the delay schedule and the
//! callback contract are the same as the TypeScript; only the classification
//! *input* differs (error text from `Err` instead of `errorMessage`).

use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use pi_protocol::{AssistantMessage, StopReason, Usage};
use regex::{Regex, RegexBuilder};
use tokio_util::sync::CancellationToken;

use crate::agent_loop::AgentError;

/// Default cap for a single agent-level backoff (`maxAgentDelayMs`).
///
/// Mirrors `DEFAULT_MAX_AGENT_RETRY_DELAY_MS`.
pub const DEFAULT_MAX_AGENT_RETRY_DELAY_MS: u64 = 60_000;

/// Default `retry.maxRetries` (`core/settings-manager.ts:930`).
pub const DEFAULT_AGENT_MAX_RETRIES: u32 = 3;

/// Default `retry.baseDelayMs` (`core/settings-manager.ts:931`).
pub const DEFAULT_AGENT_BASE_DELAY_MS: u64 = 2_000;

/// Retry policy: bounded attempts with exponential backoff
/// (`base_delay_ms * 2^(attempt-1)`), capped per attempt by
/// [`max_agent_delay_ms`](RetryPolicy::max_agent_delay_ms).
///
/// Mirrors upstream `RetryPolicy` / `settings.retry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Whether the retry loop runs at all. `false` makes
    /// [`retry_assistant_call`] a passthrough.
    pub enabled: bool,
    /// Max retry attempts (0 = no retries). The initial call never counts as
    /// a retry.
    pub max_retries: u32,
    /// Base delay in ms. Per-attempt delay is `base_delay_ms * 2^(attempt-1)`.
    pub base_delay_ms: u64,
    /// Optional cap for agent-level retry delays in ms. `None` means
    /// [`DEFAULT_MAX_AGENT_RETRY_DELAY_MS`].
    pub max_agent_delay_ms: Option<u64>,
}

impl RetryPolicy {
    /// A policy with the upstream defaults (`enabled: true`,
    /// `max_retries: 3`, `base_delay_ms: 2000`, cap 60 s).
    pub const fn new(max_retries: u32, base_delay_ms: u64) -> Self {
        Self {
            enabled: true,
            max_retries,
            base_delay_ms,
            max_agent_delay_ms: None,
        }
    }

    /// The same policy with an explicit per-attempt cap.
    pub const fn with_max_agent_delay_ms(mut self, max_agent_delay_ms: u64) -> Self {
        self.max_agent_delay_ms = Some(max_agent_delay_ms);
        self
    }

    /// Retrying switched off — [`retry_assistant_call`] then returns the
    /// first response unchanged.
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            max_retries: DEFAULT_AGENT_MAX_RETRIES,
            base_delay_ms: DEFAULT_AGENT_BASE_DELAY_MS,
            max_agent_delay_ms: None,
        }
    }
}

impl Default for RetryPolicy {
    /// Upstream's live defaults for `settings.retry`
    /// (`core/settings-manager.ts:915-932`): enabled, 3 retries, 2 s base,
    /// 60 s cap.
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: DEFAULT_AGENT_MAX_RETRIES,
            base_delay_ms: DEFAULT_AGENT_BASE_DELAY_MS,
            max_agent_delay_ms: None,
        }
    }
}

/// Delay before retry number `attempt` (1-indexed): `base_delay_ms *
/// 2^(attempt-1)`, saturating at `u64::MAX` and capped by
/// `max_agent_delay_ms` (default [`DEFAULT_MAX_AGENT_RETRY_DELAY_MS`]).
///
/// Mirrors `retryDelayMs`. Upstream clamps to `Number.MAX_SAFE_INTEGER`
/// before the `min`; `u64` arithmetic plus `saturating_mul` stands in for
/// that clamp.
pub fn retry_delay_ms(policy: &RetryPolicy, attempt: u32) -> u64 {
    let base = u128::from(policy.base_delay_ms);
    // `2 ** Math.max(0, attempt - 1)` — attempt 0 behaves like attempt 1.
    let shift = attempt.saturating_sub(1).min(63);
    let delay = base.saturating_mul(1u128 << shift);
    let safe_delay = u64::try_from(delay).unwrap_or(u64::MAX);
    let cap = policy
        .max_agent_delay_ms
        .unwrap_or(DEFAULT_MAX_AGENT_RETRY_DELAY_MS);
    safe_delay.min(cap)
}

/// Non-retryable provider-limit pattern — subscription / quota / billing
/// exhaustion, checked **before** the retryable pattern.
///
/// Mirrors `NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN`.
const NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN: &str = concat!(
    // OpenCode Go / free-tier limits returned as 429 JSON error types.
    "GoUsageLimitError",
    "|FreeUsageLimitError",
    // OpenCode Go subscription-limit text.
    "|Monthly usage limit reached",
    "|available balance",
    // Generic quota / budget / billing exhaustion.
    "|insufficient_quota",
    "|out of budget",
    "|quota exceeded",
    "|billing",
);

/// Retryable transient provider / transport pattern.
///
/// Mirrors `RETRYABLE_PROVIDER_ERROR_PATTERN`.
const RETRYABLE_PROVIDER_ERROR_PATTERN: &str = concat!(
    // Generic provider load, HTTP status, and server-side transient failures.
    "overloaded",
    "|rate.?limit",
    "|too many requests",
    "|429",
    "|500",
    "|502",
    "|503",
    "|504",
    "|524",
    "|service.?unavailable",
    "|server.?error",
    "|internal.?error",
    // Wrapper / provider text for transient upstream failures.
    "|provider.?returned.?error",
    "|exceeded request buffer limit while retrying upstream",
    // Network, proxy, and fetch transport failures.
    "|network.?error",
    "|connection.?error",
    "|connection.?refused",
    "|connection.?lost",
    "|other side closed",
    "|fetch failed",
    "|getaddrinfo",
    "|ENOTFOUND",
    "|EAI_AGAIN",
    "|upstream.?connect",
    "|reset before headers",
    "|socket hang up",
    "|socket connection was closed",
    "|timed? out",
    "|timeout",
    "|terminated",
    // WebSocket transports.
    "|websocket.?closed",
    "|websocket.?error",
    // Premature stream endings.
    "|ended without",
    "|stream ended before message_stop",
    "|stream ended before a terminal response event",
    "|http2 request did not get a response",
    // Provider-requested retry delay cap failures.
    "|retry delay",
    // Explicit retry guidance emitted mid-stream.
    "|you can retry your request",
    "|try your request again",
    "|please retry your request",
    // gRPC based providers (e.g. NVIDIA NIM).
    "|ResourceExhausted",
);

/// Case-insensitive matcher for `NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN`.
fn non_retryable_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        RegexBuilder::new(NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN)
            .case_insensitive(true)
            .build()
            .expect("non-retryable provider pattern compiles")
    })
}

/// Case-insensitive matcher for `RETRYABLE_PROVIDER_ERROR_PATTERN`.
fn retryable_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        RegexBuilder::new(RETRYABLE_PROVIDER_ERROR_PATTERN)
            .case_insensitive(true)
            .build()
            .expect("retryable provider pattern compiles")
    })
}

/// Classify error **text** as a transient provider / transport failure.
///
/// A context-window overflow is never retryable — upstream
/// `_isRetryableError` checks `isContextOverflow` before
/// `isRetryableAssistantError`, because the agent compacts instead of
/// replaying the same oversized prompt. The classifier lives in `pi-ai`
/// (`utils/overflow.ts`) and is consulted first here too.
///
/// Quota / billing exhaustion wins over the transient pattern, exactly like
/// `isRetryableAssistantError` checks the non-retryable pattern first.
pub fn is_retryable_error_message(error_message: &str) -> bool {
    if error_message.is_empty() {
        return false;
    }
    if pi_ai::is_context_overflow_error_text(error_message) {
        return false;
    }
    if non_retryable_pattern().is_match(error_message) {
        return false;
    }
    retryable_pattern().is_match(error_message)
}

/// The text [`is_retryable_error_message`] inspects for a failed attempt.
///
/// A tool failure is never retried: it is a deterministic result of the
/// model's own arguments, not a provider/transport hiccup, so it is reported
/// as `None`.
fn retryable_message_text(error: &AgentError) -> Option<&str> {
    match error {
        AgentError::Provider(message) | AgentError::Stream(message) => Some(message),
        AgentError::Tool { .. } => None,
    }
}

/// Classify an agent-loop failure — the Rust analogue of
/// `isRetryableAssistantError`, which upstream applies to a finished
/// assistant message carrying `errorMessage`.
///
/// Provider and stream failures are classified by their message text; tool
/// failures never are. A finished [`AssistantMessage`] that ended with
/// [`StopReason::Error`] carries its wording in
/// [`AssistantMessage::error_message`], but the agent loop never reaches this
/// function with one — it returns [`AgentError`] instead — so that case is not
/// classified here.
pub fn is_retryable_agent_error(error: &AgentError) -> bool {
    match retryable_message_text(error) {
        Some(message) => is_retryable_error_message(message),
        None => false,
    }
}

/// `retry_assistant_call` invokes this before the backoff sleep of a retry
/// attempt: `(attempt, max_attempts, delay_ms, error_message)`.
pub type RetryScheduledCallback = Arc<dyn Fn(u32, u32, u64, &str) + Send + Sync>;

/// Invoked after the backoff sleep, immediately before the retried call
/// starts.
pub type RetryAttemptStartCallback = Arc<dyn Fn() + Send + Sync>;

/// Invoked once when the retry loop ends: `(success, attempt, final_error)`.
pub type RetryFinishedCallback = Arc<dyn Fn(bool, u32, Option<&str>) + Send + Sync>;

/// Callbacks [`retry_assistant_call`] emits around each retry.
///
/// Mirrors `RetryCallbacks`. Upstream's are allowed to return a promise; the
/// Rust callbacks are synchronous, so a host that needs to await something
/// (e.g. a TUI repaint) should hand work to its own channel instead of
/// blocking the loop.
#[derive(Clone, Default)]
pub struct RetryCallbacks {
    /// Before the backoff sleep of each retry attempt (1-indexed).
    pub on_retry_scheduled: Option<RetryScheduledCallback>,
    /// After the backoff sleep, immediately before the retried call starts.
    pub on_retry_attempt_start: Option<RetryAttemptStartCallback>,
    /// Once when the loop ends: `attempt` is the last retry attempt that was
    /// scheduled (0 when none was), and the final error is the message that
    /// ended the loop, if it ended on one.
    pub on_retry_finished: Option<RetryFinishedCallback>,
}

impl std::fmt::Debug for RetryCallbacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryCallbacks")
            .field(
                "on_retry_scheduled",
                &self.on_retry_scheduled.as_ref().map(|_| "…"),
            )
            .field(
                "on_retry_attempt_start",
                &self.on_retry_attempt_start.as_ref().map(|_| "…"),
            )
            .field(
                "on_retry_finished",
                &self.on_retry_finished.as_ref().map(|_| "…"),
            )
            .finish()
    }
}

impl RetryCallbacks {
    /// Callbacks with nothing registered — every hook is a no-op.
    pub fn new() -> Self {
        Self::default()
    }

    fn scheduled(&self, attempt: u32, max_attempts: u32, delay_ms: u64, error_message: &str) {
        if let Some(callback) = &self.on_retry_scheduled {
            callback(attempt, max_attempts, delay_ms, error_message);
        }
    }

    fn attempt_start(&self) {
        if let Some(callback) = &self.on_retry_attempt_start {
            callback();
        }
    }

    fn finished(&self, success: bool, attempt: u32, final_error: Option<&str>) {
        if let Some(callback) = &self.on_retry_finished {
            callback(success, attempt, final_error);
        }
    }
}

/// Run a single assistant-producing call with bounded retry on transient
/// errors.
///
/// Behaviour (mirrors `retryAssistantCall`):
///
/// * a successful response returns immediately;
/// * an aborted response is terminal and never retried — but reported as
///   unsuccessful when a retry had already been scheduled;
/// * a non-retryable failure (see [`is_retryable_agent_error`], which covers
///   quota / billing exhaustion) returns immediately so deterministic errors
///   fail fast;
/// * otherwise the call is retried up to `max_retries` times with exponential
///   backoff, emitting [`RetryCallbacks::on_retry_scheduled`] before each
///   sleep, [`RetryCallbacks::on_retry_attempt_start`] after each sleep, and
///   [`RetryCallbacks::on_retry_finished`] once at the end — whether the loop
///   ends in success, exhausted retries, or an aborted backoff.
///
/// When `policy` is `None` or disabled, the first response is returned
/// unchanged (equivalent to awaiting `produce` directly).
///
/// The failed attempts surface as [`AgentError`], so there is no message shell
/// to carry over: an abort **during the backoff sleep** is normalised to an
/// aborted [`AssistantMessage`] built from `fallback_model` with empty content
/// — upstream instead reuses the failed response's content. Callers that
/// cancelled the run get the same `Aborted` stop reason either way.
pub async fn retry_assistant_call<F, Fut>(
    mut produce: F,
    policy: Option<&RetryPolicy>,
    fallback_model: &str,
    signal: &CancellationToken,
    callbacks: Option<&RetryCallbacks>,
) -> Result<AssistantMessage, AgentError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<AssistantMessage, AgentError>>,
{
    let max_attempts = match policy {
        Some(policy) if policy.enabled => policy.max_retries,
        _ => 0,
    };

    let mut attempt = 0u32;
    let mut last_retry: Option<(u32, String)> = None;
    loop {
        match produce().await {
            // Abort: terminal but not successful. Never retry an aborted
            // message.
            Ok(message) if message.stop_reason == StopReason::Aborted => {
                if let Some((attempt, _)) = last_retry {
                    report_finished(callbacks, false, attempt, None);
                }
                return Ok(message);
            }
            // Success: non-error, non-abort responses return as-is.
            Ok(message) if message.stop_reason != StopReason::Error => {
                if let Some((attempt, _)) = last_retry {
                    report_finished(callbacks, true, attempt, None);
                }
                return Ok(message);
            }
            // `StopReason::Error` without a message: nothing to classify, so
            // this is the final response.
            Ok(message) => {
                if let Some((attempt, _)) = last_retry {
                    report_finished(callbacks, false, attempt, None);
                }
                return Ok(message);
            }
            Err(error) => {
                let text = retryable_message_text(&error);
                let retryable = text.is_some_and(is_retryable_error_message);
                // Non-retryable, or budget exhausted: return the failure.
                if attempt >= max_attempts || !retryable {
                    if let Some((attempt, _)) = last_retry {
                        report_finished(callbacks, false, attempt, text);
                    }
                    return Err(error);
                }

                attempt += 1;
                let error_message = match text {
                    Some(text) if !text.is_empty() => text.to_string(),
                    _ => "Unknown error".to_string(),
                };
                last_retry = Some((attempt, error_message.clone()));
                let policy = policy.expect("a retry implies an enabled policy");
                let delay_ms = retry_delay_ms(policy, attempt);
                if let Some(callbacks) = callbacks {
                    callbacks.scheduled(attempt, max_attempts, delay_ms, &error_message);
                }

                if !sleep_backoff(delay_ms, signal).await {
                    // Cancelled while waiting: hand back an aborted message so
                    // the caller does not need to care when cancellation
                    // happened.
                    report_finished(callbacks, false, attempt, Some(&error_message));
                    return Ok(aborted_message(fallback_model));
                }
                if let Some(callbacks) = callbacks {
                    callbacks.attempt_start();
                }
            }
        }
    }
}

/// Report the end of a retry loop when callbacks are registered.
fn report_finished(
    callbacks: Option<&RetryCallbacks>,
    success: bool,
    attempt: u32,
    final_error: Option<&str>,
) {
    if let Some(callbacks) = callbacks {
        callbacks.finished(success, attempt, final_error);
    }
}

/// The `AssistantMessage` an aborted backoff produces — same shape a provider
/// stream abort produces.
fn aborted_message(model: &str) -> AssistantMessage {
    AssistantMessage {
        model: model.to_string(),
        content: Vec::new(),
        stop_reason: StopReason::Aborted,
        usage: Usage::default(),
        error_message: None,
    }
}

/// Sleep for `delay_ms`, returning `false` when `signal` was cancelled first.
///
/// Mirrors the `sleep(ms, signal)` helper: an already-aborted signal resolves
/// (here: returns) immediately without sleeping.
async fn sleep_backoff(delay_ms: u64, signal: &CancellationToken) -> bool {
    if signal.is_cancelled() {
        return false;
    }
    if delay_ms == 0 {
        return true;
    }
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => true,
        _ = signal.cancelled() => false,
    }
}
