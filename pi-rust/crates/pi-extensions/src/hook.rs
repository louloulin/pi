//! Tool-hook outcome folding — the Rust half of `tool_call` / `tool_result`.
//!
//! Upstream, a plugin handler is not a passive listener: `tool_call` can
//! **block** a call (and stop the loop after the batch) or patch the
//! arguments in place, and `tool_result` can replace what the model sees.
//! The JS side already reports everything needed to honour that
//! (`_pi_dispatch` returns each handler's return value *and* the event
//! object after in-place mutation); this module owns the folding rules so
//! the executor and the tests share one implementation instead of each
//! re-deriving them.
//!
//! Semantics mirror `ExtensionRunner.emitToolCall` /
//! `emitToolResult` in
//! `packages/coding-agent/src/core/extensions/runner.ts`:
//!
//! * `tool_call` — handlers run in registration order; the **last**
//!   non-empty result wins, except that the **first** result with
//!   `block: true` short-circuits and is returned immediately.
//! * `tool_result` — each result patches the *current* event
//!   (`content` / `details` / `isError` / `usage`), so later handlers see
//!   earlier patches; upstream returns `undefined` when nothing changed.

use pi_protocol::Content;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::host::DispatchOutcome;

/// One handler's `tool_call` return value — upstream `ToolCallEventResult`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolCallEventResult {
    /// Block execution of the call.
    #[serde(default)]
    pub block: bool,
    /// Human-readable reason shown to the model when blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Ask the loop to stop after this batch (only honoured when *every*
    /// call in the batch asks for it, matching upstream).
    #[serde(default)]
    pub terminate: bool,
}

/// One handler's `tool_result` return value — upstream
/// `ToolResultEventResult`. Every field is optional: `undefined` means
/// "leave this part alone".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolResultEventResult {
    /// Replacement content blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<Content>>,
    /// Replacement structured details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Replacement error flag.
    #[serde(rename = "isError", default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// Usage the tool itself reported. Parsed for parity; the port has no
    /// per-tool usage field on `ToolResult` to carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
}

/// Folded `tool_call` decision for one call.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolCallHookOutcome {
    /// Block the call from reaching the executor.
    pub blocked: bool,
    /// Reason surfaced to the model in the error result.
    pub reason: Option<String>,
    /// Stop after the current batch when every call asks for it.
    pub terminate: bool,
    /// Patched arguments, present only when a handler mutated
    /// `event.input`. `None` leaves the original arguments untouched.
    pub input: Option<Value>,
}

impl ToolCallHookOutcome {
    /// Allow the call with no modifications.
    pub fn allow() -> Self {
        Self::default()
    }

    /// True when nothing needs to be applied.
    pub fn is_noop(&self) -> bool {
        !self.blocked && !self.terminate && self.input.is_none()
    }

    /// Fold a dispatch summary into one decision.
    ///
    /// `original_input` is the argument object the event was built with;
    /// it is only used to tell a real patch apart from an echo of the
    /// unchanged event.
    pub fn from_dispatch(outcome: &DispatchOutcome, original_input: &Value) -> Self {
        let mut folded = ToolCallHookOutcome::allow();
        for result in &outcome.results {
            let Some(object) = result.as_object() else {
                // A handler that returned a bare string / number / null
                // said nothing about this call. Ignore it instead of
                // failing the whole hook.
                continue;
            };
            let block = object.get("block").and_then(Value::as_bool) == Some(true);
            if block {
                // First block wins and stops the fold — upstream returns
                // the blocking result immediately.
                folded.blocked = true;
                folded.reason = object
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                folded.terminate = object.get("terminate").and_then(Value::as_bool) == Some(true);
                return folded;
            }
            // Not blocking: the last handler's verdict stands.
            folded.terminate = object.get("terminate").and_then(Value::as_bool) == Some(true);
        }
        let patched = outcome
            .event
            .as_ref()
            .and_then(|event| event.get("input"))
            .filter(|input| **input != *original_input)
            .cloned();
        folded.input = patched;
        folded
    }
}

/// Folded `tool_result` patch for one result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolResultHookOutcome {
    /// Replacement content, already folded forward through every handler.
    pub content: Option<Vec<Content>>,
    /// Replacement `isError` flag.
    pub is_error: Option<bool>,
    /// Replacement structured details.
    pub details: Option<Value>,
    /// True when at least one handler changed something.
    pub modified: bool,
}

