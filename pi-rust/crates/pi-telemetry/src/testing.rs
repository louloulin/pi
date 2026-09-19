//! Runner-independent conformance suite for telemetry adapters.
//!
//! Mirrors `packages/telemetry/src/testing/`: adapter authors run these cases
//! against their own `TelemetryContext` implementation instead of relying on a
//! specific exporter (OpenTelemetry, Langfuse, ...). The suite is
//! runtime-agnostic — each case returns a future that the host awaits with any
//! executor, so it works under `futures`, `tokio` and wasm hosts alike.
//!
//! Ported groups: `callback lifecycle`, `status`, `recording`, `parentage`.
//! Three upstream cases are intentionally not ported because they exercise
//! JavaScript-only throw/Proxy semantics (`ignores failed attribute calls
//! atomically`, `ignores failed status calls atomically`, `suppresses
//! unreadable telemetry payload failures`): in Rust `TelemetrySpan` recording
//! methods cannot throw and attribute payloads cannot be unreadable, so there
//! is nothing to observe.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use futures::future::BoxFuture;

use crate::context::{SpanRef, TelemetryContext, TelemetryContextExt};
use crate::memory::RecordedTelemetrySpan;
use crate::types::{AttributeValue, SpanAttributes, SpanError, SpanOptions, SpanStatus};

/// A fresh adapter instance plus a snapshot reader, for one conformance case.
#[derive(Clone)]
pub struct TelemetryAdapterFixture {
    /// The adapter under test.
    pub context: Arc<dyn TelemetryContext>,
    get_spans: Arc<dyn Fn() -> Vec<RecordedTelemetrySpan> + Send + Sync>,
}

impl TelemetryAdapterFixture {
    /// Build a fixture from an adapter and a snapshot reader.
    pub fn new(
        context: Arc<dyn TelemetryContext>,
        get_spans: Arc<dyn Fn() -> Vec<RecordedTelemetrySpan> + Send + Sync>,
    ) -> Self {
        Self { context, get_spans }
    }

    /// Read the adapter's currently recorded spans.
    pub fn spans(&self) -> Vec<RecordedTelemetrySpan> {
        (self.get_spans)()
    }
}

/// Creates a fresh fixture for each case so cases cannot leak state into one
/// another.
pub type TelemetryAdapterFixtureFactory = Arc<dyn Fn() -> TelemetryAdapterFixture + Send + Sync>;

/// A single, runner-independent conformance case.
pub struct TelemetryAdapterConformanceCase {
    group: &'static str,
    name: &'static str,
    run: Box<dyn Fn() -> BoxFuture<'static, Result<(), String>> + Send + Sync>,
}

impl TelemetryAdapterConformanceCase {
    /// The upstream group this case belongs to.
    pub fn group(&self) -> &'static str {
        self.group
    }

    /// The upstream case name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Run the case against a fresh fixture. `Err` describes the assertion
    /// that failed.
    pub fn run(&self) -> BoxFuture<'static, Result<(), String>> {
        (self.run)()
    }
}

/// Build the conformance suite for an adapter.
pub fn create_telemetry_adapter_conformance(
    factory: TelemetryAdapterFixtureFactory,
) -> Vec<TelemetryAdapterConformanceCase> {
    let mut cases = Vec::new();
    let mut add = |group, name, test: fn(TelemetryAdapterFixture) -> _| {
        let factory = factory.clone();
        cases.push(TelemetryAdapterConformanceCase {
            group,
            name,
            run: Box::new(move || test(factory())),
        });
    };
    add(
        "callback lifecycle",
        "invokes the callback once and preserves the result",
        callback_lifecycle_success,
    );
    add(
        "callback lifecycle",
        "preserves failure values and marks the span as failed",
        callback_lifecycle_failure,
    );
    add(
        "status",
        "uses the last explicit status without automatic overwrite",
        explicit_status_wins,
    );
    add(
        "recording",
        "merges attributes and records ordered events",
        attributes_and_events,
    );
    add(
        "recording",
        "makes calls after settlement inert",
        post_settlement_is_inert,
    );
    add(
        "parentage",
        "records nested and concurrent child relationships",
        nested_and_concurrent_children,
    );
    cases
}

fn find_span(spans: &[RecordedTelemetrySpan], name: &str) -> Result<RecordedTelemetrySpan, String> {
    spans
        .iter()
        .find(|span| span.name == name)
        .cloned()
        .ok_or_else(|| {
            let recorded: Vec<&str> = spans.iter().map(|span| span.name.as_str()).collect();
            format!("expected a recorded span named {name}, found {recorded:?}")
        })
}

