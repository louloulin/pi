//! Behaviour of the default no-op adapter.
//!
//! The upstream acceptance criterion is "the default (noop) path records zero
//! events". `NoopTelemetry` has no recording surface at all, so instead of
//! querying a counter on it, the tests prove the property two ways: the adapter
//! is structurally stateless, and the *same* instrumented operation that emits
//! three events into a counting probe emits nothing when the no-op context is
//! installed.

use std::mem::size_of;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use futures::future::BoxFuture;
use pi_telemetry::{
    NoopTelemetry, SpanAttributes, SpanCallback, SpanError, SpanOptions, SpanRef, SpanStatus,
    TelemetryContext, TelemetryContextExt, TelemetrySpan, NOOP_TELEMETRY_CONTEXT,
};

// --- a probe adapter that counts every recording call it receives ----------

#[derive(Default)]
struct ProbeCounters {
    children: AtomicUsize,
    events: AtomicUsize,
    attributes: AtomicUsize,
    statuses: AtomicUsize,
}

#[derive(Clone)]
struct ProbeTelemetry {
    counters: Arc<ProbeCounters>,
}

#[derive(Clone)]
struct ProbeSpan {
    counters: Arc<ProbeCounters>,
}

impl TelemetryContext for ProbeTelemetry {
    fn start_span<'a>(
        &'a self,
        _options: SpanOptions,
        callback: SpanCallback<'a>,
    ) -> BoxFuture<'a, ()> {
        self.counters.children.fetch_add(1, Ordering::SeqCst);
        let span: SpanRef = Arc::new(ProbeSpan {
            counters: self.counters.clone(),
        });
        let inner = callback(span);
        Box::pin(async move {
            let _ = inner.await;
        })
    }
}

impl TelemetryContext for ProbeSpan {
    fn start_span<'a>(
        &'a self,
        _options: SpanOptions,
        callback: SpanCallback<'a>,
    ) -> BoxFuture<'a, ()> {
        self.counters.children.fetch_add(1, Ordering::SeqCst);
        let span: SpanRef = Arc::new(ProbeSpan {
            counters: self.counters.clone(),
        });
        let inner = callback(span);
        Box::pin(async move {
            let _ = inner.await;
        })
    }
}

impl TelemetrySpan for ProbeSpan {
    fn add_event(&self, _name: &str, _attributes: SpanAttributes) {
        self.counters.events.fetch_add(1, Ordering::SeqCst);
    }

    fn set_attributes(&self, _attributes: SpanAttributes) {
        self.counters.attributes.fetch_add(1, Ordering::SeqCst);
    }

    fn set_status(&self, _status: SpanStatus) {
        self.counters.statuses.fetch_add(1, Ordering::SeqCst);
    }
}

/// The instrumented operation used by both runs below.
async fn emits_three_events(span: SpanRef) -> Result<(), SpanError> {
    span.add_event("one", SpanAttributes::new());
    span.add_event("two", SpanAttributes::new());
    span.add_event("three", SpanAttributes::new());
    span.set_attributes(SpanAttributes::new());
    span.set_status(SpanStatus::Ok);
    Ok(())
}

#[test]
fn noop_adapter_records_zero_events() {
    // 1. Through the no-op context: perfectly passive, nothing retained.
    block_on(
        NOOP_TELEMETRY_CONTEXT.start_span_with(SpanOptions::new("pi.noop.op"), emits_three_events),
    )
    .expect("the no-op context must propagate success");

    // 2. Through the counting probe: the operation really does emit, so the
    //    zero above comes from the no-op adapter discarding the payload rather
    //    than from the operation staying silent.
    let probe = ProbeTelemetry {
        counters: Arc::new(ProbeCounters::default()),
    };
    block_on(probe.start_span_with(SpanOptions::new("pi.noop.op"), emits_three_events))
        .expect("the probe context must propagate success");
    assert_eq!(probe.counters.children.load(Ordering::SeqCst), 1);
    assert_eq!(probe.counters.events.load(Ordering::SeqCst), 3);
    assert_eq!(probe.counters.attributes.load(Ordering::SeqCst), 1);
    assert_eq!(probe.counters.statuses.load(Ordering::SeqCst), 1);

    // The no-op adapter itself holds no per-instance state: zero-sized, so
    // there is no counter, buffer or map that could retain an event.
    assert_eq!(size_of::<NoopTelemetry>(), 0);
}

#[test]
fn noop_adapter_propagates_failures() {
    let error = block_on(
        NOOP_TELEMETRY_CONTEXT
            .start_span_with(SpanOptions::new("pi.noop.failure"), |_span| async move {
                Err::<(), _>(SpanError::new("Boom", "boom"))
            }),
    )
    .expect_err("the failure must be propagated unchanged");
    assert_eq!(error, SpanError::new("Boom", "boom"));
}

#[test]
fn noop_adapter_reuses_one_inert_span() {
    let seen: Arc<Mutex<Vec<SpanRef>>> = Arc::new(Mutex::new(Vec::new()));
    let outer_seen = seen.clone();
    let inner_seen = seen.clone();

    block_on(
        NOOP_TELEMETRY_CONTEXT.start_span_with(SpanOptions::new("outer"), move |outer| {
            async move {
                lock(&outer_seen).push(outer.clone());
                // Nested spans must not allocate a new handle.
                outer
                    .start_span(
                        SpanOptions::new("inner"),
                        Box::new(move |inner| {
                            lock(&inner_seen).push(inner);
                            Box::pin(async move { Ok::<(), SpanError>(()) })
                        }),
                    )
                    .await;
                Ok::<(), SpanError>(())
            }
        }),
    )
    .expect("the no-op context must propagate success");

    let seen = lock(&seen);
    assert_eq!(seen.len(), 2, "both callbacks must receive a span");
    assert!(
        Arc::ptr_eq(&seen[0], &seen[1]),
        "the no-op adapter must reuse a single inert span"
    );
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