impl ToolResultHookOutcome {
    /// Fold a dispatch summary onto the event the handlers saw.
    ///
    /// `base` is the serialized `tool_result` event. The fold starts from
    /// it (so in-place mutations a handler made on the event object are
    /// already in there) and applies every returned patch in order.
    /// Returns `None` when nothing changed, matching upstream's
    /// `undefined` return.
    pub fn from_dispatch(base: &Value, outcome: &DispatchOutcome) -> Option<Self> {
        let mut current = outcome.event.clone().unwrap_or_else(|| base.clone());
        let mut patched = false;
        for result in &outcome.results {
            let Ok(patch) = serde_json::from_value::<ToolResultEventResult>(result.clone()) else {
                // Unparsable handler return (bare string, unexpected
                // shape): ignore it rather than dropping the patches its
                // peers returned.
                continue;
            };
            let Some(object) = current.as_object_mut() else {
                break;
            };
            patched |=
                patch.content.is_some() || patch.details.is_some() || patch.is_error.is_some();
            if let Some(content) = patch.content {
                object.insert(
                    "content".into(),
                    serde_json::to_value(content).unwrap_or(Value::Null),
                );
            }
            if let Some(details) = patch.details {
                object.insert("details".into(), details);
            }
            if let Some(is_error) = patch.is_error {
                object.insert("isError".into(), Value::Bool(is_error));
            }
        }
        // Only fields that actually changed travel back: a handler that
        // patched `details` must not make the caller rewrite the content
        // it never looked at. The echoed event also carries the host's own
        // `_ctx_*` envelope fields, so "did anything change" is decided
        // field by field on the event payload rather than by comparing the
        // whole envelope.
        let content_changed = changed(&current, base, "content");
        let error_changed = changed(&current, base, "isError");
        let details_changed = changed(&current, base, "details");
        if !patched && !content_changed && !error_changed && !details_changed {
            return None;
        }
        Some(Self {
            content: content_changed.then(|| parse_content(&current)).flatten(),
            is_error: error_changed
                .then(|| current.get("isError").and_then(Value::as_bool))
                .flatten(),
            details: details_changed
                .then(|| current.get("details").cloned())
                .flatten(),
            modified: true,
        })
    }
}

/// True when `key` differs between the folded event and the one the
/// handlers were handed.
fn changed(current: &Value, base: &Value, key: &str) -> bool {
    current.get(key) != base.get(key)
}

