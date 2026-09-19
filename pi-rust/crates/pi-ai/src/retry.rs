//! Provider-request retry — the Rust port of `packages/ai/src/utils/provider-retry.ts`.
//!
//! Upstream invokes every provider SDK with `maxRetries: 0` and wraps the
//! request in [`retry_provider_request`], because the SDKs' own retry timers
//! ignore the request `AbortSignal`. This module reproduces that behaviour:
//!
//! * [`is_retryable_stream_error`] mirrors the pinned OpenAI/Anthropic SDK
//!   policy (`x-should-retry` header override, then `408` / `409` / `429` /
//!   `5xx`, with transport failures always retryable).
//! * [`provider_retry_delay_ms`] honours a server-requested delay
//!   (`Retry-After-Ms` / numeric `Retry-After`) and otherwise falls back to
//!   the SDKs' `min(500ms · 2^n, 8s)` backoff with 0–25% jitter.
//! * [`retry_provider_request`] retries a fresh request up to
//!   [`ProviderRetryPolicy::max_retries`] times; nothing is replayed once the
//!   caller has started consuming a stream, because the whole call returns
//!   `Err` before any event is produced.
//!
//! [`RetryStreamFn`] packages the loop as a [`StreamFn`] decorator, which is
//! how `pi-coding-agent`'s `ProviderRouter` applies it to every adapter.
//!
//! Native-only: the backoff sleep uses `tokio::time` and the header parser
//! uses `reqwest::header`, neither of which exists on `wasm32-unknown-unknown`
//! (where the provider adapters are stubs anyway).
//!
//! Known gaps versus upstream, tracked for a follow-up round:
//!
//! * The HTTP-date form of `Retry-After` (`Retry-After: Wed, 21 Oct 2015
//!   07:28:00 GMT`) is not parsed; the numeric forms are.
//! * When the server asks for a delay above [`ProviderRetryPolicy::
//!   max_retry_delay_ms`], upstream replaces the error with a dedicated
//!   "Server requested Ns retry delay" message. `StreamError` has no such
//!   variant, so this port stops retrying and returns the original provider
//!   error (same status and body).
//! * The agent-level retry (`utils/retry.ts`: `retryAssistantCall`,
//!   `isRetryableAssistantError`) is a separate layer and is not ported yet.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pi_protocol::{Context, Model};
use reqwest::header::HeaderMap;

use crate::stream::{AssistantMessageEventStream, SharedStreamFn, StreamFn};
use crate::types::{AbortSignal, ProviderRetryHint, SimpleStreamOptions, StreamError};

/// Cap on a server-requested retry delay, in milliseconds — upstream
/// `DEFAULT_MAX_RETRY_DELAY_MS`.
pub const DEFAULT_MAX_PROVIDER_RETRY_DELAY_MS: u64 = 60_000;

/// Base of the exponential fallback backoff, in milliseconds — the
/// `0.5 * 2 ** retryIndex` term in upstream `getRetryDelayMs`.
pub const PROVIDER_RETRY_BASE_DELAY_MS: u64 = 500;

/// Ceiling of the exponential fallback backoff, in milliseconds — the
/// `Math.min(..., 8) * 1000` term in upstream `getRetryDelayMs`.
pub const PROVIDER_RETRY_MAX_BACKOFF_MS: u64 = 8_000;

/// Maximum jitter applied to the fallback backoff (upstream
/// `1 - Math.random() * 0.25`).
const RETRY_JITTER_FRACTION: f64 = 0.25;

/// Retry budget for one provider request.
///
/// Mirrors `ProviderRetrySettings` in
/// `packages/coding-agent/src/core/settings-manager.ts`: `max_retries`
/// defaults to `0` (the agent-level retry is what retries by default
/// upstream) and `max_retry_delay_ms` to
/// [`DEFAULT_MAX_PROVIDER_RETRY_DELAY_MS`], with `0` meaning "no cap".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRetryPolicy {
    /// Retry attempts after the initial request (`0` disables retrying).
    pub max_retries: u32,
    /// Reject a server-requested delay above this many milliseconds; `0`
    /// disables the cap.
    pub max_retry_delay_ms: u64,
}

impl ProviderRetryPolicy {
    /// No retries, default delay cap — upstream's default because
    /// `settings.retry.provider.maxRetries` starts undefined.
    pub const DEFAULT: Self = Self {
        max_retries: 0,
        max_retry_delay_ms: DEFAULT_MAX_PROVIDER_RETRY_DELAY_MS,
    };

