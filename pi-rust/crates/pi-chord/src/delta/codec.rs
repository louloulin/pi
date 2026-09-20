//! Wire codec: [`Encoder`] interns paths, [`Decoder`] resolves them.
//!
//! Rust port of the `encoder` / `decoder` half of
//! `packages/chord/src/delta/index.ts`.
//!
//! Two things are deliberately scoped to a batch:
//!
//! - **Arity omission** (`["p", 2, 0, [3]]` instead of `["p", ["xs"], 2, 0, [3]]`)
//!   lets a batch's first operation depend on the previous batch's last one, so
//!   a reader that skips or reorders a batch decodes into the wrong path.
//! - **Path ids** survive only until the next replacement, because a base batch
//!   is a recovery point: a reader replays from the last one with a fresh
//!   decoder, and an id defined before it would be unresolvable.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::path::{path_key, Path, Seg};
use super::{assert_safe_path, DeltaError, NonEmptyPath, Op, PathRef, WireOp};

/// Encodes decoded operations into the wire form.
///
/// Hold one encoder per stream. Ids are the only cross-batch state and are
/// reset by a replacement.
#[derive(Debug, Default)]
pub struct Encoder {
    seen: HashSet<String>,
    ids: HashMap<String, u64>,
    next_id: u64,
    previous: Option<String>,
}

impl Encoder {
    /// Create an encoder for a fresh stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// Encode one batch; every batch may open with a replacement.
    pub fn encode(&mut self, ops: &[Op]) -> Vec<WireOp> {
        self.previous = None;
        let mut out = Vec::with_capacity(ops.len());
        for op in ops {
            let Op::Replace(value) = op else {
                self.encode_one(op, &mut out);
                continue;
            };
            out.push(WireOp::Replace(value.clone()));
            self.seen.clear();
            self.ids.clear();
            self.next_id = 0;
            self.previous = None;
        }
        out
    }

    fn encode_one(&mut self, op: &Op, out: &mut Vec<WireOp>) {
        let path = op.path();
        let key = path_key(path);

        // Same path as the previous op: drop the ref entirely.
        if self.previous.as_deref() == Some(key.as_str()) {
            out.push(short_form(op));
            return;
        }

        let path_ref = if let Some(id) = self.ids.get(&key) {
            PathRef::Id(*id)
        } else if self.seen.contains(&key) {
            // Second use: define, then reference.
            let id = self.next_id;
            self.next_id += 1;
            self.ids.insert(key.clone(), id);
            out.push(WireOp::Define {
                id,
                path: path.to_vec(),
            });
            PathRef::Id(id)
        } else {
            // First use: inline.
            self.seen.insert(key.clone());
            PathRef::Inline(path.to_vec())
        };
        out.push(with_ref(op, path_ref));
        self.previous = Some(key);
    }
}

/// Decodes wire operations back into decoded operations.
///
/// Hold one decoder per stream; the path dictionary persists across batches and
/// is reset by a replacement.
#[derive(Debug, Default)]
pub struct Decoder {
    paths: HashMap<u64, Path>,
}

impl Decoder {
    /// Create a decoder for a fresh stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// Decode one batch, resolving ids and short forms.
    pub fn decode(&mut self, wire: &[WireOp]) -> Result<Vec<Op>, DeltaError> {
        let mut previous: Option<Path> = None;
        let mut out = Vec::with_capacity(wire.len());
        for op in wire {
            match op {
                WireOp::Define { id, path } => {
                    assert_safe_path(path)?;
                    self.paths.insert(*id, path.clone());
                    continue;
                }
                WireOp::Replace(value) => {
                    out.push(Op::Replace(value.clone()));
                    self.paths.clear();
                    previous = None;
                    continue;
                }
                _ => {}
            }

            let (path_ref, body) = split(op);
            let short = matches!(path_ref, PathRef::Omitted);
            let path = if short {
                previous
                    .clone()
                    .ok_or_else(|| DeltaError::Path("[]".to_owned()))?
            } else {
                let resolved = match path_ref {
                    PathRef::Id(id) => self.paths.get(id).cloned().ok_or_else(|| {
                        DeltaError::Path(serde_json::Value::Number((*id).into()).to_string())
                    })?,
                    PathRef::Inline(path) => path.clone(),
                    PathRef::Omitted => previous.clone().expect("short form checked above"),
                };
                previous = Some(resolved.clone());
                resolved
            };

            out.push(body.into_op(path)?);
        }
        Ok(out)
    }
}

/// The payload of a non-`#`, non-`r` wire operation.
#[derive(Debug)]
enum Body {
    Set(Value),
    Delete,
    Append(String),
    Truncate(u64),
    Patch {
        index: u64,
        remove: u64,
        items: Vec<Value>,
    },
}

impl Body {
    fn into_op(self, path: Path) -> Result<Op, DeltaError> {
        match self {
            Body::Patch {
                index,
                remove,
                items,
            } => Ok(Op::Patch {
                path,
                index,
                remove,
                items,
            }),
            Body::Set(value) => Ok(Op::Set(non_empty(&path)?, value)),
            Body::Delete => Ok(Op::Delete(non_empty(&path)?)),
            Body::Append(text) => Ok(Op::Append(non_empty(&path)?, text)),
            Body::Truncate(count) => Ok(Op::Truncate(non_empty(&path)?, count)),
        }
    }
}

