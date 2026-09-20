//! Operation vocabulary, validation and JSON codec — the `Op` / `WireOp` half
//! of `packages/chord/src/delta/index.ts`.
//!
//! Two vocabularies live here:
//!
//! - [`Op`] — *decoded*: every operation carries its complete inline path.
//!   This is what a producer flushes and what [`apply`](super::apply) accepts.
//! - [`WireOp`] — what crosses a boundary: adds path interning (`#`) and
//!   arity-based path omission on top of [`Op`].
//!
//! The distinction matters because the two grammars overlap but are not the
//! same: a two-element `["s", value]` is a legal wire operation (reuse the
//! previous path) and an illegal decoded one. Each vocabulary therefore gets
//! the validator that matches it, exactly as upstream.

use serde_json::Value;

use super::path::{assert_safe_path, path_from_json, path_to_json, Path, Seg};
use super::DeltaError;
use super::NonEmptyPath;

/// A path inline, or an id assigned by the encoder, or omitted (reuse the
/// previous path in the batch).
#[derive(Clone, Debug, PartialEq)]
pub enum PathRef {
    /// A complete inline path.
    Inline(Path),
    /// A numeric id defined earlier by a `#` operation.
    Id(u64),
    /// The short form: reuse the previous operation's path.
    Omitted,
}

/// A decoded delta operation.
///
/// `Replace` is the only verb that replaces a whole value. `Set`, `Delete`,
/// `Append` and `Truncate` cannot target the root — [`NonEmptyPath`] enforces
/// that. `Patch` may target the root, because a tracked value can itself be an
/// array.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// Replace the complete value.
    Replace(Value),
    /// Set a property or array element.
    Set(NonEmptyPath, Value),
    /// Delete an object property, or remove one array element.
    Delete(NonEmptyPath),
    /// Append text to a string.
    Append(NonEmptyPath, String),
    /// Remove `count` UTF-16 code units from the front of a string.
    Truncate(NonEmptyPath, u64),
    /// Splice an array.
    Patch {
        /// The array path; may be empty (the root).
        path: Path,
        /// First index affected.
        index: u64,
        /// Number of elements removed.
        remove: u64,
        /// Elements inserted at `index`.
        items: Vec<Value>,
    },
}

/// A wire delta operation.
#[derive(Clone, Debug, PartialEq)]
pub enum WireOp {
    /// Complete replacement; identical to the decoded form.
    Replace(Value),
    /// Set using an inline path, an interned id, or the previous path.
    Set(PathRef, Value),
    /// Delete using an inline path, an interned id, or the previous path.
    Delete(PathRef),
    /// Append using an inline path, an interned id, or the previous path.
    Append(PathRef, String),
    /// Front-truncate using an inline path, an interned id, or the previous path.
    Truncate(PathRef, u64),
    /// Splice using an inline path, an interned id, or the previous path.
    Patch {
        /// Path reference.
        path: PathRef,
        /// First index affected.
        index: u64,
        /// Number of elements removed.
        remove: u64,
        /// Inserted elements.
        items: Vec<Value>,
    },
    /// Define a numeric path id.
    Define {
        /// The id being defined.
        id: u64,
        /// The path it names.
        path: Path,
    },
}

impl Op {
    /// Whether this is the whole-value replacement verb.
    pub fn is_replace(&self) -> bool {
        matches!(self, Op::Replace(_))
    }

    /// The inline path of the operation. Empty for [`Op::Replace`].
    pub(crate) fn path(&self) -> &[Seg] {
        match self {
            Op::Set(path, _) => path.as_slice(),
            Op::Delete(path) => path.as_slice(),
            Op::Append(path, _) => path.as_slice(),
            Op::Truncate(path, _) => path.as_slice(),
            Op::Patch { path, .. } => path.as_slice(),
            Op::Replace(_) => &[],
        }
    }

