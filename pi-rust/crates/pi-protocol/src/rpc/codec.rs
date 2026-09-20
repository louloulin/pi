//! Validating framed-message codec.
//!
//! Rust port of `packages/protocol/src/codec.ts`. Messages are validated
//! against the strict JSON schema from [`super::protocol`], CBOR-encoded, and
//! prefixed with their length; decoders reverse that pipeline and latch into a
//! failed state after the first bad frame, exactly like upstream.

use std::fmt;

use serde_json::{Map, Value};

use super::cbor::{decode_cbor, encode_cbor, CborOptions};
use super::framing::{encode_frame, FrameDecoder, DEFAULT_MAX_FRAME_LENGTH};
use super::protocol::{
    is_server_id, ClientMessage, ProtocolError, RpcTarget, ServerMessage, PROTOCOL_VERSION,
};

/// A message failed schema validation or codec encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolValidationError {
    message: String,
}

impl ProtocolValidationError {
    /// Creates a validation error with `message`.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The error detail.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ProtocolValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProtocolValidationError {}

type Result<T> = std::result::Result<T, ProtocolValidationError>;

fn invalid(message: impl Into<String>) -> ProtocolValidationError {
    ProtocolValidationError::new(message)
}

fn object<'a>(value: &'a Value, kind: &str) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("Invalid {kind}")))
}

fn assert_keys(
    map: &Map<String, Value>,
    required: &[&str],
    optional: &[&str],
    kind: &str,
) -> Result<()> {
    for key in map.keys() {
        if !required.contains(&key.as_str()) && !optional.contains(&key.as_str()) {
            return Err(invalid(format!("Invalid {kind}: unexpected field {key}")));
        }
    }
    for key in required {
        if !map.contains_key(*key) {
            return Err(invalid(format!("Invalid {kind}: missing field {key}")));
        }
    }
    Ok(())
}

fn id_string(map: &Map<String, Value>, key: &str, kind: &str) -> Result<String> {
    map.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid(format!("Invalid {kind}")))
}

fn protocol_error(value: &Value, kind: &str) -> Result<ProtocolError> {
    let map = object(value, kind)?;
    assert_keys(map, &["code", "message"], &[], kind)?;
    let code = id_string(map, "code", kind)?;
    let message = map
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("Invalid {kind}")))?;
    Ok(ProtocolError {
        code,
        message: message.to_owned(),
    })
}

fn rpc_target(value: &Value) -> Result<RpcTarget> {
    let map = object(value, "RPC target")?;
    if map.contains_key("sessionId") || map.contains_key("attachmentId") {
        assert_keys(
            map,
            &["serverId", "sessionId", "attachmentId"],
            &[],
            "session target",
        )?;
        Ok(RpcTarget::Session(super::protocol::SessionTarget {
            server_id: id_string(map, "serverId", "session target")?,
            session_id: id_string(map, "sessionId", "session target")?,
            attachment_id: id_string(map, "attachmentId", "session target")?,
        }))
    } else {
        assert_keys(map, &["serverId"], &[], "server target")?;
        let server_id = id_string(map, "serverId", "server target")?;
        if !is_server_id(&server_id) {
            return Err(invalid(
                "Invalid server target: serverId must be a canonical UUIDv4",
            ));
        }
        Ok(RpcTarget::Server(super::protocol::ServerTarget {
            server_id,
        }))
    }
}

fn validate_attachment(value: &Value) -> Result<()> {
    if value.is_null() {
        return Ok(());
    }
    let map = object(value, "attachment target")?;
    assert_keys(
        map,
        &["serverId", "sessionId", "attachmentId"],
        &[],
        "attachment target",
    )?;
    Ok(())
}

