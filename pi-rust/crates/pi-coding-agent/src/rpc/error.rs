//! JSON-RPC error codes and the [`JsonRpcError`] payload.
//!
//! The codes follow the JSON-RPC 2.0 specification. We also pin down the
//! implementation-defined `-32000` range with a single `busy` code so an
//! embedder can tell "the server refuses right now" apart from a
//! malformed request.

use serde::{Deserialize, Serialize};

/// Numeric error codes used by the RPC server.
pub mod codes {
    /// Invalid JSON was received (`-32700`).
    pub const PARSE_ERROR: i64 = -32700;
    /// The JSON was valid but not a well-formed request (`-32600`).
    pub const INVALID_REQUEST: i64 = -32600;
    /// The requested method does not exist (`-32601`).
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Method params failed validation (`-32602`).
    pub const INVALID_PARAMS: i64 = -32602;
    /// Unexpected server-side failure (`-32603`).
    pub const INTERNAL_ERROR: i64 = -32603;
    /// A turn is already in flight and the server refuses a second one
    /// (`-32000`, implementation-defined).
    pub const BUSY: i64 = -32000;
}

/// JSON-RPC error object.
///
/// Serializes to `{ "code": …, "message": …, "data": … }` with `data`
/// omitted when absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// Numeric error code (see [`codes`]).
    pub code: i64,
    /// Human-readable message. Kept on stderr-friendly single lines.
    pub message: String,
    /// Optional structured detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcError {
    /// Construct an error with an explicit code.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// `-32700` — the line was not valid JSON.
    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::new(codes::PARSE_ERROR, message)
    }

    /// `-32600` — valid JSON, malformed JSON-RPC frame.
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(codes::INVALID_REQUEST, message)
    }

    /// `-32601` — unknown method.
    pub fn method_not_found(method: &str) -> Self {
        Self::new(codes::METHOD_NOT_FOUND, format!("method not found: {method}"))
    }

    /// `-32602` — params did not match the method's schema.
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(codes::INVALID_PARAMS, message)
    }

    /// `-32603` — internal failure (agent error, provider error, …).
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(codes::INTERNAL_ERROR, message)
    }

    /// `-32000` — a turn is already running.
    pub fn busy() -> Self {
        Self::new(
            codes::BUSY,
            "a turn is already in flight; wait for turn_end or send abort",
        )
    }

    /// Attach structured detail to the error.
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_the_jsonrpc_spec() {
        assert_eq!(codes::PARSE_ERROR, -32700);
        assert_eq!(codes::INVALID_REQUEST, -32600);
        assert_eq!(codes::METHOD_NOT_FOUND, -32601);
        assert_eq!(codes::INVALID_PARAMS, -32602);
        assert_eq!(codes::INTERNAL_ERROR, -32603);
        assert_eq!(codes::BUSY, -32000);
    }

    #[test]
    fn errors_without_data_omit_the_field() {
        let value = serde_json::to_value(JsonRpcError::busy()).unwrap();
        assert_eq!(value["code"], -32000);
        assert!(value.get("data").is_none());
    }

    #[test]
    fn with_data_round_trips() {
        let err = JsonRpcError::invalid_params("bad model").with_data(serde_json::json!({"model": "nope"}));
        let value = serde_json::to_value(&err).unwrap();
        assert_eq!(value["data"]["model"], "nope");
    }
}
