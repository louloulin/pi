//! The client error vocabulary — Rust port of `packages/client/src/errors.ts`.
//!
//! Upstream exposes three error classes (`ServerError`, `DisconnectedError`,
//! `ClientDisposedError`) plus two coercion helpers. Rust keeps the same
//! distinctions as one enum so callers can `match` instead of relying on
//! `instanceof`, and the two coercion helpers become constructors:
//!
//! | Upstream | Rust |
//! | --- | --- |
//! | `ServerError(error)` | [`ClientError::Server`] |
//! | `DisconnectedError` | [`ClientError::Disconnected`] |
//! | `ClientDisposedError` | [`ClientError::Disposed`] |
//! | `toError(error)` | — (Rust errors are already typed) |
//! | `toDisconnectedError(error)` | [`to_disconnected`] |

use std::fmt;

use pi_protocol::rpc::ProtocolError;

/// Every failure a [`Client`](crate::Client) can report.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientError {
    /// The client was disposed and no longer accepts operations.
    Disposed,
    /// The caller cancelled an in-flight request.
    Cancelled,
    /// The connection is (or became) unusable.
    ///
    /// The message carries the underlying transport or lifecycle detail; the
    /// client never keeps a boxed source so the error stays `Clone` and cheap
    /// to fan out to every pending request.
    Disconnected {
        /// Human-readable reason.
        message: String,
    },
    /// The server answered with one of its bounded protocol errors.
    Server(ProtocolError),
    /// A wire message violated the protocol contract.
    Protocol(String),
    /// [`ClientOptions`](crate::ClientOptions) were rejected before connecting.
    Configuration(String),
    /// A subscriber callback failed; the failure is reported for diagnostics
    /// only and never affects protocol or transport state.
    Listener(String),
}

impl ClientError {
    /// Builds a [`ClientError::Disconnected`].
    pub fn disconnected(message: impl Into<String>) -> Self {
        ClientError::Disconnected {
            message: message.into(),
        }
    }

    /// Builds a [`ClientError::Protocol`].
    pub fn protocol(message: impl Into<String>) -> Self {
        ClientError::Protocol(message.into())
    }

    /// Builds a [`ClientError::Configuration`].
    pub fn configuration(message: impl Into<String>) -> Self {
        ClientError::Configuration(message.into())
    }

    /// Builds a [`ClientError::Listener`].
    pub fn listener(message: impl Into<String>) -> Self {
        ClientError::Listener(message.into())
    }

    /// Whether this error describes a connection that cannot be used.
    pub fn is_disconnected(&self) -> bool {
        matches!(self, ClientError::Disconnected { .. })
    }

    /// The wire code of a [`ClientError::Server`] failure, if any.
    pub fn server_code(&self) -> Option<&str> {
        match self {
            ClientError::Server(error) => Some(error.code.as_str()),
            _ => None,
        }
    }

    /// Wraps any non-disconnected failure in [`ClientError::Disconnected`].
    ///
    /// This is the Rust form of upstream's `toDisconnectedError`: transport
    /// callbacks reject with whatever the implementation produced, and the
    /// connection publishes that as its terminal state.
    pub fn into_disconnected(self) -> ClientError {
        match self {
            ClientError::Disconnected { .. } => self,
            other => ClientError::disconnected(other.to_string()),
        }
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Disposed => formatter.write_str("Client is disposed"),
            ClientError::Cancelled => formatter.write_str("The operation was aborted"),
            ClientError::Disconnected { message } => formatter.write_str(message),
            ClientError::Server(error) => write!(formatter, "{}: {}", error.code, error.message),
            ClientError::Protocol(message) => formatter.write_str(message),
            ClientError::Configuration(message) => formatter.write_str(message),
            ClientError::Listener(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<ProtocolError> for ClientError {
    fn from(error: ProtocolError) -> Self {
        ClientError::Server(error)
    }
}

impl From<pi_protocol::rpc::ProtocolValidationError> for ClientError {
    fn from(error: pi_protocol::rpc::ProtocolValidationError) -> Self {
        ClientError::Protocol(error.message().to_owned())
    }
}

impl From<pi_protocol::rpc::FrameError> for ClientError {
    fn from(error: pi_protocol::rpc::FrameError) -> Self {
        ClientError::Protocol(error.message().to_owned())
    }
}

impl From<pi_chord::services::ServiceError> for ClientError {
    fn from(error: pi_chord::services::ServiceError) -> Self {
        ClientError::Protocol(error.to_string())
    }
}

/// Coerces any failure into the terminal connection error upstream reports.
pub fn to_disconnected(error: ClientError) -> ClientError {
    error.into_disconnected()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_codes_match_the_wire_vocabulary() {
        let server = ClientError::Server(ProtocolError {
            code: "session_not_found".to_owned(),
            message: "Unknown session: s".to_owned(),
        });
        assert_eq!(server.to_string(), "session_not_found: Unknown session: s");
        assert_eq!(server.server_code(), Some("session_not_found"));

        assert_eq!(
            ClientError::disconnected("Byte transport closed").to_string(),
            "Byte transport closed"
        );
        assert!(ClientError::disconnected("x").is_disconnected());
        assert!(ClientError::Disposed.to_string().contains("disposed"));
    }

    #[test]
    fn protocol_errors_are_wrapped_by_the_coercion() {
        let wrapped = ClientError::protocol("bad frame").into_disconnected();
        assert!(wrapped.is_disconnected());
        assert_eq!(wrapped.to_string(), "bad frame");
        assert_eq!(
            ClientError::disconnected("already").into_disconnected(),
            ClientError::disconnected("already")
        );
    }
}
