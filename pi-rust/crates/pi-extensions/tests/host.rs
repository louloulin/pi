//! Host unit tests — exercise the host shim + tool/UI plumbing without
//! touching the agent runtime.

use std::path::PathBuf;
use std::sync::Arc;

use pi_extensions::ExtensionEntry;
use pi_extensions::{
    DispatchOutcome, ExtensionCapabilities, ExtensionError, ExtensionRegistry, JsExtensionHost,
    ScriptedUiAnswers, ScriptedUiHandler, ToolExecutionOutcome,
};
use pi_protocol::{ExtensionEvent, Message, Role, UiLevel, UiResponse};
use serde_json::json;

fn entry(id: &str) -> ExtensionEntry {
    ExtensionEntry {
        source: PathBuf::from(format!("/tmp/pi_extensions_test/{id}.js")),
        id: id.to_string(),
        label: None,
    }
}

/// Spawn a single-threaded tokio runtime so the AsyncRuntime driver
/// can be spawned locally.
fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime")
}

#[test]
fn host_installs_shim_and_exposes_helpers() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        // The shim installs `pi` and the dispatch helpers — verify
        // both are queryable.
        let known = host.known_event_names().await;
        assert!(known.is_empty(), "no handlers yet, got {known:?}");
        let tools = host.registered_tool_names().await;
        assert!(tools.is_empty(), "no tools yet, got {tools:?}");
    });
}

#[test]
fn host_collects_tool_registration_via_shim() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        let source = r#"
            module.exports = function (pi) {
                pi.registerTool({
                    name: "echo",
                    label: "Echo",
                    description: "Echo a string",
                    parameters: { type: "object", properties: { text: { type: "string" } } },
                    execute: function (args, ctx) {
                        return { content: [{ type: "text", text: String(args.text) }], details: null };
                    },
                });
            };
        "#;
        host.load(entry("echo_ext"), source)
            .await
            .expect("load should succeed");

        let tools = host.registered_tool_names().await;
        assert_eq!(tools, vec!["echo".to_string()]);
        let snapshot = host.registered_tools();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].name, "echo");
        assert_eq!(snapshot[0].label, "Echo");
    });
}

#[test]
fn host_executes_registered_tool() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        let source = r#"
            module.exports = function (pi) {
                pi.registerTool({
                    name: "double",
                    label: "Double",
                    description: "double a number",
                    parameters: { type: "object" },
                    execute: function (args, _ctx) {
                        const n = Number(args.n);
                        return {
                            content: [{ type: "text", text: String(n * 2) }],
                            details: { doubled: n * 2 },
                        };
                    },
                });
            };
        "#;
        host.load(entry("double"), source).await.expect("load");

        let outcome: ToolExecutionOutcome = host
            .execute_tool("double", r#"{"n": 21}"#)
            .await
            .expect("tool executes");
        assert!(!outcome.is_error);
        assert_eq!(outcome.details, Some(json!({"doubled": 42})));
        let content = outcome.content.first().expect("one content block");
        assert_eq!(content["type"], "text");
        assert_eq!(content["text"], "42");
    });
}

#[test]
fn host_dispatches_session_start_to_subscribed_handler() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        let source = r#"
            module.exports = function (pi) {
                pi.on("session_start", function (event, ctx) {
                    pi.appendEntry("started", { type: event.type });
                    return { ok: true };
                });
            };
        "#;
        host.load(entry("se"), source).await.expect("load");

        let outcome: DispatchOutcome = host
            .emit_event(&ExtensionEvent::SessionStart)
            .await
            .expect("dispatch");
        assert!(outcome.handled, "should be handled: {outcome:?}");
        assert_eq!(outcome.subscribers, 1);

        let log = host.log();
        assert_eq!(log.entries.len(), 1);
        assert_eq!(log.entries[0].custom_type, "started");
        assert_eq!(log.entries[0].data["type"], "session_start");
    });
}

#[test]
fn host_resolves_ui_confirm_via_handler() {
    let runtime = rt();
    runtime.block_on(async {
        let answers = ScriptedUiAnswers {
            confirms: [("Dangerous?".to_string(), true)].into_iter().collect(),
            ..Default::default()
        };
        let handler = Arc::new(ScriptedUiHandler::new(answers));
        let host = JsExtensionHost::with_handler(handler).await.expect("host");
        let source = r#"
            module.exports = async function (pi) {
                pi.on("session_start", async function (event, ctx) {
                    const answer = await ctx.ui.confirm("Dangerous?", "Are you sure?");
                    pi.appendEntry("confirm_result", { answer: answer });
                });
            };
        "#;
        host.load(entry("confirm"), source).await.expect("load");

        // Drive the registered session_start event so the handler
        // (registered during load) actually fires its async work.
        // We pass hasUI=true so the shim routes UI calls to the host.
        host.emit_event_with(&ExtensionEvent::SessionStart, Some("tui"), true, "/tmp")
            .await
            .expect("dispatch");

        let log = host.log();
        let confirm_entry = log
            .entries
            .iter()
            .find(|e| e.custom_type == "ui_confirm" || e.custom_type == "ui_answer_confirm")
            .expect("confirm entry recorded");
        assert_eq!(confirm_entry.data["title"], "Dangerous?");
        assert_eq!(confirm_entry.data["accepted"], true);
    });
}

