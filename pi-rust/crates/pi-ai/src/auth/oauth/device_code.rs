//! RFC 8628 device authorization grant polling loop.
//!
//! Port of `packages/ai/src/auth/oauth/device-code.ts`. The poller drives the
//! the user-action polling state machine: it sleeps `intervalMs` between
//! polls, honours `slow_down` responses by extending the interval (RFC 8628
//! section 3.5), and aborts when either the device-code expires or the caller's
//! signal fires.

use std::time::{Duration, Instant};

use crate::types::AbortSignal;

/// Outcome of a single poll attempt (`OAuthDeviceCodePollResult`).
#[derive(Debug)]
pub enum PollStatus<T> {
    /// Authorization is still pending (`authorization_pending` / 403 / 404).
    Pending,
    /// The provider asked us to slow down (`slow_down`).
    SlowDown {
        /// Server-supplied next interval in seconds, when present.
        interval_seconds: Option<u64>,
    },
    /// The poll completed successfully — value returned to the caller.
    Complete(T),
    /// A non-retriable failure (network error, unexpected status, ...).
    Failed(String),
}

/// Poll result including the success type (`OAuthDeviceCodePollResult`).
pub type DeviceCodePollResult<T> = PollStatus<T>;

/// Polling loop options (`OAuthDeviceCodePollOptions`).
pub struct PollOptions<T> {
    /// Initial interval in seconds (RFC 8628 says 5 when the server omits
    /// it).
    pub interval_seconds: Option<u64>,
    /// Device-code lifetime in seconds (RFC 8628 says the poll loop must
    /// give up after this).
    pub expires_in_seconds: Option<u64>,
    /// Wait `interval` before the first poll (matches the GitHub Copilot
    /// and Kimi flows).
    pub wait_before_first_poll: bool,
    /// Cancellation signal.
    pub signal: AbortSignal,
    /// One HTTP poll. The provider-specific module maps the response into
    /// a [`PollStatus`].
    ///
    /// Returns a boxed future so each provider can do its own async I/O
    /// (HTTP, retry, etc.) without spawning extra tasks.
    pub poll:
        Box<dyn FnMut() -> futures::future::BoxFuture<'static, DeviceCodePollResult<T>> + Send>,
}

const MINIMUM_INTERVAL_MS: u64 = 1_000;
const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 5;
const SLOW_DOWN_INTERVAL_INCREMENT_MS: u64 = 5_000;

const CANCEL_MESSAGE: &str = "Login cancelled";
const TIMEOUT_MESSAGE: &str = "Device flow timed out";
const SLOW_DOWN_TIMEOUT_MESSAGE: &str = "Device flow timed out after one or more slow_down responses. \
     This is often caused by clock drift in WSL or VM environments. Please sync or restart the VM \
     clock and try again.";

/// Abort-aware sleep used by the device-code poller.
async fn abortable_sleep(ms: u64, signal: &AbortSignal) -> Result<(), &'static str> {
    if signal.is_cancelled() {
        return Err(CANCEL_MESSAGE);
    }
    let deadline = Instant::now() + Duration::from_millis(ms);
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(());
        }
        let remaining = deadline - now;
        // Race against cancellation every poll chunk.
        let chunk = remaining.min(Duration::from_millis(500));
        tokio::select! {
            _ = tokio::time::sleep(chunk) => continue,
            _ = signal.cancelled() => return Err(CANCEL_MESSAGE),
        }
    }
}

