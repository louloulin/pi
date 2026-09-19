//! Serializable telemetry schema data.
//!
//! Mirrors the schema types in `packages/telemetry/src/index.ts`. In TypeScript
//! the schema is a compile-time contract; in Rust it is plain data with serde
//! derives so a host can load, ship and snapshot it (for example from a JSON
//! file) without depending on a validation runtime.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// The supported attribute value types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TelemetryAttributeType {
    /// A scalar string.
    #[serde(rename = "string")]
    String,
    /// A scalar number.
    #[serde(rename = "number")]
    Number,
    /// A scalar boolean.
    #[serde(rename = "boolean")]
    Boolean,
    /// A list of strings.
    #[serde(rename = "string[]")]
    StringArray,
    /// A list of numbers.
    #[serde(rename = "number[]")]
    NumberArray,
    /// A list of booleans.
    #[serde(rename = "boolean[]")]
    BooleanArray,
}

/// Expected cardinality bucket for an attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TelemetryAttributeCardinality {
    /// Few distinct values; safe to use as a dimension.
    Low,
    /// Many distinct values.
    High,
}

/// Enumerated or example values for an attribute.
///
/// The variant must match the attribute's [`TelemetryAttributeType`]; the
/// mapping is documented rather than enforced because the schema is data.
///
/// Deserialization is untagged, so an empty JSON array decodes as
/// [`TelemetryAttributeValues::Strings`]: prefer a non-empty list when the
/// element type matters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TelemetryAttributeValues {
    /// Values for `string` attributes.
    Strings(Vec<String>),
    /// Values for `number` attributes.
    Numbers(Vec<f64>),
    /// Values for `boolean` attributes.
    Booleans(Vec<bool>),
    /// Values for `string[]` attributes.
    StringArrays(Vec<Vec<String>>),
    /// Values for `number[]` attributes.
    NumberArrays(Vec<Vec<f64>>),
    /// Values for `boolean[]` attributes.
    BooleanArrays(Vec<Vec<bool>>),
}

/// A declared attribute: description plus optional cardinality, sensitive flag
/// and value metadata.
///
/// `required` is only meaningful for start attributes and event attributes; it
/// is serialized as `required` next to `type`, matching the upstream
/// intersection types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetryAttributeDefinition {
    /// What the attribute means.
    pub description: String,
    /// Value type of the attribute.
    #[serde(rename = "type")]
    pub value_type: TelemetryAttributeType,
    /// Whether the attribute is required when the span starts / event fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// Whether the value is sensitive and subject to a data policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitive: Option<bool>,
    /// Expected cardinality bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cardinality: Option<TelemetryAttributeCardinality>,
    /// Enumerated values, for low-cardinality attributes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<TelemetryAttributeValues>,
    /// Allowed element values, for array attributes.
    #[serde(
        default,
        rename = "elementValues",
        skip_serializing_if = "Option::is_none"
    )]
    pub element_values: Option<TelemetryAttributeValues>,
    /// Example values, for free-form attributes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub examples: Option<TelemetryAttributeValues>,
}

impl TelemetryAttributeDefinition {
    /// An attribute recorded on the span's start event.
    pub fn start(description: impl Into<String>, value_type: TelemetryAttributeType) -> Self {
        Self::new(description, value_type, true)
    }

    /// An attribute recorded on an event inside the span.
    pub fn event(description: impl Into<String>, value_type: TelemetryAttributeType) -> Self {
        Self::new(description, value_type, true)
    }

    /// An optional attribute captured when the span ends.
    pub fn end(description: impl Into<String>, value_type: TelemetryAttributeType) -> Self {
        Self::new(description, value_type, false)
    }

    fn new(
        description: impl Into<String>,
        value_type: TelemetryAttributeType,
        carries_required: bool,
    ) -> Self {
        Self {
            description: description.into(),
            value_type,
            required: carries_required.then_some(false),
            sensitive: None,
            cardinality: None,
            values: None,
            element_values: None,
            examples: None,
        }
    }

    /// Mark the attribute required.
    pub fn required(mut self, required: bool) -> Self {
        self.required = Some(required);
        self
    }

    /// Mark the attribute sensitive.
    pub fn sensitive(mut self, sensitive: bool) -> Self {
        self.sensitive = Some(sensitive);
        self
    }

    /// Declare the expected cardinality.
    pub fn cardinality(mut self, cardinality: TelemetryAttributeCardinality) -> Self {
        self.cardinality = Some(cardinality);
        self
    }

    /// Declare enumerated values.
    pub fn values(mut self, values: TelemetryAttributeValues) -> Self {
        self.values = Some(values);
        self
    }

    /// Declare allowed element values for an array attribute.
    pub fn element_values(mut self, element_values: TelemetryAttributeValues) -> Self {
        self.element_values = Some(element_values);
        self
    }

    /// Declare example values.
    pub fn examples(mut self, examples: TelemetryAttributeValues) -> Self {
        self.examples = Some(examples);
        self
    }
}

/// Which spans may be the parent of a span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TelemetryParentDefinition {
    /// Any span.
    Any,
    /// Only a root span or a span created outside pi.
    RootOrExternal,
    /// Only the named spans.
    Spans {
        /// Allowed parent span names.
        spans: Vec<String>,
    },
}

/// A declared event inside a span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetryEventDefinition {
    /// What the event means.
    pub description: String,
    /// Attributes recorded with the event.
    #[serde(default)]
    pub attributes: IndexMap<String, TelemetryAttributeDefinition>,
}

/// The default status of a span before any error is observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TelemetryStatusDefault {
    /// The span completes normally unless an error is observed.
    Ok,
}

/// How a span's final status is derived.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetryStatusRule {
    /// Status used when no error is observed.
    pub default: TelemetryStatusDefault,
    /// Human-readable description of when the span becomes an error.
    #[serde(rename = "errorWhen")]
    pub error_when: String,
}

/// A declared span: description, allowed parents, attributes, events, status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySpanDefinition {
    /// What the span covers.
    pub description: String,
    /// Which spans may parent it.
    pub parents: TelemetryParentDefinition,
    /// Attributes known when the span starts.
    pub start_attributes: IndexMap<String, TelemetryAttributeDefinition>,
    /// Attributes captured when the span ends.
    pub end_attributes: IndexMap<String, TelemetryAttributeDefinition>,
    /// Events the span may emit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<IndexMap<String, TelemetryEventDefinition>>,
    /// How the final status is derived.
    pub status: TelemetryStatusRule,
}

/// A versioned schema: the set of spans a subsystem may emit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetrySchemaDefinition {
    /// Schema version, bumped on breaking changes.
    pub version: u32,
    /// Span definitions keyed by dotted span name.
    pub spans: IndexMap<String, TelemetrySpanDefinition>,
}

/// Identity helper for typed schema literals.
///
/// Mirrors `defineTelemetrySchema` from upstream: it exists so schema
/// definitions can be written and inferred in one place. The value is returned
/// unchanged — validation stays with the exporter, and the schema is plain
/// serializable data.
pub fn define_telemetry_schema(schema: TelemetrySchemaDefinition) -> TelemetrySchemaDefinition {
    schema
}
