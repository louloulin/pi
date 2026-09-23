//! Vendor-neutral telemetry contracts for pi.
//!
//! Rust port of `packages/telemetry`. The package gives subsystems a single
//! callback-based contract (`TelemetryContext` / `TelemetrySpan`) plus a
//! default no-op adapter, an in-memory reference adapter and a conformance
//! suite for adapter authors. Hosts supply the exporter (OpenTelemetry,
//! Langfuse, ...) themselves, so pi core never depends on a vendor SDK.
//!
//! # The contract
//!
//! A span is opened around a callback. The callback receives the new span and
//! is also the explicit parent context for child spans; there is no public
//! `end()` — the span settles when the callback's future settles.
//!
//! ```
//! use pi_telemetry::{NoopTelemetry, SpanOptions, SpanError, TelemetryContextExt};
//! use futures::executor::block_on;
//!
//! let context = NoopTelemetry;
//! let result = block_on(context.start_span_with(
//!     SpanOptions::new("pi.example.operation").with_attribute("attempt", 1i64),
//!     |span| async move {
//!         span.add_event("pi.example.event", Default::default());
//!         Ok::<u32, SpanError>(7)
//!     },
//! ));
//! assert_eq!(result.unwrap(), 7);
//! ```
//!
//! # Adapters
//!
//! - [`NoopTelemetry`] / [`NOOP_TELEMETRY_CONTEXT`] — the default; stores
//!   nothing and forwards no payload to any vendor exporter.
//! - [`MemoryTelemetry`] — records spans in process memory for tests and
//!   local debugging, with detached snapshots via `spans()`.
//!
//! Recording is passive by design: [`TelemetrySpan`] methods cannot fail and
//! calls made after a span settles are ignored, so telemetry can never change
//! the outcome of the instrumented operation.
//!
//! # WASM
//!
//! The crate is runtime-agnostic: it uses no ambient context, no clock, no
//! filesystem and no thread runtime, so it compiles unchanged for
//! `wasm32-unknown-unknown` and needs no `wasm` feature gate.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod context;
mod memory;
mod noop;
mod schema;
mod sync;
mod types;

pub mod testing;

pub use context::{SpanCallback, SpanRef, TelemetryContext, TelemetryContextExt, TelemetrySpan};
pub use memory::{MemoryTelemetry, RecordedTelemetryEvent, RecordedTelemetrySpan};
pub use noop::{NoopTelemetry, NOOP_TELEMETRY_CONTEXT};
pub use schema::{
    define_telemetry_schema, TelemetryAttributeCardinality, TelemetryAttributeDefinition,
    TelemetryAttributeType, TelemetryAttributeValues, TelemetryEventDefinition,
    TelemetryParentDefinition, TelemetrySchemaDefinition, TelemetrySpanDefinition,
    TelemetryStatusDefault, TelemetryStatusRule,
};
pub use types::{
    AttributeValue, IntoTelemetryError, SpanAttributes, SpanError, SpanOptions, SpanResult,
    SpanStatus,
};
