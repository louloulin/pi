//! Path vocabulary and path safety — the `Path` / `Seg` half of
//! `packages/chord/src/delta/index.ts`.
//!
//! A path is a sequence of object keys and array indices. Keys are strings,
//! indices are non-negative integers; the type split is what lets the applier
//! reject a string-spelled array index (`["xs", "7"]`) without guessing.

use std::fmt;

use super::DeltaError;

/// One segment (component) of a delta path.
///
/// Serialises to a JSON string or a JSON integer, exactly like the upstream
/// `Seg` (`string | number`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Seg {
    /// An object key.
    Key(String),
    /// An array index.
    Index(u64),
}

impl Seg {
    /// Build a key segment.
    pub fn key(key: impl Into<String>) -> Self {
        Seg::Key(key.into())
    }

    /// Build an index segment.
    pub fn index(index: u64) -> Self {
        Seg::Index(index)
    }

    /// Borrow the segment as a key, if it is one.
    pub fn as_key(&self) -> Option<&str> {
        match self {
            Seg::Key(key) => Some(key),
            Seg::Index(_) => None,
        }
    }

    /// Return the segment as an index, if it is one.
    pub fn as_index(&self) -> Option<u64> {
        match self {
            Seg::Key(_) => None,
            Seg::Index(index) => Some(*index),
        }
    }

    /// Convert the segment into JSON.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Seg::Key(key) => serde_json::Value::String(key.clone()),
            Seg::Index(index) => serde_json::Value::Number((*index).into()),
        }
    }

    /// Parse a segment from JSON.
    ///
    /// Only a string or a non-negative integer is accepted. Anything else is an
    /// unsafe segment, matching upstream `assertSafePath`, which rejects
    /// non-integer numbers and non-strings with the same error.
    pub fn from_json(value: &serde_json::Value) -> Result<Self, DeltaError> {
        match value {
            serde_json::Value::String(key) => Ok(Seg::Key(key.clone())),
            serde_json::Value::Number(number) if number.as_u64().is_some() => {
                Ok(Seg::Index(number.as_u64().expect("checked above")))
            }
            other => Err(DeltaError::UnsafePath(other.to_string())),
        }
    }
}

impl From<&str> for Seg {
    fn from(value: &str) -> Self {
        Seg::Key(value.to_owned())
    }
}

impl From<String> for Seg {
    fn from(value: String) -> Self {
        Seg::Key(value)
    }
}

impl From<usize> for Seg {
    fn from(value: usize) -> Self {
        Seg::Index(value as u64)
    }
}

impl From<u64> for Seg {
    fn from(value: u64) -> Self {
        Seg::Index(value)
    }
}

impl fmt::Display for Seg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Seg::Key(key) => write!(f, "{key}"),
            Seg::Index(index) => write!(f, "{index}"),
        }
    }
}

/// A path from the root to a value. May be empty (the root itself).
pub type Path = Vec<Seg>;

/// A path that is guaranteed to have at least one segment.
///
/// `s`, `d`, `a` and `t` cannot target the root; the type is what enforces the
/// upstream comment "the type forbids it".
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NonEmptyPath(Vec<Seg>);

impl NonEmptyPath {
    /// Build a non-empty path, rejecting the empty one.
    pub fn try_new(path: Vec<Seg>) -> Result<Self, DeltaError> {
        if path.is_empty() {
            return Err(DeltaError::InvalidOp("path is empty".to_owned()));
        }
        Ok(NonEmptyPath(path))
    }

    /// Build a non-empty path from string keys. Primarily a test convenience.
    pub fn from_keys(keys: &[&str]) -> Self {
        Self::try_new(keys.iter().map(|key| Seg::Key((*key).to_owned())).collect())
            .expect("from_keys always receives at least one key")
    }

    /// Borrow the segments.
    pub fn as_slice(&self) -> &[Seg] {
        &self.0
    }

    /// Clone the segments into a plain [`Path`].
    pub fn to_path(&self) -> Path {
        self.0.clone()
    }

    /// Consume the wrapper and return the segments.
    pub fn into_path(self) -> Path {
        self.0
    }
}

impl AsRef<[Seg]> for NonEmptyPath {
    fn as_ref(&self) -> &[Seg] {
        &self.0
    }
}