    /// A policy with `max_retries` attempts and the default delay cap.
    pub fn new(max_retries: u32) -> Self {
        Self {
            max_retries,
            ..Self::DEFAULT
        }
    }

    /// A policy with an explicit `max_retries` and delay cap.
    pub fn with_max_retry_delay_ms(max_retries: u32, max_retry_delay_ms: u64) -> Self {
        Self {
            max_retries,
            max_retry_delay_ms,
        }
    }

    /// Whether a request may be retried at all.
    pub fn is_enabled(&self) -> bool {
        self.max_retries > 0
    }
}

impl Default for ProviderRetryPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Whether an HTTP status is retryable on its own — upstream
/// `isRetryableProviderError`, after the `X-Should-Retry` check.
///
/// `status` is `None` for a transport failure (no response arrived), which
/// upstream treats as retryable.
pub fn is_retryable_provider_status(status: Option<u16>) -> bool {
    match status {
        None => true,
        Some(status) => status == 408 || status == 409 || status == 429 || status >= 500,
    }
}

/// Whether a failed provider request should be retried.
///
/// `X-Should-Retry: true` / `false` wins over the status code, exactly as in
/// upstream `isRetryableProviderError`.
pub fn is_retryable_provider_response(status: u16, should_retry: Option<bool>) -> bool {
    match should_retry {
        Some(should_retry) => should_retry,
        None => is_retryable_provider_status(Some(status)),
    }
}

/// Classify a [`StreamError`] the way [`retry_provider_request`] does.
///
/// * `Provider` — decided by [`is_retryable_provider_response`].
/// * `Transport` — retryable (DNS/TLS/timeout/connection failures).
/// * `Malformed` / `Aborted` / `Io` — deterministic, never retried.
pub fn is_retryable_stream_error(error: &StreamError) -> bool {
    match error {
        StreamError::Provider {
            status,
            hint,
            body: _,
        } => is_retryable_provider_response(*status, hint.should_retry),
        StreamError::Transport(_) => true,
        StreamError::Malformed(_) | StreamError::Aborted | StreamError::Io(_) => false,
    }
}

/// Extract the fallback backoff for retry `retry_index` (0-based) before
/// jitter — upstream `Math.min(0.5 * 2 ** retryIndex, 8) * 1000`.
pub fn provider_retry_backoff_ms(retry_index: u32) -> u64 {
    let factor = 1u64.checked_shl(retry_index).unwrap_or(u64::MAX);
    PROVIDER_RETRY_BASE_DELAY_MS
        .saturating_mul(factor)
        .min(PROVIDER_RETRY_MAX_BACKOFF_MS)
}

/// Apply upstream's jitter to a computed backoff: `delay * (1 - 0.25f)` for
/// a fraction `f` in `[0, 1]`.
pub fn apply_retry_jitter(delay_ms: u64, jitter_fraction: f64) -> u64 {
    let fraction = jitter_fraction.clamp(0.0, 1.0);
    (delay_ms as f64 * (1.0 - RETRY_JITTER_FRACTION * fraction)).round() as u64
}

/// The delay before retry `retry_index` (0-based), or `None` when the server
/// requested a delay above `policy.max_retry_delay_ms` — upstream
/// `validateServerRetryDelayMs` throws in that case, so the call must fail
/// instead of sleeping.
///
/// A server-requested delay is used verbatim (it is not jittered or capped by
/// the exponential ceiling); everything else falls back to
/// [`provider_retry_backoff_ms`] with [`apply_retry_jitter`].
pub fn provider_retry_delay_ms(
    policy: &ProviderRetryPolicy,
    hint: ProviderRetryHint,
    retry_index: u32,
    jitter_fraction: f64,
) -> Option<u64> {
    if let Some(requested) = hint.retry_after_ms {
        let exceeds_cap = policy.max_retry_delay_ms > 0 && requested > policy.max_retry_delay_ms;
        return if exceeds_cap { None } else { Some(requested) };
    }
    Some(apply_retry_jitter(
        provider_retry_backoff_ms(retry_index),
        jitter_fraction,
    ))
}

