//! `tool_call` / `tool_result` hook round-trip through the real QuickJS host.
//!
//! These tests drive the shipped shim (`pi-ext-shim.mjs`) with a real
//! extension source, so they cover the part a pure-Rust unit test cannot:
//! the **event object echo**. Upstream patches tool arguments by mutating
//! `event.input` in place, and the host only learns about the patch if the
//! shim hands the mutated object back (see `DispatchOutcome::event`).
//!
//! Folding rules themselves are unit-tested in `src/hook.rs`.

use std::path::PathBuf;

use pi_extensions::{
    DispatchOutcome, ExtensionEntry, JsExtensionHost, ToolCallHookOutcome, ToolResultHookOutcome,
};
use pi_protocol::{Content, ExtensionEvent};
use serde_json::json;

/// One extension exercising both hooks: `bash` is blocked, `read` gets its
/// argument patched, and every `read` result is redacted.
const HOOKS_JS: &str = r#"
module.exports = function (pi) {
    pi.on("tool_call", function (event) {
        if (event.toolName === "bash") {
            return { block: true, reason: "blocked-by-extension" };
        }
        if (event.toolName === "read") {
            // Upstream contract: mutate `event.input` in place.
            event.input.path = "patched.txt";
        }
        return null;
    });
    pi.on("tool_result", function (event) {
        if (event.toolName === "read") {
            return { content: [{ type: "text", text: "REDACTED" }], isError: true };
        }
        return undefined;
    });
};
"#;

/// An extension that subscribes to neither hook.
const INERT_JS: &str = r#"
module.exports = function (pi) {
    pi.on("session_start", function () {});
};
"#;

fn entry(id: &str) -> ExtensionEntry {
    ExtensionEntry {
        source: PathBuf::from(format!("/tmp/pi_extensions_hooks/{id}.js")),
        id: id.to_string(),
        label: None,
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime")
}

async fn host_with(id: &str, source: &str) -> JsExtensionHost {
    let host = JsExtensionHost::new().await.expect("host");
    host.load(entry(id), source).await.expect("load extension");
    host
}

async fn dispatch(host: &JsExtensionHost, event: &ExtensionEvent) -> DispatchOutcome {
    host.emit_event_with(event, Some("print"), false, "/tmp")
        .await
        .expect("dispatch")
}

fn tool_call(name: &str, input: serde_json::Value) -> ExtensionEvent {
    ExtensionEvent::ToolCall {
        tool_call_id: "call-1".into(),
        tool_name: name.into(),
        input,
    }
}

#[test]
fn blocking_handler_reports_block_and_reason() {
    rt().block_on(async {
        let host = host_with("hooks", HOOKS_JS).await;
        let outcome = dispatch(&host, &tool_call("bash", json!({"command": "rm -rf /"}))).await;
        let folded = ToolCallHookOutcome::from_dispatch(&outcome, &json!({"command": "rm -rf /"}));
        assert!(folded.blocked, "the handler asked for a block");
        assert_eq!(folded.reason.as_deref(), Some("blocked-by-extension"));
        assert_eq!(folded.input, None, "a blocked call needs no patch");
    });
}

#[test]
fn in_place_input_mutation_survives_the_shim_round_trip() {
    rt().block_on(async {
        let host = host_with("hooks", HOOKS_JS).await;
        let original = json!({"path": "original.txt"});
        let outcome = dispatch(&host, &tool_call("read", original.clone())).await;
        let folded = ToolCallHookOutcome::from_dispatch(&outcome, &original);
        assert!(!folded.blocked);
        assert_eq!(
            folded.input,
            Some(json!({"path": "patched.txt"})),
            "the mutation the handler made must come back as a patch"
        );
    });
}

#[test]
fn untouched_input_is_not_reported_as_a_patch() {
    rt().block_on(async {
        let host = host_with("hooks", HOOKS_JS).await;
        let original = json!({"command": "echo hi"});
        let outcome = dispatch(&host, &tool_call("bash", original.clone())).await;
        // `bash` is blocked here, so only check the echo: the event came
        // back with the input unchanged.
        let echoed = outcome.event.as_ref().expect("event echoed");
        assert_eq!(echoed["input"], original);
        assert_eq!(echoed["toolName"], "bash");
    });
}

#[test]
fn tool_result_handler_replaces_content_and_error_flag() {
    rt().block_on(async {
        let host = host_with("hooks", HOOKS_JS).await;
        let event = ExtensionEvent::ToolResult {
            tool_call_id: "call-2".into(),
            tool_name: "read".into(),
            input: json!({"path": "secret.env"}),
            content: vec![Content::text("SUPER_SECRET=1")],
            is_error: false,
            details: None,
        };
        let base = serde_json::to_value(&event).expect("serialize");
        let outcome = dispatch(&host, &event).await;
        let folded =
            ToolResultHookOutcome::from_dispatch(&base, &outcome).expect("the handler modified it");
        assert_eq!(folded.content, Some(vec![Content::text("REDACTED")]));
        assert_eq!(folded.is_error, Some(true));
    });
}

#[test]
fn a_result_nobody_touched_folds_to_none() {
    rt().block_on(async {
        let host = host_with("hooks", HOOKS_JS).await;
        let event = ExtensionEvent::ToolResult {
            tool_call_id: "call-3".into(),
            tool_name: "bash".into(),
            input: json!({"command": "true"}),
            content: vec![Content::text("ok")],
            is_error: false,
            details: None,
        };
        let base = serde_json::to_value(&event).expect("serialize");
        let outcome = dispatch(&host, &event).await;
        assert_eq!(
            ToolResultHookOutcome::from_dispatch(&base, &outcome),
            None,
            "no handler patch means the caller keeps the original result"
        );
    });
}

#[test]
fn extension_without_hook_subscriptions_folds_to_a_noop() {
    rt().block_on(async {
        let host = host_with("inert", INERT_JS).await;
        let outcome = dispatch(&host, &tool_call("bash", json!({"command": "true"}))).await;
        assert_eq!(outcome.subscribers, 0, "nobody subscribed to tool_call");
        let folded = ToolCallHookOutcome::from_dispatch(&outcome, &json!({"command": "true"}));
        assert!(folded.is_noop());
    });
}
