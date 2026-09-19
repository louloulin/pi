//! Tolerant JSON parsing — the Rust port of `packages/ai/src/utils/json-parse.ts`.
//!
//! Providers regularly emit JSON that is *almost* valid: Claude streams a raw
//! control character inside a string literal instead of its `\n` / `\t`
//! escape, a gateway doubles (or forgets to double) a backslash, and a
//! tool-call argument payload is still being assembled when the UI wants to
//! show it. Upstream funnels every such payload through `parseJsonWithRepair`
//! (SSE event frames) and `parseStreamingJson` (tool-call arguments); this
//! module reproduces both so the Rust providers degrade the same way instead
//! of failing the whole turn.
//!
//! * [`repair_json`] mirrors upstream `repairJson`: escape raw control
//!   characters inside string literals and double backslashes that do not
//!   introduce a valid escape. It is the identity function on valid JSON.
//! * [`parse_json_with_repair`] is `JSON.parse` with the repair retry: a
//!   strict parse first, then a repaired parse, otherwise the original error.
//! * [`parse_streaming_json`] is the total function used at tool-call
//!   finalisation. It never fails — an unrecoverable payload becomes `{}`,
//!   matching upstream's `return {} as T`.
//!
//! Known gap versus upstream: the partial fallback in `parseStreamingJson` is
//! the `partial-json` npm package, which walks the truncated document and
//! returns the longest value it could assemble (`{"a": [1, {` → `{"a": [1,
//! {}]}` — the half-built object survives as `{}`). [`close_partial_json`] is
//! a smaller, deterministic recovery: truncate to the last structurally
//! complete value and append the missing closers, so the same input yields
//! `{"a": [1]}` (the incomplete element is dropped, never filled in).

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// Escape sequences that are valid inside a JSON string literal — upstream
/// `VALID_JSON_ESCAPES`.
const VALID_JSON_ESCAPES: [char; 9] = ['"', '\\', '/', 'b', 'f', 'n', 'r', 't', 'u'];

/// Repairs malformed JSON string literals by escaping raw control characters
/// inside strings and doubling backslashes before invalid escape characters.
///
/// Equivalent to upstream `repairJson`; the returned string equals the input
/// whenever the input was already valid JSON.
pub fn repair_json(json: &str) -> String {
    // JSON structure is ASCII, but string bodies are arbitrary UTF-8, so scan
    // over `char`s and re-emit them untouched (only escapes are rewritten).
    let chars: Vec<char> = json.chars().collect();
    let mut repaired = String::with_capacity(json.len());
    let mut in_string = false;
    let mut index = 0;

    while index < chars.len() {
        let ch = chars[index];

        if !in_string {
            repaired.push(ch);
            if ch == '"' {
                in_string = true;
            }
            index += 1;
            continue;
        }

        if ch == '"' {
            repaired.push(ch);
            in_string = false;
            index += 1;
            continue;
        }

        if ch == '\\' {
            let Some(next) = chars.get(index + 1).copied() else {
                // Trailing backslash: `\\` is the only way to close it.
                repaired.push_str("\\\\");
                index += 1;
                continue;
            };

            if next == 'u' {
                let digits: String = chars.iter().skip(index + 2).take(4).collect();
                if digits.len() == 4 && digits.chars().all(|d| d.is_ascii_hexdigit()) {
                    repaired.push_str("\\u");
                    repaired.push_str(&digits);
                    index += 6;
                    continue;
                }
                // `\u` with bad digits is a valid escape prefix as far as
                // upstream's `VALID_JSON_ESCAPES` check goes: it is copied
                // verbatim and the offending digits are left in place.
            }

            if VALID_JSON_ESCAPES.contains(&next) {
                repaired.push('\\');
                repaired.push(next);
                index += 2;
                continue;
            }

            // `\z` and friends: double the backslash so the pair parses as a
            // literal backslash and the character keeps its meaning.
            repaired.push_str("\\\\");
            index += 1;
            continue;
        }

        if is_control_character(ch) {
            repaired.push_str(&escape_control_character(ch));
        } else {
            repaired.push(ch);
        }
        index += 1;
    }

    repaired
}

/// Parse `json` as `T`, retrying once with [`repair_json`] when the strict
/// parse fails. Mirrors upstream `parseJsonWithRepair`, including the
/// behaviour of re-raising the *original* error when repair changed nothing.
pub fn parse_json_with_repair<T: DeserializeOwned>(json: &str) -> Result<T, serde_json::Error> {
    match serde_json::from_str::<T>(json) {
        Ok(value) => Ok(value),
        Err(error) => {
            let repaired = repair_json(json);
            if repaired == json {
                return Err(error);
            }
            serde_json::from_str::<T>(&repaired)
        }
    }
}

/// [`parse_json_with_repair`] returning a [`Value`] — the form the provider
/// adapters need, since every wire struct is deserialised from the same frame.
pub fn parse_value_with_repair(json: &str) -> Result<Value, serde_json::Error> {
    parse_json_with_repair::<Value>(json)
}

