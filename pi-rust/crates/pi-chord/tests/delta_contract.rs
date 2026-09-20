//! Delta contract tests: the properties the replicated-state layer depends on.
//!
//! Upstream `packages/chord/test/delta.test.ts` covers the same ground. These tests are written
//! against the *public* API only, so they also pin the crate's exported surface.

use pi_chord::delta::{
    apply, apply_immutable, diff, is_base, ops_from_json, ops_to_json, wire_ops_to_json, Decoder,
    Encoder, NonEmptyPath, Op, Seg, Tracker,
};
use pi_chord::json::JsonValue;
use serde_json::json;

fn path(keys: &[&str]) -> NonEmptyPath {
    NonEmptyPath::from_keys(keys)
}

/// A path mixing object keys and array indices: `["list", 1]` is
/// `Seg::Key("list"), Seg::Index(1)`, while `path(["list", "1"])` would be an
/// object lookup and therefore an unsafe array segment.
fn indexed(keys: &[&str], index: u64) -> NonEmptyPath {
    let mut parts: Vec<Seg> = keys.iter().map(|key| Seg::key(*key)).collect();
    parts.push(Seg::index(index));
    NonEmptyPath::try_new(parts).expect("non-empty path")
}

#[test]
fn a_batch_applies_in_order() {
    // Every verb in one batch, each op observing the result of the previous one.
    let ops = vec![
        Op::Replace(json!({ "list": [1, 2, 3], "text": "hello", "drop": true })),
        Op::Set(indexed(&["list"], 1), json!(99)),
        Op::Append(path(&["text"]), " world".to_owned()),
        // `t` removes the first N UTF-16 units, i.e. `String.prototype.slice(n)`.
        Op::Truncate(path(&["text"]), 5),
        // `p` replaces `remove` items at `index` with `items`.
        Op::Patch {
            path: vec![Seg::key("list")],
            index: 0,
            remove: 1,
            items: vec![json!("a"), json!("b")],
        },
        Op::Delete(path(&["drop"])),
    ];

    let value = apply(None, &ops).expect("the batch applies");
    assert_eq!(
        value,
        json!({ "list": ["a", "b", 99, 3], "text": " world" })
    );
}

#[test]
fn apply_is_pure_and_deterministic() {
    let before = json!({ "a": { "b": [1, 2, 3] } });
    let after = json!({ "a": { "b": [1, 4] }, "c": true });

    let first = apply_immutable(Some(before.clone()), &diff(&before, &after)).expect("applies");
    let second = apply_immutable(Some(before.clone()), &diff(&before, &after)).expect("applies");

    assert_eq!(first, after);
    assert_eq!(second, after);
    // The input value is untouched: `apply_immutable` clones rather than adopts.
    assert_eq!(before, json!({ "a": { "b": [1, 2, 3] } }));
}

#[test]
fn a_diff_round_trips_and_is_idempotent() {
    let cases = [
        (json!({ "a": 1 }), json!({ "a": 1 })),
        (json!({ "a": 1 }), json!({ "a": 2, "b": [1, 2, 3] })),
        (json!(""), json!("a long enough string to need overlap")),
        (json!([1, 2, 3]), json!([1, 2, 3, 4, 5])),
        (json!([1, 2, 3, 4]), json!([1, 4])),
        (json!({ "a": { "b": 1 } }), json!({ "a": { "b": [1] } })),
    ];
    for (before, after) in cases {
        let ops = diff(&before, &after);
        assert_eq!(
            apply_immutable(Some(before.clone()), &ops).expect("applies"),
            after,
            "diff({before}) -> {ops:?}"
        );
        // Diffing a value against itself produces nothing.
        assert!(diff(&after, &after).is_empty());
    }
}

#[test]
fn a_tracker_rebases_flushes_and_discards() {
    let mut tracker = Tracker::new(json!({ "count": 0 }));
    let base = tracker.flush().expect("base batch");
    assert!(is_base(&base));
    assert_eq!(tracker.flush().expect("clean"), Vec::new());

    // A second flush after mutation emits a relative batch, and the new value is
    // published once it has been flushed.
    *tracker.target_mut() = json!({ "count": 1 });
    let update = tracker.flush().expect("relative batch");
    assert!(!is_base(&update));
    assert_eq!(base.len(), 1);

    // Rebase accepts the current value without emitting: the next flush is empty
    // and is *not* a base batch, which is what a replica that already received
    // the same value needs.
    *tracker.target_mut() = json!({ "count": 2 });
    tracker.rebase();
    assert!(!tracker.dirty());
    assert_eq!(tracker.flush().expect("clean"), Vec::new());

    // Discard rolls the pending mutation back to the published value.
    *tracker.target_mut() = json!({ "count": 3 });
    tracker.discard();
    assert_eq!(tracker.target(), &json!({ "count": 2 }));

    // Sync adopts a value as both current and published, so no base batch is owed.
    tracker.sync(json!({ "count": 9 }));
    assert!(!tracker.dirty());
    assert_eq!(tracker.flush().expect("clean"), Vec::new());
    assert_eq!(tracker.target(), &json!({ "count": 9 }));
}

