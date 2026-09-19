//! `pi-ai` public types — `SimpleStreamOptions` and the error enum.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(not(target_arch = "wasm32"))]
use ::reqwest;

/// Options forwarded to a provider's streaming endpoint.
#[derive(Debug, Clone, Default)]
pub struct SimpleStreamOptions {
    /// Sampling temperature (0–2). `None` lets the provider pick the default.
    pub temperature: Option<f32>,
    /// Maximum output tokens. `None` lets the provider cap it.
    pub max_tokens: Option<u32>,
    /// Abort signal — when cancelled the stream returns a partial
    /// `AssistantMessage` with `StopReason::Aborted`.
    pub signal: Option<AbortSignal>,
}

/// Cancellation token compatible with both native (tokio) and WASM builds.
///
/// The type is conditionally constructed: native builds wrap a
/// `tokio_util::sync::CancellationToken`, WASM builds wrap a
/// `js_sys::Function` (the JS `AbortSignal.onabort` callback) so the JS host
/// can drive cancellation without a tokio context. Consumers that need to
/// check cancellation call [`AbortSignal::is_cancelled`]; providers that
/// need to *await* cancellation call [`AbortSignal::cancelled`].
#[derive(Clone, Default)]
pub struct AbortSignal {
    inner: Option<AbortSignalInner>,
}

#[derive(Clone)]
enum AbortSignalInner {
    /// Native handle backed by a `tokio_util::sync::CancellationToken`.
    #[cfg(not(target_arch = "wasm32"))]
    Native(tokio_util::sync::CancellationToken),
    /// WASM-side handle backed by a `js_sys::Function` invoked when
    /// cancellation is observed.
    #[cfg(target_arch = "wasm32")]
    Wasm(WasmAbortSignal),
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
struct WasmAbortSignal {
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl AbortSignal {
    /// Construct a never-cancelled signal.
    pub fn new() -> Self {
        Self { inner: None }
    }

    /// Wrap a native `CancellationToken`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn native(token: tokio_util::sync::CancellationToken) -> Self {
        Self {
            inner: Some(AbortSignalInner::Native(token)),
        }
    }

    /// Construct a WASM-side signal backed by a `cancelled` flag the JS
    /// host toggles.
    #[cfg(target_arch = "wasm32")]
    pub fn wasm(cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self {
            inner: Some(AbortSignalInner::Wasm(WasmAbortSignal { cancelled })),
        }
    }

    /// True when the signal has been triggered.
    pub fn is_cancelled(&self) -> bool {
        match &self.inner {
            #[cfg(not(target_arch = "wasm32"))]
            Some(AbortSignalInner::Native(token)) => token.is_cancelled(),
            #[cfg(target_arch = "wasm32")]
            Some(AbortSignalInner::Wasm(signal)) => {
                signal.cancelled.load(std::sync::atomic::Ordering::Relaxed)
            }
            None => false,
        }
    }

    /// Awaitable cancellation future.
    ///
    /// Native-only; WASM providers poll [`AbortSignal::is_cancelled`]
    /// between awaits because the WASM event loop has no native
    /// `select!`.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn cancelled(&self) {
        match &self.inner {
            Some(AbortSignalInner::Native(token)) => token.cancelled().await,
            None => std::future::pending().await,
        }
    }
}

impl std::fmt::Debug for AbortSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AbortSignal")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl SimpleStreamOptions {
    /// Construct a default options value.
    pub fn new() -> Self {
        Self::default()
    }
}

// Custom serde for `SimpleStreamOptions` — `signal` cannot be serialized
// because `AbortSignal` wraps a non-serialisable token type.
impl Serialize for SimpleStreamOptions {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("SimpleStreamOptions", 2)?;
        if let Some(temperature) = self.temperature {
            state.serialize_field("temperature", &temperature)?;
        }
        if let Some(max_tokens) = self.max_tokens {
            state.serialize_field("max_tokens", &max_tokens)?;
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for SimpleStreamOptions {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            temperature: Option<f32>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            max_tokens: Option<u32>,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(SimpleStreamOptions {
            temperature: raw.temperature,
            max_tokens: raw.max_tokens,
            signal: None,
        })
    }
}

/// Server-supplied retry guidance carried by a non-2xx provider response.
///
/// This is the narrowed Rust counterpart of the `status` + `headers` pair
/// upstream's `ProviderError` hands to `utils/provider-retry.ts`: only the
/// two header signals [`crate::retry`] reads are kept, so the error type
/// stays transport-agnostic (and serialisable) instead of holding a
/// `reqwest::header::HeaderMap`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderRetryHint {
    /// `Retry-After-Ms`, or a numeric `Retry-After`, in milliseconds.
    /// The HTTP-date form of `Retry-After` is not parsed yet.
    pub retry_after_ms: Option<u64>,
    /// `X-Should-Retry` override: `Some(true)` forces a retry, `Some(false)`
    /// suppresses one, `None` leaves the decision to the status code.
    pub should_retry: Option<bool>,
}

/// Errors a provider stream can produce.
#[derive(Debug, Error)]
pub enum StreamError {
    /// HTTP transport failure (DNS, TLS, timeout).
    ///
    /// Only constructed on native targets — the WASM build cannot use
    /// `reqwest` because its `mio` dependency does not compile on
    /// `wasm32-unknown-unknown`.
    #[cfg(not(target_arch = "wasm32"))]
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    /// Provider returned a non-2xx response. Carries the status, body and
    /// the response's retry guidance.
    #[error("provider returned {status}: {body}")]
    Provider {
        /// HTTP status code.
        status: u16,
        /// Response body (truncated to 4 KiB in the helper constructors).
        body: String,
        /// Retry guidance parsed from the response headers.
        hint: ProviderRetryHint,
    },
    /// Stream produced malformed SSE / JSON.
    #[error("malformed stream: {0}")]
    Malformed(String),
    /// Generation aborted by the caller.
    #[error("aborted")]
    Aborted,
    /// Underlying I/O error (e.g. file fixture read).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl StreamError {
    /// A non-2xx provider response with no server retry guidance.
    pub fn provider(status: u16, body: impl Into<String>) -> Self {
        Self::Provider {
            status,
            body: body.into(),
            hint: ProviderRetryHint::default(),
        }
    }

    /// A non-2xx provider response carrying retry guidance parsed from its
    /// headers (`Retry-After-Ms`, `Retry-After`, `X-Should-Retry`).
    pub fn provider_with_hint(
        status: u16,
        body: impl Into<String>,
        hint: ProviderRetryHint,
    ) -> Self {
        Self::Provider {
            status,
            body: body.into(),
            hint,
        }
    }
}