/// Read the `content` array back out of a folded event.
///
/// A field that is absent, or present with an unusable shape, folds to
/// `None` = "leave the result's content alone".
fn parse_content(event: &Value) -> Option<Vec<Content>> {
    let raw = event.get("content")?;
    if raw.is_null() {
        return None;
    }
    serde_json::from_value::<Vec<Content>>(raw.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn outcome(results: Vec<Value>, event: Option<Value>) -> DispatchOutcome {
        DispatchOutcome {
            handled: !results.is_empty(),
            subscribers: results.len(),
            results,
            errored: None,
            event,
        }
    }

    // --- tool_call ---------------------------------------------------

    #[test]
    fn no_handlers_allows_the_call() {
        let folded = ToolCallHookOutcome::from_dispatch(&outcome(vec![], None), &json!({"a": 1}));
        assert!(folded.is_noop());
    }

    #[test]
    fn first_block_wins_and_stops_the_fold() {
        let folded = ToolCallHookOutcome::from_dispatch(
            &outcome(
                vec![
                    json!({"block": true, "reason": "no rm -rf", "terminate": true}),
                    json!({"block": false, "reason": "later"}),
                ],
                None,
            ),
            &json!({}),
        );
        assert!(folded.blocked);
        assert_eq!(folded.reason.as_deref(), Some("no rm -rf"));
        assert!(
            folded.terminate,
            "the blocking result's terminate rides along"
        );
    }

    #[test]
    fn last_non_blocking_result_owns_terminate() {
        let folded = ToolCallHookOutcome::from_dispatch(
            &outcome(
                vec![json!({"terminate": true}), json!({"terminate": false})],
                None,
            ),
            &json!({}),
        );
        assert!(!folded.blocked);
        assert!(!folded.terminate, "last verdict stands");
    }

    #[test]
    fn blocking_without_reason_leaves_reason_empty() {
        let folded = ToolCallHookOutcome::from_dispatch(
            &outcome(vec![json!({"block": true})], None),
            &json!({}),
        );
        assert!(folded.blocked);
        assert_eq!(folded.reason, None, "the caller supplies the default text");
    }

    #[test]
    fn in_place_input_mutation_is_read_back() {
        let folded = ToolCallHookOutcome::from_dispatch(
            &outcome(
                vec![json!(null)],
                Some(json!({"type": "tool_call", "input": {"path": "rewritten.rs"}})),
            ),
            &json!({"path": "original.rs"}),
        );
        assert_eq!(folded.input, Some(json!({"path": "rewritten.rs"})));
    }

    #[test]
    fn unchanged_event_is_not_a_patch() {
        let input = json!({"path": "same.rs"});
        let folded = ToolCallHookOutcome::from_dispatch(
            &outcome(
                vec![json!({"block": false})],
                Some(json!({"type": "tool_call", "input": input.clone()})),
            ),
            &input,
        );
        assert_eq!(folded.input, None, "an echoed event must not re-patch");
        assert!(folded.is_noop());
    }

    #[test]
    fn non_object_results_are_ignored() {
        let folded = ToolCallHookOutcome::from_dispatch(
            &outcome(
                vec![json!("nope"), json!(7), json!(null), json!({"block": true})],
                None,
            ),
            &json!({}),
        );
        assert!(folded.blocked, "the one real result still counts");
    }

    // --- tool_result --------------------------------------------------

    fn base_event() -> Value {
        json!({
            "type": "tool_result",
            "toolCallId": "t1",
            "toolName": "read",
            "input": {"path": "a.rs"},
            "content": [{"type": "text", "text": "raw"}],
            "isError": false,
        })
    }

    #[test]
    fn no_change_returns_none() {
        assert_eq!(
            ToolResultHookOutcome::from_dispatch(&base_event(), &outcome(vec![], None)),
            None
        );
    }

    #[test]
    fn patches_are_applied_in_order_and_later_wins() {
        let folded = ToolResultHookOutcome::from_dispatch(
            &base_event(),
            &outcome(
                vec![
                    json!({"content": [{"type": "text", "text": "first"}]}),
                    json!({"content": [{"type": "text", "text": "second"}], "isError": true}),
                ],
                None,
            ),
        )
        .expect("modified");
        assert_eq!(
            folded.content,
            Some(vec![Content::text("second")]),
            "the later patch replaced the earlier one"
        );
        assert_eq!(folded.is_error, Some(true));
    }

    #[test]
    fn in_place_event_mutation_alone_counts_as_modified() {
        let mut mutated = base_event();
        mutated["isError"] = json!(true);
        let folded =
            ToolResultHookOutcome::from_dispatch(&base_event(), &outcome(vec![], Some(mutated)))
                .expect("the event object changed");
        assert_eq!(folded.is_error, Some(true));
    }

    #[test]
    fn details_only_patch_is_modified() {
        let folded = ToolResultHookOutcome::from_dispatch(
            &base_event(),
            &outcome(vec![json!({"details": {"exitCode": 2}})], None),
        )
        .expect("modified");
        assert_eq!(folded.content, None, "content untouched");
        assert_eq!(folded.details, Some(json!({"exitCode": 2})));
    }

    #[test]
    fn unparsable_result_is_skipped_not_fatal() {
        let folded = ToolResultHookOutcome::from_dispatch(
            &base_event(),
            &outcome(vec![json!("just a string"), json!({"isError": true})], None),
        )
        .expect("the real patch landed");
        assert_eq!(folded.is_error, Some(true));
    }

    #[test]
    fn empty_content_array_folds_to_empty_text() {
        let folded = ToolResultHookOutcome::from_dispatch(
            &base_event(),
            &outcome(vec![json!({"content": []})], None),
        )
        .expect("modified");
        assert_eq!(folded.content, Some(Vec::new()));
    }

    #[test]
    fn usage_is_parsed_for_parity() {
        let parsed: ToolResultEventResult = serde_json::from_value(json!({
            "usage": {"input": 1, "output": 2},
        }))
        .unwrap();
        assert!(parsed.usage.is_some());
        assert!(parsed.content.is_none());
    }

    /// The host injects `_ctx_*` context fields before dispatch and the shim
    /// echoes the whole envelope back. Those fields must not be mistaken for
    /// a handler patch — the QuickJS round-trip test caught exactly this.
    #[test]
    fn injected_context_fields_are_not_a_patch() {
        let echoed = json!({
            "type": "tool_result",
            "toolCallId": "t1",
            "toolName": "read",
            "input": {"path": "a.rs"},
            "content": [{"type": "text", "text": "raw"}],
            "isError": false,
            "_ctx_mode": "print",
            "_ctx_hasUI": false,
            "_ctx_cwd": "/tmp",
        });
        assert_eq!(
            ToolResultHookOutcome::from_dispatch(&base_event(), &outcome(vec![], Some(echoed))),
            None,
            "an echoed envelope with only _ctx_* additions is not a modification"
        );
    }
}
