//! Tool-call argument coercion against a tool's JSON Schema.
//!
//! Rust port of the coercion half of `packages/ai/src/utils/validation.ts`.
//! Upstream validates every model-issued tool call with TypeBox and then
//! *coerces* values the model mis-typed before handing them to the tool:
//! `"5"` becomes `5`, `"true"` becomes `true`, `1` becomes `true`, an
//! unexpected `null` on an optional property is dropped. Models emit those
//! shapes often enough that strict `serde_json::from_value` would otherwise
//! fail a call the TypeScript runtime accepts.
//!
//! This module ports the coercion path faithfully (primitive coercion,
//! `allOf` / `anyOf` / `oneOf` union selection, nested objects and arrays,
//! `additionalProperties` propagation, and optional-null normalization) but
//! *not* the TypeBox validation step that follows it. Final validation stays
//! where it already lives: the per-tool `serde_json::from_value` call, which
//! rejects anything still ill-typed. That keeps the change dependency-free —
//! `jsonschema` is a workspace dependency but is not used here, and adding a
//! resolver step would change `Cargo.lock`.
//!
//! Deliberate deviations from upstream, both driven by the same choice:
//!
//! * Union selection (`anyOf` / `oneOf`) uses [`schema_accepts`], a small
//!   structural checker, instead of a compiled TypeBox validator. It
//!   understands `type`, `required`, `properties`, `additionalProperties`,
//!   `items`, `$ref` (permissive) and the three combinators; unknown keywords
//!   are treated as unconstrained so it never rejects a value upstream would
//!   accept into the wrong branch.
//! * Upstream runs TypeBox `Value.Convert` (which also applies `default`) and
//!   only then the plain-JSON-Schema coercion pass. The built-in tools declare
//!   plain JSON Schemas, so this module always runs the coercion pass;
//!   `default` application is skipped because every built-in tool uses
//!   `#[serde(default)]` instead.

use std::collections::HashSet;

use serde_json::{Map, Value};

/// Coerce a model-issued tool-call argument object against `parameters`.
///
/// Returns the coerced arguments; the input is left untouched. Callers that
/// need upstream's exact failure behaviour should keep their own strict
/// deserialization afterwards — this function never errors.
pub fn coerce_tool_arguments(parameters: &Value, arguments: &Value) -> Value {
    let mut args = arguments.clone();
    normalize_optional_nulls(&mut args, parameters);
    coerce_with_json_schema(args, parameters)
}

/// JSON Schema `type` values declared by `schema`, if any.
fn schema_types(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(ty)) => vec![ty.as_str()],
        Some(Value::Array(types)) => types.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