/// Recovers the longest structurally complete value from a truncated JSON
/// document by appending the closers it is missing.
///
/// Returns `None` when the document has no complete value to cut back to
/// (`{`, `[1,`, a bare partial literal). The recovery only ever *drops* the
/// incomplete tail — it never invents values — so the result is a valid
/// document only if a parse-able cut point exists.
pub fn close_partial_json(json: &str) -> Option<String> {
    let repaired = repair_json(json);
    let mut chars: Vec<char> = repaired.chars().collect();

    // Cut points where a value is structurally complete, paired with the
    // containers still open at that moment (outermost first).
    let mut candidates: Vec<(usize, Vec<char>)> = Vec::new();
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaping = false;
    let mut index = 0;

    while index < chars.len() {
        let ch = chars[index];

        if in_string {
            if escaping {
                escaping = false;
            } else if ch == '\\' {
                escaping = true;
            } else if ch == '"' {
                in_string = false;
                candidates.push((index + 1, stack.clone()));
            }
            index += 1;
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' | '[' => stack.push(ch),
            '}' | ']' => {
                stack.pop();
                candidates.push((index + 1, stack.clone()));
            }
            ':' | ',' => {}
            _ if ch.is_whitespace() => {}
            _ => {
                // A bare scalar (`1`, `true`, `null`, `-2.5e3`): consume it up
                // to the next delimiter and treat the end as a cut point.
                while index + 1 < chars.len() {
                    let next = chars[index + 1];
                    if next == ',' || next == '}' || next == ']' || next.is_whitespace() {
                        break;
                    }
                    index += 1;
                }
                candidates.push((index + 1, stack.clone()));
            }
        }
        index += 1;
    }

    if in_string {
        // An unterminated string literal is repaired by closing the quote; the
        // trailing backslash case was already doubled by `repair_json`.
        chars.push('"');
        candidates.push((chars.len(), stack.clone()));
    }

    // Longest candidate first, so a truncated payload keeps as much content as
    // it legitimately can. `serde_json` validates every attempt, which is what
    // lets the scan stay simple: a cut point that lands between an object key
    // and its value (`{"a"`) simply fails and the next one is tried.
    for (end, open) in candidates.iter().rev() {
        let mut closed: String = chars[..*end].iter().collect();
        for container in open.iter().rev() {
            closed.push(if *container == '{' { '}' } else { ']' });
        }
        if serde_json::from_str::<Value>(&closed).is_ok() {
            return Some(closed);
        }
    }

    None
}

/// Parses possibly-incomplete JSON, always returning a value.
///
/// Equivalent to upstream `parseStreamingJson`: an absent or blank payload is
/// `{}`, and a payload that neither parses nor can be closed degrades to `{}`
/// rather than failing. Callers (tool-call finalisation) treat the result as
/// the tool's argument object.
pub fn parse_streaming_json(partial_json: Option<&str>) -> Value {
    let Some(json) = partial_json else {
        return empty_object();
    };
    if json.trim().is_empty() {
        return empty_object();
    }

    if let Ok(value) = parse_value_with_repair(json) {
        return value;
    }

    close_partial_json(json)
        .and_then(|closed| serde_json::from_str::<Value>(&closed).ok())
        .unwrap_or_else(empty_object)
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

fn is_control_character(ch: char) -> bool {
    ch <= '\u{1f}'
}

fn escape_control_character(ch: char) -> String {
    match ch {
        '\u{8}' => "\\b".to_string(),
        '\u{c}' => "\\f".to_string(),
        '\n' => "\\n".to_string(),
        '\r' => "\\r".to_string(),
        '\t' => "\\t".to_string(),
        other => format!("\\u{:04x}", other as u32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn valid_json_is_left_untouched() {
        let valid = r#"{"a":"line\nbreak \"quoted\" \\ slash","b":[1,2,{"c":null}],"d":true}"#;
        assert_eq!(repair_json(valid), valid);
        let parsed: Value = parse_json_with_repair(valid).expect("parses");
        assert_eq!(parsed["b"][2]["c"], Value::Null);
    }

    #[test]
    fn unicode_escapes_are_preserved() {
        let valid = r#"{"emoji":"\ud83d\ude48","tab":"\t"}"#;
        assert_eq!(repair_json(valid), valid);
        assert_eq!(
            parse_json_with_repair::<Value>(valid).expect("parses")["emoji"],
            json!("🙈")
        );
    }

    #[test]
    fn raw_control_characters_inside_strings_are_escaped() {
        let malformed = "{\"text\":\"line\nbreak\ttab\u{1}ctrl\u{c}ff\"}";
        assert!(serde_json::from_str::<Value>(malformed).is_err());
        let repaired = repair_json(malformed);
        assert_eq!(repaired, r#"{"text":"line\nbreak\ttab\u0001ctrl\fff"}"#);
        let parsed: Value = parse_json_with_repair(malformed).expect("parses after repair");
        assert_eq!(parsed["text"], "line\nbreak\ttab\u{1}ctrl\u{c}ff");
    }

    #[test]
    fn control_characters_outside_strings_are_not_escaped() {
        // Upstream copies them verbatim outside strings; JSON.parse then
        // rejects the document, which is what the strict-first contract wants.
        let malformed = "{\u{1}\"a\":1}";
        assert_eq!(repair_json(malformed), malformed);
        assert!(parse_json_with_repair::<Value>(malformed).is_err());
    }

    #[test]
    fn invalid_escapes_are_doubled() {
        let malformed = r#"{"path":"C:\Users\q"}"#;
        assert!(serde_json::from_str::<Value>(malformed).is_err());
        let repaired = repair_json(malformed);
        assert_eq!(repaired, r#"{"path":"C:\\Users\\q"}"#);
        let parsed: Value = parse_json_with_repair(malformed).expect("parses after repair");
        assert_eq!(parsed["path"], r"C:\Users\q");
    }

    #[test]
    fn trailing_backslash_is_doubled() {
        let malformed = "{\"path\":\"C:\\";
        let repaired = repair_json(malformed);
        assert_eq!(repaired, "{\"path\":\"C:\\\\");
        // Still unterminated — the closing brace is missing, so the strict
        // and repaired parses both fail and the original error surfaces.
        let error = parse_json_with_repair::<Value>(malformed).expect_err("still malformed");
        assert!(error.is_eof(), "expected an EOF error, got {error}");
    }

    #[test]
    fn invalid_unicode_escape_passes_through() {
        // `\uZZZZ` is not repairable upstream (the `u` branch lets it through)
        // and stays malformed here too.
        let malformed = r#"{"a":"\uZZZZ"}"#;
        assert_eq!(repair_json(malformed), malformed);
        assert!(parse_json_with_repair::<Value>(malformed).is_err());
    }

    #[test]
    fn strict_parse_short_circuits_repair() {
        // Valid JSON must not take the repair path: an escaped literal that
        // repair would rewrite is preserved as-is.
        let parsed: Value = parse_json_with_repair(r#"{"a":"\\n"}"#).expect("parses");
        assert_eq!(parsed["a"], r"\n");
    }

    #[test]
    fn close_partial_json_completes_open_containers() {
        assert_eq!(
            close_partial_json(r#"{"city": "San Francisco"#).as_deref(),
            Some(r#"{"city": "San Francisco"}"#)
        );
        assert_eq!(
            close_partial_json(r#"{"a": [1, 2"#).as_deref(),
            Some(r#"{"a": [1, 2]}"#)
        );
        assert_eq!(
            close_partial_json(r#"{"a": [1, {"b": 2"#).as_deref(),
            Some(r#"{"a": [1, {"b": 2}]}"#)
        );
        assert_eq!(close_partial_json(r#"[1,2"#).as_deref(), Some("[1,2]"));
    }

    #[test]
    fn close_partial_json_drops_the_incomplete_tail() {
        // The half-built key/value pair is dropped, not invented.
        assert_eq!(
            close_partial_json(r#"{"a": 1, "b":"#).as_deref(),
            Some(r#"{"a": 1}"#)
        );
        assert_eq!(
            close_partial_json(r#"{"a": 1, "b": tru"#).as_deref(),
            Some(r#"{"a": 1}"#)
        );
        // A dangling key still recovers: the key becomes a string value, which
        // is the closest thing to intent the document actually contains.
        assert_eq!(
            close_partial_json(r#"{"a": "key-only""#).as_deref(),
            Some(r#"{"a": "key-only"}"#)
        );
        assert_eq!(close_partial_json("[1,").as_deref(), Some("[1]"));
    }

    #[test]
    fn close_partial_json_reports_nothing_to_recover() {
        assert_eq!(close_partial_json("{"), None);
        assert_eq!(close_partial_json("["), None);
        assert_eq!(close_partial_json(""), None);
        assert_eq!(close_partial_json("   "), None);
    }

    #[test]
    fn close_partial_json_handles_escaped_quotes() {
        assert_eq!(
            close_partial_json(r#"{"a":"x\"y"#).as_deref(),
            Some(r#"{"a":"x\"y"}"#)
        );
    }

    #[test]
    fn parse_streaming_json_never_fails() {
        assert_eq!(parse_streaming_json(None), json!({}));
        assert_eq!(parse_streaming_json(Some("   ")), json!({}));
        assert_eq!(parse_streaming_json(Some("not json at all")), json!({}));
        assert_eq!(parse_streaming_json(Some("{")), json!({}));
    }

    #[test]
    fn parse_streaming_json_parses_complete_and_partial_payloads() {
        assert_eq!(
            parse_streaming_json(Some(r#"{"city":"Berlin"}"#)),
            json!({"city": "Berlin"})
        );
        assert_eq!(
            parse_streaming_json(Some(r#"{"city": "Ber"#)),
            json!({"city": "Ber"})
        );
        // Repair path: a raw newline inside the string.
        assert_eq!(
            parse_streaming_json(Some("{\"city\":\"Ber\nlin\"}")),
            json!({"city": "Ber\nlin"})
        );
        // Truncated mid-value: the complete sibling survives.
        assert_eq!(
            parse_streaming_json(Some(r#"{"city": "Ber", "unit": "#)),
            json!({"city": "Ber"})
        );
    }
}
