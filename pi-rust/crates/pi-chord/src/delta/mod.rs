//! Replicated-state deltas — Rust port of `packages/chord/src/delta`.
//!
//! The module is a self-contained, dependency-free value-diff protocol:
//!
//! - [`diff`] compares two JSON values and returns the operations that
//!   transform one into the other.
//! - [`apply`] replays operations over a value; [`apply_immutable`] does the
//!   same while leaving the previous value untouched.
//! - [`Encoder`] / [`Decoder`] translate between the decoded vocabulary and the
//!   wire vocabulary (path interning and arity omission).
//! - [`Tracker`] is the producer-side façade over `diff`.
//!
//! # Sequence numbers are not here
//!
//! The protocol deliberately has no notion of a sequence number, a gap or an
//! expected next batch. A delta is applied or it is not; ordering is the job of
//! the transport that carries it (Stage 18). The one ordering guarantee the
//! module does encode is that a batch starts with a replacement or is relative
//! to the previous one — see [`is_base`].
//!
//! # Paths are data
//!
//! Batches arrive from a facet, a plugin compartment or a tool whose details
//! may echo model output, so the applier treats paths as untrusted:
//! [`RESERVED_SEGMENTS`] rejects prototype-chain keys and array indices are
//! range-checked.
//!
//! # Soundness of the port
//!
//! | Upstream | Port | Note |
//! | --- | --- | --- |
//! | `OverlapError` / `PathError` / `UnsafePathError` | [`DeltaError`] | one enum, message-compatible |
//! | `Proxy`-based tracker | [`Tracker`] + `target_mut` | no access trap in Rust |
//! | `splice` with `undefined` holes | dense arrays only | sparse arrays do not survive JSON |
//! | lone surrogates in strings | [`DeltaError::NotCharAligned`] | `String` cannot hold one |
//!
//! Modules mirror the upstream sections: [`path`] the segment vocabulary,
//! [`op`] the two grammars, [`diff`], [`apply`], [`codec`], [`tracker`].

pub mod apply;
pub mod codec;
pub mod diff;
pub mod op;
pub mod path;
pub mod tracker;

pub use apply::{apply, apply_immutable};
pub use codec::{Decoder, Encoder};
pub use diff::{
    diff, diff_with_scan, json_equal, overlap, overlap_with, DEFAULT_MAX_OVERLAP_SCAN,
};
pub use op::{
    assert_valid_op, assert_valid_wire_op, is_base, is_base_wire, ops_from_json, ops_to_json,
    wire_ops_from_json, wire_ops_to_json, Op, PathRef, WireOp,
};
pub use path::{
    assert_safe_path, is_reserved_segment, path_from_json, path_to_json, NonEmptyPath, Path, Seg,
    RESERVED_SEGMENTS,
};
pub use tracker::Tracker;

/// Every way a delta can be malformed or inapplicable.
///
/// The variants mirror the three upstream error classes so a caller can decide
/// whether the batch is corrupt (`InvalidOp`, `UnsafePath`) or merely
/// out of order (`Path`).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DeltaError {
    /// A path segment reaches the prototype chain or is not a valid index.
    #[error("unsafe path segment: {0}")]
    UnsafePath(String),
    /// An operation refers to a path that does not exist in the target.
    #[error("unresolvable path: {0}")]
    Path(String),
    /// An operation is not a valid tuple in its vocabulary.
    #[error("{0}")]
    InvalidOp(String),
    /// A string operation landed inside a surrogate pair.
    #[error("{0}")]
    NotCharAligned(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_encoded_batch_round_trips_through_the_decoder() {
        let mut tracker = Tracker::new(json!({ "a": { "b": "xy" }, "xs": [1] }));
        let base = tracker.flush().unwrap();
        tracker.target_mut()["a"]["b"] = json!("xyz");
        tracker.target_mut()["xs"]
            .as_array_mut()
            .expect("array")
            .push(json!(2));
        let next = tracker.flush().unwrap();

        let mut encoder = Encoder::new();
        let mut decoder = Decoder::new();
        let mut replica = None;
        for batch in [base, next] {
            let wire = encoder.encode(&batch);
            let decoded = decoder.decode(&wire).unwrap();
            assert_eq!(decoded, batch);
            replica = Some(apply(replica, &decoded).unwrap());
        }
        assert_eq!(replica.unwrap(), json!({ "a": { "b": "xyz" }, "xs": [1, 2] }));
    }

    #[test]
    fn error_messages_keep_the_upstream_wording() {
        assert_eq!(
            DeltaError::UnsafePath("__proto__".into()).to_string(),
            "unsafe path segment: __proto__"
        );
        assert_eq!(
            DeltaError::Path("[\"a\"]".into()).to_string(),
            "unresolvable path: [\"a\"]"
        );
    }
}