    /// Convert to the decoded JSON tuple form.
    pub fn to_json(&self) -> Value {
        match self {
            Op::Replace(value) => Value::Array(vec![Value::String("r".into()), value.clone()]),
            Op::Set(path, value) => Value::Array(vec![
                Value::String("s".into()),
                path_to_json(path),
                value.clone(),
            ]),
            Op::Delete(path) => Value::Array(vec![Value::String("d".into()), path_to_json(path)]),
            Op::Append(path, text) => Value::Array(vec![
                Value::String("a".into()),
                path_to_json(path),
                Value::String(text.clone()),
            ]),
            Op::Truncate(path, count) => Value::Array(vec![
                Value::String("t".into()),
                path_to_json(path),
                Value::Number((*count).into()),
            ]),
            Op::Patch {
                path,
                index,
                remove,
                items,
            } => Value::Array(vec![
                Value::String("p".into()),
                path_to_json(path),
                Value::Number((*index).into()),
                Value::Number((*remove).into()),
                Value::Array(items.clone()),
            ]),
        }
    }

    /// Parse and validate an operation from the decoded JSON tuple form.
    pub fn from_json(value: &Value) -> Result<Op, DeltaError> {
        let tuple = as_tuple(value)?;
        let verb = as_verb(tuple)?;
        match verb {
            "r" => {
                expect_arity(tuple, 2, "r arity")?;
                Ok(Op::Replace(tuple[1].clone()))
            }
            "s" => {
                expect_arity(tuple, 3, "s arity")?;
                let path = non_empty_path(&tuple[1])?;
                Ok(Op::Set(path, tuple[2].clone()))
            }
            "d" => {
                expect_arity(tuple, 2, "d arity")?;
                Ok(Op::Delete(non_empty_path(&tuple[1])?))
            }
            "a" => {
                expect_arity(tuple, 3, "a shape")?;
                let path = non_empty_path(&tuple[1])?;
                let text = tuple[2]
                    .as_str()
                    .ok_or_else(|| DeltaError::InvalidOp("a value".to_owned()))?;
                Ok(Op::Append(path, text.to_owned()))
            }
            "t" => {
                expect_arity(tuple, 3, "t shape")?;
                let path = non_empty_path(&tuple[1])?;
                Ok(Op::Truncate(path, as_count(&tuple[2], "t")?))
            }
            "p" => {
                expect_arity(tuple, 5, "p arity")?;
                let path = inline_path(&tuple[1])?;
                let index = as_count(&tuple[2], "p index")?;
                let remove = as_count(&tuple[3], "p remove")?;
                let items = items(&tuple[4], "p items")?;
                Ok(Op::Patch {
                    path,
                    index,
                    remove,
                    items,
                })
            }
            other => Err(DeltaError::InvalidOp(format!("unknown op verb: {other}"))),
        }
    }

    /// Validate an operation in its decoded JSON tuple form.
    pub fn assert_valid(value: &Value) -> Result<(), DeltaError> {
        Op::from_json(value).map(|_| ())
    }
}

impl WireOp {
    /// Whether this is the whole-value replacement verb.
    pub fn is_replace(&self) -> bool {
        matches!(self, WireOp::Replace(_))
    }

