//! Applying decoded operations to a JSON value.
//!
//! Rust port of the `apply` / `applyOps` / `applyImmutable` / `resolve` /
//! `copyContainers` section of `packages/chord/src/delta/index.ts`.
//!
//! Two differences are deliberate and documented in the crate README:
//!
//! - **Adoption.** Upstream `r` takes ownership of the payload
//!   (`root = op[1]`), so fanning one batch out to several consumers in-process
//!   makes their replicas alias each other. Rust has no shared mutable
//!   references, so [`apply`] clones the payload instead. The ownership rule
//!   becomes a copy cost, never a correctness hazard.
//! - **UTF-16.** `t` removes *code units* from the front, exactly like
//!   `String.prototype.slice`. A count that splits a surrogate pair cannot be
//!   represented by a Rust `String`; upstream would silently produce a lone
//!   surrogate, so the port rejects the operation instead of corrupting the
//!   value.

use serde_json::Value;

use super::diff::{from_utf16, utf16};
use super::path::path_error;
use super::{assert_safe_path, DeltaError, Op, Seg};

/// Apply decoded operations to `target`, returning the new value.
///
/// `target` is `None` when no root has been established yet; only a batch whose
/// first operation is a replacement can start from nothing. A batch that leaves
/// the root unset is an error rather than `undefined`, because every call site
/// downstream needs a value.
pub fn apply(target: Option<Value>, ops: &[Op]) -> Result<Value, DeltaError> {
    let mut root = target;
    for op in ops {
        if let Op::Replace(value) = op {
            root = Some(value.clone());
            continue;
        }
        let path = op.path();
        assert_safe_path(path)?;
        if let Op::Patch {
            index,
            remove,
            items,
            ..
        } = op
        {
            let node = if path.is_empty() {
                root.as_mut()
            } else {
                root.as_mut()
                    .map(|root| resolve(&mut *root, path))
                    .transpose()?
            }
            .ok_or_else(|| path_error(path))?;
            let array = node.as_array_mut().ok_or_else(|| path_error(path))?;
            splice(array, *index, *remove, items);
            continue;
        }
        let (parent_path, last) = path.split_at(path.len() - 1);
        let parent = root
            .as_mut()
            .map(|root| resolve(&mut *root, parent_path))
            .transpose()?
            .ok_or_else(|| path_error(path))?;
        let key = &last[0];
        assert_array_key(parent, key)?;
        apply_leaf(parent, key, op, path)?;
    }
    root.ok_or_else(|| path_error(&[]))
}

/// Apply decoded operations without mutating the previous value.
///
/// Upstream hands out one immutable snapshot per change; the port rebuilds the
/// containers along the touched path and applies a single operation to the
/// copy. Untouched subtrees are still cloned, which is the cost of representing
/// persistent data with `serde_json::Value` rather than a persistent tree.
pub fn apply_immutable(target: Option<Value>, ops: &[Op]) -> Result<Value, DeltaError> {
    let mut root = target;
    for op in ops {
        if let Op::Replace(value) = op {
            root = Some(value.clone());
            continue;
        }
        let path = op.path();
        assert_safe_path(path)?;
        let container_path: &[Seg] = if matches!(op, Op::Patch { .. }) {
            path
        } else {
            &path[..path.len() - 1]
        };
        let current = root.as_ref().ok_or_else(|| path_error(container_path))?;
        let copied = copy_path(current, container_path, container_path)?;
        let single = std::slice::from_ref(op);
        root = Some(apply(Some(copied), single)?);
    }
    root.ok_or_else(|| path_error(&[]))
}

/// Reject array keys that are not in range, or not numeric at all.
///
/// The range rule is not an arbitrary cap: a sparse array does not survive a
/// JSON round trip, so allowing `xs[7] = x` on a length-3 array produces state
/// a replica cannot match. It also removes the amplification (`["s", ["xs",
/// 4294967290], 1]` allocating a 4.29-billion-entry array from one op).
fn assert_array_key(parent: &Value, key: &Seg) -> Result<(), DeltaError> {
    if let Value::Array(items) = parent {
        let Seg::Index(index) = key else {
            return Err(DeltaError::UnsafePath(key.to_string()));
        };
        if *index > items.len() as u64 {
            return Err(DeltaError::UnsafePath(key.to_string()));
        }
    }
    Ok(())
}

