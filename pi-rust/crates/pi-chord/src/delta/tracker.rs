//! The producer-side change tracker.
//!
//! Upstream `createTracker` returns a `Target` whose every nested node is a
//! `Proxy`, so a mutation records exactly which paths became dirty and `flush`
//! walks only those. Rust has no property-access trap, so the port exposes the
//! value mutably ([`Tracker::target_mut`]) and reconstructs the batch by
//! diffing the published baseline against the current value.
//!
//! The difference is cost, not semantics: a flush over a fully "dirty" tree
//! emits the same operations the incremental walk would emit, because the walk
//! was only ever a way to reach the same comparison. What the port cannot
//! reproduce is the producer's *intent* where a mutation has no unique
//! diff — `xs.push(1); xs.shift()` on `[1]` is indistinguishable from no change
//! at all, and upstream records the appended value while the diff records
//! nothing. Both replicas still converge on the current value.

use serde_json::Value;

use super::diff::DEFAULT_MAX_OVERLAP_SCAN;
use super::{diff_with_scan, DeltaError, Op};

/// Tracks a value and produces the operations that transform the last flushed
/// state into the current one.
#[derive(Clone, Debug)]
pub struct Tracker {
    baseline: Value,
    target: Value,
    force_base: bool,
    dirty: bool,
    scan: usize,
}

impl Tracker {
    /// Create a tracker over `root`. The first [`Tracker::flush`] is a base
    /// batch.
    pub fn new(root: Value) -> Self {
        Self::with_scan(root, DEFAULT_MAX_OVERLAP_SCAN)
    }

    /// [`Tracker::new`] with an explicit string-overlap scan bound.
    pub fn with_scan(root: Value, scan: usize) -> Self {
        Tracker {
            baseline: root.clone(),
            target: root,
            force_base: true,
            dirty: true,
            scan,
        }
    }

    /// The current value.
    pub fn target(&self) -> &Value {
        &self.target
    }

    /// The current value, for mutation. Marks the tracker dirty.
    pub fn target_mut(&mut self) -> &mut Value {
        self.dirty = true;
        &mut self.target
    }

    /// Whether a flush would emit anything.
    pub fn dirty(&self) -> bool {
        self.force_base || self.dirty
    }

    /// Replace the whole value. The next flush is a base batch.
    pub fn replace_root(&mut self, next: Value) {
        self.target = next;
        self.force_base = true;
        self.dirty = true;
    }

    /// Take the pending batch.
    ///
    /// The first call after construction (or after
    /// [`Tracker::replace_root`]) is always a single replacement, because a
    /// replica that has never seen this state needs the whole value before it
    /// can apply anything relative to it.
    pub fn flush(&mut self) -> Result<Vec<Op>, DeltaError> {
        if self.force_base {
            let value = self.target.clone();
            self.baseline = self.target.clone();
            self.force_base = false;
            self.dirty = false;
            return Ok(vec![Op::Replace(value)]);
        }
        if !self.dirty {
            return Ok(Vec::new());
        }
        let ops = diff_with_scan(&self.baseline, &self.target, self.scan);
        self.baseline = self.target.clone();
        self.dirty = false;
        Ok(ops)
    }

    /// Accept the current value as published without emitting anything.
    pub fn rebase(&mut self) {
        self.baseline = self.target.clone();
        self.force_base = false;
        self.dirty = false;
    }

    /// Drop pending changes, returning to the published value.
    pub fn discard(&mut self) {
        self.target = self.baseline.clone();
        self.force_base = false;
        self.dirty = false;
    }

    /// Adopt `value` as both the current and the published value.
    ///
    /// Used when a producer hands its state to a cold replica: the replica's
    /// baseline is already `value`, so no base batch is owed.
    pub fn sync(&mut self, value: Value) {
        self.target = value.clone();
        self.baseline = value;
        self.force_base = false;
        self.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn first_flush_is_a_base_batch() {
        let mut tracker = Tracker::new(json!({ "a": 1 }));
        assert_eq!(
            tracker.flush().unwrap(),
            vec![Op::Replace(json!({ "a": 1 }))]
        );
    }

    #[test]
    fn a_clean_tracker_emits_nothing() {
        let mut tracker = Tracker::new(json!({ "a": 1 }));
        tracker.flush().unwrap();
        assert!(tracker.flush().unwrap().is_empty());
        assert!(!tracker.dirty());
    }

    #[test]
    fn flush_emits_only_the_change() {
        let mut tracker = Tracker::new(json!({ "a": 1, "b": 2 }));
        tracker.flush().unwrap();
        tracker.target_mut()["a"] = json!(3);
        assert_eq!(
            tracker.flush().unwrap(),
            vec![Op::Set(
                super::super::NonEmptyPath::from_keys(&["a"]),
                json!(3)
            )]
        );
        assert!(tracker.flush().unwrap().is_empty());
    }

    #[test]
    fn replace_root_forces_another_base_batch() {
        let mut tracker = Tracker::new(json!({ "a": 1 }));
        tracker.flush().unwrap();
        tracker.replace_root(json!({ "b": 2 }));
        assert_eq!(
            tracker.flush().unwrap(),
            vec![Op::Replace(json!({ "b": 2 }))]
        );
    }

    #[test]
    fn discard_restores_the_published_value() {
        let mut tracker = Tracker::new(json!({ "a": 1 }));
        tracker.flush().unwrap();
        tracker.target_mut()["a"] = json!(2);
        tracker.discard();
        assert!(!tracker.dirty());
        assert_eq!(tracker.target(), &json!({ "a": 1 }));
    }

    #[test]
    fn sync_prevents_a_base_batch() {
        let mut tracker = Tracker::new(json!({ "a": 1 }));
        tracker.flush().unwrap();
        tracker.sync(json!({ "a": 5 }));
        assert!(tracker.flush().unwrap().is_empty());
    }
}
