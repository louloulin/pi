//! Host, routing and lifecycle errors that may cross the protocol boundary —
//! Rust port of `packages/server/src/errors.ts`.

use std::fmt;

use pi_chord::services::{RemoteServiceError, RemoteServiceErrorCode, ServiceError};
use pi_protocol::rpc::ProtocolError;

/// The message reported for an unexpected internal failure.
pub const INTERNAL_SERVER_ERROR_MESSAGE: &str = "Internal server error";

/// Every error code the server may return to a client.
///
/// This is `RemoteServiceErrorCode | ServerOperationErrorCode` from upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServerErrorCode {
    /// The target address belongs to a different server.
    WrongServer,
    /// No hosted session matches the requested id.
    SessionNotFound,
    /// The session id matched more than one hosted session.
    SessionAmbiguous,
    /// The client has no matching attachment for the addressed session.
    SessionNotAttached,
    /// The server is shutting down.
    ServerDraining,
    /// An unexpected internal failure.
    Internal,
    /// A coded remote-service failure.
    Remote(RemoteServiceErrorCode),
}

impl ServerErrorCode {
    /// The wire spelling of the code.
    pub fn as_str(self) -> &'static str {
        match self {
            ServerErrorCode::WrongServer => "wrong_server",
            ServerErrorCode::SessionNotFound => "session_not_found",
            ServerErrorCode::SessionAmbiguous => "session_ambiguous",
            ServerErrorCode::SessionNotAttached => "session_not_attached",
            ServerErrorCode::ServerDraining => "server_draining",
            ServerErrorCode::Internal => "internal_error",
            ServerErrorCode::Remote(code) => code.as_str(),
        }
    }
}

impl fmt::Display for ServerErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A host or lifecycle failure that can safely be reported to a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerError {
    code: ServerErrorCode,
    message: String,
}

impl ServerError {
    /// Creates an error with an explicit code.
    pub fn new(code: ServerErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// The request was addressed to another server.
    pub fn wrong_server() -> Self {
        Self::new(
            ServerErrorCode::WrongServer,
            "Request was addressed to another server",
        )
    }

    /// No session matched `message` (defaults to upstream's text).
    pub fn session_not_found(message: impl Into<String>) -> Self {
        Self::new(ServerErrorCode::SessionNotFound, message)
    }

    /// The session id matched more than one session.
    pub fn session_ambiguous() -> Self {
        Self::new(
            ServerErrorCode::SessionAmbiguous,
            "Session ID matches more than one session",
        )
    }

    /// The session is not attached to this client.
    pub fn session_not_attached() -> Self {
        Self::new(
            ServerErrorCode::SessionNotAttached,
            "Session is not attached to this client",
        )
    }

    /// The server is draining.
    pub fn server_draining() -> Self {
        Self::new(ServerErrorCode::ServerDraining, "Server is draining")
    }

    /// A coded remote-service failure.
    pub fn remote(code: RemoteServiceErrorCode, message: impl Into<String>) -> Self {
        Self::new(ServerErrorCode::Remote(code), message)
    }

    /// An internal failure with no wire code.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ServerErrorCode::Internal, message)
    }

    /// The error code.
    pub fn code(&self) -> ServerErrorCode {
        self.code
    }

    /// The human-readable message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Converts to the boundary form used by `hello_error` and `response`.
    pub fn to_protocol_error(&self) -> ProtocolError {
        ProtocolError {
            code: self.code.as_str().to_owned(),
            message: self.message.clone(),
        }
    }
}

impl fmt::Display for ServerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ServerError {}

impl From<std::io::Error> for ServerError {
    fn from(error: std::io::Error) -> Self {
        ServerError::internal(error.to_string())
    }
}

impl From<RemoteServiceError> for ServerError {
    fn from(error: RemoteServiceError) -> Self {
        ServerError::remote(error.code(), error.message())
    }
}

impl From<ServiceError> for ServerError {
    fn from(error: ServiceError) -> Self {
        match error.code() {
            Some(code) => ServerError::remote(code, error.to_string()),
            None => match error {
                ServiceError::Message(message) => ServerError::internal(message),
                other => ServerError::internal(other.to_string()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_map_to_wire_spellings() {
        assert_eq!(ServerError::wrong_server().code().as_str(), "wrong_server");
        assert_eq!(
            ServerError::session_not_found("Unknown session: s").message(),
            "Unknown session: s"
        );
        assert_eq!(
            ServerError::remote(RemoteServiceErrorCode::ServiceNotFound, "x")
                .to_protocol_error()
                .code,
            "service_not_found"
        );
    }
}
