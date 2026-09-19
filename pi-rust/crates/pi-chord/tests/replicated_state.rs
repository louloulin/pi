//! Replicated-state contract tests: hydration, in-order updates, and the failure modes a
//! transport-fed replica has to detect (out-of-order, duplicated or missing batches).

use pi_chord::context::{background_context, create_context_key, with_context_value};
use pi_chord::delta::{apply_immutable, is_base, ops_to_json, Op};
use pi_chord::state::{replicated_state, StateReplica};
use pi_chord::types::DeliveryKind;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Counter {
    count: u64,
    label: String,
}

impl Counter {
    fn new(count: u64, label: &str) -> Self {
        Self {
            count,
            label: label.to_owned(),
        }
    }
}

#[test]
fn a_producer_publishes_only_when_something_changed() {
    let state = replicated_state(Counter::new(0, "a")).expect("json-representable");
    assert_eq!(state.sequence(), 0);
    assert!(is_base(&state.last_batch()));

    let deliveries = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = Arc::clone(&deliveries);
    let _subscription = state
        .subscribe(move |value: &Counter, _context, delivery| {
            seen.lock()
                .expect("lock")
                .push((delivery.kind, delivery.sequence, value.count));
        })
        .expect("subscribes");

    // An unchanged publish is a no-op: no sequence bump, no listener call.
    state.publish(background_context()).expect("publish");
    assert_eq!(state.sequence(), 0);

    state.state_mut().count = 1;
    state.publish(background_context()).expect("publish");
    assert_eq!(state.sequence(), 1);

    state.state_mut().label = "b".to_owned();
    state.publish(background_context()).expect("publish");
    assert_eq!(state.sequence(), 2);

    let seen = deliveries.lock().expect("lock");
    assert_eq!(
        *seen,
        vec![
            (DeliveryKind::Hydrate, 0, 0),
            (DeliveryKind::Update, 1, 1),
            (DeliveryKind::Update, 2, 1),
        ]
    );
}

#[test]
fn source_listeners_receive_the_batches_in_order() {
    let state = replicated_state(Counter::new(0, "a")).expect("json-representable");
    let batches = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = Arc::clone(&batches);
    let subscription = state.subscribe_source(move |ops, sequence, context| {
        captured
            .lock()
            .expect("lock")
            .push((sequence, ops_to_json(ops), context.label().to_owned()));
    });

    state.state_mut().count = 1;
    state.publish(background_context()).expect("publish");
    state.state_mut().count = 2;
    state.publish(background_context()).expect("publish");

    let batches = batches.lock().expect("lock");
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].0, 1);
    assert_eq!(batches[1].0, 2);

    // Unsubscribing stops delivery.
    drop(subscription);
    state.state_mut().count = 3;
    state.publish(background_context()).expect("publish");
    assert_eq!(batches.len(), 2);
}

#[test]
fn a_cold_replica_hydrates_then_applies_consecutive_updates() {
    let state = replicated_state(Counter::new(1, "a")).expect("json-representable");
    state.state_mut().count = 2;
    state.publish(background_context()).expect("publish");
    state.state_mut().count = 3;
    state.publish(background_context()).expect("publish");

    let replica = StateReplica::<Counter>::new();
    assert_eq!(replica.value(), None);
    assert_eq!(replica.sequence(), None);

    let received = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = Arc::clone(&received);
    let _subscription = replica.subscribe(move |value: &Counter, _context, delivery| {
        captured
            .lock()
            .expect("lock")
            .push((delivery.kind, delivery.sequence, value.count));
    });

    // The snapshot that a transport would deliver first.
    replica
        .hydrate(0, &[Op::Replace(to_json(&Counter::new(1, "a")))], background_context())
        .expect("hydrates");
    assert_eq!(replica.value(), Some(Counter::new(1, "a")));

    replica
        .update(1, &[Op::Set(nested("count"), to_json(&2))], background_context())
        .expect("applies");
    replica
        .update(2, &[Op::Set(nested("count"), to_json(&3))], background_context())
        .expect("applies");
    assert_eq!(replica.value(), Some(Counter::new(3, "a")));

    let received = received.lock().expect("lock");
    assert_eq!(
        *received,
        vec![
            (DeliveryKind::Hydrate, 0, 1),
            (DeliveryKind::Update, 1, 2),
            (DeliveryKind::Update, 2, 3),
        ]
    );
}

#[test]
fn an_update_before_hydration_is_refused() {
    let replica = StateReplica::<Counter>::new();
    let error = replica
        .update(1, &[Op::Replace(to_json(&Counter::new(1, "a")))], background_context())
        .expect_err("refused");
    assert_eq!(
        error.to_string(),
        "Replicated state received an update before hydration"
    );
    assert_eq!(replica.value(), None);
}

#[test]
fn a_non_base_snapshot_is_refused() {
    let replica = StateReplica::<Counter>::new();
    let error = replica
        .hydrate(0, &[Op::Set(nested("count"), to_json(&1))], background_context())
        .expect_err("refused");
    assert_eq!(
        error.to_string(),
        "Replicated state snapshot is not a base operation batch"
    );
}

#[test]
fn duplicate_and_out_of_order_updates_clear_the_replica() {
    let replica = StateReplica::<Counter>::new();
    replica
        .hydrate(0, &[Op::Replace(to_json(&Counter::new(1, "a")))], background_context())
        .expect("hydrates");
    replica
        .update(1, &[Op::Set(nested("count"), to_json(&2))], background_context())
        .expect("applies");

    // A duplicate of the batch that was already applied.
    let error = replica
        .update(1, &[Op::Set(nested("count"), to_json(&9))], background_context())
        .expect_err("refused");
    assert_eq!(error.to_string(), "Replicated state update sequence has a gap");
    assert_eq!(replica.value(), None, "the replica is cleared");
    assert_eq!(replica.sequence(), None);

    // Out of order after a gap: the replica is unusable until a new snapshot.
    replica
        .hydrate(0, &[Op::Replace(to_json(&Counter::new(1, "a")))], background_context())
        .expect("hydrates again");
    let error = replica
        .update(5, &[Op::Set(nested("count"), to_json(&2))], background_context())
        .expect_err("refused");
    assert_eq!(error.to_string(), "Replicated state update sequence has a gap");
    assert_eq!(replica.value(), None);
}

#[test]
fn a_snapshot_batch_reproduces_the_published_value() {
    let state = replicated_state(Counter::new(7, "seven")).expect("json-representable");
    let snapshot = state.last_batch();
    let json = apply_immutable(None, &snapshot).expect("applies");
    let counter: Counter = serde_json::from_value(json).expect("valid");
    assert_eq!(counter, Counter::new(7, "seven"));
}

#[test]
fn context_values_reach_the_listener() {
    let state = replicated_state(Counter::new(0, "a")).expect("json-representable");
    let key = create_context_key::<&'static str>("requester");
    let context = with_context_value(key, "hello", background_context());

    let seen = Arc::new(std::sync::Mutex::new(None));
    let captured = Arc::clone(&seen);
    let _subscription = state
        .subscribe(move |_value: &Counter, context, _delivery| {
            *captured.lock().expect("lock") = Some(context.value(&key).copied());
        })
        .expect("subscribes");

    state.state_mut().count = 1;
    state.publish(&context).expect("publish");
    assert_eq!(*seen.lock().expect("lock"), Some(Some("hello")));
}

fn nested(key: &str) -> pi_chord::delta::NonEmptyPath {
    pi_chord::delta::NonEmptyPath::from_keys(&[key])
}

fn to_json<T: Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).expect("serialisable")
}
