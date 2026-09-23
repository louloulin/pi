//! JSON-RPC 2.0 wire types for RPC mode.
//!
//! RPC mode speaks *strict* JSON-RPC 2.0 over stdio: one JSON object per
//! line on stdin (requests / notifications) and one JSON object per line
//! on stdout (responses / event notifications). There is no
//! `Content-Length` framing — that is the `pi-protocol` codec's job for
//! the socket transport, not this stdio surface.
//!
//! Types:
//!
//! * [`Request`] — `{ "jsonrpc": "2.0", "id", "method", "params" }`.
//! * [`Notification`] — same as [`Request`] but without `id`; the server
//!   never replies to one.
//! * [`Response`] — `{ "jsonrpc": "2.0", "id", "result" | "error" }`.
//!
//! `id` is kept as a [`serde_json::Value`] so both numeric and string
//! correlation ids round-trip untouched (JSON-RPC allows either).
//!
//! Note on serde tags: JSON-RPC frames carry no discriminant field, so
//! the outer wire types cannot use `#[serde(tag = …)]`. The tagged
//! surface lives one level down — every agent event is an
//! `AgentEvent`-derived object with a `type` tag (see
//! [`super::events`]), and [`Response`] serializes exactly one of
//! `result` / `error` via `skip_serializing_if`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::JsonRpcError;

/// Version string every frame must carry.
pub const JSONRPC_VERSION: &str = "2.0";

/// A JSON-RPC request (expects a response).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Protocol version — always `"2.0"`.
    pub jsonrpc: String,
    /// Correlation id. Reused verbatim in the [`Response`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// Method name (`prompt`, `abort`, `getState`, `setModel`, …).
    pub method: String,
    /// Method parameters. Object (by-name) or absent for our methods.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// A JSON-RPC notification (no id, no response).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// Protocol version — always `"2.0"`.
    pub jsonrpc: String,
    /// Method name.
    pub method: String,
    /// Method parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// A JSON-RPC response. Exactly one of `result` / `error` is present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Protocol version — always `"2.0"`.
    pub jsonrpc: String,
    /// Correlation id copied from the request. `None` (serialized as
    /// `null`) when the request was unparseable.
    #[serde(default)]
    pub id: Option<Value>,
    /// Success payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Failure payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl Response {
    /// Build a success response for `id`.
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Build an error response for `id`.
    pub fn error(id: Option<Value>, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// A parsed inbound message.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// Request with an `id` — the server must reply.
    Request(Request),
    /// Notification without an `id` — the server must not reply.
    Notification(Notification),
}

impl Incoming {
    /// Correlation id, if this message expects a response.
    pub fn id(&self) -> Option<&Value> {
        match self {
            Incoming::Request(request) => request.id.as_ref(),
            Incoming::Notification(_) => None,
        }
    }

    /// Method name.
    pub fn method(&self) -> &str {
        match self {
            Incoming::Request(request) => &request.method,
            Incoming::Notification(notification) => &notification.method,
        }
    }

    /// Params, if present.
    pub fn params(&self) -> Option<&Value> {
        match self {
            Incoming::Request(request) => request.params.as_ref(),
            Incoming::Notification(notification) => notification.params.as_ref(),
        }
    }
}