/// JS `typeof`-style membership test used by upstream `matchesJsonType`.
fn matches_json_type(value: &Value, ty: &str) -> bool {
    match ty {
        "number" => value.is_number(),
        "integer" => match value {
            Value::Number(number) => {
                number.is_i64() || number.is_u64() || number.as_f64().is_some_and(is_integral)
            }
            _ => false,
        },
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "null" => value.is_null(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

fn is_integral(number: f64) -> bool {
    number.is_finite() && number.fract() == 0.0
}

/// Build a JSON number, preferring the integer representation so a coerced
/// `"5"` deserializes into an `i64` field the same way an emitted `5` does.
fn json_number(number: f64) -> Value {
    if is_integral(number) && number >= i64::MIN as f64 && number <= i64::MAX as f64 {
        Value::Number((number as i64).into())
    } else {
        serde_json::Number::from_f64(number)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}

/// Parse a string to an integer-valued JSON number, mirroring JS `Number()`.
fn parse_json_number(text: &str) -> Option<Value> {
    let parsed: f64 = text.trim().parse().ok()?;
    if parsed.is_finite() {
        Some(json_number(parsed))
    } else {
        None
    }
}

/// Upstream `coercePrimitiveByType`: convert `value` to `ty` when the shape
/// allows it, otherwise return it unchanged.
fn coerce_primitive_by_type(value: &Value, ty: &str) -> Value {
    match ty {
        "number" => match value {
            Value::Null => json_number(0.0),
            Value::String(text) if !text.trim().is_empty() => {
                parse_json_number(text).unwrap_or_else(|| value.clone())
            }
            Value::Bool(flag) => json_number(if *flag { 1.0 } else { 0.0 }),
            _ => value.clone(),
        },
        "integer" => match value {
            Value::Null => json_number(0.0),
            Value::String(text) if !text.trim().is_empty() => match parse_json_number(text) {
                Some(parsed) if matches_json_type(&parsed, "integer") => parsed,
                _ => value.clone(),
            },
            Value::Bool(flag) => json_number(if *flag { 1.0 } else { 0.0 }),
            _ => value.clone(),
        },
        "boolean" => match value {
            Value::Null => Value::Bool(false),
            Value::String(text) if text == "true" => Value::Bool(true),
            Value::String(text) if text == "false" => Value::Bool(false),
            Value::Number(number) => match number.as_f64() {
                Some(1.0) => Value::Bool(true),
                Some(0.0) => Value::Bool(false),
                _ => value.clone(),
            },
            _ => value.clone(),
        },
        "string" => match value {
            Value::Null => Value::String(String::new()),
            Value::Number(number) => Value::String(number.to_string()),
            Value::Bool(flag) => Value::String(flag.to_string()),
            _ => value.clone(),
        },
        "null" => match value {
            Value::String(text) if text.is_empty() => Value::Null,
            Value::Number(number) if number.as_f64() == Some(0.0) => Value::Null,
            Value::Bool(false) => Value::Null,
            _ => value.clone(),
        },
        _ => value.clone(),
    }
}

/// Structural schema check used to pick a union branch or decide whether an
/// optional `null` is representable. Unknown keywords are ignored on purpose:
/// a false negative here would pick the wrong coercion branch.
fn schema_accepts(schema: &Value, value: &Value) -> bool {
    if let Some(all_of) = schema.get("allOf").and_then(Value::as_array) {
        if !all_of.iter().all(|nested| schema_accepts(nested, value)) {
            return false;
        }
    }
    if let Some(any_of) = schema.get("anyOf").and_then(Value::as_array) {
        if !any_of.iter().any(|nested| schema_accepts(nested, value)) {
            return false;
        }
    }
    if let Some(one_of) = schema.get("oneOf").and_then(Value::as_array) {
        if !one_of.iter().any(|nested| schema_accepts(nested, value)) {
            return false;
        }
    }

    let types = schema_types(schema);
    if !types.is_empty() && !types.iter().any(|ty| matches_json_type(value, ty)) {
        return false;
    }

    if types.contains(&"object") {
        if let Value::Object(map) = value {
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                if required
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|key| !map.contains_key(key))
                {
                    return false;
                }
            }
            let properties = schema.get("properties").and_then(Value::as_object);
            let additional = schema.get("additionalProperties");
            for (key, item) in map {
                if let Some(property_schema) = properties.and_then(|props| props.get(key)) {
                    if !schema_accepts(property_schema, item) {
                        return false;
                    }
                } else if additional == Some(&Value::Bool(false)) {
                    return false;
                }
            }
        }
    }

    if types.contains(&"array") {
        if let (Value::Array(items), Some(schema_items)) = (value, schema.get("items")) {
            if let Some(tuple) = schema_items.as_array() {
                for (index, item) in items.iter().enumerate() {
                    if let Some(item_schema) = tuple.get(index) {
                        if !schema_accepts(item_schema, item) {
                            return false;
                        }
                    }
                }
            } else {
                for item in items {
                    if !schema_accepts(schema_items, item) {
                        return false;
                    }
                }
            }
        }
    }

    true
}

/// Upstream `coerceWithUnionSchema`: keep the value if any branch already
/// accepts it, otherwise try each branch's coercion and take the first that
/// then validates.
fn coerce_with_union_schema(value: Value, schemas: &[Value]) -> Value {
    for schema in schemas {
        if schema_accepts(schema, &value) {
            return value;
        }
    }
    for schema in schemas {
        let candidate = coerce_with_json_schema(value.clone(), schema);
        if schema_accepts(schema, &candidate) {
            return candidate;
        }
    }
    value
}

fn coerce_object(mut map: Map<String, Value>, schema: &Value) -> Map<String, Value> {
    let properties = schema.get("properties").and_then(Value::as_object);
    let defined: HashSet<&str> = properties
        .map(|props| props.keys().map(String::as_str).collect())
        .unwrap_or_default();

    if let Some(props) = properties {
        let keys: Vec<String> = props
            .keys()
            .filter(|key| map.contains_key(key.as_str()))
            .cloned()
            .collect();
        for key in keys {
            let property_schema = props.get(&key).expect("key came from props");
            let item = map.remove(&key).expect("key came from map");
            map.insert(key, coerce_with_json_schema(item, property_schema));
        }
    }

    if let Some(additional) = schema
        .get("additionalProperties")
        .filter(|value| value.is_object())
    {
        let keys: Vec<String> = map
            .keys()
            .filter(|key| !defined.contains(key.as_str()))
            .cloned()
            .collect();
        for key in keys {
            let item = map.remove(&key).expect("key came from map");
            map.insert(key, coerce_with_json_schema(item, additional));
        }
    }

    map
}

fn coerce_array(items: Vec<Value>, schema: &Value) -> Vec<Value> {
    match schema.get("items") {
        Some(Value::Array(tuple)) => items
            .into_iter()
            .enumerate()
            .map(|(index, item)| match tuple.get(index) {
                Some(item_schema) => coerce_with_json_schema(item, item_schema),
                None => item,
            })
            .collect(),
        Some(item_schema) if item_schema.is_object() => items
            .into_iter()
            .map(|item| coerce_with_json_schema(item, item_schema))
            .collect(),
        _ => items,
    }
}

/// Upstream `coerceWithJsonSchema` (plain JSON Schema branch).
fn coerce_with_json_schema(value: Value, schema: &Value) -> Value {
    let mut next = value;

    if let Some(all_of) = schema.get("allOf").and_then(Value::as_array) {
        for nested in all_of {
            next = coerce_with_json_schema(next, nested);
        }
    }
    if let Some(any_of) = schema.get("anyOf").and_then(Value::as_array) {
        next = coerce_with_union_schema(next, any_of);
    }
    if let Some(one_of) = schema.get("oneOf").and_then(Value::as_array) {
        next = coerce_with_union_schema(next, one_of);
    }

    let types = schema_types(schema);
    let matches_union_member =
        types.len() > 1 && types.iter().any(|ty| matches_json_type(&next, ty));
    if !types.is_empty() && !matches_union_member {
        for ty in &types {
            let candidate = coerce_primitive_by_type(&next, ty);
            if candidate != next {
                next = candidate;
                break;
            }
        }
    }

    if types.contains(&"object") {
        if let Value::Object(map) = next {
            next = Value::Object(coerce_object(map, schema));
        }
    }
    if types.contains(&"array") {
        if let Value::Array(items) = next {
            next = Value::Array(coerce_array(items, schema));
        }
    }

    next
}

/// Upstream `normalizeOptionalNulls`: drop a `null` on an optional property
/// the schema does not allow, recursing through objects and arrays.
fn normalize_optional_nulls(value: &mut Value, schema: &Value) {
    match value {
        Value::Array(items) => {
            if let Some(schema_items) = schema.get("items") {
                if let Some(tuple) = schema_items.as_array() {
                    for (index, item) in items.iter_mut().enumerate() {
                        if let Some(item_schema) = tuple.get(index) {
                            normalize_optional_nulls(item, item_schema);
                        }
                    }
                } else if schema_items.is_object() {
                    for item in items {
                        normalize_optional_nulls(item, schema_items);
                    }
                }
            }
        }
        Value::Object(map) => {
            let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
                return;
            };
            let required: HashSet<&str> = schema
                .get("required")
                .and_then(Value::as_array)
                .map(|names| names.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let keys: Vec<String> = properties
                .keys()
                .filter(|key| map.contains_key(key.as_str()))
                .cloned()
                .collect();
            for key in keys {
                let Some(property_schema) = properties.get(&key) else {
                    continue;
                };
                let is_ref = property_schema.get("$ref").is_some_and(Value::is_string);
                let drop_null = {
                    let slot = map.get_mut(&key).expect("key came from map");
                    slot.is_null()
                        && !required.contains(key.as_str())
                        && !is_ref
                        && !schema_accepts(property_schema, &Value::Null)
                };
                if drop_null {
                    map.remove(&key);
                } else if let Some(slot) = map.get_mut(&key) {
                    normalize_optional_nulls(slot, property_schema);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn integer_string_is_coerced() {
        let schema = json!({
            "type": "object",
            "properties": { "limit": { "type": "integer" } }
        });
        let coerced = coerce_tool_arguments(&schema, &json!({ "limit": "5" }));
        assert_eq!(coerced["limit"], json!(5));
        assert!(coerced["limit"].is_i64());
    }

    #[test]
    fn non_integral_string_is_left_alone_for_integer_schema() {
        let schema = json!({ "type": "integer" });
        assert_eq!(coerce_tool_arguments(&schema, &json!("5.5")), json!("5.5"));
    }

    #[test]
    fn number_accepts_boolean_and_string() {
        let schema = json!({ "type": "number" });
        assert_eq!(coerce_tool_arguments(&schema, &json!(true)), json!(1));
        assert_eq!(coerce_tool_arguments(&schema, &json!("2.5")), json!(2.5));
        assert_eq!(coerce_tool_arguments(&schema, &json!("")), json!(""));
    }

    #[test]
    fn boolean_accepts_strings_and_numbers() {
        let schema = json!({ "type": "boolean" });
        assert_eq!(coerce_tool_arguments(&schema, &json!("true")), json!(true));
        assert_eq!(
            coerce_tool_arguments(&schema, &json!("false")),
            json!(false)
        );
        assert_eq!(coerce_tool_arguments(&schema, &json!(1)), json!(true));
        assert_eq!(coerce_tool_arguments(&schema, &json!(0)), json!(false));
        assert_eq!(coerce_tool_arguments(&schema, &json!(2)), json!(2));
    }

    #[test]
    fn string_accepts_numbers_and_booleans() {
        let schema = json!({ "type": "string" });
        assert_eq!(coerce_tool_arguments(&schema, &json!(7)), json!("7"));
        assert_eq!(
            coerce_tool_arguments(&schema, &json!(false)),
            json!("false")
        );
        assert_eq!(coerce_tool_arguments(&schema, &json!(true)), json!("true"));
    }

    #[test]
    fn optional_null_is_dropped_but_required_null_is_kept() {
        let schema = json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string" },
                "offset": { "type": "integer" }
            }
        });
        let coerced = coerce_tool_arguments(&schema, &json!({ "path": null, "offset": null }));
        assert_eq!(coerced["path"], json!(""));
        assert!(coerced.get("offset").is_none(), "optional null should drop");
    }

    #[test]
    fn nullable_property_keeps_its_null() {
        let schema = json!({
            "type": "object",
            "properties": { "note": { "type": ["string", "null"] } }
        });
        let coerced = coerce_tool_arguments(&schema, &json!({ "note": null }));
        assert_eq!(coerced["note"], Value::Null);
    }

    #[test]
    fn nested_objects_and_arrays_are_coerced() {
        let schema = json!({
            "type": "object",
            "properties": {
                "options": {
                    "type": "object",
                    "properties": { "timeout": { "type": "integer" } }
                },
                "ids": { "type": "array", "items": { "type": "integer" } }
            }
        });
        let coerced = coerce_tool_arguments(
            &schema,
            &json!({ "options": { "timeout": "30" }, "ids": ["1", 2, "3"] }),
        );
        assert_eq!(coerced["options"]["timeout"], json!(30));
        assert_eq!(coerced["ids"], json!([1, 2, 3]));
    }

    #[test]
    fn union_string_member_is_not_coerced() {
        let schema = json!({ "type": ["number", "string"] });
        assert_eq!(
            coerce_tool_arguments(&schema, &json!("5")),
            json!("5"),
            "a string is already a valid union member"
        );
    }

    #[test]
    fn any_of_picks_the_coercible_branch() {
        let schema = json!({
            "anyOf": [
                { "type": "integer" },
                { "type": "array", "items": { "type": "string" } }
            ]
        });
        assert_eq!(coerce_tool_arguments(&schema, &json!("12")), json!(12));
    }

    #[test]
    fn additional_properties_schema_propagates() {
        let schema = json!({
            "type": "object",
            "properties": { "known": { "type": "string" } },
            "additionalProperties": { "type": "integer" }
        });
        let coerced = coerce_tool_arguments(&schema, &json!({ "known": 1, "extra": "9" }));
        assert_eq!(coerced["known"], json!("1"));
        assert_eq!(coerced["extra"], json!(9));
    }

    #[test]
    fn tuple_items_are_coerced_positionally() {
        let schema = json!({
            "type": "array",
            "items": [{ "type": "integer" }, { "type": "boolean" }]
        });
        assert_eq!(
            coerce_tool_arguments(&schema, &json!(["4", "true", "left"])),
            json!([4, true, "left"])
        );
    }

    #[test]
    fn no_change_for_already_valid_arguments() {
        let schema = json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "limit": { "type": "integer" }
            }
        });
        let args = json!({ "path": "/tmp/x", "limit": 10 });
        assert_eq!(coerce_tool_arguments(&schema, &args), args);
    }
}