/// Validates a decoded client message against the strict wire schema.
pub fn validate_client_message(value: &Value) -> Result<()> {
    let map = object(value, "client protocol message")?;
    match map.get("type").and_then(Value::as_str) {
        Some("hello") => {
            assert_keys(map, &["type", "version"], &[], "client hello")?;
            if !map.get("version").is_some_and(Value::is_u64) {
                return Err(invalid(
                    "Invalid client hello: version must be a non-negative integer",
                ));
            }
            Ok(())
        }
        Some("request") => {
            assert_keys(
                map,
                &["type", "id", "target", "call"],
                &[],
                "request envelope",
            )?;
            id_string(map, "id", "request envelope")?;
            rpc_target(map.get("target").unwrap_or(&Value::Null))?;
            Ok(())
        }
        Some("cancel") => {
            assert_keys(map, &["type", "id", "target"], &[], "cancel envelope")?;
            id_string(map, "id", "cancel envelope")?;
            rpc_target(map.get("target").unwrap_or(&Value::Null))?;
            Ok(())
        }
        _ => Err(invalid("Invalid client protocol message")),
    }
}

/// Validates a decoded server message against the strict wire schema.
pub fn validate_server_message(value: &Value) -> Result<()> {
    let map = object(value, "server protocol message")?;
    match map.get("type").and_then(Value::as_str) {
        Some("hello") => {
            assert_keys(map, &["type", "version", "serverId"], &[], "server hello")?;
            let version = map.get("version").and_then(Value::as_u64);
            if version != Some(PROTOCOL_VERSION as u64) {
                return Err(invalid(
                    "Invalid server hello: unsupported protocol version",
                ));
            }
            let server_id = id_string(map, "serverId", "server hello")?;
            if !is_server_id(&server_id) {
                return Err(invalid(
                    "Invalid server hello: serverId must be a canonical UUIDv4",
                ));
            }
            Ok(())
        }
        Some("hello_error") => {
            assert_keys(map, &["type", "error"], &[], "server hello error")?;
            protocol_error(
                map.get("error").unwrap_or(&Value::Null),
                "server hello error",
            )?;
            Ok(())
        }
        Some("response") => {
            assert_keys(
                map,
                &["type", "id", "ok"],
                &["result", "error"],
                "response envelope",
            )?;
            id_string(map, "id", "response envelope")?;
            match map.get("ok").and_then(Value::as_bool) {
                Some(true) => {
                    if map.contains_key("error") {
                        return Err(invalid(
                            "Invalid response envelope: ok response carries an error",
                        ));
                    }
                    Ok(())
                }
                Some(false) => {
                    if map.contains_key("result") {
                        return Err(invalid(
                            "Invalid response envelope: failed response carries a result",
                        ));
                    }
                    protocol_error(
                        map.get("error").unwrap_or(&Value::Null),
                        "response envelope",
                    )?;
                    Ok(())
                }
                None => Err(invalid("Invalid response envelope: ok must be a boolean")),
            }
        }
        Some("service_update") => {
            assert_keys(
                map,
                &["type", "subscriptionId", "update"],
                &[],
                "service update",
            )?;
            id_string(map, "subscriptionId", "service update")?;
            Ok(())
        }
        Some("attachment") => {
            assert_keys(map, &["type", "attachment"], &[], "attachment envelope")?;
            validate_attachment(map.get("attachment").unwrap_or(&Value::Null))
        }
        _ => Err(invalid("Invalid server protocol message")),
    }
}

/// Parses and validates a decoded client message.
pub fn parse_client_message(value: &Value) -> Result<ClientMessage> {
    validate_client_message(value)?;
    serde_json::from_value(value.clone())
        .map_err(|error| invalid(format!("Invalid client protocol message: {error}")))
}

/// Parses and validates a decoded server message.
pub fn parse_server_message(value: &Value) -> Result<ServerMessage> {
    validate_server_message(value)?;
    serde_json::from_value(value.clone())
        .map_err(|error| invalid(format!("Invalid server protocol message: {error}")))
}