fn non_empty(path: &[Seg]) -> Result<NonEmptyPath, DeltaError> {
    NonEmptyPath::try_new(path.to_vec()).map_err(|_| DeltaError::Path(path_json(path)))
}

fn path_json(path: &[Seg]) -> String {
    serde_json::Value::Array(path.iter().map(Seg::to_json).collect()).to_string()
}

fn split(op: &WireOp) -> (&PathRef, Body) {
    match op {
        WireOp::Set(path, value) => (path, Body::Set(value.clone())),
        WireOp::Delete(path) => (path, Body::Delete),
        WireOp::Append(path, text) => (path, Body::Append(text.clone())),
        WireOp::Truncate(path, count) => (path, Body::Truncate(*count)),
        WireOp::Patch {
            path,
            index,
            remove,
            items,
        } => (
            path,
            Body::Patch {
                index: *index,
                remove: *remove,
                items: items.clone(),
            },
        ),
        WireOp::Replace(_) | WireOp::Define { .. } => {
            unreachable!("replace and define are handled before split")
        }
    }
}

fn short_form(op: &Op) -> WireOp {
    match op {
        Op::Replace(value) => WireOp::Replace(value.clone()),
        Op::Set(_, value) => WireOp::Set(PathRef::Omitted, value.clone()),
        Op::Delete(_) => WireOp::Delete(PathRef::Omitted),
        Op::Append(_, text) => WireOp::Append(PathRef::Omitted, text.clone()),
        Op::Truncate(_, count) => WireOp::Truncate(PathRef::Omitted, *count),
        Op::Patch {
            index,
            remove,
            items,
            ..
        } => WireOp::Patch {
            path: PathRef::Omitted,
            index: *index,
            remove: *remove,
            items: items.clone(),
        },
    }
}

fn with_ref(op: &Op, path: PathRef) -> WireOp {
    match op {
        Op::Replace(value) => WireOp::Replace(value.clone()),
        Op::Set(_, value) => WireOp::Set(path, value.clone()),
        Op::Delete(_) => WireOp::Delete(path),
        Op::Append(_, text) => WireOp::Append(path, text.clone()),
        Op::Truncate(_, count) => WireOp::Truncate(path, *count),
        Op::Patch {
            index,
            remove,
            items,
            ..
        } => WireOp::Patch {
            path,
            index: *index,
            remove: *remove,
            items: items.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta::{wire_ops_from_json, wire_ops_to_json};
    use serde_json::{json, Value};

    fn ops(tuples: &[Value]) -> Vec<Op> {
        tuples.iter().map(|op| Op::from_json(op).unwrap()).collect()
    }

    #[test]
    fn interns_a_path_on_its_second_use() {
        let batch = ops(&[
            json!(["s", ["a"], 1]),
            json!(["s", ["b"], 2]),
            json!(["s", ["a"], 3]),
        ]);
        let wire = Encoder::new().encode(&batch);
        assert_eq!(
            wire_ops_to_json(&wire),
            json!([
                ["s", ["a"], 1],
                ["s", ["b"], 2],
                ["#", 0, ["a"]],
                ["s", 0, 3]
            ])
        );
        assert_eq!(Decoder::new().decode(&wire).unwrap(), batch);
    }

    #[test]
    fn omits_the_path_when_it_repeats() {
        let batch = ops(&[json!(["t", ["s"], 1]), json!(["a", ["s"], "x"])]);
        let wire = Encoder::new().encode(&batch);
        assert_eq!(
            wire_ops_to_json(&wire),
            json!([["t", ["s"], 1], ["a", "x"]])
        );
        assert_eq!(Decoder::new().decode(&wire).unwrap(), batch);
    }

    #[test]
    fn path_keys_do_not_collide_on_null_characters() {
        let batch = ops(&[
            json!(["s", ["a\u{0}b"], 1]),
            json!(["s", ["a", "b"], 2]),
            json!(["s", ["a\u{0}b"], 3]),
        ]);
        let wire = Encoder::new().encode(&batch);
        assert_eq!(Decoder::new().decode(&wire).unwrap(), batch);
    }

    #[test]
    fn replacement_resets_the_dictionary() {
        let batch = ops(&[
            json!(["s", ["a"], 1]),
            json!(["r", {}]),
            json!(["s", ["a"], 2]),
        ]);
        let wire = Encoder::new().encode(&batch);
        assert_eq!(
            wire_ops_to_json(&wire),
            json!([["s", ["a"], 1], ["r", {}], ["s", ["a"], 2]])
        );
    }

    #[test]
    fn decode_rejects_an_unresolvable_id() {
        let wire = wire_ops_from_json(&json!([["s", 7, 1]])).unwrap();
        assert!(matches!(
            Decoder::new().decode(&wire),
            Err(DeltaError::Path(_))
        ));
    }

    #[test]
    fn decode_rejects_a_leading_short_form() {
        let wire = wire_ops_from_json(&json!([["d"]])).unwrap();
        assert!(matches!(
            Decoder::new().decode(&wire),
            Err(DeltaError::Path(_))
        ));
    }
}
