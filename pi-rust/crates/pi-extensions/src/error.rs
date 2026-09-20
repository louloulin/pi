//! Errors produced by the JS extension host.

use thiserror::Error;

/// Failure mode surfaced by [`JsExtensionHost`](crate::JsExtensionHost).
#[derive(Debug, Error)]
pub enum ExtensionError {
    /// The JS source could not be parsed or evaluated.
    #[error("failed to load extension: {0}")]
    Load(String),

    /// The host import or event handler raised a JS exception that the
    /// engine reported back to Rust.
    #[error("runtime error: {0}")]
    Runtime(String),

    /// A host import or event handler exceeded the per-call timeout.
    #[error("extension call timed out after {0:?}")]
    Timeout(std::time::Duration),

    /// The extension sent a payload that did not match the expected
    /// wire shape (UI response missing, malformed registration JSON,
    /// etc.).
    #[error("protocol error: {0}")]
    Protocol(String),
}

impl From<rquickjs_core::Error> for ExtensionError {
    fn from(err: rquickjs_core::Error) -> Self {
        ExtensionError::Runtime(err.to_string())
    }
}

impl From<rquickjs_core::CaughtError<'_>> for ExtensionError {
    fn from(err: rquickjs_core::CaughtError<'_>) -> Self {
        ExtensionError::Runtime(err.to_string())
    }
}

impl From<serde_json::Error> for ExtensionError {
    fn from(err: serde_json::Error) -> Self {
        ExtensionError::Protocol(err.to_string())
    }
}