/// Validate a decoded JSON value as a JSON-RPC request or notification.
///
/// Returns [`JsonRpcError::invalid_request`] for values that are not
/// well-formed JSON-RPC 2.0 frames (wrong version, missing method,
/// non-structured params, …). Parse failures (`-32700`) are the caller's
/// job — this function assumes `value` already decoded from JSON.
pub fn parse_incoming(value: Value) -> Result<Incoming, JsonRpcError> {
    let object = value
        .as_object()
        .ok_or_else(|| JsonRpcError::invalid_request("message must be a JSON object"))?;

    match object.get("jsonrpc").and_then(Value::as_str) {
        Some(JSONRPC_VERSION) => {}
        Some(other) => {
            return Err(JsonRpcError::invalid_request(format!(
                "unsupported jsonrpc version: {other}"
            )))
        }
        None => {
            return Err(JsonRpcError::invalid_request(
                "missing \"jsonrpc\": \"2.0\" member",
            ))
        }
    }

    match object.get("method") {
        Some(Value::String(method)) if !method.is_empty() => {}
        Some(_) => {
            return Err(JsonRpcError::invalid_request(
                "\"method\" must be a non-empty string",
            ))
        }
        None => return Err(JsonRpcError::invalid_request("missing \"method\" member")),
    }

    if let Some(id) = object.get("id") {
        let valid = matches!(id, Value::String(_) | Value::Number(_) | Value::Null);
        if !valid {
            return Err(JsonRpcError::invalid_request(
                "\"id\" must be a string, number, or null",
            ));
        }
    }

    if let Some(params) = object.get("params") {
        if !matches!(params, Value::Object(_) | Value::Array(_)) {
            return Err(JsonRpcError::invalid_request(
                "\"params\" must be an object or array when present",
            ));
        }
    }

    if object.contains_key("id") {
        let request: Request = serde_json::from_value(value)
            .map_err(|err| JsonRpcError::invalid_request(err.to_string()))?;
        Ok(Incoming::Request(request))
    } else {
        let notification: Notification = serde_json::from_value(value)
            .map_err(|err| JsonRpcError::invalid_request(err.to_string()))?;
        Ok(Incoming::Notification(notification))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_a_request_with_numeric_id() {
        let incoming = parse_incoming(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getState",
        }))
        .expect("valid request");
        let Incoming::Request(request) = incoming else {
            panic!("expected a request");
        };
        assert_eq!(request.method, "getState");
        assert_eq!(request.id, Some(json!(1)));
        assert_eq!(request.params, None);
    }

    #[test]
    fn parses_a_request_with_string_id_and_params() {
        let incoming = parse_incoming(json!({
            "jsonrpc": "2.0",
            "id": "abc",
            "method": "prompt",
            "params": {"text": "hi"},
        }))
        .expect("valid request");
        assert_eq!(incoming.id(), Some(&json!("abc")));
        assert_eq!(incoming.method(), "prompt");
        assert_eq!(incoming.params(), Some(&json!({"text": "hi"})));
    }

    #[test]
    fn parses_a_notification() {
        let incoming = parse_incoming(json!({
            "jsonrpc": "2.0",
            "method": "abort",
        }))
        .expect("valid notification");
        assert!(matches!(incoming, Incoming::Notification(_)));
        assert!(incoming.id().is_none());
    }

    #[test]
    fn rejects_missing_version() {
        let err = parse_incoming(json!({"id": 1, "method": "getState"})).unwrap_err();
        assert_eq!(err.code, super::super::error::codes::INVALID_REQUEST);
    }

    #[test]
    fn rejects_wrong_version() {
        let err = parse_incoming(json!({"jsonrpc": "1.0", "id": 1, "method": "x"})).unwrap_err();
        assert_eq!(err.code, super::super::error::codes::INVALID_REQUEST);
    }

    #[test]
    fn rejects_non_structured_params() {
        let err = parse_incoming(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "prompt",
            "params": "nope",
        }))
        .unwrap_err();
        assert_eq!(err.code, super::super::error::codes::INVALID_REQUEST);
    }

    #[test]
    fn response_serializes_only_one_of_result_or_error() {
        let ok = Response::success(Some(json!(7)), json!({"turn": 1}));
        let ok_value = serde_json::to_value(&ok).unwrap();
        assert_eq!(ok_value["id"], 7);
        assert_eq!(ok_value["result"]["turn"], 1);
        assert!(ok_value.get("error").is_none());

        let err = Response::error(Some(json!(7)), JsonRpcError::method_not_found("nope"));
        let err_value = serde_json::to_value(&err).unwrap();
        assert_eq!(err_value["error"]["code"], -32601);
        assert!(err_value.get("result").is_none());
    }
}