#[test]
fn replace_root_forces_a_new_base_batch() {
    let mut tracker = Tracker::new(json!({ "a": 1 }));
    tracker.flush().expect("base batch");
    *tracker.target_mut() = json!({ "a": 2 });
    tracker.flush().expect("update");
    tracker.replace_root(json!({ "b": true }));
    let batch = tracker.flush().expect("replacement");
    assert!(is_base(&batch));
    assert_eq!(
        apply_immutable(None, &batch).expect("applies"),
        json!({ "b": true })
    );
}

#[test]
fn wire_round_trip_preserves_operations() {
    let ops = vec![
        Op::Replace(json!({ "deep": { "path": 1 }, "other": 0 })),
        Op::Set(path(&["deep", "path"]), json!(2)),
        Op::Set(path(&["other"]), json!(1)),
        Op::Set(path(&["deep", "path"]), json!(3)),
        Op::Append(path(&["deep", "path"]), "x".to_owned()),
    ];

    // The JSON grammar round-trips.
    let json_ops = ops_to_json(&ops);
    assert_eq!(json_ops[0][0], json!("r"));
    assert_eq!(ops_from_json(&json_ops).expect("decodes"), ops);

    // The streaming codec agrees with the batch codec. A path is spelled out on
    // first use, interned with `#` on second use, and dropped entirely when it
    // repeats the previous operation's path.
    let mut encoder = Encoder::new();
    let mut decoder = Decoder::new();
    let wire = encoder.encode(&ops);
    let wire_json = wire_ops_to_json(&wire);
    assert_eq!(wire_json[1].as_array().map(Vec::len), Some(3));
    assert_eq!(wire_json[3][0], json!("#"));
    assert_eq!(wire_json[4][0], json!("s"));
    assert_eq!(wire_json[4][1], json!(0));
    assert_eq!(wire_json[5].as_array().map(Vec::len), Some(2));
    let decoded = decoder.decode(&wire).expect("decodes");
    assert_eq!(decoded, ops);
}

#[test]
fn repeated_operations_are_ordered_not_deduplicated() {
    // `set` is naturally idempotent for the value...
    let twice = vec![
        Op::Replace(json!({ "a": 1 })),
        Op::Set(path(&["a"]), json!(2)),
        Op::Set(path(&["a"]), json!(2)),
    ];
    assert_eq!(apply(None, &twice).expect("applies"), json!({ "a": 2 }));

    // ...but `append` is not: a duplicated append is visible, because the delta
    // layer applies a batch in order and never collapses operations.
    let appends = vec![
        Op::Replace(json!({ "text": "a" })),
        Op::Append(path(&["text"]), "b".to_owned()),
        Op::Append(path(&["text"]), "b".to_owned()),
    ];
    assert_eq!(
        apply(None, &appends).expect("applies"),
        json!({ "text": "abb" })
    );
}

#[test]
fn conflicting_paths_are_rejected() {
    // A non-empty-path op must resolve a container, not a leaf.
    let ops = vec![
        Op::Replace(json!({ "leaf": 1 })),
        Op::Set(path(&["leaf", "nested"]), json!(true)),
    ];
    let error = apply(None, &ops).expect_err("a scalar cannot be indexed");
    assert!(
        matches!(error, pi_chord::delta::DeltaError::Path(_)),
        "unexpected error: {error}"
    );

    // Arrays only accept integer segments, in range.
    let ops = vec![
        Op::Replace(json!({ "list": [1, 2] })),
        Op::Set(
            NonEmptyPath::try_new(vec![Seg::key("list"), Seg::key("nope")]).expect("non-empty"),
            json!(1),
        ),
    ];
    assert!(matches!(
        apply(None, &ops),
        Err(pi_chord::delta::DeltaError::UnsafePath(_))
    ));

    let ops = vec![
        Op::Replace(json!({ "list": [1, 2] })),
        Op::Set(
            NonEmptyPath::try_new(vec![Seg::key("list"), Seg::index(9)]).expect("non-empty"),
            json!(1),
        ),
    ];
    assert!(matches!(
        apply(None, &ops),
        Err(pi_chord::delta::DeltaError::UnsafePath(_))
    ));

    // The prototype chain is off limits.
    let reserved: JsonValue = json!([["s", ["__proto__"], 1]]);
    let error = ops_from_json(&reserved).expect_err("reserved segments are refused");
    assert!(error.to_string().contains("unsafe path segment"));
}

#[test]
fn interning_keeps_similar_paths_apart() {
    // `["a:b"]` and `["a", "b"]` must not share a dictionary entry just because
    // their text renders the same way.
    let ops = vec![
        Op::Replace(json!({ "a:b": [1, 2], "a": { "b": 3 } })),
        Op::Set(indexed(&["a:b"], 0), json!(9)),
        Op::Set(path(&["a", "b"]), json!(10)),
        Op::Delete(indexed(&["a:b"], 0)),
    ];
    let mut encoder = Encoder::new();
    let mut decoder = Decoder::new();
    let decoded = decoder.decode(&encoder.encode(&ops)).expect("decodes");
    assert_eq!(decoded, ops);
    assert_eq!(
        apply(None, &decoded).expect("applies"),
        json!({ "a:b": [2], "a": { "b": 10 } })
    );
}

#[test]
fn an_empty_batch_cannot_establish_a_root() {
    let value = json!({ "a": 1 });
    assert_eq!(
        apply_immutable(Some(value.clone()), &[]).expect("applies"),
        value
    );
    let error = apply(None, &[]).expect_err("no root was set");
    assert_eq!(error.to_string(), "unresolvable path: []");
}