fn attrs<const N: usize>(items: [(&str, AttributeValue); N]) -> SpanAttributes {
    items
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

fn callback_lifecycle_success(
    fixture: TelemetryAdapterFixture,
) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = calls.clone();
        let result = fixture
            .context
            .start_span_with(
                SpanOptions::new("pi.conformance.success")
                    .with_attribute("attempt", 1i64)
                    .with_optional_attribute("absent", None::<String>),
                move |span| {
                    let callback_calls = callback_calls.clone();
                    async move {
                        callback_calls.fetch_add(1, Ordering::SeqCst);
                        span.set_attributes(attrs([("answer", AttributeValue::from(42.0))]));
                        Ok::<&'static str, SpanError>("done")
                    }
                },
            )
            .await;
        match &result {
            Ok("done") => {}
            other => return Err(format!("expected Ok(\"done\"), got {other:?}")),
        }
        if calls.load(Ordering::SeqCst) != 1 {
            return Err(format!(
                "expected the callback to run exactly once, ran {} times",
                calls.load(Ordering::SeqCst)
            ));
        }
        let spans = fixture.spans();
        let span = find_span(&spans, "pi.conformance.success")?;
        if span.status != SpanStatus::Ok {
            return Err(format!("expected ok status, got {:?}", span.status));
        }
        if !span.settled {
            return Err("expected the span to be settled".to_owned());
        }
        if span.attributes.get("attempt") != Some(&AttributeValue::from(1.0)) {
            return Err(format!("start attribute lost: {:?}", span.attributes));
        }
        if span.attributes.get("answer") != Some(&AttributeValue::from(42.0)) {
            return Err(format!("post-start attribute lost: {:?}", span.attributes));
        }
        if span.attributes.contains_key("absent") {
            return Err("an absent optional attribute was recorded".to_owned());
        }
        Ok(())
    })
}

fn callback_lifecycle_failure(
    fixture: TelemetryAdapterFixture,
) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let expected = SpanError::new("ConformanceError", "expected failure");
        let failure = expected.clone();
        let result: Result<(), SpanError> = fixture
            .context
            .start_span_with(
                SpanOptions::new("pi.conformance.failure"),
                move |_| async move { Err(failure) },
            )
            .await;
        match &result {
            Err(error) if *error == expected => {}
            other => return Err(format!("expected the original failure, got {other:?}")),
        }
        let spans = fixture.spans();
        let span = find_span(&spans, "pi.conformance.failure")?;
        if span.status != SpanStatus::Error(Some(expected)) {
            return Err(format!(
                "expected the automatic error status, got {:?}",
                span.status
            ));
        }
        if !span.settled {
            return Err("expected the span to be settled".to_owned());
        }
        Ok(())
    })
}

fn explicit_status_wins(
    fixture: TelemetryAdapterFixture,
) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let expected = SpanError::new("DomainError", "handled");
        let reported = expected.clone();
        let result: Result<(), SpanError> = fixture
            .context
            .start_span_with(
                SpanOptions::new("pi.conformance.explicit-status"),
                move |span| async move {
                    span.set_status(SpanStatus::error("FirstError", "discarded"));
                    span.set_status(SpanStatus::Error(Some(reported)));
                    Err(SpanError::new("IgnoredError", "ignored"))
                },
            )
            .await;
        match &result {
            Err(error) if error.name == "IgnoredError" => {}
            other => {
                return Err(format!(
                    "expected the original failure to be preserved, got {other:?}"
                ))
            }
        }
        let spans = fixture.spans();
        let span = find_span(&spans, "pi.conformance.explicit-status")?;
        if span.status != SpanStatus::Error(Some(expected)) {
            return Err(format!(
                "expected the last explicit status, got {:?}",
                span.status
            ));
        }
        Ok(())
    })
}

fn attributes_and_events(
    fixture: TelemetryAdapterFixture,
) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let result: Result<(), SpanError> = fixture
            .context
            .start_span_with(
                SpanOptions::new("pi.conformance.recording").with_attribute("start", true),
                |span| async move {
                    span.set_attributes(attrs([
                        ("a", AttributeValue::from("one")),
                        ("b", AttributeValue::from("two")),
                    ]));
                    span.set_attributes(attrs([
                        ("b", AttributeValue::from("two-updated")),
                        ("c", AttributeValue::from("three")),
                    ]));
                    span.add_event("first", attrs([("step", AttributeValue::from(1i64))]));
                    span.add_event("second", SpanAttributes::new());
                    Ok(())
                },
            )
            .await;
        if result.is_err() {
            return Err("expected the instrumented operation to succeed".to_owned());
        }
        let spans = fixture.spans();
        let span = find_span(&spans, "pi.conformance.recording")?;
        let expected_attributes = attrs([
            ("start", AttributeValue::from(true)),
            ("a", AttributeValue::from("one")),
            ("b", AttributeValue::from("two-updated")),
            ("c", AttributeValue::from("three")),
        ]);
        if span.attributes != expected_attributes {
            return Err(format!(
                "expected merged attributes in insertion order, got {:?}",
                span.attributes
            ));
        }
        let event_names: Vec<&str> = span
            .events
            .iter()
            .map(|event| event.name.as_str())
            .collect();
        if event_names != ["first", "second"] {
            return Err(format!("expected ordered events, got {event_names:?}"));
        }
        if span.events[0].attributes != attrs([("step", AttributeValue::from(1i64))]) {
            return Err(format!(
                "event attributes lost: {:?}",
                span.events[0].attributes
            ));
        }
        Ok(())
    })
}

