//! Remote service error vocabulary — Rust port of `packages/chord/src/services/errors.ts`.
//!
//! Upstream distinguishes a *coded* transport failure ([`RemoteServiceError`], whose `code` is one
//! of the eight `service_*` values) from the plain `Error`/`TypeError` a host throws for
//! programming mistakes (a disposed provider, an invalid implementation). Rust has no untyped
//! throw, so both become one enum, [`ServiceError`], which keeps the code when there is one and the
//! message otherwise.

use std::error::Error;
use std::fmt;

use crate::delta::DeltaError;
use crate::types::FacetError;

/// The eight upstream remote-service error codes, in declaration order.
pub const REMOTE_SERVICE_ERROR_CODES: [&str; 8] = [
    "service_not_allowed",
    "service_not_found",
    "service_mode_mismatch",
    "service_member_not_found",
    "service_member_mismatch",
    "service_instance_not_found",
    "service_stale_instance",
    "service_invalid_value",
];

/// A coded remote-service failure (upstream `RemoteServiceErrorCode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RemoteServiceErrorCode {
    /// The service is process-local or not in the source's allowlist.
    ServiceNotAllowed,
    /// The remote source has no provider for the service.
    ServiceNotFound,
    /// The service was addressed with the wrong mode (singleton vs keyed).
    ServiceModeMismatch,
    /// The addressed member does not exist.
    ServiceMemberNotFound,
    /// The member exists but is not the kind the caller used it as.
    ServiceMemberMismatch,
    /// The keyed service has no live instance for the requested key.
    ServiceInstanceNotFound,
    /// The addressed keyed instance belongs to an older generation.
    ServiceStaleInstance,
    /// An argument or result was not a valid strict-JSON service value.
    ServiceInvalidValue,
}

impl RemoteServiceErrorCode {
    /// The wire/display spelling (`"service_not_allowed"`, ...).
    pub fn as_str(self) -> &'static str {
        REMOTE_SERVICE_ERROR_CODES[self as usize]
    }

    /// Parses a wire spelling, returning `None` for an unknown code.
    pub fn parse(value: &str) -> Option<Self> {
        REMOTE_SERVICE_ERROR_CODES
            .iter()
            .position(|candidate| *candidate == value)
            .map(|index| match index {
                0 => RemoteServiceErrorCode::ServiceNotAllowed,
                1 => RemoteServiceErrorCode::ServiceNotFound,
                2 => RemoteServiceErrorCode::ServiceModeMismatch,
                3 => RemoteServiceErrorCode::ServiceMemberNotFound,
                4 => RemoteServiceErrorCode::ServiceMemberMismatch,
                5 => RemoteServiceErrorCode::ServiceInstanceNotFound,
                6 => RemoteServiceErrorCode::ServiceStaleInstance,
                _ => RemoteServiceErrorCode::ServiceInvalidValue,
            })
    }
}

impl fmt::Display for RemoteServiceErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Returns whether `value` is one of the eight remote-service codes.
pub fn is_remote_service_error_code(value: &str) -> bool {
    RemoteServiceErrorCode::parse(value).is_some()
}

/// A coded remote-service failure (upstream `RemoteServiceError`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteServiceError {
    code: RemoteServiceErrorCode,
    message: String,
}

impl RemoteServiceError {
    /// Creates a coded error.
    pub fn new(code: RemoteServiceErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// The error code.
    pub fn code(&self) -> RemoteServiceErrorCode {
        self.code
    }

    /// The human-readable message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for RemoteServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for RemoteServiceError {}

/// Every failure the service layer can report.
///
/// The variant keeps the upstream distinction: [`ServiceError::Remote`] carries one of the eight
/// wire codes, [`ServiceError::Message`] is the Rust counterpart of a thrown `Error`/`TypeError`,
/// and [`ServiceError::Aggregate`] mirrors `AggregateError` (a teardown that failed several things
/// at once).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceError {
    /// A coded remote-service failure.
    Remote(RemoteServiceError),
    /// A host-level failure with no wire code.
    Message(String),
    /// Several failures collected while publishing, activating or tearing down.
    Aggregate {
        /// The aggregate message (`Failed to dispose services`, ...).
        message: String,
        /// The collected failures, in delivery order.
        causes: Vec<ServiceError>,
    },
}

impl ServiceError {
    /// Creates a plain message error.
    pub fn message(message: impl Into<String>) -> Self {
        ServiceError::Message(message.into())
    }