    /// Convert to the wire JSON tuple form.
    pub fn to_json(&self) -> Value {
        let ref_json = |path: &PathRef| match path {
            PathRef::Inline(path) => path_to_json(path),
            PathRef::Id(id) => Value::Number((*id).into()),
            PathRef::Omitted => Value::Null,
        };
        match self {
            WireOp::Replace(value) => Value::Array(vec![Value::String("r".into()), value.clone()]),
            WireOp::Set(path, value) => match path {
                PathRef::Omitted => Value::Array(vec![Value::String("s".into()), value.clone()]),
                other => Value::Array(vec![
                    Value::String("s".into()),
                    ref_json(other),
                    value.clone(),
                ]),
            },
            WireOp::Delete(PathRef::Omitted) => Value::Array(vec![Value::String("d".into())]),
            WireOp::Delete(path) => Value::Array(vec![Value::String("d".into()), ref_json(path)]),
            WireOp::Append(PathRef::Omitted, text) => {
                Value::Array(vec![Value::String("a".into()), Value::String(text.clone())])
            }
            WireOp::Append(path, text) => Value::Array(vec![
                Value::String("a".into()),
                ref_json(path),
                Value::String(text.clone()),
            ]),
            WireOp::Truncate(PathRef::Omitted, count) => Value::Array(vec![
                Value::String("t".into()),
                Value::Number((*count).into()),
            ]),
            WireOp::Truncate(path, count) => Value::Array(vec![
                Value::String("t".into()),
                ref_json(path),
                Value::Number((*count).into()),
            ]),
            WireOp::Patch {
                path,
                index,
                remove,
                items,
            } => {
                let mut tuple = vec![Value::String("p".into())];
                if !matches!(path, PathRef::Omitted) {
                    tuple.push(ref_json(path));
                }
                tuple.push(Value::Number((*index).into()));
                tuple.push(Value::Number((*remove).into()));
                tuple.push(Value::Array(items.clone()));
                Value::Array(tuple)
            }
            WireOp::Define { id, path } => Value::Array(vec![
                Value::String("#".into()),
                Value::Number((*id).into()),
                path_to_json(path),
            ]),
        }
    }

    /// Parse and validate a wire operation from JSON.
    pub fn from_json(value: &Value) -> Result<WireOp, DeltaError> {
        let tuple = as_tuple(value)?;
        let verb = as_verb(tuple)?;
        match verb {
            "r" => {
                expect_arity(tuple, 2, "r arity")?;
                Ok(WireOp::Replace(tuple[1].clone()))
            }
            "s" => match tuple.len() {
                3 => Ok(WireOp::Set(path_ref(&tuple[1])?, tuple[2].clone())),
                2 => Ok(WireOp::Set(PathRef::Omitted, tuple[1].clone())),
                _ => Err(DeltaError::InvalidOp("s arity".to_owned())),
            },
            "d" => match tuple.len() {
                2 => Ok(WireOp::Delete(path_ref(&tuple[1])?)),
                1 => Ok(WireOp::Delete(PathRef::Omitted)),
                _ => Err(DeltaError::InvalidOp("d arity".to_owned())),
            },
            "a" => match tuple.len() {
                3 => {
                    let text = text_arg(&tuple[2])?;
                    Ok(WireOp::Append(path_ref(&tuple[1])?, text))
                }
                2 => Ok(WireOp::Append(PathRef::Omitted, text_arg(&tuple[1])?)),
                _ => Err(DeltaError::InvalidOp("a arity".to_owned())),
            },
            "t" => match tuple.len() {
                3 => Ok(WireOp::Truncate(
                    path_ref(&tuple[1])?,
                    as_count(&tuple[2], "t count")?,
                )),
                2 => Ok(WireOp::Truncate(
                    PathRef::Omitted,
                    as_count(&tuple[1], "t count")?,
                )),
                _ => Err(DeltaError::InvalidOp("t arity".to_owned())),
            },
            "p" => match tuple.len() {
                5 => Ok(WireOp::Patch {
                    path: path_ref(&tuple[1])?,
                    index: as_count(&tuple[2], "p index")?,
                    remove: as_count(&tuple[3], "p remove")?,
                    items: items(&tuple[4], "p items")?,
                }),
                4 => Ok(WireOp::Patch {
                    path: PathRef::Omitted,
                    index: as_count(&tuple[1], "p index")?,
                    remove: as_count(&tuple[2], "p remove")?,
                    items: items(&tuple[3], "p items")?,
                }),
                _ => Err(DeltaError::InvalidOp("p arity".to_owned())),
            },
            "#" => {
                if tuple.len() != 3 {
                    return Err(DeltaError::InvalidOp("# shape".to_owned()));
                }
                let id = as_count(&tuple[1], "bad path id")?;
                let path = inline_path(&tuple[2])?;
                Ok(WireOp::Define { id, path })
            }
            other => Err(DeltaError::InvalidOp(format!("unknown op verb: {other}"))),
        }
    }

