//! `pi-ai` public types — `SimpleStreamOptions` and the error enum.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Options forwarded to a provider's streaming endpoint.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SimpleStreamOptions {
    /// Sampling temperature (0–2). `None` lets the provider pick the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Maximum output tokens. `None` lets the provider cap it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Abort signal — when cancelled the stream returns a partial
    /// `AssistantMessage` with `StopReason::Aborted`.
    #[serde(default, skip)]
    pub signal: Option<tokio_util::sync::CancellationToken>,
}

impl SimpleStreamOptions {
    /// Construct a default options value.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Errors a provider stream can produce.
#[derive(Debug, Error)]
pub enum StreamError {
    /// HTTP transport failure (DNS, TLS, timeout).
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    /// Provider returned a non-2xx response. Carries the status and body.
    #[error("provider returned {status}: {body}")]
    Provider {
        /// HTTP status code.
        status: u16,
        /// Response body (truncated to 4 KiB in the helper constructors).
        body: String,
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
