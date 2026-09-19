//! Diffing a JSON value into decoded operations.
//!
//! Rust port of the `overlap` / `diffValue` / `diffObject` / `diffArray` /
//! `diffString` half of `packages/chord/src/delta/index.ts`. The upstream
//! producer is a `Proxy` that records which paths were touched and then walks
//! only those; Rust has no property-access trap, so the port always compares
//! the published baseline with the current value and treats every path as
//! dirty. That is the semantic core of a flush — the dirty tree is a
//! performance optimisation, not a different result — and it produces the same
//! operations unless a specific array mutation would have been narrowed by the
//! append fast path.

use serde_json::{Map, Number, Value};

use super::path::{Path, RESERVED_SEGMENTS, Seg};
use super::{DeltaError, NonEmptyPath, Op};

/// Default cap on how far back the string-overlap scan looks.
pub const DEFAULT_MAX_OVERLAP_SCAN: usize = 65_536;

const PROBE: usize = 64;
const MAX_CANDIDATES: usize = 8;

/// Longest suffix of `a` that is a prefix of `b`, measured in UTF-16 code
/// units.
///
/// Upstream probes with `indexOf` and verifies exact substring equality so the
/// hot loop runs in native code. The Rust port keeps the same candidate
/// strategy: probe with a long head first (few candidates, and it catches the
/// large overlaps a rolling window produces), then fall back to one unit. The
/// candidate count is bounded because repetitive output — a build log, or any
/// run of one character — makes a long head match at thousands of positions;
/// giving up returns 0, which emits a set: larger, never wrong.
pub fn overlap(a: &str, b: &str, scan: usize) -> usize {
    overlap_with(a, b, scan, PROBE, MAX_CANDIDATES)
}

/// [`overlap`] with an explicit probe length and candidate bound.
pub fn overlap_with(a: &str, b: &str, scan: usize, probe: usize, max_candidates: usize) -> usize {
    if a.is_empty() || b.is_empty() || scan == 0 {
        return 0;
    }
    let a_units = utf16(a);
    let b_units = utf16(b);
    let tail = if a_units.len() > scan {
        &a_units[a_units.len() - scan..]
    } else {
        &a_units[..]
    };
    for head_len in [probe.min(b_units.len()), 1] {
        let head = &b_units[..head_len];
        let mut tried = 0usize;
        let mut from = 0usize;
        while let Some(k) = find_subslice(tail, head, from) {
            tried += 1;
            if tried > max_candidates {
                break;
            }
            let n = tail.len() - k;
            if n <= b_units.len() && tail[k..] == b_units[..n] {
                return n;
            }
            from = k + 1;
        }
        if head_len == 1 {
            break;
        }
    }
    0
}

/// Diff `before` into `after`, returning the operations that transform one
/// into the other.
///
/// An empty batch means the values are deeply equal.
pub fn diff(before: &Value, after: &Value) -> Vec<Op> {
    diff_with_scan(before, after, DEFAULT_MAX_OVERLAP_SCAN)
}

/// [`diff`] with an explicit string-overlap scan bound.
pub fn diff_with_scan(before: &Value, after: &Value, scan: usize) -> Vec<Op> {
    let mut out = Vec::new();
    let mut path = Path::new();
    diff_value(Some(before), Some(after), &mut path, scan, &mut out)
        .expect("diffing two present values cannot produce a root delete");
    out
}

fn diff_value(
    before: Option<&Value>,
    after: Option<&Value>,
    path: &mut Path,
    scan: usize,
    out: &mut Vec<Op>,
) -> Result<(), DeltaError> {
    match (before, after) {
        (None, None) => {}
        (None, Some(value)) => emit_set(path, value, out),
        (Some(_), None) => emit_delete(path, out)?,
        (Some(before), Some(after)) => {
            if json_equal(before, after) {
                return Ok(());
            }
            match (before, after) {
                (Value::String(before), Value::String(after)) => diff_string(before, after, path, scan, out),
                (Value::Array(before), Value::Array(after)) => diff_array(before, after, path, scan, out)?,
                (Value::Object(before), Value::Object(after)) => diff_object(before, after, path, scan, out)?,
                _ => emit_set(path, after, out),
            }
        }
    }
    Ok(())
}

fn emit_set(path: &[Seg], value: &Value, out: &mut Vec<Op>) {
    if path.is_empty() {
        out.push(Op::Replace(value.clone()));
    } else {
        out.push(Op::Set(
            NonEmptyPath::try_new(path.to_vec()).expect("path is non-empty"),
            value.clone(),
        ));
    }
}

fn emit_delete(path: &[Seg], out: &mut Vec<Op>) -> Result<(), DeltaError> {
    if path.is_empty() {
        return Err(DeltaError::InvalidOp("the tracked root cannot be deleted".to_owned()));
    }
    out.push(Op::Delete(
        NonEmptyPath::try_new(path.to_vec()).expect("path is non-empty"),
    ));
    Ok(())
}