    /// Validate a wire operation in JSON form.
    pub fn assert_valid(value: &Value) -> Result<(), DeltaError> {
        WireOp::from_json(value).map(|_| ())
    }
}

/// Whether a decoded batch begins with a replacement.
///
/// Flush guarantees `r` is at index 0 or absent, so this is exact rather than a
/// heuristic.
pub fn is_base(ops: &[Op]) -> bool {
    matches!(ops.first(), Some(Op::Replace(_)))
}

/// Whether a wire batch begins with a replacement.
pub fn is_base_wire(ops: &[WireOp]) -> bool {
    matches!(ops.first(), Some(WireOp::Replace(_)))
}

/// Validate a decoded operation in JSON form (upstream `assertValidOp`).
pub fn assert_valid_op(value: &Value) -> Result<(), DeltaError> {
    Op::assert_valid(value)
}

/// Validate a wire operation in JSON form (upstream `assertValidWireOp`).
pub fn assert_valid_wire_op(value: &Value) -> Result<(), DeltaError> {
    WireOp::assert_valid(value)
}

fn as_tuple(value: &Value) -> Result<&[Value], DeltaError> {
    value
        .as_array()
        .map(Vec::as_slice)
        .filter(|tuple| !tuple.is_empty())
        .ok_or_else(|| DeltaError::InvalidOp("op is not a tuple".to_owned()))
}

fn as_verb(tuple: &[Value]) -> Result<&str, DeltaError> {
    tuple[0]
        .as_str()
        .ok_or_else(|| DeltaError::InvalidOp("op verb is not a string".to_owned()))
}

fn expect_arity(tuple: &[Value], arity: usize, message: &str) -> Result<(), DeltaError> {
    if tuple.len() != arity {
        return Err(DeltaError::InvalidOp(message.to_owned()));
    }
    Ok(())
}

fn inline_path(value: &Value) -> Result<Path, DeltaError> {
    let path = path_from_json(value)?;
    assert_safe_path(&path)?;
    Ok(path)
}

fn non_empty_path(value: &Value) -> Result<NonEmptyPath, DeltaError> {
    NonEmptyPath::try_new(inline_path(value)?)
}

fn path_ref(value: &Value) -> Result<PathRef, DeltaError> {
    if let Some(id) = value.as_u64() {
        return Ok(PathRef::Id(id));
    }
    if value.is_null() {
        return Err(DeltaError::InvalidOp("path is not an array".to_owned()));
    }
    Ok(PathRef::Inline(inline_path(value)?))
}

fn as_count(value: &Value, label: &str) -> Result<u64, DeltaError> {
    value
        .as_u64()
        .ok_or_else(|| DeltaError::InvalidOp(format!("{label} is not a non-negative integer")))
}

fn text_arg(value: &Value) -> Result<String, DeltaError> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| DeltaError::InvalidOp("a value".to_owned()))
}

fn items(value: &Value, label: &str) -> Result<Vec<Value>, DeltaError> {
    value
        .as_array()
        .cloned()
        .ok_or_else(|| DeltaError::InvalidOp(label.to_owned()))
}

/// Serialise a decoded batch to JSON.
pub fn ops_to_json(ops: &[Op]) -> Value {
    Value::Array(ops.iter().map(Op::to_json).collect())
}

/// Parse a decoded batch from JSON.
pub fn ops_from_json(value: &Value) -> Result<Vec<Op>, DeltaError> {
    value
        .as_array()
        .ok_or_else(|| DeltaError::InvalidOp("ops is not an array".to_owned()))?
        .iter()
        .map(Op::from_json)
        .collect()
}