fn apply_leaf(parent: &mut Value, key: &Seg, op: &Op, path: &[Seg]) -> Result<(), DeltaError> {
    match op {
        Op::Set(_, value) => write_child(parent, key, value.clone(), path),
        Op::Delete(_) => match (parent, key) {
            (Value::Array(items), Seg::Index(index)) => {
                let index = *index as usize;
                if index >= items.len() {
                    return Err(path_error(path));
                }
                items.remove(index);
                Ok(())
            }
            (Value::Object(map), Seg::Key(key)) => {
                map.remove(key);
                Ok(())
            }
            (Value::Object(map), Seg::Index(index)) => {
                map.remove(&index.to_string());
                Ok(())
            }
            _ => Err(path_error(path)),
        },
        Op::Append(_, suffix) => {
            let mut next = read_child(parent, key, path)?.to_owned();
            next.push_str(suffix);
            write_child(parent, key, Value::String(next), path)
        }
        Op::Truncate(_, count) => {
            let current = read_child(parent, key, path)?.to_owned();
            let units = utf16(&current);
            let skip = (*count as usize).min(units.len());
            let next = from_utf16(&units[skip..]).ok_or_else(|| {
                DeltaError::NotCharAligned(format!("truncate at {count} splits a surrogate pair"))
            })?;
            write_child(parent, key, Value::String(next), path)
        }
        Op::Replace(_) | Op::Patch { .. } => unreachable!("handled by the caller"),
    }
}

fn read_child<'a>(parent: &'a Value, key: &Seg, path: &[Seg]) -> Result<&'a str, DeltaError> {
    let current = match (parent, key) {
        (Value::Array(items), Seg::Index(index)) => items.get(*index as usize),
        (Value::Object(map), Seg::Key(key)) => map.get(key),
        (Value::Object(map), Seg::Index(index)) => map.get(&index.to_string()),
        _ => None,
    };
    match current {
        Some(Value::String(text)) => Ok(text),
        _ => Err(path_error(path)),
    }
}

fn write_child(parent: &mut Value, key: &Seg, value: Value, path: &[Seg]) -> Result<(), DeltaError> {
    match (parent, key) {
        (Value::Array(items), Seg::Index(index)) => {
            let index = *index as usize;
            if index < items.len() {
                items[index] = value;
            } else {
                items.push(value);
            }
            Ok(())
        }
        (Value::Object(map), Seg::Key(key)) => {
            map.insert(key.clone(), value);
            Ok(())
        }
        (Value::Object(map), Seg::Index(index)) => {
            map.insert(index.to_string(), value);
            Ok(())
        }
        (Value::Array(_), Seg::Key(key)) => Err(DeltaError::UnsafePath(key.clone())),
        _ => Err(path_error(path)),
    }
}

fn splice(items: &mut Vec<Value>, index: u64, remove: u64, inserts: &[Value]) {
    let start = (index as usize).min(items.len());
    let end = start.saturating_add(remove as usize).min(items.len());
    if end > start {
        items.drain(start..end);
    }
    if !inserts.is_empty() {
        items.splice(start..start, inserts.iter().cloned());
    }
}

/// Resolve a path to a container, requiring every step to be present.
fn resolve<'a>(root: &'a mut Value, path: &[Seg]) -> Result<&'a mut Value, DeltaError> {
    let node = resolve_value(root, path, path)?;
    if !node.is_object() && !node.is_array() {
        return Err(path_error(path));
    }
    Ok(node)
}

fn resolve_value<'a>(
    root: &'a mut Value,
    path: &[Seg],
    error_path: &[Seg],
) -> Result<&'a mut Value, DeltaError> {
    let mut node = root;
    for segment in path {
        node = match (node, segment) {
            (Value::Object(map), Seg::Key(key)) => {
                map.get_mut(key).ok_or_else(|| path_error(error_path))?
            }
            (Value::Object(map), Seg::Index(index)) => map
                .get_mut(&index.to_string())
                .ok_or_else(|| path_error(error_path))?,
            (Value::Array(items), Seg::Index(index)) => items
                .get_mut(*index as usize)
                .ok_or_else(|| path_error(error_path))?,
            (Value::Array(_), Seg::Key(key)) => return Err(DeltaError::UnsafePath(key.clone())),
            _ => return Err(path_error(error_path)),
        };
    }
    Ok(node)
}

/// Copy the root and every container along `path`, leaving all other nodes
/// shared.
fn copy_path(node: &Value, path: &[Seg], full: &[Seg]) -> Result<Value, DeltaError> {
    let mut copy = copy_container(node, full)?;
    if path.is_empty() {
        return Ok(copy);
    }
    let segment = &path[0];
    let child = lookup(node, segment, full)?;
    let copied_child = copy_path(child, &path[1..], full)?;
    set_child(&mut copy, segment, copied_child, full)?;
    Ok(copy)
}

fn copy_container(node: &Value, full: &[Seg]) -> Result<Value, DeltaError> {
    match node {
        Value::Array(items) => Ok(Value::Array(items.clone())),
        Value::Object(map) => Ok(Value::Object(map.clone())),
        _ => Err(path_error(full)),
    }
}

fn lookup<'a>(node: &'a Value, segment: &Seg, full: &[Seg]) -> Result<&'a Value, DeltaError> {
    match (node, segment) {
        (Value::Object(map), Seg::Key(key)) => map.get(key).ok_or_else(|| path_error(full)),
        (Value::Object(map), Seg::Index(index)) => {
            map.get(&index.to_string()).ok_or_else(|| path_error(full))
        }
        (Value::Array(items), Seg::Index(index)) => items
            .get(*index as usize)
            .ok_or_else(|| path_error(full)),
        (Value::Array(_), Seg::Key(key)) => Err(DeltaError::UnsafePath(key.clone())),
        _ => Err(path_error(full)),
    }
}