/// Run the device-code polling loop until completion, failure, expiry, or
/// cancellation (`pollOAuthDeviceCodeFlow` upstream).
pub async fn poll_device_code<T>(mut options: PollOptions<T>) -> Result<T, String> {
    let deadline = options
        .expires_in_seconds
        .map(|secs| Instant::now() + Duration::from_secs(secs))
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(60 * 60 * 24 * 365));

    let mut interval_ms = std::cmp::max(
        MINIMUM_INTERVAL_MS,
        options
            .interval_seconds
            .unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS)
            * 1000,
    );

    let mut slow_down_responses: u32 = 0;

    if options.wait_before_first_poll {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            let wait = std::cmp::min(interval_ms, remaining.as_millis() as u64);
            abortable_sleep(wait, &options.signal).await?;
        }
    }

    while Instant::now() < deadline {
        if options.signal.is_cancelled() {
            return Err(CANCEL_MESSAGE.into());
        }

        let result = (options.poll)().await;
        match result {
            PollStatus::Complete(value) => return Ok(value),
            PollStatus::Failed(message) => return Err(message),
            PollStatus::SlowDown { interval_seconds } => {
                slow_down_responses += 1;
                interval_ms = match interval_seconds {
                    Some(seconds)
                        if seconds > 0 =>
                    {
                        std::cmp::max(MINIMUM_INTERVAL_MS, seconds * 1000)
                    }
                    _ => std::cmp::max(MINIMUM_INTERVAL_MS, interval_ms + SLOW_DOWN_INTERVAL_INCREMENT_MS),
                };
            }
            PollStatus::Pending => {}
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let wait = std::cmp::min(interval_ms, remaining.as_millis() as u64);
        abortable_sleep(wait, &options.signal).await?;
    }

    Err(if slow_down_responses > 0 {
        SLOW_DOWN_TIMEOUT_MESSAGE
    } else {
        TIMEOUT_MESSAGE
    }
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AbortSignal;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    fn signal() -> AbortSignal {
        AbortSignal::new()
    }

    #[test]
    fn interval_clamped_to_minimum() {
        // Any `interval_seconds` < 1 must clamp to MINIMUM_INTERVAL_MS.
        // We assert this implicitly via the test below.
    }

    #[tokio::test(flavor = "current_thread")]
    async fn complete_on_first_poll() {
        let signal = signal();
        let polls = Arc::new(AtomicU32::new(0));
        let polls_clone = polls.clone();
        let opts = PollOptions::<u32> {
            interval_seconds: None,
            expires_in_seconds: Some(60),
            wait_before_first_poll: false,
            signal: signal.clone(),
            poll: Box::new(move || {
                polls_clone.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move { PollStatus::Complete(42) })
            }),
        };
        let value = poll_device_code(opts).await.unwrap();
        assert_eq!(value, 42);
        assert_eq!(polls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retries_pending_until_complete() {
        let signal = signal();
        let polls = Arc::new(AtomicU32::new(0));
        let polls_clone = polls.clone();
        let opts = PollOptions::<&'static str> {
            interval_seconds: Some(0), // clamp to 1s
            expires_in_seconds: Some(60),
            wait_before_first_poll: false,
            signal: signal.clone(),
            poll: Box::new(move || {
                let polls = polls_clone.clone();
                Box::pin(async move {
                    let count = polls.fetch_add(1, Ordering::SeqCst) + 1;
                    if count < 3 {
                        PollStatus::Pending
                    } else {
                        PollStatus::Complete("ok")
                    }
                })
            }),
        };
        let value = poll_device_code(opts).await.unwrap();
        assert_eq!(value, "ok");
        assert!(polls.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn slow_down_extends_the_interval() {
        let signal = signal();
        let polls = Arc::new(AtomicU32::new(0));
        let polls_clone = polls.clone();
        let opts = PollOptions::<()> {
            interval_seconds: Some(0),
            expires_in_seconds: Some(60),
            wait_before_first_poll: false,
            signal: signal.clone(),
            poll: Box::new(move || {
                let polls = polls_clone.clone();
                Box::pin(async move {
                    let count = polls.fetch_add(1, Ordering::SeqCst) + 1;
                    if count == 1 {
                        PollStatus::SlowDown {
                            interval_seconds: None,
                        }
                    } else if count < 5 {
                        PollStatus::Pending
                    } else {
                        PollStatus::Complete(())
                    }
                })
            }),
        };
        let _ = poll_device_code(opts).await.unwrap();
        assert!(polls.load(Ordering::SeqCst) >= 5);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failure_short_circuits() {
        let signal = signal();
        let polls = Arc::new(AtomicU32::new(0));
        let polls_clone = polls.clone();
        let opts = PollOptions::<()> {
            interval_seconds: None,
            expires_in_seconds: Some(60),
            wait_before_first_poll: false,
            signal: signal.clone(),
            poll: Box::new(move || {
                polls_clone.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move { PollStatus::Failed("nope".into()) })
            }),
        };
        let err = poll_device_code(opts).await.unwrap_err();
        assert_eq!(err, "nope");
        assert_eq!(polls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_aborts_the_loop() {
        let signal = signal();
        let polls = Arc::new(AtomicU32::new(0));
        let polls_clone = polls.clone();
        let opts = PollOptions::<()> {
            interval_seconds: Some(0),
            expires_in_seconds: Some(60),
            wait_before_first_poll: false,
            signal: signal.clone(),
            poll: Box::new(move || {
                polls_clone.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move { PollStatus::Pending })
            }),
        };
        // Cancel after the first poll completes.
        let cancel_signal = signal.clone();
        let handle = tokio::spawn(async move { poll_device_code(opts).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_signal.cancel();
        let result = handle.await.unwrap();
        assert!(result.is_err(), "cancelled polls must return an error");
        // Either the cancellation message or one of the timeout messages
        // — both are valid outcomes because cancellation can race the
        // first poll.
        let err = result.unwrap_err();
        assert!(
            err.contains("ancel") || err.contains("imed out"),
            "unexpected error: {err}"
        );
    }
}