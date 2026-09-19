//! Strict-JSON judgement and normalisation — Rust port of `packages/chord/src/json.ts`.
//!
//! The Rust port represents a JSON value with [`serde_json::Value`], which can
//! only hold finite numbers, plain objects and dense arrays, so the upstream
//! `isJsonValue` walk is mostly a structural guarantee here. The walk is kept
//! because (a) it enforces the same depth cap the TypeScript implementation
//! enforces, and (b) it gives callers an explicit check to run on values built
//! by hand rather than parsed by `serde_json`.
//!
//! The two runtimes differ in one visible way: JavaScript objects may carry
//! `undefined` values, sparse arrays and accessors, none of which exist in
//! `serde_json::Value`. Those are already rejected by the type, so the checks
//! for them collapse into "cannot happen".

use serde_json::Value;

/// Maximum nesting depth accepted by [`is_json_value`].
///
/// Mirrors the `depth > 512` guard in the upstream walk.
pub const MAX_JSON_DEPTH: usize = 512;

/// The strict-JSON value type shared by Chord's public API.
pub type JsonValue = Value;

/// The JSON kind of a value, used for diagnostics and normalisation decisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JsonKind {
    /// `null`.
    Null,
    /// `true` / `false`.
    Boolean,
    /// A finite number.
    Number,
    /// A string.
    String,
    /// An array.
    Array,
    /// A plain object.
    Object,
}

impl JsonKind {
    /// A stable lower-case name matching upstream diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            JsonKind::Null => "null",
            JsonKind::Boolean => "boolean",
            JsonKind::Number => "number",
            JsonKind::String => "string",
            JsonKind::Array => "array",
            JsonKind::Object => "object",
        }
    }
}

impl std::fmt::Display for JsonKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Return the [`JsonKind`] of a value.
pub fn json_kind(value: &JsonValue) -> JsonKind {
    match value {
        Value::Null => JsonKind::Null,
        Value::Bool(_) => JsonKind::Boolean,
        Value::Number(_) => JsonKind::Number,
        Value::String(_) => JsonKind::String,
        Value::Array(_) => JsonKind::Array,
        Value::Object(_) => JsonKind::Object,
    }
}

/// Return whether a value is finite strict JSON with plain objects, no cycles
/// and a nesting depth of at most [`MAX_JSON_DEPTH`].
///
/// `serde_json::Value` cannot represent a cycle, an accessor, a sparse array,
/// a non-finite number or a non-plain object, so for values parsed by
/// `serde_json` this always returns `true`. It stays useful as a depth guard
/// for values assembled programmatically.
pub fn is_json_value(value: &JsonValue) -> bool {
    check(value, 0)
}

/// Assert [`is_json_value`], returning a description of the first violation.
pub fn assert_json_value(value: &JsonValue) -> Result<(), String> {
    check_verbose(value, 0)
}

fn check(value: &JsonValue, depth: usize) -> bool {
    if depth > MAX_JSON_DEPTH {
        return false;
    }
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => true,
        Value::Number(_) => true,
        Value::Array(items) => items.iter().all(|item| check(item, depth + 1)),
        Value::Object(entries) => entries.values().all(|item| check(item, depth + 1)),
    }
}

fn check_verbose(value: &JsonValue, depth: usize) -> Result<(), String> {
    if depth > MAX_JSON_DEPTH {
        return Err(format!("value nests deeper than {MAX_JSON_DEPTH} levels"));
    }
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) | Value::Number(_) => Ok(()),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                check_verbose(item, depth + 1).map_err(|error| format!("[{index}]: {error}"))?;
            }
            Ok(())
        }
        Value::Object(entries) => {
            for (key, item) in entries {
                check_verbose(item, depth + 1).map_err(|error| format!("[{key}]: {error}"))?;
            }
            Ok(())
        }
    }
}

/// Rebuild a value into canonical strict JSON.
///
/// The port keeps a normaliser even though [`is_json_value`] accepts every
/// `serde_json::Value`, because callers that build values from external input
/// (a JS shim, a database blob) want one function that both validates and
/// detaches. Objects with duplicate keys are already collapsed by
/// `serde_json`'s map type, and the traversal clones the tree, so the result
/// shares no memory with the input.
pub fn normalize_json(value: &JsonValue) -> Result<JsonValue, String> {
    assert_json_value(value)?;
    Ok(value.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_every_json_shape() {
        for value in [
            json!(null),
            json!(true),
            json!(0),
            json!(-0.5),
            json!(""),
            json!([]),
            json!({}),
            json!({ "a": [1, { "b": null }] }),
        ] {
            assert!(is_json_value(&value), "{value} should be JSON");
            assert_eq!(normalize_json(&value).unwrap(), value);
        }
    }

    #[test]
    fn rejects_excessive_nesting() {
        let mut value = json!(null);
        for _ in 0..(MAX_JSON_DEPTH + 2) {
            value = Value::Array(vec![value]);
        }
        assert!(!is_json_value(&value));
        assert!(assert_json_value(&value).is_err());
    }

    #[test]
    fn reports_a_path_for_the_violation() {
        let mut value = json!(null);
        for _ in 0..(MAX_JSON_DEPTH + 2) {
            value = Value::Array(vec![value]);
        }
        assert!(assert_json_value(&value).unwrap_err().starts_with("[0]:"));
    }

    #[test]
    fn names_each_kind() {
        assert_eq!(json_kind(&json!([])), JsonKind::Array);
        assert_eq!(json_kind(&json!({})).as_str(), "object");
        assert_eq!(JsonKind::Null.to_string(), "null");
    }
}