fn set_child(copy: &mut Value, segment: &Seg, value: Value, full: &[Seg]) -> Result<(), DeltaError> {
    match (copy, segment) {
        (Value::Object(map), Seg::Key(key)) => {
            map.insert(key.clone(), value);
            Ok(())
        }
        (Value::Object(map), Seg::Index(index)) => {
            map.insert(index.to_string(), value);
            Ok(())
        }
        (Value::Array(items), Seg::Index(index)) => {
            let index = *index as usize;
            if index < items.len() {
                items[index] = value;
            } else {
                items.push(value);
            }
            Ok(())
        }
        (Value::Array(_), Seg::Key(key)) => Err(DeltaError::UnsafePath(key.clone())),
        _ => Err(path_error(full)),
    }
}

/// Convenience: a single-segment set operation. Test-only; production code
/// builds [`Op`] values through [`crate::delta::diff`].
#[cfg(test)]
pub(crate) fn set_op(path: crate::delta::NonEmptyPath, value: Value) -> Op {
    Op::Set(path, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta::NonEmptyPath;
    use serde_json::json;

    fn non_empty(keys: &[&str]) -> NonEmptyPath {
        NonEmptyPath::from_keys(keys)
    }

    #[test]
    fn applies_in_batch_order() {
        let ops = vec![
            set_op(non_empty(&["a"]), json!(1)),
            set_op(non_empty(&["a"]), json!(2)),
        ];
        assert_eq!(apply(Some(json!({})), &ops).unwrap(), json!({ "a": 2 }));
    }

    #[test]
    fn duplicate_deltas_are_idempotent() {
        let ops = vec![
            set_op(non_empty(&["a"]), json!(2)),
            set_op(non_empty(&["a"]), json!(2)),
        ];
        assert_eq!(apply(Some(json!({ "a": 1 })), &ops).unwrap(), json!({ "a": 2 }));
    }

    #[test]
    fn out_of_order_delta_is_refused() {
        let ops = vec![set_op(non_empty(&["missing", "a"]), json!(1))];
        assert!(matches!(
            apply(Some(json!({})), &ops),
            Err(DeltaError::Path(_))
        ));
    }

    #[test]
    fn unsafe_and_out_of_range_paths_are_refused() {
        for op in [
            json!(["s", ["__proto__"], 1]),
            json!(["s", ["xs", 4], 1]),
            json!(["s", ["xs", "1"], 1]),
        ] {
            if let Ok(parsed) = Op::from_json(&op) {
                assert!(
                    apply(Some(json!({ "xs": [] })), &[parsed]).is_err(),
                    "{op} should not apply"
                );
            }
        }
        // The reserved segment is rejected while the operation is parsed.
        assert!(matches!(
            Op::from_json(&json!(["s", ["__proto__"], 1])),
            Err(DeltaError::UnsafePath(_))
        ));
    }

    #[test]
    fn string_ops_work_on_utf16_units() {
        let target = Some(json!({ "s": "a😀b" }));
        let truncate = Op::from_json(&json!(["t", ["s"], 3])).unwrap();
        assert_eq!(apply(target.clone(), &[truncate]).unwrap(), json!({ "s": "b" }));
        let misaligned = Op::from_json(&json!(["t", ["s"], 2])).unwrap();
        assert!(matches!(
            apply(target, &[misaligned]),
            Err(DeltaError::NotCharAligned(_))
        ));
    }

    #[test]
    fn patch_splices_like_javascript() {
        let patch = Op::from_json(&json!(["p", ["xs"], 1, 2, [9]])).unwrap();
        assert_eq!(
            apply(Some(json!({ "xs": [1, 2, 3, 4] })), &[patch]).unwrap(),
            json!({ "xs": [1, 9, 4] })
        );
    }

    #[test]
    fn replacement_adopts_a_new_root() {
        let replace = Op::from_json(&json!(["r", { "a": 1 }])).unwrap();
        assert_eq!(apply(None, &[replace]).unwrap(), json!({ "a": 1 }));
        assert!(apply(None, &[]).is_err());
    }

    #[test]
    fn immutable_apply_leaves_the_original_alone() {
        let original = json!({ "a": { "b": [1, 2] } });
        let path = NonEmptyPath::try_new(vec![Seg::key("a"), Seg::key("b"), Seg::index(1)])
            .expect("non-empty");
        let ops = vec![set_op(path, json!(9))];
        let next = apply_immutable(Some(original.clone()), &ops).unwrap();
        assert_eq!(next, json!({ "a": { "b": [1, 9] } }));
        assert_eq!(original, json!({ "a": { "b": [1, 2] } }));
    }
}
