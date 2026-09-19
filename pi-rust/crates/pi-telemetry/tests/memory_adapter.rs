//! Behaviour of the in-memory reference adapter.

use futures::executor::block_on;
use pi_telemetry::{
    AttributeValue, MemoryTelemetry, SpanAttributes, SpanError, SpanOptions, SpanStatus,
    TelemetryContext, TelemetryContextExt,
};

fn attrs<const N: usize>(items: [(&str, AttributeValue); N]) -> SpanAttributes {
    items
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

#[test]
fn starts_empty() {
    let telemetry = MemoryTelemetry::new();
    assert!(telemetry.is_empty());
    assert_eq!(telemetry.len(), 0);
    assert!(telemetry.spans().is_empty());
    assert_eq!(MemoryTelemetry::default().len(), 0);
}

#[test]
fn snapshots_are_detached_from_later_recording() {
    let telemetry = MemoryTelemetry::new();
    block_on(
        telemetry.start_span_with(SpanOptions::new("first"), |span| async move {
            span.set_attributes(attrs([("key", AttributeValue::from("value"))]));
            Ok::<(), SpanError>(())
        }),
    )
    .expect("the operation must succeed");

    let snapshot = telemetry.spans();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].id, 1);
    assert_eq!(snapshot[0].attributes.len(), 1);
    assert!(snapshot[0].settled);
    assert_eq!(snapshot[0].end_sequence, Some(1));

    block_on(
        telemetry.start_span_with(SpanOptions::new("second"), |_| async move {
            Ok::<(), SpanError>(())
        }),
    )
    .expect("the operation must succeed");

    // Recording more telemetry must not mutate an earlier snapshot.
    assert_eq!(snapshot.len(), 1);
    assert_eq!(telemetry.len(), 2);
    assert_eq!(telemetry.spans()[1].id, 2);
}

#[test]
fn erased_span_api_records_status_without_a_return_value() {
    let telemetry = MemoryTelemetry::new();
    block_on(telemetry.start_span(
        SpanOptions::new("pi.memory.raw"),
        Box::new(|span| {
            Box::pin(async move {
                span.add_event("attempted", SpanAttributes::new());
                span.set_status(SpanStatus::error("RawError", "raw"));
                Err(SpanError::new("RawError", "raw"))
            })
        }),
    ));

    let spans = telemetry.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].status, SpanStatus::error("RawError", "raw"));
    assert_eq!(spans[0].events.len(), 1);
    assert!(spans[0].settled);
}

#[test]
fn matching_error_impls_are_available() {
    fn describe(error: &impl pi_telemetry::IntoTelemetryError) -> SpanError {
        error.telemetry_error()
    }

    assert_eq!(
        describe(&SpanError::new("Named", "message")),
        SpanError::new("Named", "message")
    );
    assert_eq!(describe(&"raw").name, "Error");
    assert_eq!(describe(&"raw".to_string()).message, "raw");
    assert_eq!(describe(&std::io::Error::other("io")).name, "IoError");
}

#[test]
fn telemetry_adapter_never_changes_the_operation_outcome() {
    let telemetry = MemoryTelemetry::new();
    let value = block_on(
        telemetry.start_span_with(SpanOptions::new("pi.memory.value"), |_span| async move {
            Ok::<u32, SpanError>(7)
        }),
    )
    .expect("the value must be preserved");
    assert_eq!(value, 7);
}