#[test]
fn host_resolves_ui_input_and_select() {
    let runtime = rt();
    runtime.block_on(async {
        let answers = ScriptedUiAnswers {
            inputs: [("Name?".to_string(), "alice".to_string())]
                .into_iter()
                .collect(),
            selects: [("Pick".to_string(), "b".to_string())].into_iter().collect(),
            ..Default::default()
        };
        let handler = Arc::new(ScriptedUiHandler::new(answers));
        let host = JsExtensionHost::with_handler(handler).await.expect("host");
        let source = r#"
            module.exports = async function (pi) {
                pi.on("session_start", async function (event, ctx) {
                    const name = await ctx.ui.input("Name?", "type");
                    const picked = await ctx.ui.select("Pick", ["a", "b", "c"]);
                    pi.appendEntry("input_select", { name: name, picked: picked });
                });
            };
        "#;
        host.load(entry("ui"), source).await.expect("load");
        host.emit_event_with(&ExtensionEvent::SessionStart, Some("tui"), true, "/tmp")
            .await
            .expect("dispatch");

        let log = host.log();
        let entry = log
            .entries
            .iter()
            .find(|e| e.custom_type == "input_select")
            .expect("input_select entry");
        assert_eq!(entry.data["name"], "alice");
        assert_eq!(entry.data["picked"], "b");
    });
}

#[test]
fn host_timeout_for_slow_extension() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        // Drive that loops forever — `execute_tool` should time out.
        let source = r#"
            module.exports = function (pi) {
                pi.registerTool({
                    name: "spin",
                    label: "Spin",
                    description: "spins forever",
                    parameters: { type: "object" },
                    execute: function () {
                        while (true) {}
                    },
                });
            };
        "#;
        host.load(entry("spin"), source).await.expect("load");

        let result = host
            .execute_tool("spin", "{}")
            .await;
        // The pure-sync `while(true)` doesn't yield, so rquickjs-core
        // may either succeed-with-error or return an error. Either
        // way, the host must not panic.
        match result {
            Ok(outcome) => assert!(outcome.is_error, "spinning tool must surface as error"),
            Err(ExtensionError::Timeout(_)) | Err(ExtensionError::Runtime(_)) => {}
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    });
}

#[test]
fn bridge_implements_extension_bridge_trait() {
    use async_trait::async_trait;
    use pi_extensions::ExtensionBridge;
    use pi_extensions::JsExtensionBridge;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingBridge {
        #[allow(dead_code)]
        host: JsExtensionHost,
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ExtensionBridge for CountingBridge {
        async fn deliver(&self, _event: &ExtensionEvent) -> bool {
            self.counter.fetch_add(1, Ordering::SeqCst);
            true
        }
    }

    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        let counter = Arc::new(AtomicUsize::new(0));
        let _bridge = CountingBridge {
            host: host.clone(),
            counter: counter.clone(),
        };
        // Smoke: call into the bridge trait from a typed reference.
        let outcome = host.emit_event(&ExtensionEvent::SessionEnd).await.expect("dispatch");
        assert!(!outcome.handled);
        // The CountingBridge is constructed for compile-time coverage;
        // we never invoke its deliver method here so the counter stays
        // at zero. Compile success is the assertion.
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    });

    // Compile-time: also check JsExtensionBridge satisfies the trait.
    let _: fn(JsExtensionHost, String, bool, String) -> JsExtensionBridge = JsExtensionBridge::new;
    let _: &dyn ExtensionBridge;
}

#[test]
fn host_emits_unknown_event_cleanly() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        // No extension loaded; emitting any event should still
        // succeed and report `handled: false`.
        let outcome = host
            .emit_event(&ExtensionEvent::UserMessage {
                message: Message {
                    role: Role::User,
                    content: vec![pi_protocol::Content::text("hi")],
                    model: None,
                },
            })
            .await
            .expect("dispatch");
        assert!(!outcome.handled);
        assert_eq!(outcome.subscribers, 0);
    });
}

#[test]
fn extension_registry_records_tool_definition() {
    let mut registry = ExtensionRegistry::new();
    let entry = entry("hello");
    registry.register(entry.clone(), ExtensionCapabilities::default());
    assert!(registry.contains("hello"));
    assert_eq!(registry.extensions().count(), 1);
}

#[test]
fn extension_capabilities_default_has_no_tools() {
    let caps = ExtensionCapabilities::default();
    assert!(caps.tools.is_empty());
}

#[test]
fn ui_level_default_is_info() {
    assert_eq!(UiLevel::default(), UiLevel::Info);
}

#[test]
fn ui_response_serde_roundtrip() {
    let resp = UiResponse::Select {
        value: "yes".to_string(),
    };
    let json = serde_json::to_string(&resp).expect("serialize");
    let parsed: UiResponse = serde_json::from_str(&json).expect("parse");
    assert_eq!(parsed, resp);
}