fn encode_protocol_message<T, F>(
    message: &T,
    options: CborOptions,
    parse: F,
    kind: &str,
) -> Result<Vec<u8>>
where
    T: serde::Serialize,
    F: FnOnce(&Value) -> Result<()>,
{
    let value = serde_json::to_value(message)
        .map_err(|error| invalid(format!("Unable to encode {kind} protocol message: {error}")))?;
    parse(&value)?;
    let payload = encode_cbor(&value, options)
        .map_err(|error| invalid(format!("Unable to encode {kind} protocol message: {error}")))?;
    encode_frame(&payload)
        .map_err(|error| invalid(format!("Unable to encode {kind} protocol message: {error}")))
}

/// Validates and encodes one complete length-prefixed client message.
pub fn encode_client_message(
    message: &ClientMessage,
    max_frame_length: Option<usize>,
) -> Result<Vec<u8>> {
    let limit = max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
    encode_protocol_message(
        message,
        CborOptions::new(limit, DEFAULT_MAX_CBOR_CONTAINER, DEFAULT_MAX_CBOR_DEPTH),
        validate_client_message,
        "client",
    )
}

/// Validates and encodes one complete length-prefixed server message.
pub fn encode_server_message(
    message: &ServerMessage,
    max_frame_length: Option<usize>,
) -> Result<Vec<u8>> {
    let limit = max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
    encode_protocol_message(
        message,
        CborOptions::new(limit, DEFAULT_MAX_CBOR_CONTAINER, DEFAULT_MAX_CBOR_DEPTH),
        validate_server_message,
        "server",
    )
}

const DEFAULT_MAX_CBOR_CONTAINER: usize = super::cbor::DEFAULT_MAX_CBOR_CONTAINER_LENGTH;
const DEFAULT_MAX_CBOR_DEPTH: usize = super::cbor::DEFAULT_MAX_CBOR_DEPTH;

struct ValidatedMessageDecoder<T> {
    frames: FrameDecoder,
    parse: fn(&Value) -> Result<T>,
    kind: &'static str,
    options: CborOptions,
    failed: bool,
}