fn diff_string(before: &str, after: &str, path: &[Seg], scan: usize, out: &mut Vec<Op>) {
    if before == after {
        return;
    }
    if path.is_empty() {
        emit_set(path, &Value::String(after.to_owned()), out);
        return;
    }
    let before_units = utf16(before);
    let after_units = utf16(after);
    let at = || NonEmptyPath::try_new(path.to_vec()).expect("path is non-empty");

    // A pure append. Upstream compares `after.slice(0, before.length) === before`
    // because `after` is usually a cons string and `startsWith` walks it char by
    // char; Rust strings are flat, so `starts_with` on units is a memcmp.
    if after_units.len() > before_units.len() && after_units.starts_with(&before_units) {
        if let Some(suffix) = from_utf16(&after_units[before_units.len()..]) {
            out.push(Op::Append(at(), suffix));
            return;
        }
    }
    let shared = overlap(before, after, scan);
    if shared == 0 {
        emit_set(path, &Value::String(after.to_owned()), out);
        return;
    }
    let truncate = before_units.len() - shared;
    let kept = from_utf16(&before_units[truncate..]);
    let tail = from_utf16(&after_units[shared..]);
    match (kept, tail) {
        (Some(_), Some(tail)) => {
            out.push(Op::Truncate(at(), truncate as u64));
            if after_units.len() > shared {
                out.push(Op::Append(at(), tail));
            }
        }
        // The overlap boundary splits a surrogate pair on one side. JavaScript
        // would emit a lone surrogate; Rust strings cannot hold one, so the
        // batch degrades to a whole-value set. Correct on both runtimes.
        _ => emit_set(path, &Value::String(after.to_owned()), out),
    }
}

fn diff_object(
    before: &Map<String, Value>,
    after: &Map<String, Value>,
    path: &mut Path,
    scan: usize,
    out: &mut Vec<Op>,
) -> Result<(), DeltaError> {
    if before
        .keys()
        .chain(after.keys())
        .any(|key| RESERVED_SEGMENTS.contains(&key.as_str()))
    {
        emit_set(path, &Value::Object(after.clone()), out);
        return Ok(());
    }
    for (key, value) in after {
        path.push(Seg::Key(key.clone()));
        diff_value(before.get(key), Some(value), path, scan, out)?;
        path.pop();
    }
    for key in before.keys() {
        if !after.contains_key(key) {
            path.push(Seg::Key(key.clone()));
            emit_delete(path, out)?;
            path.pop();
        }
    }
    Ok(())
}

fn diff_array(
    before: &[Value],
    after: &[Value],
    path: &mut Path,
    scan: usize,
    out: &mut Vec<Op>,
) -> Result<(), DeltaError> {
    if before.len() == after.len() {
        for index in 0..after.len() {
            path.push(Seg::Index(index as u64));
            diff_value(Some(&before[index]), Some(&after[index]), path, scan, out)?;
            path.pop();
        }
        return Ok(());
    }

    let mut prefix = 0usize;
    while prefix < before.len() && prefix < after.len() && json_equal(&before[prefix], &after[prefix]) {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < before.len() - prefix
        && suffix < after.len() - prefix
        && json_equal(&before[before.len() - 1 - suffix], &after[after.len() - 1 - suffix])
    {
        suffix += 1;
    }
    let shorter = before.len().min(after.len());
    if prefix + suffix == shorter {
        let remove = before.len() - prefix - suffix;
        let items = after[prefix..after.len() - suffix].to_vec();
        if prefix == 0 && remove == before.len() {
            emit_set(path, &Value::Array(after.to_vec()), out);
        } else {
            out.push(Op::Patch {
                path: path.clone(),
                index: prefix as u64,
                remove: remove as u64,
                items,
            });
        }
        return Ok(());
    }

    // Structural movement combined with retained-index edits has no unique
    // alignment. Preserve the retained index deltas and express only the tail
    // length change structurally. It may be broader than the producer's intent,
    // but never degrades those edits to a whole-array replacement.
    for index in 0..shorter {
        path.push(Seg::Index(index as u64));
        diff_value(Some(&before[index]), Some(&after[index]), path, scan, out)?;
        path.pop();
    }
    if after.len() > before.len() {
        out.push(Op::Patch {
            path: path.clone(),
            index: before.len() as u64,
            remove: 0,
            items: after[before.len()..].to_vec(),
        });
    } else if after.is_empty() {
        emit_set(path, &Value::Array(Vec::new()), out);
    } else {
        out.push(Op::Patch {
            path: path.clone(),
            index: after.len() as u64,
            remove: (before.len() - after.len()) as u64,
            items: Vec::new(),
        });
    }
    Ok(())
}

/// Deep structural equality on strict JSON values.
///
/// Numbers compare by value, so `1` and `1.0` are equal — JavaScript has one
/// number type and `===` cannot tell them apart. Integers outside the f64
/// exact range are compared exactly when both sides are integers.
pub fn json_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => number_equal(a, b),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| json_equal(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len() && a.iter().all(|(key, value)| b.get(key).is_some_and(|other| json_equal(value, other)))
        }
        _ => false,
    }
}

