//! Contract-level tests for the core value types.

use futures::executor::block_on;
use pi_telemetry::{
    AttributeValue, IntoTelemetryError, SpanAttributes, SpanError, SpanOptions, SpanStatus,
    TelemetryContext, NOOP_TELEMETRY_CONTEXT,
};

#[test]
fn attribute_values_convert_from_native_types() {
    assert_eq!(
        AttributeValue::from("text"),
        AttributeValue::String("text".to_owned())
    );
    assert_eq!(
        AttributeValue::from("text".to_owned()),
        AttributeValue::String("text".to_owned())
    );
    assert_eq!(AttributeValue::from(true), AttributeValue::Boolean(true));
    assert_eq!(AttributeValue::from(1.5f64), AttributeValue::Number(1.5));
    assert_eq!(AttributeValue::from(3i64), AttributeValue::Number(3.0));
    assert_eq!(AttributeValue::from(3u64), AttributeValue::Number(3.0));
    assert_eq!(AttributeValue::from(3i32), AttributeValue::Number(3.0));
    assert_eq!(AttributeValue::from(3u32), AttributeValue::Number(3.0));
    assert_eq!(AttributeValue::from(3usize), AttributeValue::Number(3.0));
    assert_eq!(
        AttributeValue::from(vec!["a".to_owned()]),
        AttributeValue::Strings(vec!["a".to_owned()])
    );
    assert_eq!(
        AttributeValue::from(vec![1.5f64]),
        AttributeValue::Numbers(vec![1.5])
    );
    assert_eq!(
        AttributeValue::from(vec![1i64, 2]),
        AttributeValue::Numbers(vec![1.0, 2.0])
    );
    assert_eq!(
        AttributeValue::from(vec![true]),
        AttributeValue::Booleans(vec![true])
    );
}

#[test]
fn span_options_preserve_insertion_order_and_skip_absent_values() {
    let options = SpanOptions::new("pi.test.span")
        .with_attribute("first", 1i64)
        .with_optional_attribute("skipped", None::<&str>)
        .with_optional_attribute("second", Some("value"));
    assert_eq!(options.name(), "pi.test.span");
    let keys: Vec<&str> = options.attributes.keys().map(String::as_str).collect();
    assert_eq!(keys, ["first", "second"]);
}

#[test]
fn attributes_are_ordered_and_last_write_wins() {
    let mut attributes = SpanAttributes::new();
    attributes.insert("a".to_owned(), AttributeValue::from("one"));
    attributes.insert("b".to_owned(), AttributeValue::from("two"));
    attributes.insert("a".to_owned(), AttributeValue::from("one-updated"));

    let keys: Vec<&str> = attributes.keys().map(String::as_str).collect();
    assert_eq!(keys, ["a", "b"]);
    assert_eq!(attributes["a"], AttributeValue::from("one-updated"));
}

#[test]
fn span_status_helpers_agree_with_their_variants() {
    assert!(SpanStatus::Ok.is_ok());
    assert!(!SpanStatus::Ok.is_error());
    assert!(SpanStatus::error_without_details().is_error());
    assert_eq!(SpanStatus::error_without_details(), SpanStatus::Error(None));
    assert_eq!(
        SpanStatus::error("Code", "message"),
        SpanStatus::Error(Some(SpanError::new("Code", "message")))
    );
    assert_eq!(SpanStatus::default(), SpanStatus::Ok);
}

#[test]
fn error_conversion_reports_names_and_messages() {
    let boxed: Box<dyn std::error::Error + Send + Sync> = "broken".into();
    assert_eq!(boxed.telemetry_error(), SpanError::new("Error", "broken"));
    assert_eq!((5u8).to_string().telemetry_error().name, "Error");
}

#[test]
fn raw_span_callback_receives_the_span_and_settles_once() {
    // The erased API is what adapters implement against; make sure a callback
    // that ignores the default status still settles the span exactly once.
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = calls.clone();
    block_on(NOOP_TELEMETRY_CONTEXT.start_span(
        SpanOptions::new("pi.test.raw"),
        Box::new(move |span| {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            span.set_status(SpanStatus::Ok);
            Box::pin(async move { Ok(()) })
        }),
    ));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}