/// Serialise a wire batch to JSON.
pub fn wire_ops_to_json(ops: &[WireOp]) -> Value {
    Value::Array(ops.iter().map(WireOp::to_json).collect())
}

/// Parse a wire batch from JSON.
pub fn wire_ops_from_json(value: &Value) -> Result<Vec<WireOp>, DeltaError> {
    value
        .as_array()
        .ok_or_else(|| DeltaError::InvalidOp("ops is not an array".to_owned()))?
        .iter()
        .map(WireOp::from_json)
        .collect()
}

/// Convenience: build an inline path reference.
pub fn inline(path: Path) -> PathRef {
    PathRef::Inline(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_decoded_ops_and_rejects_wire_forms() {
        for op in [
            json!(["r", { "a": 1 }]),
            json!(["s", ["a"], 1]),
            json!(["d", ["a"]]),
            json!(["a", ["a"], "x"]),
            json!(["t", ["a"], 2]),
            json!(["p", ["a"], 0, 0, []]),
        ] {
            assert_valid_op(&op).unwrap_or_else(|error| panic!("{op} should be valid: {error}"));
        }
        for wire_only in [
            json!(["s", 1]),
            json!(["d"]),
            json!(["a", "x"]),
            json!(["t", 2]),
            json!(["p", 0, 0, []]),
            json!(["#", 0, ["a"]]),
            json!(["s", 0, 1]),
        ] {
            assert!(assert_valid_op(&wire_only).is_err(), "{wire_only}");
            assert_valid_wire_op(&wire_only).unwrap_or_else(|error| panic!("{wire_only}: {error}"));
        }
    }

    #[test]
    fn rejects_unknown_verbs_and_bad_shapes() {
        assert!(assert_valid_op(&json!(["ZZZ", ["a"], 9])).is_err());
        assert!(assert_valid_op(&json!(["p", ["a"], 0, 0, "not-an-array"])).is_err());
        assert!(assert_valid_op(&json!(["s", "a", 9])).is_err());
        assert!(assert_valid_op(&json!({ "op": "s" })).is_err());
        assert!(assert_valid_op(&json!(null)).is_err());
        assert!(assert_valid_op(&json!(["t", ["a"], -1])).is_err());
    }

    #[test]
    fn round_trips_decoded_ops() {
        let ops = vec![
            Op::Replace(json!({ "a": 1 })),
            Op::Set(NonEmptyPath::from_keys(&["a"]), json!(2)),
            Op::Delete(NonEmptyPath::from_keys(&["b"])),
            Op::Append(NonEmptyPath::from_keys(&["c"]), "x".into()),
            Op::Truncate(NonEmptyPath::from_keys(&["c"]), 1),
            Op::Patch {
                path: vec![Seg::key("d")],
                index: 0,
                remove: 1,
                items: vec![json!(1)],
            },
        ];
        let json = ops_to_json(&ops);
        assert_eq!(ops_from_json(&json).unwrap(), ops);
    }

    #[test]
    fn round_trips_wire_ops() {
        let ops = vec![
            WireOp::Replace(json!(1)),
            WireOp::Define {
                id: 0,
                path: vec![Seg::key("a")],
            },
            WireOp::Append(PathRef::Id(0), "x".into()),
            WireOp::Set(PathRef::Omitted, json!(2)),
            WireOp::Delete(PathRef::Omitted),
            WireOp::Truncate(PathRef::Omitted, 3),
            WireOp::Patch {
                path: PathRef::Omitted,
                index: 1,
                remove: 0,
                items: vec![json!("y")],
            },
        ];
        let json = wire_ops_to_json(&ops);
        assert_eq!(wire_ops_from_json(&json).unwrap(), ops);
    }

    #[test]
    fn base_detection() {
        assert!(is_base(&[Op::Replace(json!(null))]));
        assert!(!is_base(&[]));
        assert!(!is_base(&[Op::Delete(NonEmptyPath::from_keys(&["a"]))]));
    }
}
