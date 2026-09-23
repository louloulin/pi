//! The callback-based telemetry contract.
//!
//! Mirrors `TelemetryContext` / `TelemetrySpan` in
//! `packages/telemetry/src/index.ts`. A span is opened around a callback; the
//! callback receives the new span and is also the explicit parent context for
//! child spans. There is no public `end()` — the span settles when the
//! callback's future settles.

use std::future::Future;
use std::sync::Arc;

use futures::future::BoxFuture;

use crate::sync::lock;
use crate::types::{IntoTelemetryError, SpanAttributes, SpanOptions, SpanResult, SpanStatus};

/// A cheap, cloneable handle to the currently active span.
///
/// Pass it to lower-level work to create explicit nesting. Cloning the handle
/// is the Rust counterpart of passing the JavaScript `TelemetrySpan` down the
/// call stack.
pub type SpanRef = Arc<dyn TelemetrySpan>;

/// A boxed callback invoked exactly once with the span that was just started.
pub type SpanCallback<'a> = Box<dyn FnOnce(SpanRef) -> BoxFuture<'a, SpanResult> + Send + 'a>;

/// Starts callback-managed child spans.
pub trait TelemetryContext: Send + Sync {
    /// Start a span, invoke `callback` with the new span, and resolve when the
    /// callback's future settles.
    ///
    /// Implementations must:
    ///
    /// - invoke `callback` exactly once;
    /// - keep the span open until the returned future resolves;
    /// - treat `Ok(())` as a successful span and `Err` as a failed span unless
    ///   the callback set an explicit status;
    /// - let recording methods be passive and non-failing, ignoring calls made
    ///   after settlement.
    fn start_span<'a>(
        &'a self,
        options: SpanOptions,
        callback: SpanCallback<'a>,
    ) -> BoxFuture<'a, ()>;
}

/// Records attributes, events and status on the active span.
///
/// A span is also the explicit parent context for child spans, matching the
/// upstream `TelemetrySpan extends TelemetryContext` relationship.
pub trait TelemetrySpan: TelemetryContext {
    /// Record a named occurrence during the span.
    fn add_event(&self, name: &str, attributes: SpanAttributes);

    /// Merge attributes into the span. Later defined values win and insertion
    /// order is preserved.
    fn set_attributes(&self, attributes: SpanAttributes);

    /// Set the final status. Repeated calls are last-write-wins.
    fn set_status(&self, status: SpanStatus);
}

/// Typed convenience layer over [`TelemetryContext::start_span`].
///
/// The raw contract erases the callback's return value so that adapters stay
/// object-safe. Instrumented code usually wants the value back, so this
/// extension wraps the callback, keeps its `Result`, and hands the error to
/// the adapter as the automatic span status.
pub trait TelemetryContextExt: TelemetryContext {
    /// Start a span around an async operation and return its result.
    ///
    /// On `Err`, the span is recorded as failed with the error's name and
    /// message unless the callback set an explicit status.
    fn start_span_with<'a, T, E, F, Fut>(
        &'a self,
        options: SpanOptions,
        callback: F,
    ) -> BoxFuture<'a, Result<T, E>>
    where
        T: Send + 'a,
        E: IntoTelemetryError + Send + 'a,
        F: FnOnce(SpanRef) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, E>> + Send + 'a;
}

impl<C: TelemetryContext + ?Sized> TelemetryContextExt for C {
    fn start_span_with<'a, T, E, F, Fut>(
        &'a self,
        options: SpanOptions,
        callback: F,
    ) -> BoxFuture<'a, Result<T, E>>
    where
        T: Send + 'a,
        E: IntoTelemetryError + Send + 'a,
        F: FnOnce(SpanRef) -> Fut + Send + 'a,
        Fut: Future<Output = Result<T, E>> + Send + 'a,
    {
        let slot: Arc<std::sync::Mutex<Option<Result<T, E>>>> =
            Arc::new(std::sync::Mutex::new(None));
        let callback_slot = slot.clone();
        let inner: BoxFuture<'a, ()> = self.start_span(
            options,
            Box::new(move |span| {
                Box::pin(async move {
                    match callback(span).await {
                        Ok(value) => {
                            *lock(&callback_slot) = Some(Ok(value));
                            Ok(())
                        }
                        Err(error) => {
                            let telemetry_error = error.telemetry_error();
                            *lock(&callback_slot) = Some(Err(error));
                            Err(telemetry_error)
                        }
                    }
                })
            }),
        );

        Box::pin(async move {
            inner.await;
            lock(&slot)
                .take()
                .expect("telemetry adapter must invoke its span callback exactly once")
        })
    }
}