fn post_settlement_is_inert(
    fixture: TelemetryAdapterFixture,
) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let (span_tx, span_rx) = futures::channel::oneshot::channel::<SpanRef>();
        let result: Result<(), SpanError> = fixture
            .context
            .start_span_with(
                SpanOptions::new("pi.conformance.settled"),
                move |span| async move {
                    let _ = span_tx.send(span);
                    Ok(())
                },
            )
            .await;
        if result.is_err() {
            return Err("expected the instrumented operation to succeed".to_owned());
        }
        let span = span_rx
            .await
            .map_err(|error| format!("span handle was not delivered: {error}"))?;

        // Every recording call after settlement must be dropped.
        span.set_attributes(attrs([("late", AttributeValue::from("value"))]));
        span.add_event("late-event", SpanAttributes::new());
        span.set_status(SpanStatus::error("LateError", "late"));

        // A child started from a settled span is admitted but not recorded.
        let child_ran = Arc::new(AtomicBool::new(false));
        let child_flag = child_ran.clone();
        span.start_span(
            SpanOptions::new("pi.conformance.late-child"),
            Box::new(move |_| {
                child_flag.store(true, Ordering::SeqCst);
                Box::pin(async move { Ok::<(), SpanError>(()) })
            }),
        )
        .await;
        if !child_ran.load(Ordering::SeqCst) {
            return Err("a child of a settled span must still run its callback".to_owned());
        }

        let spans = fixture.spans();
        if spans.len() != 1 {
            return Err(format!(
                "expected exactly one recorded span, got {}",
                spans.len()
            ));
        }
        let span = &spans[0];
        if span.status != SpanStatus::Ok {
            return Err(format!("late status write was recorded: {:?}", span.status));
        }
        if !span.events.is_empty() {
            return Err(format!("late event was recorded: {:?}", span.events));
        }
        if span.attributes.contains_key("late") {
            return Err(format!(
                "late attribute was recorded: {:?}",
                span.attributes
            ));
        }
        Ok(())
    })
}

fn nested_and_concurrent_children(
    fixture: TelemetryAdapterFixture,
) -> BoxFuture<'static, Result<(), String>> {
    Box::pin(async move {
        let (release_tx, release_rx) = futures::channel::oneshot::channel::<()>();
        let result: Result<(), SpanError> = fixture
            .context
            .start_span_with(
                SpanOptions::new("pi.conformance.parent"),
                move |parent| async move {
                    let first_parent = parent.clone();
                    let first = first_parent.start_span_with(
                        SpanOptions::new("pi.conformance.first"),
                        move |_| async move {
                            let _ = release_rx.await;
                            Ok(())
                        },
                    );
                    let second_parent = parent.clone();
                    let second = second_parent.start_span_with(
                        SpanOptions::new("pi.conformance.second"),
                        |_| async move { Ok(()) },
                    );
                    // The second child settles while the first is still open.
                    second.await?;
                    let _ = release_tx.send(());
                    first.await?;
                    Ok(())
                },
            )
            .await;
        if result.is_err() {
            return Err("expected the parent operation to succeed".to_owned());
        }

        let spans = fixture.spans();
        let parent = find_span(&spans, "pi.conformance.parent")?;
        let first = find_span(&spans, "pi.conformance.first")?;
        let second = find_span(&spans, "pi.conformance.second")?;
        if parent.parent_id.is_some() {
            return Err("a root span must not have a parent".to_owned());
        }
        if first.parent_id != Some(parent.id) || second.parent_id != Some(parent.id) {
            return Err(format!(
                "expected both children to point at parent {}, got {:?} and {:?}",
                parent.id, first.parent_id, second.parent_id
            ));
        }
        let parent_end = parent
            .end_sequence
            .ok_or_else(|| "parent span was not settled".to_owned())?;
        let first_end = first
            .end_sequence
            .ok_or_else(|| "first child was not settled".to_owned())?;
        let second_end = second
            .end_sequence
            .ok_or_else(|| "second child was not settled".to_owned())?;
        if !(second_end < first_end && first_end < parent_end) {
            return Err(format!(
                "expected settlement order second({second_end}) < first({first_end}) < parent({parent_end})"
            ));
        }
        Ok(())
    })
}
