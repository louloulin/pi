//! Wire types for the server/client RPC protocol.
//!
//! Rust port of the type surface in `packages/protocol/src/protocol.ts`. The
//! upstream package validates its messages with TypeBox schemas
//! (`additionalProperties: false`, tagged unions), so these Rust types mirror
//! the same strict JSON shapes and the codec layer re-validates decoded values
//! against them.
//!
//! The transport framing and CBOR codec live in [`super::framing`] and
//! [`super::codec`]; this module is only the vocabulary.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol version negotiated during the handshake.
pub const PROTOCOL_VERSION: u32 = 8;

/// One safe-to-serialize error that crosses the protocol boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolError {
    /// Stable error code (a remote-service code or a server lifecycle code).
    pub code: String,
    /// Human-readable detail.
    pub message: String,
}

/// A server-wide RPC target, fenced to one logical server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerTarget {
    /// The logical server identity.
    pub server_id: String,
}

/// A session RPC target, fenced to one server, session and live attachment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionTarget {
    /// The logical server identity.
    pub server_id: String,
    /// The durable session identity.
    pub session_id: String,
    /// The live presentation attachment identity.
    pub attachment_id: String,
}

/// Where an RPC request is addressed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcTarget {
    /// A server-wide call.
    Server(ServerTarget),
    /// A session call.
    Session(SessionTarget),
}

impl RpcTarget {
    /// The logical server this target is fenced to.
    pub fn server_id(&self) -> &str {
        match self {
            RpcTarget::Server(target) => &target.server_id,
            RpcTarget::Session(target) => &target.server_id,
        }
    }
}

/// A service request envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestEnvelope {
    /// Correlation id chosen by the client.
    pub id: String,
    /// The addressed target.
    pub target: RpcTarget,
    /// The opaque service call payload.
    pub call: Value,
}

/// A cancellation envelope for one in-flight request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CancelEnvelope {
    /// The request id to cancel.
    pub id: String,
    /// The target the cancelled request was addressed to.
    pub target: RpcTarget,
}

/// The first frame a client sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientHello {
    /// The client's protocol version.
    pub version: u32,
}

/// Any message a client may send.
///
/// The `type` discriminant is part of the wire encoding, so this is an
/// internally tagged enum rather than a struct with a free-form tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// The mandatory first frame.
    Hello {
        /// The client's protocol version.
        version: u32,
    },
    /// A service request.
    Request {
        /// Correlation id.
        id: String,
        /// Addressed target.
        target: RpcTarget,
        /// Opaque service call.
        call: Value,
    },
    /// A cancellation for one in-flight request.
    Cancel {
        /// The request id to cancel.
        id: String,
        /// The target the cancelled request was addressed to.
        target: RpcTarget,
    },
}

/// The server's handshake acknowledgement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerHello {
    /// Always [`PROTOCOL_VERSION`].
    pub version: u32,
    /// The logical server identity.
    pub server_id: String,
}

/// A terminal handshake failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerHelloError {
    /// The bounded protocol error.
    pub error: ProtocolError,
}

/// A server response to one request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseEnvelope {
    /// The request id being answered.
    pub id: String,
    /// Whether the call succeeded.
    pub ok: bool,
    /// The success payload, omitted when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The failure payload, present exactly when `ok == false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

/// An out-of-band service state update.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceEventEnvelope {
    /// The subscription this update belongs to.
    pub subscription_id: String,
    /// The opaque update payload.
    pub update: Value,
}

/// An out-of-band change to the presentation's selected session route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentEnvelope {
    /// The attached target, or `null` after a detach.
    pub attachment: Option<SessionTarget>,
}

/// Any message a server may send.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// The handshake acknowledgement.
    Hello {
        /// Always [`PROTOCOL_VERSION`].
        version: u32,
        /// The logical server identity.
        #[serde(rename = "serverId")]
        server_id: String,
    },
    /// A terminal handshake failure.
    HelloError {
        /// The bounded protocol error.
        error: ProtocolError,
    },
    /// A response to one request.
    Response {
        /// The request id being answered.
        id: String,
        /// Whether the call succeeded.
        ok: bool,
        /// The success payload, omitted when `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        /// The failure payload, present exactly when `ok == false`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<ProtocolError>,
    },
    /// An out-of-band service state update.
    ServiceUpdate {
        /// The subscription this update belongs to.
        #[serde(rename = "subscriptionId")]
        subscription_id: String,
        /// The opaque update payload.
        update: Value,
    },
    /// An out-of-band attachment change.
    Attachment {
        /// The attached target, or `null` after a detach.
        attachment: Option<SessionTarget>,
    },
}

impl ServerMessage {
    /// Builds a successful response, omitting the result when it is `None`.
    pub fn response_ok(id: impl Into<String>, result: Option<Value>) -> Self {
        ServerMessage::Response {
            id: id.into(),
            ok: true,
            result,
            error: None,
        }
    }

    /// Builds a failed response.
    pub fn response_error(id: impl Into<String>, error: ProtocolError) -> Self {
        ServerMessage::Response {
            id: id.into(),
            ok: false,
            result: None,
            error: Some(error),
        }
    }

    /// Builds a service update envelope.
    pub fn service_update(subscription_id: impl Into<String>, update: Value) -> Self {
        ServerMessage::ServiceUpdate {
            subscription_id: subscription_id.into(),
            update,
        }
    }

    /// Builds an attachment envelope.
    pub fn attachment(attachment: Option<SessionTarget>) -> Self {
        ServerMessage::Attachment { attachment }
    }

    /// Builds the handshake acknowledgement.
    pub fn hello(server_id: impl Into<String>) -> Self {
        ServerMessage::Hello {
            version: PROTOCOL_VERSION,
            server_id: server_id.into(),
        }
    }

    /// Builds a terminal handshake failure.
    pub fn hello_error(error: ProtocolError) -> Self {
        ServerMessage::HelloError { error }
    }
}

impl ClientMessage {
    /// Builds a client handshake.
    pub fn hello(version: u32) -> Self {
        ClientMessage::Hello { version }
    }

    /// Builds a request envelope.
    pub fn request(id: impl Into<String>, target: RpcTarget, call: Value) -> Self {
        ClientMessage::Request {
            id: id.into(),
            target,
            call,
        }
    }

    /// Builds a cancellation envelope.
    pub fn cancel(id: impl Into<String>, target: RpcTarget) -> Self {
        ClientMessage::Cancel {
            id: id.into(),
            target,
        }
    }
}

/// Returns whether `version` is the one protocol version this build speaks.
pub fn is_supported_protocol_version(version: u32) -> bool {
    version == PROTOCOL_VERSION
}

/// Validates a canonical lowercase UUIDv4 server identity.
///
/// Mirrors the TypeBox `ServerIdSchema` regex from the upstream package
/// without pulling a regex engine into this crate.
pub fn is_server_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        match index {
            8 | 13 | 18 | 23 => {
                if *byte != b'-' {
                    return false;
                }
            }
            _ => {
                if !byte.is_ascii_digit() && !(b'a'..=b'f').contains(byte) {
                    return false;
                }
            }
        }
    }
    bytes[14] == b'4' && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
}