    /// Creates a coded error.
    pub fn remote(code: RemoteServiceErrorCode, message: impl Into<String>) -> Self {
        ServiceError::Remote(RemoteServiceError::new(code, message))
    }

    /// Creates an aggregated error from `causes`.
    ///
    /// A single cause is returned unwrapped, matching upstream's
    /// `throw errors.length === 1 ? errors[0] : new AggregateError(...)`.
    pub fn aggregate(message: impl Into<String>, mut causes: Vec<ServiceError>) -> Self {
        match causes.len() {
            0 => ServiceError::Message(message.into()),
            1 => causes.pop().expect("length checked"),
            _ => ServiceError::Aggregate {
                message: message.into(),
                causes,
            },
        }
    }

    /// The remote code, when this is a coded failure.
    pub fn code(&self) -> Option<RemoteServiceErrorCode> {
        match self {
            ServiceError::Remote(error) => Some(error.code()),
            _ => None,
        }
    }

    /// Whether this failure carries `code`.
    pub fn is_code(&self, code: RemoteServiceErrorCode) -> bool {
        self.code() == Some(code)
    }

    /// The failures nested in an aggregate, or this error itself for a leaf.
    pub fn causes(&self) -> Vec<&ServiceError> {
        match self {
            ServiceError::Aggregate { causes, .. } => causes.iter().collect(),
            other => vec![other],
        }
    }

    /// The human-readable message without aggregate causes.
    pub fn text(&self) -> &str {
        match self {
            ServiceError::Remote(error) => error.message(),
            ServiceError::Message(message) => message,
            ServiceError::Aggregate { message, .. } => message,
        }
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServiceError::Remote(error) => write!(formatter, "{error}"),
            ServiceError::Message(message) => formatter.write_str(message),
            ServiceError::Aggregate { message, causes } => {
                formatter.write_str(message)?;
                if !causes.is_empty() {
                    formatter.write_str(": ")?;
                    for (index, cause) in causes.iter().enumerate() {
                        if index > 0 {
                            formatter.write_str("; ")?;
                        }
                        write!(formatter, "{cause}")?;
                    }
                }
                Ok(())
            }
        }
    }
}

impl Error for ServiceError {}

impl From<RemoteServiceError> for ServiceError {
    fn from(error: RemoteServiceError) -> Self {
        ServiceError::Remote(error)
    }
}

impl From<RemoteServiceErrorCode> for RemoteServiceError {
    fn from(code: RemoteServiceErrorCode) -> Self {
        RemoteServiceError::new(code, code.as_str())
    }
}

impl From<FacetError> for ServiceError {
    fn from(error: FacetError) -> Self {
        ServiceError::Message(error.to_string())
    }
}

impl From<DeltaError> for ServiceError {
    fn from(error: DeltaError) -> Self {
        ServiceError::Message(error.to_string())
    }
}

impl From<&str> for ServiceError {
    fn from(message: &str) -> Self {
        ServiceError::Message(message.to_owned())
    }
}

impl From<String> for ServiceError {
    fn from(message: String) -> Self {
        ServiceError::Message(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip_through_their_wire_spelling() {
        for code in REMOTE_SERVICE_ERROR_CODES {
            let parsed = RemoteServiceErrorCode::parse(code).expect("known code");
            assert_eq!(parsed.as_str(), code);
        }
        assert!(is_remote_service_error_code("service_stale_instance"));
        assert!(!is_remote_service_error_code("service_unknown"));
    }

    #[test]
    fn an_aggregate_of_one_is_unwrapped() {
        let single = ServiceError::aggregate("Failed", vec![ServiceError::message("boom")]);
        assert_eq!(single, ServiceError::Message("boom".to_owned()));
        let many = ServiceError::aggregate(
            "Failed",
            vec![ServiceError::message("a"), ServiceError::message("b")],
        );
        assert_eq!(many.to_string(), "Failed: a; b");
        assert_eq!(many.causes().len(), 2);
    }

    #[test]
    fn coded_failures_keep_their_code() {
        let error: ServiceError =
            RemoteServiceError::new(RemoteServiceErrorCode::ServiceNotFound, "missing").into();
        assert!(error.is_code(RemoteServiceErrorCode::ServiceNotFound));
        assert_eq!(error.to_string(), "missing");
    }
}
