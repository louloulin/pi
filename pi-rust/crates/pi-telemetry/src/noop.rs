//! The default no-op telemetry context.

use std::sync::{Arc, OnceLock};

use futures::future::BoxFuture;

use crate::context::{SpanCallback, SpanRef, TelemetryContext, TelemetrySpan};
use crate::types::{SpanAttributes, SpanOptions, SpanStatus};

/// Inert telemetry context used when an application does not configure one.
///
/// It admits every callback and propagates its result without inspecting or
/// retaining any telemetry payload: no span/event attribute, no status and no
/// error is ever stored or exported. This is the default so that telemetry is
/// zero-overhead unless a host explicitly installs an adapter.
///
/// The type is a zero-sized value; [`NOOP_TELEMETRY_CONTEXT`] can be reused
/// anywhere a `&dyn TelemetryContext` is expected.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopTelemetry;

/// Shared no-op telemetry context.
///
/// Mirrors `NOOP_TELEMETRY_CONTEXT` from `packages/telemetry/src/noop.ts`.
pub const NOOP_TELEMETRY_CONTEXT: NoopTelemetry = NoopTelemetry;

impl TelemetryContext for NoopTelemetry {
    fn start_span<'a>(
        &'a self,
        _options: SpanOptions,
        callback: SpanCallback<'a>,
    ) -> BoxFuture<'a, ()> {
        admit_noop(callback)
    }
}

fn admit_noop(callback: SpanCallback<'_>) -> BoxFuture<'_, ()> {
    // Invoke the callback before the returned future starts polling so the
    // inert span is available synchronously, matching the upstream contract.
    let inner = callback(noop_span());
    Box::pin(async move {
        // The operation's result already travelled through the callback; the
        // no-op adapter intentionally drops it.
        let _ = inner.await;
    })
}

/// Reuse one shared inert span, as `noop.ts` does.
fn noop_span() -> SpanRef {
    static NOOP_SPAN: OnceLock<SpanRef> = OnceLock::new();
    NOOP_SPAN
        .get_or_init(|| Arc::new(NoopSpan) as SpanRef)
        .clone()
}

#[derive(Debug)]
struct NoopSpan;

impl TelemetryContext for NoopSpan {
    fn start_span<'a>(
        &'a self,
        _options: SpanOptions,
        callback: SpanCallback<'a>,
    ) -> BoxFuture<'a, ()> {
        admit_noop(callback)
    }
}

impl TelemetrySpan for NoopSpan {
    fn add_event(&self, _name: &str, _attributes: SpanAttributes) {}

    fn set_attributes(&self, _attributes: SpanAttributes) {}

    fn set_status(&self, _status: SpanStatus) {}
}
