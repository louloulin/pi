//! Core value types shared by every telemetry adapter.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// A primitive value that can be attached to a span or to an event.
///
/// Mirrors `AttributeValue` in `packages/telemetry/src/index.ts`: scalars and
/// flat arrays of scalars only. Prompts, completions, tool arguments or
/// output, file contents, provider payloads, headers and credentials must not
/// be recorded unless a caller's schema and data policy explicitly allow it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttributeValue {
    /// A UTF-8 string.
    String(String),
    /// A floating point number.
    Number(f64),
    /// A boolean.
    Boolean(bool),
    /// A list of strings.
    Strings(Vec<String>),
    /// A list of numbers.
    Numbers(Vec<f64>),
    /// A list of booleans.
    Booleans(Vec<bool>),
}

impl From<String> for AttributeValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for AttributeValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<bool> for AttributeValue {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}

impl From<f64> for AttributeValue {
    fn from(value: f64) -> Self {
        Self::Number(value)
    }
}

impl From<i64> for AttributeValue {
    fn from(value: i64) -> Self {
        Self::Number(value as f64)
    }
}

impl From<u64> for AttributeValue {
    fn from(value: u64) -> Self {
        Self::Number(value as f64)
    }
}

impl From<i32> for AttributeValue {
    fn from(value: i32) -> Self {
        Self::Number(f64::from(value))
    }
}

impl From<u32> for AttributeValue {
    fn from(value: u32) -> Self {
        Self::Number(f64::from(value))
    }
}

impl From<usize> for AttributeValue {
    fn from(value: usize) -> Self {
        Self::Number(value as f64)
    }
}

impl From<Vec<String>> for AttributeValue {
    fn from(value: Vec<String>) -> Self {
        Self::Strings(value)
    }
}

impl From<Vec<f64>> for AttributeValue {
    fn from(value: Vec<f64>) -> Self {
        Self::Numbers(value)
    }
}

impl From<Vec<i64>> for AttributeValue {
    fn from(value: Vec<i64>) -> Self {
        Self::Numbers(value.into_iter().map(|item| item as f64).collect())
    }
}

impl From<Vec<bool>> for AttributeValue {
    fn from(value: Vec<bool>) -> Self {
        Self::Booleans(value)
    }
}

/// An open, ordered bag of span or event attributes.
///
/// Insertion order is preserved (like a JavaScript object) so recorded
/// snapshots and JSON output stay stable and readable. Later writes to the
/// same key replace the earlier value in place.
pub type SpanAttributes = IndexMap<String, AttributeValue>;

/// Options passed when a span is started.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpanOptions {
    /// Dotted span name, e.g. `pi.ai.request`.
    pub name: String,
    /// Attributes known when the span starts.
    pub attributes: SpanAttributes,
}

impl SpanOptions {
    /// Create options with a name and no start attributes.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attributes: SpanAttributes::new(),
        }
    }

    /// Attach a start attribute.
    pub fn with_attribute(
        mut self,
        name: impl Into<String>,
        value: impl Into<AttributeValue>,
    ) -> Self {
        self.attributes.insert(name.into(), value.into());
        self
    }

    /// Attach a start attribute when the value is known.
    ///
    /// `None` mirrors the JavaScript `undefined` case and records nothing.
    pub fn with_optional_attribute(
        mut self,
        name: impl Into<String>,
        value: Option<impl Into<AttributeValue>>,
    ) -> Self {
        if let Some(value) = value {
            self.attributes.insert(name.into(), value.into());
        }
        self
    }

    /// The span name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// A structured error name / message pair recorded on a failed span.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{name}: {message}")]
pub struct SpanError {
    /// Error type name, e.g. `ProviderError`.
    pub name: String,
    /// Human-readable message.
    pub message: String,
}

impl SpanError {
    /// Build an error record.
    pub fn new(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            message: message.into(),
        }
    }
}

/// Final outcome of a span.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SpanStatus {
    /// The operation completed normally.
    #[default]
    Ok,
    /// The operation failed. Details are optional because an adapter may only
    /// know that a failure happened.
    Error(Option<SpanError>),
}

impl SpanStatus {
    /// An error status carrying a name and message.
    pub fn error(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Error(Some(SpanError::new(name, message)))
    }

    /// An error status without details.
    pub fn error_without_details() -> Self {
        Self::Error(None)
    }

    /// True when the span completed normally.
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }

    /// True when the span is recorded as failed.
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }
}

/// Result of an instrumented operation.
///
/// Returning `Err` marks the span as failed unless the callback already set an
/// explicit status. This is the Rust counterpart of "throws / rejects" in the
/// upstream callback contract.
pub type SpanResult = Result<(), SpanError>;

/// Extracts the name and message recorded on a span when an operation fails.
///
/// Implemented for [`SpanError`] itself, `String`, `&str`, `std::io::Error`
/// and `Box<dyn std::error::Error + Send + Sync>`. Domain crates implement it
/// for their own error enums.
pub trait IntoTelemetryError {
    /// Describe this error for the recorded span status.
    fn telemetry_error(&self) -> SpanError;
}

impl IntoTelemetryError for SpanError {
    fn telemetry_error(&self) -> SpanError {
        self.clone()
    }
}

impl IntoTelemetryError for String {
    fn telemetry_error(&self) -> SpanError {
        SpanError::new("Error", self.clone())
    }
}

impl IntoTelemetryError for &str {
    fn telemetry_error(&self) -> SpanError {
        SpanError::new("Error", (*self).to_owned())
    }
}

impl IntoTelemetryError for std::io::Error {
    fn telemetry_error(&self) -> SpanError {
        SpanError::new("IoError", self.to_string())
    }
}

impl IntoTelemetryError for Box<dyn std::error::Error + Send + Sync> {
    fn telemetry_error(&self) -> SpanError {
        SpanError::new("Error", self.to_string())
    }
}