/// Parse the retry guidance a non-2xx response carries in its headers.
///
/// Reads `Retry-After-Ms` first, then a numeric `Retry-After` (seconds), then
/// `X-Should-Retry`. The HTTP-date `Retry-After` form is ignored; see the
/// module docs.
pub fn retry_hint_from_headers(headers: &HeaderMap) -> ProviderRetryHint {
    let mut hint = ProviderRetryHint::default();

    // Upstream prefers the millisecond header; both spellings are common in
    // the wild (`retry-after-ms` is what OpenAI's SDKs read).
    for name in ["retry-after-ms", "retry-after"] {
        let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) else {
            continue;
        };
        let parsed = if name == "retry-after-ms" {
            value.trim().parse::<f64>().ok().map(|ms| ms.round())
        } else {
            value
                .trim()
                .parse::<f64>()
                .ok()
                .map(|s| (s * 1000.0).round())
        };
        if let Some(ms) = parsed {
            if ms.is_finite() && ms >= 0.0 {
                hint.retry_after_ms = Some(ms.min(u64::MAX as f64) as u64);
                break;
            }
        }
    }

    if let Some(should_retry) = headers
        .get("x-should-retry")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| match value.trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        })
    {
        hint.should_retry = Some(should_retry);
    }

    hint
}

/// Run `request` with the provider-request retry loop.
///
/// The request is only retried when it fails *before* producing a response —
/// `request` must be cheap to repeat (a fresh HTTP call), which is true for
/// every `StreamFn::stream_simple` adapter: it returns `Err` before any event
/// exists. A cancelled `signal` is checked before the first retry and during
/// every backoff sleep, so cancellation is never delayed by a pending
/// backoff; it surfaces as [`StreamError::Aborted`].
pub async fn retry_provider_request<T, F, Fut>(
    policy: &ProviderRetryPolicy,
    signal: Option<&AbortSignal>,
    mut request: F,
) -> Result<T, StreamError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, StreamError>>,
{
    let mut retries_remaining = policy.max_retries;

    loop {
        let error = match request().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };

        if is_cancelled(signal) {
            return Err(StreamError::Aborted);
        }
        if retries_remaining == 0 || !is_retryable_stream_error(&error) {
            return Err(error);
        }

        let retry_index = policy.max_retries - retries_remaining;
        retries_remaining -= 1;

        let hint = match &error {
            StreamError::Provider { hint, .. } => *hint,
            _ => ProviderRetryHint::default(),
        };
        // `None` means the server asked for longer than the cap allows: fail
        // now instead of sleeping past it (upstream throws here).
        let Some(delay_ms) = provider_retry_delay_ms(policy, hint, retry_index, random_fraction())
        else {
            return Err(error);
        };

        sleep_or_abort(delay_ms, signal).await?;
    }
}

/// Whether `signal` has already been cancelled.
fn is_cancelled(signal: Option<&AbortSignal>) -> bool {
    signal.is_some_and(AbortSignal::is_cancelled)
}

/// Sleep for `delay_ms`, returning [`StreamError::Aborted`] when `signal`
/// fires first.
async fn sleep_or_abort(delay_ms: u64, signal: Option<&AbortSignal>) -> Result<(), StreamError> {
    let sleep = tokio::time::sleep(Duration::from_millis(delay_ms));
    match signal {
        Some(signal) => tokio::select! {
            () = sleep => Ok(()),
            () = signal.cancelled() => Err(StreamError::Aborted),
        },
        None => {
            sleep.await;
            Ok(())
        }
    }
}

/// A pseudorandom fraction in `[0, 1)` used for backoff jitter.
///
/// A time-seeded splitmix64 step instead of a `rand` dependency: jitter only
/// has to de-synchronise concurrent retries, and upstream's `Math.random()`
/// carries no cryptographic meaning here either.
fn random_fraction() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// A [`StreamFn`] that retries its inner adapter with a
/// [`ProviderRetryPolicy`].
///
/// `ProviderRouter` wraps every registered adapter in this decorator, so the
/// retry budget applies uniformly to all providers without each adapter
/// growing its own loop.
pub struct RetryStreamFn<S> {
    inner: S,
    policy: ProviderRetryPolicy,
}

impl<S> RetryStreamFn<S> {
    /// Wrap `inner` with `policy`.
    pub fn new(inner: S, policy: ProviderRetryPolicy) -> Self {
        Self { inner, policy }
    }

    /// The policy this decorator retries with.
    pub fn policy(&self) -> ProviderRetryPolicy {
        self.policy
    }