fn number_equal(left: &Number, right: &Number) -> bool {
    if let (Some(a), Some(b)) = (left.as_i64(), right.as_i64()) {
        return a == b;
    }
    if let (Some(a), Some(b)) = (left.as_u64(), right.as_u64()) {
        return a == b;
    }
    match (left.as_f64(), right.as_f64()) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Encode a string as UTF-16 code units, the unit JavaScript string ops use.
pub(crate) fn utf16(value: &str) -> Vec<u16> {
    value.encode_utf16().collect()
}

/// Decode UTF-16 code units back into a string, returning `None` when the
/// range splits a surrogate pair.
pub(crate) fn from_utf16(units: &[u16]) -> Option<String> {
    String::from_utf16(units).ok()
}

fn find_subslice(haystack: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() || from > haystack.len() - needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|&start| &haystack[start..start + needle.len()] == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn set(keys: &[&str], value: Value) -> Op {
        Op::Set(NonEmptyPath::from_keys(keys), value)
    }

    #[test]
    fn finds_an_overlap_shorter_than_the_long_probe() {
        assert_eq!(overlap("abcdefgh", "defghxyz", DEFAULT_MAX_OVERLAP_SCAN), 5);
    }

    #[test]
    fn honors_a_disabled_scan() {
        assert_eq!(overlap("abcdef", "defghi", 0), 0);
    }

    #[test]
    fn tracks_numbers_by_value() {
        assert!(json_equal(&json!(1), &json!(1.0)));
        assert!(json_equal(&json!([1, { "a": null }]), &json!([1.0, { "a": null }])));
        assert!(!json_equal(&json!(1), &json!("1")));
    }

    #[test]
    fn identical_values_produce_no_ops() {
        assert!(diff(&json!({ "a": [1, { "b": "c" }] }), &json!({ "a": [1, { "b": "c" }] })).is_empty());
    }

    #[test]
    fn emits_an_append_for_a_string_growth() {
        let ops = diff(&json!({ "s": "ab" }), &json!({ "s": "abcd" }));
        assert_eq!(ops, vec![Op::Append(NonEmptyPath::from_keys(&["s"]), "cd".into())]);
    }

    #[test]
    fn rolling_window_emits_truncate_then_append() {
        let ops = diff(&json!({ "s": "abcdefgh" }), &json!({ "s": "defghxyz" }));
        assert_eq!(
            ops,
            vec![
                Op::Truncate(NonEmptyPath::from_keys(&["s"]), 3),
                Op::Append(NonEmptyPath::from_keys(&["s"]), "xyz".into()),
            ]
        );
    }

    #[test]
    fn unrelated_string_replacement_emits_a_set() {
        let ops = diff(&json!({ "s": "abc" }), &json!({ "s": "xyz" }));
        assert_eq!(ops, vec![set(&["s"], json!("xyz"))]);
    }

    #[test]
    fn a_root_string_change_is_a_replace() {
        assert_eq!(
            diff(&json!("ab"), &json!("abcd")),
            vec![Op::Replace(json!("abcd"))]
        );
    }

    #[test]
    fn array_append_emits_one_patch() {
        let ops = diff(&json!({ "xs": [1, 2] }), &json!({ "xs": [1, 2, 3] }));
        assert_eq!(
            ops,
            vec![Op::Patch {
                path: vec![Seg::key("xs")],
                index: 2,
                remove: 0,
                items: vec![json!(3)],
            }]
        );
    }

    #[test]
    fn empty_to_non_empty_array_is_a_set() {
        let ops = diff(&json!({ "xs": [] }), &json!({ "xs": [1] }));
        assert_eq!(ops, vec![set(&["xs"], json!([1]))]);
    }

    #[test]
    fn object_key_removal_emits_a_delete() {
        let ops = diff(&json!({ "a": 1, "b": 2 }), &json!({ "b": 2 }));
        assert_eq!(ops, vec![Op::Delete(NonEmptyPath::from_keys(&["a"]))]);
    }

    #[test]
    fn nested_object_edit_keeps_its_path() {
        let ops = diff(&json!({ "a": { "b": 1 } }), &json!({ "a": { "b": 2 } }));
        assert_eq!(ops, vec![set(&["a", "b"], json!(2))]);
    }

    #[test]
    fn reserved_keys_degrade_to_a_set() {
        let ops = diff(&json!({ "a": 1 }), &json!({ "a": 1, "constructor": 2 }));
        assert_eq!(
            ops,
            vec![Op::Replace(json!({ "a": 1, "constructor": 2 }))]
        );
    }
}