impl<T> ValidatedMessageDecoder<T> {
    fn new(
        kind: &'static str,
        parse: fn(&Value) -> Result<T>,
        max_frame_length: Option<usize>,
    ) -> Self {
        let limit = max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
        Self {
            frames: FrameDecoder::with_max_frame_length(limit),
            parse,
            kind,
            options: CborOptions::new(limit, DEFAULT_MAX_CBOR_CONTAINER, DEFAULT_MAX_CBOR_DEPTH),
            failed: false,
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Result<Vec<T>> {
        if self.failed {
            return Err(invalid(format!("{} message decoder has failed", self.kind)));
        }
        let result = (|| {
            let frames = self.frames.push(chunk).map_err(|error| {
                invalid(format!("Invalid {} protocol framing: {error}", self.kind))
            })?;
            let mut messages = Vec::with_capacity(frames.len());
            for frame in frames {
                let value = decode_cbor(&frame, self.options).map_err(|error| {
                    invalid(format!("Invalid {} protocol frame: {error}", self.kind))
                })?;
                messages.push((self.parse)(&value)?);
            }
            Ok(messages)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn end(&mut self) -> Result<()> {
        if self.failed {
            return Err(invalid(format!("{} message decoder has failed", self.kind)));
        }
        if let Err(error) = self.frames.end() {
            self.failed = true;
            return Err(invalid(format!(
                "Invalid {} protocol framing: {error}",
                self.kind
            )));
        }
        Ok(())
    }
}

/// Incrementally decodes and validates framed client messages.
pub struct ClientMessageDecoder {
    inner: ValidatedMessageDecoder<ClientMessage>,
}

impl ClientMessageDecoder {
    /// Creates a decoder, optionally overriding the maximum frame length.
    pub fn new(max_frame_length: Option<usize>) -> Self {
        Self {
            inner: ValidatedMessageDecoder::new("client", parse_client_message, max_frame_length),
        }
    }

    /// Pushes a chunk and returns every complete message it closed.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<ClientMessage>> {
        self.inner.push(chunk)
    }

    /// Signals end-of-stream.
    pub fn end(&mut self) -> Result<()> {
        self.inner.end()
    }
}

/// Incrementally decodes and validates framed server messages.
pub struct ServerMessageDecoder {
    inner: ValidatedMessageDecoder<ServerMessage>,
}

impl ServerMessageDecoder {
    /// Creates a decoder, optionally overriding the maximum frame length.
    pub fn new(max_frame_length: Option<usize>) -> Self {
        Self {
            inner: ValidatedMessageDecoder::new("server", parse_server_message, max_frame_length),
        }
    }

    /// Pushes a chunk and returns every complete message it closed.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<ServerMessage>> {
        self.inner.push(chunk)
    }

    /// Signals end-of-stream.
    pub fn end(&mut self) -> Result<()> {
        self.inner.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::protocol::{ServerTarget, SessionTarget};
    use serde_json::json;

    const SERVER_ID: &str = "00000000-0000-4000-8000-000000000001";

    #[test]
    fn client_hello_round_trips_through_a_fragmented_stream() {
        let encoded = encode_client_message(&ClientMessage::hello(PROTOCOL_VERSION), None).unwrap();
        let mut decoder = ClientMessageDecoder::new(None);
        let mut messages = Vec::new();
        for byte in &encoded {
            messages.extend(decoder.push(&[*byte]).unwrap());
        }
        decoder.end().unwrap();
        assert_eq!(messages, vec![ClientMessage::hello(PROTOCOL_VERSION)]);
    }

    #[test]
    fn server_message_round_trips() {
        let messages = vec![
            ServerMessage::hello(SERVER_ID),
            ServerMessage::response_ok("request-1", Some(json!({"ok": true}))),
            ServerMessage::response_error(
                "request-2",
                ProtocolError {
                    code: "wrong_server".into(),
                    message: "nope".into(),
                },
            ),
            ServerMessage::service_update("sub-1", json!({"type": "unavailable"})),
            ServerMessage::attachment(Some(SessionTarget {
                server_id: SERVER_ID.into(),
                session_id: "session-1".into(),
                attachment_id: "attachment-1".into(),
            })),
            ServerMessage::attachment(None),
        ];
        let mut stream = Vec::new();
        for message in &messages {
            stream.extend_from_slice(&encode_server_message(message, None).unwrap());
        }
        let mut decoder = ServerMessageDecoder::new(None);
        assert_eq!(decoder.push(&stream).unwrap(), messages);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let value = json!({"type": "hello", "version": 8, "extra": 1});
        assert!(validate_client_message(&value).is_err());
        let value = json!({"type": "response", "id": "1", "ok": true, "error": {"code": "x", "message": "y"}});
        assert!(validate_server_message(&value).is_err());
    }

    #[test]
    fn server_id_schema_is_enforced() {
        assert!(is_server_id(SERVER_ID));
        assert!(!is_server_id("00000000-0000-4000-8000-00000000000Z"));
        assert!(!is_server_id("00000000-0000-3000-8000-000000000001"));
        let value = json!({
            "type": "request",
            "id": "1",
            "target": {"serverId": "not-a-uuid"},
            "call": {}
        });
        assert!(validate_client_message(&value).is_err());
    }

    #[test]
    fn target_variants_parse() {
        let target: RpcTarget = serde_json::from_value(json!({"serverId": SERVER_ID})).unwrap();
        assert_eq!(
            target,
            RpcTarget::Server(ServerTarget {
                server_id: SERVER_ID.into()
            })
        );
        let target: RpcTarget = serde_json::from_value(json!({
            "serverId": SERVER_ID,
            "sessionId": "s",
            "attachmentId": "a"
        }))
        .unwrap();
        assert!(matches!(target, RpcTarget::Session(_)));
    }
}