impl std::ops::Deref for NonEmptyPath {
    type Target = [Seg];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Segments that reach the prototype chain.
///
/// `JSON.parse` is safe on its own — it makes `__proto__` an own property — but
/// an applier writing `parent[key] = value` is not, and paths are data: a
/// `["s", ["__proto__", "isAdmin"], true]` from a plugin or a tool echo would
/// pollute `Object.prototype` for the whole process. Rust has no prototype
/// chain, so this is not a memory-safety issue here; the port keeps the rule
/// because a Rust-produced batch is applied by the JavaScript runtime too, and
/// because rejecting an ambiguous key is cheaper than reasoning about which
/// consumer is which.
pub const RESERVED_SEGMENTS: [&str; 3] = ["__proto__", "constructor", "prototype"];

/// Return whether a key segment reaches the prototype chain.
pub fn is_reserved_segment(segment: &Seg) -> bool {
    matches!(segment, Seg::Key(key) if RESERVED_SEGMENTS.contains(&key.as_str()))
}

/// Reject paths that contain a reserved or malformed segment.
pub fn assert_safe_path(path: &[Seg]) -> Result<(), DeltaError> {
    for segment in path {
        if is_reserved_segment(segment) {
            return Err(DeltaError::UnsafePath(segment.to_string()));
        }
    }
    Ok(())
}

/// Serialise a path to a JSON array.
pub fn path_to_json(path: &[Seg]) -> serde_json::Value {
    serde_json::Value::Array(path.iter().map(Seg::to_json).collect())
}

/// Build an "unresolvable path" error carrying the JSON form of the path.
///
/// Matches the upstream `PathError` message (`unresolvable path: ["a",0]`).
pub(crate) fn path_error(path: &[Seg]) -> DeltaError {
    DeltaError::Path(path_to_json(path).to_string())
}

/// Parse a path from a JSON array.
pub fn path_from_json(value: &serde_json::Value) -> Result<Path, DeltaError> {
    let items = value
        .as_array()
        .ok_or_else(|| DeltaError::InvalidOp("path is not an array".to_owned()))?;
    items.iter().map(Seg::from_json).collect()
}

/// Canonical, collision-free key for a path.
///
/// The upstream encoder keys its dictionary on `JSON.stringify(path)`. A JSON
/// string is not used here because the encoder never has to agree with the
/// TypeScript one — the dictionary is local to a stream — and a length-prefixed
/// form is cheaper and cannot collide on a key containing `\0`
/// (`["a\0b"]` vs `["a", "b"]`).
pub(crate) fn path_key(path: &[Seg]) -> String {
    let mut key = String::new();
    for segment in path {
        match segment {
            Seg::Key(value) => {
                key.push('s');
                key.push_str(&value.len().to_string());
                key.push(':');
                key.push_str(value);
            }
            Seg::Index(index) => {
                key.push('i');
                key.push_str(&index.to_string());
                key.push(';');
            }
        }
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_and_serialises_segments() {
        assert_eq!(Seg::from_json(&json!("a")).unwrap(), Seg::Key("a".into()));
        assert_eq!(Seg::from_json(&json!(7)).unwrap(), Seg::Index(7));
        assert!(matches!(
            Seg::from_json(&json!(1.5)),
            Err(DeltaError::UnsafePath(_))
        ));
        assert!(matches!(
            Seg::from_json(&json!(true)),
            Err(DeltaError::UnsafePath(_))
        ));
        assert_eq!(Seg::Index(3).to_json(), json!(3));
    }

    #[test]
    fn rejects_empty_non_empty_paths() {
        assert!(NonEmptyPath::try_new(vec![]).is_err());
        assert_eq!(
            NonEmptyPath::from_keys(&["a", "b"]).as_slice(),
            &[Seg::Key("a".into()), Seg::Key("b".into())]
        );
    }

    #[test]
    fn rejects_reserved_segments() {
        assert!(assert_safe_path(&[Seg::key("a")]).is_ok());
        assert!(assert_safe_path(&[Seg::key("__proto__")]).is_err());
        assert!(assert_safe_path(&[Seg::index(0), Seg::key("constructor")]).is_err());
    }

    #[test]
    fn path_keys_do_not_collide() {
        assert_ne!(
            path_key(&[Seg::Key("a\0b".into())]),
            path_key(&[Seg::Key("a".into()), Seg::Key("b".into())])
        );
        assert_eq!(
            path_key(&[Seg::Key("a".into()), Seg::Index(0)]),
            path_key(&[Seg::Key("a".into()), Seg::Index(0)])
        );
    }
}