    /// Wrap `inner` and hand it back as a [`SharedStreamFn`].
    pub fn shared(inner: S, policy: ProviderRetryPolicy) -> SharedStreamFn
    where
        S: StreamFn + 'static,
    {
        Arc::new(Self::new(inner, policy))
    }
}

#[async_trait]
impl<S: StreamFn> StreamFn for RetryStreamFn<S> {
    async fn stream_simple(
        &self,
        model: &Model,
        ctx: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        retry_provider_request(&self.policy, options.signal.as_ref(), || {
            self.inner.stream_simple(model, ctx, options)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use futures::StreamExt;
    use pi_protocol::{AssistantMessageEvent, StopReason, Usage};
    use std::sync::Mutex;

    fn hint(retry_after_ms: Option<u64>, should_retry: Option<bool>) -> ProviderRetryHint {
        ProviderRetryHint {
            retry_after_ms,
            should_retry,
        }
    }

    /// Fails `failures` times with `error`, then returns a one-event stream.
    struct FlakyStreamFn {
        failures: usize,
        error: StreamError,
        calls: Mutex<usize>,
    }

    impl FlakyStreamFn {
        fn new(failures: usize, error: StreamError) -> Self {
            Self {
                failures,
                error,
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().expect("lock")
        }

        /// Drive one retried call. The model/context/options live outside
        /// the closure so the future it returns can borrow them.
        async fn call(
            &self,
            policy: &ProviderRetryPolicy,
            signal: Option<&AbortSignal>,
        ) -> Result<AssistantMessageEventStream, StreamError> {
            let model = model();
            let context = Context::new("t");
            let options = options();
            retry_provider_request(policy, signal, || {
                self.stream_simple(&model, &context, &options)
            })
            .await
        }

        /// [`call`](Self::call) for the cases that must fail —
        /// `expect_err` is unavailable because the success type (the event
        /// stream) is not `Debug`.
        async fn call_err(
            &self,
            policy: &ProviderRetryPolicy,
            signal: Option<&AbortSignal>,
        ) -> StreamError {
            match self.call(policy, signal).await {
                Ok(_) => panic!("expected the call to fail"),
                Err(error) => error,
            }
        }
    }

    #[async_trait]
    impl StreamFn for FlakyStreamFn {
        async fn stream_simple(
            &self,
            _model: &Model,
            _ctx: &Context,
            _options: &SimpleStreamOptions,
        ) -> Result<AssistantMessageEventStream, StreamError> {
            let mut calls = self.calls.lock().expect("lock");
            *calls += 1;
            let call = *calls;
            drop(calls);
            if call <= self.failures {
                return Err(match &self.error {
                    StreamError::Provider { status, body, hint } => {
                        StreamError::provider_with_hint(*status, body.clone(), *hint)
                    }
                    StreamError::Malformed(message) => StreamError::Malformed(message.clone()),
                    StreamError::Aborted => StreamError::Aborted,
                    other => panic!("unsupported test error: {other:?}"),
                });
            }
            let events = vec![Ok(AssistantMessageEvent::Done {
                content: Vec::new(),
                stop_reason: StopReason::Stop,
                usage: Usage::default(),
            })];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    fn model() -> Model {
        serde_json::from_value(serde_json::json!({
            "id": "test-model",
            "name": "Test",
            "provider": "test",
            "api": "faux",
        }))
        .expect("model fixture")
    }

    fn options() -> SimpleStreamOptions {
        SimpleStreamOptions::default()
    }

    /// A retryable 503 whose server-requested delay is zero, so the test
    /// exercises the loop without sleeping.
    fn transient_503() -> StreamError {
        StreamError::provider_with_hint(503, "overloaded", hint(Some(0), None))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn disabled_policy_returns_the_first_error() {
        let inner = FlakyStreamFn::new(1, transient_503());
        let err = inner.call_err(&ProviderRetryPolicy::default(), None).await;
        assert!(matches!(err, StreamError::Provider { status: 503, .. }));
        assert_eq!(inner.calls(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retries_a_transient_error_until_the_request_succeeds() {
        let inner = FlakyStreamFn::new(2, transient_503());
        let stream = inner
            .call(&ProviderRetryPolicy::new(3), None)
            .await
            .expect("third attempt succeeds");
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 1);
        assert_eq!(inner.calls(), 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exhausted_retries_return_the_last_error() {
        let inner = FlakyStreamFn::new(5, transient_503());
        let err = inner.call_err(&ProviderRetryPolicy::new(2), None).await;
        assert!(matches!(err, StreamError::Provider { status: 503, .. }));
        assert_eq!(inner.calls(), 3, "initial call plus two retries");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn non_retryable_errors_fail_without_a_second_call() {
        for error in [
            StreamError::provider(400, "bad request"),
            StreamError::provider(401, "unauthorized"),
            StreamError::Malformed("bad json".into()),
        ] {
            let inner = FlakyStreamFn::new(3, error);
            inner.call_err(&ProviderRetryPolicy::new(3), None).await;
            assert_eq!(inner.calls(), 1, "deterministic errors must fail fast");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn should_retry_false_suppresses_a_retryable_status() {
        let inner = FlakyStreamFn::new(
            1,
            StreamError::provider_with_hint(503, "overloaded", hint(None, Some(false))),
        );
        inner.call_err(&ProviderRetryPolicy::new(3), None).await;
        assert_eq!(inner.calls(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_cancelled_signal_aborts_before_the_first_retry() {
        let inner = FlakyStreamFn::new(2, transient_503());
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let signal = AbortSignal::native(token);
        let err = inner
            .call_err(&ProviderRetryPolicy::new(3), Some(&signal))
            .await;
        assert!(matches!(err, StreamError::Aborted));
        assert_eq!(inner.calls(), 1, "cancellation must not add a request");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn aborting_during_the_backoff_returns_aborted() {
        let inner = FlakyStreamFn::new(
            2,
            StreamError::provider_with_hint(429, "slow down", hint(Some(5_000), None)),
        );
        let token = tokio_util::sync::CancellationToken::new();
        let signal = AbortSignal::native(token.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            token.cancel();
        });
        let err = inner
            .call_err(&ProviderRetryPolicy::new(3), Some(&signal))
            .await;
        assert!(matches!(err, StreamError::Aborted));
        assert_eq!(inner.calls(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_server_delay_above_the_cap_fails_instead_of_sleeping() {
        let inner = FlakyStreamFn::new(
            1,
            StreamError::provider_with_hint(
                429,
                "slow down",
                hint(Some(DEFAULT_MAX_PROVIDER_RETRY_DELAY_MS + 1), None),
            ),
        );
        let err = inner.call_err(&ProviderRetryPolicy::new(3), None).await;
        assert!(matches!(err, StreamError::Provider { status: 429, .. }));
        assert_eq!(inner.calls(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retry_stream_fn_decorates_an_adapter() {
        let inner = Arc::new(FlakyStreamFn::new(1, transient_503()));
        let decorator = RetryStreamFn::new(inner.clone(), ProviderRetryPolicy::new(1));
        let stream = decorator
            .stream_simple(&model(), &Context::new("t"), &options())
            .await
            .expect("second attempt succeeds");
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 1);
        assert_eq!(inner.calls(), 2);
        assert_eq!(
            decorator.policy(),
            ProviderRetryPolicy::new(1),
            "the decorator exposes its policy"
        );

        // The `shared` constructor erases the concrete type for `ProviderRouter`.
        let shared = RetryStreamFn::shared(inner.clone(), ProviderRetryPolicy::new(1));
        assert!(shared
            .stream_simple(&model(), &Context::new("t"), &options())
            .await
            .is_ok());
        assert_eq!(inner.calls(), 3, "the stream got a third, successful call");
    }

    #[test]
    fn status_classification_matches_the_sdk_policy() {
        for status in [408, 409, 429, 500, 502, 503, 504, 524] {
            assert!(
                is_retryable_provider_status(Some(status)),
                "{status} is retryable"
            );
        }
        for status in [400, 401, 403, 404, 422] {
            assert!(
                !is_retryable_provider_status(Some(status)),
                "{status} is not retryable"
            );
        }
        assert!(
            is_retryable_provider_status(None),
            "a transport failure has no status and is retryable"
        );
    }

    #[test]
    fn should_retry_header_overrides_the_status() {
        assert!(is_retryable_provider_response(400, Some(true)));
        assert!(!is_retryable_provider_response(503, Some(false)));
        assert!(!is_retryable_provider_response(400, None));
        assert!(is_retryable_provider_response(503, None));
    }

    #[test]
    fn transport_errors_are_retryable_but_deterministic_ones_are_not() {
        assert!(is_retryable_stream_error(&StreamError::Transport(
            reqwest::Client::new()
                .get("not a url")
                .build()
                .expect_err("invalid url")
        )));
        assert!(!is_retryable_stream_error(&StreamError::Malformed(
            "bad json".into()
        )));
        assert!(!is_retryable_stream_error(&StreamError::Aborted));
    }

    #[test]
    fn fallback_backoff_is_exponential_and_capped() {
        assert_eq!(provider_retry_backoff_ms(0), 500);
        assert_eq!(provider_retry_backoff_ms(1), 1_000);
        assert_eq!(provider_retry_backoff_ms(2), 2_000);
        assert_eq!(provider_retry_backoff_ms(3), 4_000);
        assert_eq!(provider_retry_backoff_ms(4), 8_000);
        assert_eq!(provider_retry_backoff_ms(5), 8_000, "capped");
        assert_eq!(
            provider_retry_backoff_ms(u32::MAX),
            8_000,
            "a huge index must not overflow"
        );
    }

    #[test]
    fn jitter_never_exceeds_the_backoff_and_shrinks_it_by_at_most_a_quarter() {
        assert_eq!(apply_retry_jitter(1_000, 0.0), 1_000);
        assert_eq!(apply_retry_jitter(1_000, 1.0), 750);
        assert_eq!(apply_retry_jitter(1_000, 0.5), 875);
        assert_eq!(apply_retry_jitter(1_000, 2.0), 750, "clamped");
        assert_eq!(apply_retry_jitter(1_000, -1.0), 1_000, "clamped");
    }

    #[test]
    fn a_server_requested_delay_is_used_verbatim() {
        let policy = ProviderRetryPolicy::new(3);
        assert_eq!(
            provider_retry_delay_ms(&policy, hint(Some(1_234), None), 0, 1.0),
            Some(1_234),
            "the hint wins over the fallback backoff and is not jittered"
        );
        assert_eq!(
            provider_retry_delay_ms(&policy, hint(None, None), 0, 1.0),
            Some(375)
        );
        assert_eq!(
            provider_retry_delay_ms(&policy, hint(None, None), 3, 0.0),
            Some(4_000)
        );
    }

    #[test]
    fn the_delay_cap_rejects_long_server_requests_and_zero_disables_it() {
        let capped = ProviderRetryPolicy::with_max_retry_delay_ms(3, 1_000);
        assert_eq!(
            provider_retry_delay_ms(&capped, hint(Some(1_000), None), 0, 0.0),
            Some(1_000),
            "exactly at the cap is allowed"
        );
        assert_eq!(
            provider_retry_delay_ms(&capped, hint(Some(1_001), None), 0, 0.0),
            None
        );

        let uncapped = ProviderRetryPolicy::with_max_retry_delay_ms(3, 0);
        assert_eq!(
            provider_retry_delay_ms(&uncapped, hint(Some(600_000), None), 0, 0.0),
            Some(600_000),
            "a zero cap means no limit"
        );
        assert_eq!(
            provider_retry_delay_ms(&uncapped, hint(None, None), 0, 0.0),
            Some(500),
            "the cap never applies to the fallback backoff"
        );
    }

    #[test]
    fn retry_hints_are_parsed_from_the_response_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after-ms", "1500".parse().expect("header"));
        headers.insert("x-should-retry", "true".parse().expect("header"));
        assert_eq!(
            retry_hint_from_headers(&headers),
            hint(Some(1_500), Some(true))
        );

        let mut seconds = HeaderMap::new();
        seconds.insert("retry-after", "2.5".parse().expect("header"));
        assert_eq!(retry_hint_from_headers(&seconds), hint(Some(2_500), None));

        let mut override_off = HeaderMap::new();
        override_off.insert("x-should-retry", "false".parse().expect("header"));
        assert_eq!(
            retry_hint_from_headers(&override_off),
            hint(None, Some(false))
        );

        // The HTTP-date form and unknown values are ignored.
        let mut date = HeaderMap::new();
        date.insert(
            "retry-after",
            "Wed, 21 Oct 2015 07:28:00 GMT".parse().expect("header"),
        );
        date.insert("x-should-retry", "maybe".parse().expect("header"));
        assert_eq!(retry_hint_from_headers(&date), hint(None, None));

        assert_eq!(retry_hint_from_headers(&HeaderMap::new()), hint(None, None));
    }
}
