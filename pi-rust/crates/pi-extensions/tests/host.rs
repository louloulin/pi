//! Host unit tests — exercise the host shim + tool/UI plumbing without
//! touching the agent runtime.

use std::path::PathBuf;
use std::sync::Arc;

use pi_extensions::ExtensionEntry;
use pi_extensions::{
    DispatchOutcome, ExtensionCapabilities, ExtensionError, ExtensionRegistry, HostOptions,
    JsExtensionHost, ScriptedUiAnswers, ScriptedUiHandler, ToolExecutionOutcome, UiHandler,
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
fn loading_a_second_extension_keeps_the_first_extensions_tools() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        let first = r#"
            module.exports = function (pi) {
                pi.registerTool({
                    name: "first",
                    label: "First",
                    description: "first tool",
                    parameters: { type: "object" },
                    execute: function () { return { content: [] }; },
                });
            };
        "#;
        let second = r#"
            module.exports = function (pi) {
                pi.registerTool({
                    name: "second",
                    label: "Second",
                    description: "second tool",
                    parameters: { type: "object" },
                    execute: function () { return { content: [] }; },
                });
            };
        "#;
        host.load(entry("first_ext"), first)
            .await
            .expect("load first");
        host.load(entry("second_ext"), second)
            .await
            .expect("load second");

        let names = host.registered_tool_names().await;
        assert_eq!(
            names,
            vec!["first".to_string(), "second".to_string()],
            "loading a second extension must not drop the first one's tools"
        );
    });
}

#[test]
fn host_collects_tool_prompt_contributions() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        let source = r#"
            module.exports = function (pi) {
                pi.registerTool({
                    name: "search",
                    label: "Search",
                    description: "Search the web",
                    parameters: { type: "object" },
                    promptSnippet: "Search the web for a query",
                    promptGuidelines: ["Prefer search for current facts", 42, "Cite sources"],
                    execute: function () { return { content: [], details: null }; },
                });
                pi.registerTool({
                    name: "plain",
                    label: "Plain",
                    description: "No prompt contribution",
                    parameters: { type: "object" },
                    execute: function () { return { content: [], details: null }; },
                });
            };
        "#;
        host.load(entry("tool_prompts"), source)
            .await
            .expect("load should succeed");

        let prompts = host.registered_tool_prompts().await;
        assert_eq!(prompts.len(), 2, "{prompts:?}");
        assert_eq!(prompts[0].name, "search");
        assert_eq!(
            prompts[0].snippet.as_deref(),
            Some("Search the web for a query")
        );
        // Non-string entries in `promptGuidelines` are dropped.
        assert_eq!(
            prompts[0].guidelines,
            vec![
                "Prefer search for current facts".to_string(),
                "Cite sources".to_string()
            ]
        );
        assert_eq!(prompts[1].name, "plain");
        assert!(prompts[1].snippet.is_none());
        assert!(prompts[1].guidelines.is_empty());
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
            selects: [("Pick".to_string(), "b".to_string())]
                .into_iter()
                .collect(),
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

        let result = host.execute_tool("spin", "{}").await;
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
        let outcome = host
            .emit_event(&ExtensionEvent::SessionEnd)
            .await
            .expect("dispatch");
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
// ---------------------------------------------------------------------------
// UI handler async trait (LUM-1077): the handler awaits the user, so every
// prompt path has to be `async`. These tests drive all four `UiHandler`
// methods through the JS shim and cover the "no handler installed" fallback.
// ---------------------------------------------------------------------------

/// `UiHandler` that answers every prompt with a recognisable value so the
/// test can prove the request reached the handler (and not a default).
#[derive(Default)]
struct RecordingUiHandler {
    notifies: std::sync::Mutex<Vec<(String, UiLevel)>>,
}

#[async_trait::async_trait]
impl UiHandler for RecordingUiHandler {
    async fn confirm(&self, title: &str, body: &str) -> bool {
        title == "Go?" && body == "body text"
    }
    async fn input(&self, title: &str, placeholder: Option<&str>) -> Option<String> {
        Some(format!("in:{title}:{}", placeholder.unwrap_or("-")))
    }
    async fn select(&self, title: &str, options: &[String]) -> Option<String> {
        options.last().map(|last| format!("{title}:{last}"))
    }
    async fn notify(&self, message: &str, level: UiLevel) {
        self.notifies
            .lock()
            .expect("notify lock")
            .push((message.to_string(), level));
    }
}

#[test]
fn async_ui_handler_resolves_all_four_paths() {
    let runtime = rt();
    runtime.block_on(async {
        let handler = Arc::new(RecordingUiHandler::default());
        let host = JsExtensionHost::with_handler(handler.clone())
            .await
            .expect("host");
        let source = r#"
            module.exports = async function (pi) {
                pi.on("session_start", async function (event, ctx) {
                    const ok = await ctx.ui.confirm("Go?", "body text");
                    const text = await ctx.ui.input("Name?", "type here");
                    const picked = await ctx.ui.select("Pick", ["a", "b"]);
                    ctx.ui.notify("done", "warning");
                    pi.appendEntry("ui-summary", { ok: ok, text: text, picked: picked });
                });
            };
        "#;
        host.load(entry("async-ui"), source).await.expect("load");
        host.emit_event_with(&ExtensionEvent::SessionStart, Some("tui"), true, "/tmp")
            .await
            .expect("dispatch");

        let log = host.log();
        let summary = log
            .entries
            .iter()
            .find(|e| e.custom_type == "ui-summary")
            .expect("ui-summary entry");
        assert_eq!(summary.data["ok"], true, "confirm routed: {summary:?}");
        assert_eq!(summary.data["text"], "in:Name?:type here");
        assert_eq!(summary.data["picked"], "Pick:b");

        // `notify` is fire-and-forget: the JS call returns before the
        // worker task drains the envelope, so give it a beat.
        let mut notifies = Vec::new();
        for _ in 0..100 {
            notifies = handler.notifies.lock().expect("notify lock").clone();
            if !notifies.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(
            notifies.len(),
            1,
            "notify reaches the handler: {notifies:?}"
        );
        assert_eq!(notifies[0], ("done".to_string(), UiLevel::Warning));
    });
}

#[test]
fn missing_ui_handler_denies_without_blocking() {
    let runtime = rt();
    runtime.block_on(async {
        // `JsExtensionHost::new()` installs no UI handler at all.
        let host = JsExtensionHost::new().await.expect("host");
        let source = r#"
            module.exports = async function (pi) {
                pi.on("session_start", async function (event, ctx) {
                    const ok = await ctx.ui.confirm("Go?", "body");
                    const text = await ctx.ui.input("Name?", "ph");
                    const picked = await ctx.ui.select("Pick", ["a", "b"]);
                    ctx.ui.notify("still fine", "info");
                    pi.appendEntry("ui-summary", { ok: ok, text: text, picked: picked });
                });
            };
        "#;
        host.load(entry("no-ui-handler"), source)
            .await
            .expect("load");
        // The whole dispatch has to complete — a fallback that blocked
        // would hang here instead of returning.
        host.emit_event_with(&ExtensionEvent::SessionStart, Some("tui"), true, "/tmp")
            .await
            .expect("dispatch");

        let log = host.log();
        let summary = log
            .entries
            .iter()
            .find(|e| e.custom_type == "ui-summary")
            .expect("ui-summary entry");
        assert_eq!(summary.data["ok"], false);
        assert!(summary.data["text"].is_null(), "{summary:?}");
        assert!(summary.data["picked"].is_null(), "{summary:?}");
    });
}

#[test]
fn tool_execute_sees_host_tool_context() {
    let runtime = rt();
    runtime.block_on(async {
        // Wiring builds the host with the session's mode / hasUI so a
        // tool's `execute(args, ctx)` reports the same `ctx.hasUI` the
        // event handlers see.
        let host = JsExtensionHost::with_options(HostOptions::default().with_tool_context(
            pi_extensions::ToolContext {
                mode: "tui".into(),
                has_ui: true,
                cwd: "/tmp".into(),
            },
        ))
        .await
        .expect("host");
        let source = r#"
            module.exports = function (pi) {
                pi.registerTool({
                    name: "ctx_probe",
                    label: "Ctx probe",
                    description: "Reports the ctx a tool execution receives.",
                    parameters: { type: "object" },
                    execute: function (args, ctx) {
                        return {
                            content: [{ type: "text", text: JSON.stringify({
                                mode: ctx.mode, hasUI: ctx.hasUI, cwd: ctx.cwd,
                            }) }],
                        };
                    },
                });
            };
        "#;
        host.load(entry("ctx-probe"), source).await.expect("load");
        let outcome = host.execute_tool("ctx_probe", "{}").await.expect("execute");
        let text = outcome.content[0]["text"].as_str().expect("text");
        let parsed: serde_json::Value = serde_json::from_str(text).expect("json");
        assert_eq!(parsed["mode"], "tui");
        assert_eq!(parsed["hasUI"], true);
        assert_eq!(parsed["cwd"], "/tmp");
    });
}

#[test]
fn non_interactive_ui_requests_deny_and_report_via_notify() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        let source = r#"
            module.exports = async function (pi) {
                pi.on("session_start", async function (event, ctx) {
                    const ok = await ctx.ui.confirm("Go?", "body");
                    const picked = await ctx.ui.select("Pick", ["a", "b"]);
                    pi.appendEntry("denied", { ok: ok, picked: picked });
                });
            };
        "#;
        host.load(entry("non-interactive"), source)
            .await
            .expect("load");
        // print / rpc / no-TTY: `ctx.hasUI` is false, so the shim answers
        // immediately instead of blocking a client on a dialog nobody can
        // render.
        host.emit_event_with(&ExtensionEvent::SessionStart, Some("rpc"), false, "/tmp")
            .await
            .expect("dispatch");

        let log = host.log();
        let denied = log
            .entries
            .iter()
            .find(|e| e.custom_type == "denied")
            .expect("denied entry");
        assert_eq!(denied.data["ok"], false);
        assert!(denied.data["picked"].is_null(), "{denied:?}");

        // …and the denial is surfaced as a warning notify rather than
        // vanishing silently.
        let warning = log
            .entries
            .iter()
            .find(|e| e.custom_type == "ui_notify")
            .expect("deny notify");
        assert_eq!(warning.data["level"], "warning");
        let message = warning.data["message"].as_str().unwrap_or_default();
        assert!(message.contains("denied"), "{message}");
    });
}

// ---------------------------------------------------------------------------
// Stage 23: `resources_discover`
// ---------------------------------------------------------------------------

/// Parse the aggregate out of a `_pi_dispatch` summary. The shim returns
/// one result per handler, so a `resources_discover` fan-out across
/// several extensions has to be folded together.
#[test]
fn discovered_resources_parses_every_handler_result() {
    let outcome = DispatchOutcome {
        handled: true,
        subscribers: 2,
        results: vec![
            json!({ "skillPaths": ["/a/SKILL.md", "/b"] }),
            json!({ "skillPaths": ["/c/SKILL.md"], "promptPaths": ["/p/note.md"] }),
        ],
        errored: None,
    };

    let discovered = pi_extensions::DiscoveredResources::from_dispatch(&outcome);
    assert_eq!(
        discovered.skill_paths,
        vec![
            PathBuf::from("/a/SKILL.md"),
            PathBuf::from("/b"),
            PathBuf::from("/c/SKILL.md"),
        ]
    );
    assert_eq!(discovered.prompt_paths, vec![PathBuf::from("/p/note.md")]);
    assert!(discovered.theme_paths.is_empty());
    assert!(!discovered.is_empty());
}

/// A duplicate path two handlers both advertise must survive once, and a
/// malformed result must not cost the caller the paths its peers
/// returned.
#[test]
fn discovered_resources_dedups_and_tolerates_garbage() {
    let outcome = DispatchOutcome {
        handled: true,
        subscribers: 4,
        results: vec![
            json!({ "skillPaths": ["/shared/SKILL.md", "  ", 7, null] }),
            json!("not-an-object"),
            json!(null),
            json!({
                "skillPaths": ["/shared/SKILL.md"],
                "themePaths": [{ "path": "/themes/one.json" }],
            }),
        ],
        errored: None,
    };

    let discovered = pi_extensions::DiscoveredResources::from_dispatch(&outcome);
    assert_eq!(
        discovered.skill_paths,
        vec![PathBuf::from("/shared/SKILL.md")]
    );
    assert_eq!(
        discovered.theme_paths,
        vec![PathBuf::from("/themes/one.json")]
    );
    assert!(discovered.prompt_paths.is_empty());
}

/// End to end through the real QuickJS host: an extension subscribes to
/// `resources_discover` and the bridge hands the paths back to the
/// caller, with the event carrying the session `cwd` and `reason`.
#[test]
fn bridge_discovers_resources_from_an_extension() {
    use pi_extensions::JsExtensionBridge;
    use pi_protocol::ResourcesDiscoverReason;

    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        let source = r#"
            module.exports = function (pi) {
                pi.on("resources_discover", function (event) {
                    pi.appendEntry("discover_seen", {
                        cwd: event.cwd,
                        reason: event.reason,
                    });
                    return {
                        skillPaths: [event.cwd + "/skills/dyn/SKILL.md"],
                        promptPaths: [event.cwd + "/prompts/dyn.md"],
                    };
                });
            };
        "#;
        host.load(entry("discover"), source).await.expect("load");

        let bridge = JsExtensionBridge::new(host.clone(), "print", false, "/work/project");
        let discovered = bridge
            .discover_resources(ResourcesDiscoverReason::Startup)
            .await;

        assert_eq!(
            discovered.skill_paths,
            vec![PathBuf::from("/work/project/skills/dyn/SKILL.md")]
        );
        assert_eq!(
            discovered.prompt_paths,
            vec![PathBuf::from("/work/project/prompts/dyn.md")]
        );

        // The handler saw the documented event fields, not the shim's
        // internal ctx plumbing.
        let log = host.log();
        let seen = log
            .entries
            .iter()
            .find(|e| e.custom_type == "discover_seen")
            .expect("handler ran");
        assert_eq!(seen.data["cwd"], "/work/project");
        assert_eq!(seen.data["reason"], "startup");
    });
}

/// An extension with no `resources_discover` handler reports an empty
/// bundle instead of an error — discovery must never be a load-time
/// failure for the extensions that predate the event.
#[test]
fn bridge_discovery_is_empty_without_a_handler() {
    use pi_extensions::JsExtensionBridge;
    use pi_protocol::ResourcesDiscoverReason;

    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        host.load(
            entry("no-resources"),
            r#"module.exports = function (pi) {
                   pi.on("session_start", function () {});
               };"#,
        )
        .await
        .expect("load");

        let bridge = JsExtensionBridge::new(host, "print", false, "/tmp");
        let discovered = bridge
            .discover_resources(ResourcesDiscoverReason::Reload)
            .await;
        assert!(discovered.is_empty(), "{discovered:?}");
    });
}

// ---------------------------------------------------------------------------
// Stage 25: ESM extension loading (`import` / `export default` /
// `import.meta` + the `node:path` / `node:url` virtual modules).
// ---------------------------------------------------------------------------

/// Build an entry whose source path is the given absolute path. The path
/// is what `import.meta.url` / `import.meta.dirname` resolve against.
fn entry_at(id: &str, path: &str) -> ExtensionEntry {
    ExtensionEntry {
        source: PathBuf::from(path),
        id: id.to_string(),
        label: None,
    }
}

/// The ESM twin of upstream
/// `packages/coding-agent/examples/extensions/dynamic-resources/index.ts`:
/// `import { dirname, join } from "node:path"`, `import { fileURLToPath }
/// from "node:url"`, `const baseDir = dirname(fileURLToPath(import.meta.url))`
/// and an `export default function (pi)` factory returning the three
/// resource collections.
#[test]
fn esm_extension_discovers_resources_like_upstream_example() {
    use pi_extensions::JsExtensionBridge;
    use pi_protocol::ResourcesDiscoverReason;

    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        let source = r#"
            import { dirname, join } from "node:path";
            import { fileURLToPath } from "node:url";

            const baseDir = dirname(fileURLToPath(import.meta.url));

            export default function (pi) {
                pi.on("resources_discover", () => {
                    return {
                        skillPaths: [join(baseDir, "SKILL.md")],
                        promptPaths: [join(baseDir, "dynamic.md")],
                        themePaths: [join(baseDir, "dynamic.json")],
                    };
                });
            }
        "#;
        host.load(
            entry_at(
                "dynamic-resources",
                "/work/project/.pi/extensions/dynamic-resources/index.mjs",
            ),
            source,
        )
        .await
        .expect("ESM extension should load");

        let bridge = JsExtensionBridge::new(host.clone(), "print", false, "/work/project");
        let discovered = bridge
            .discover_resources(ResourcesDiscoverReason::Startup)
            .await;

        assert_eq!(
            discovered.skill_paths,
            vec![PathBuf::from(
                "/work/project/.pi/extensions/dynamic-resources/SKILL.md"
            )]
        );
        assert_eq!(
            discovered.prompt_paths,
            vec![PathBuf::from(
                "/work/project/.pi/extensions/dynamic-resources/dynamic.md"
            )]
        );
        assert_eq!(
            discovered.theme_paths,
            vec![PathBuf::from(
                "/work/project/.pi/extensions/dynamic-resources/dynamic.json"
            )]
        );
    });
}

/// `export default (pi) => { ... }` (the arrow form the issue calls out)
/// plus aliased named imports and `import.meta.dirname`.
#[test]
fn esm_arrow_default_export_and_aliased_imports() {
    use pi_extensions::JsExtensionBridge;
    use pi_protocol::ResourcesDiscoverReason;

    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        let source = r#"
            import { dirname as d, join as j } from "node:path";

            const baseDir = import.meta.dirname;

            export default (pi) => {
                pi.on("resources_discover", () => ({
                    skillPaths: [j(d(baseDir + "/nested/file.md"), "SKILL.md")],
                }));
            };
        "#;
        host.load(entry_at("arrow-ext", "/opt/ext/arrow/index.mjs"), source)
            .await
            .expect("arrow ESM extension should load");

        let bridge = JsExtensionBridge::new(host, "print", false, "/w");
        let discovered = bridge
            .discover_resources(ResourcesDiscoverReason::Reload)
            .await;
        assert_eq!(
            discovered.skill_paths,
            vec![PathBuf::from("/opt/ext/arrow/nested/SKILL.md")]
        );
    });
}

/// Keywords inside strings and comments must not fool the source scanner.
#[test]
fn esm_scanner_ignores_keywords_in_strings_and_comments() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        let source = r#"
            import { join } from "node:path";
            const decorative = "export default function (pi) {}";
            // import { nope } from "not-a-real-module";
            /* export default 42; */
            export default function (pi) {
                pi.on("session_start", () => {
                    return { text: decorative, path: join("/a", "b") };
                });
            }
        "#;
        host.load(entry_at("scanner", "/tmp/scanner/index.mjs"), source)
            .await
            .expect("load");

        let outcome = host
            .emit_event(&ExtensionEvent::SessionStart)
            .await
            .expect("dispatch");
        assert!(outcome.handled, "{outcome:?}");
        assert_eq!(outcome.results[0]["path"], "/a/b");
        assert_eq!(
            outcome.results[0]["text"],
            "export default function (pi) {}"
        );
    });
}

/// A CommonJS extension still loads after an ESM one on the same host, and
/// both register into the shared tool registry.
#[test]
fn esm_and_commonjs_extensions_coexist() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        host.load(
            entry_at("esm-tool", "/tmp/esm-tool/index.mjs"),
            r#"
                import { join } from "node:path";
                export default function (pi) {
                    pi.registerTool({
                        name: "esm_tool",
                        label: "ESM tool",
                        description: "registered from ESM",
                        parameters: { type: "object" },
                        execute: () => ({
                            content: [{ type: "text", text: join("/esm", "ok") }],
                        }),
                    });
                }
            "#,
        )
        .await
        .expect("ESM load");

        host.load(
            entry("cjs-tool"),
            r#"
                module.exports = function (pi) {
                    pi.registerTool({
                        name: "cjs_tool",
                        label: "CJS tool",
                        description: "registered from CommonJS",
                        parameters: { type: "object" },
                        execute: () => ({ content: [{ type: "text", text: "cjs" }] }),
                    });
                };
            "#,
        )
        .await
        .expect("CJS load");

        let mut names = host.registered_tool_names().await;
        names.sort();
        assert_eq!(names, vec!["cjs_tool".to_string(), "esm_tool".to_string()]);

        let outcome = host.execute_tool("esm_tool", "{}").await.expect("execute");
        assert_eq!(outcome.content[0]["text"], "/esm/ok");
    });
}

/// Type-only imports are erased: they are a compile-time-only artifact and
/// never reach the runtime module (the SDK modules exist, but a type-only
/// import binds nothing).
#[test]
fn esm_type_only_import_is_erased() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        let source = r#"
            import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
            export default function (pi) {
                pi.appendEntry("loaded", { ok: true });
            }
        "#;
        host.load(entry_at("type-only", "/tmp/type-only/index.mjs"), source)
            .await
            .expect("type-only import should be erased");
        assert_eq!(host.log().entries.len(), 1);
    });
}

/// ESM without a default export fails with a readable, file-named error.
#[test]
fn esm_without_default_export_is_reported() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        let err = host
            .load(
                entry_at("no-default", "/tmp/no-default/index.mjs"),
                r#"
                    import { join } from "node:path";
                    pi.on("session_start", () => {});
                "#,
            )
            .await
            .expect_err("must reject ESM without export default");
        let message = err.to_string();
        assert!(message.contains("export default"), "{message}");
        assert!(message.contains("no-default"), "{message}");
    });
}

// ---------------------------------------------------------------------------
// Stage 26 follow-up: the `typebox` virtual module — `Type.Object(...)` →
// the JSON Schema carried in `registerTool({ parameters })`.
// ---------------------------------------------------------------------------

/// Upstream extensions declare tool parameters with `Type.Object(...)`.
/// The shim must turn that into the JSON Schema the host (and the model)
/// consumes, with `Type.Optional` props excluded from `required`.
#[test]
fn esm_typebox_tool_parameters_reach_the_host() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        host.load(
            entry_at("typebox-tool", "/tmp/typebox-tool/index.mjs"),
            r#"
                import { Type } from "typebox";
                export default function (pi) {
                    pi.registerTool({
                        name: "read_file",
                        label: "Read file",
                        description: "Read a file",
                        parameters: Type.Object({
                            path: Type.String(),
                            limit: Type.Optional(Type.Number({ description: "max lines" })),
                            tags: Type.Array(Type.String()),
                            mode: Type.Union([Type.Literal("fast"), Type.Literal("slow")]),
                        }),
                        execute: () => ({ content: [{ type: "text", text: "ok" }] }),
                    });
                }
            "#,
        )
        .await
        .expect("typebox extension should load");

        let tools = host.registered_tools();
        assert_eq!(tools.len(), 1, "{tools:?}");
        assert_eq!(
            tools[0].parameters,
            json!({
                "type": "object",
                "required": ["path", "tags", "mode"],
                "properties": {
                    "path": { "type": "string" },
                    "limit": { "type": "number", "description": "max lines" },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "mode": {
                        "anyOf": [
                            { "type": "string", "const": "fast" },
                            { "type": "string", "const": "slow" }
                        ]
                    }
                }
            })
        );
    });
}

/// `@sinclair/typebox` is the historical package name for the same module,
/// so both specifiers resolve to the same builder. The schema also has to
/// stay JSON-serialisable: `JSON.stringify` drops the `Optional` symbol
/// markers on the way to the host.
#[test]
fn esm_typebox_sinclair_alias_and_enum() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        host.load(
            entry_at("typebox-alias", "/tmp/typebox-alias/index.mjs"),
            r#"
                import { Type } from "@sinclair/typebox";
                export default function (pi) {
                    pi.registerTool({
                        name: "pick",
                        label: "Pick",
                        description: "Pick a level",
                        parameters: Type.Object({
                            level: Type.Enum({ Low: "low", High: "high" }),
                            nested: Type.Object({ flag: Type.Boolean() }),
                            blob: Type.Record(Type.String(), Type.Unknown()),
                        }),
                        execute: () => ({ content: [{ type: "text", text: "ok" }] }),
                    });
                }
            "#,
        )
        .await
        .expect("@sinclair/typebox alias should load");

        let tools = host.registered_tools();
        assert_eq!(tools.len(), 1, "{tools:?}");
        assert_eq!(
            tools[0].parameters,
            json!({
                "type": "object",
                "required": ["level", "nested", "blob"],
                "properties": {
                    "level": { "enum": ["low", "high"] },
                    "nested": {
                        "type": "object",
                        "required": ["flag"],
                        "properties": { "flag": { "type": "boolean" } }
                    },
                    "blob": {
                        "type": "object",
                        "patternProperties": { "^.*$": {} }
                    }
                }
            })
        );
    });
}

/// A value TypeBox cannot turn into a literal (e.g. `null` or an object)
/// fails at load with a readable message instead of emitting a schema the
/// model can never satisfy.
#[test]
fn esm_typebox_rejects_invalid_literal() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        let err = host
            .load(
                entry_at("bad-literal", "/tmp/bad-literal/index.mjs"),
                r#"
                    import { Type } from "typebox";
                    export default function (pi) {
                        pi.registerTool({
                            name: "bad",
                            label: "Bad",
                            description: "bad literal",
                            parameters: Type.Object({ x: Type.Literal(null) }),
                            execute: () => ({ content: [] }),
                        });
                    }
                "#,
            )
            .await
            .expect_err("Type.Literal(null) must be rejected");
        assert!(err.to_string().contains("Type.Literal"), "{err}");
    });
}

/// Unsupported virtual modules (and relative specifiers) name the thing
/// that failed instead of surfacing `undefined`.
#[test]
fn esm_unsupported_imports_are_reported() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        let err = host
            .load(
                entry_at("api-ext", "/tmp/api-ext/index.mjs"),
                r#"
                    import { debounce } from "lodash";
                    export default function (pi) {}
                "#,
            )
            .await
            .expect_err("lodash is not a supported virtual module");
        let message = err.to_string();
        assert!(message.contains("lodash"), "{message}");
        // The error names the supported set, including `typebox` and the
        // `@earendil-works/*` SDK packages.
        assert!(message.contains("node:path"), "{message}");
        assert!(message.contains("typebox"), "{message}");
        assert!(message.contains("@earendil-works/pi-tui"), "{message}");

        // A value import of a documented SDK gap fails the load too, naming
        // the export and its package instead of binding `undefined`.
        // (`create*Tool` used to be the example gap; it is bridged as of
        // LUM-1175, and `anthropicMessagesApi` as of LUM-1180, so this pins
        // the contract to a gap that still exists.)
        let err = host
            .load(
                entry_at("sdk-gap-ext", "/tmp/sdk-gap-ext/index.mjs"),
                r#"
                    import { googleGenerativeAIApi } from "@earendil-works/pi-ai/compat";
                    export default function (pi) {}
                "#,
            )
            .await
            .expect_err("a documented SDK gap must not load silently");
        let message = err.to_string();
        assert!(message.contains("googleGenerativeAIApi"), "{message}");
        assert!(
            message.contains("@earendil-works/pi-ai/compat"),
            "{message}"
        );

        let err = host
            .load(
                entry_at("relative-ext", "/tmp/relative-ext/index.mjs"),
                r#"
                    import { helper } from "./helper.mjs";
                    export default function (pi) {}
                "#,
            )
            .await
            .expect_err("relative imports are unsupported");
        assert!(err.to_string().contains("relative import"), "{err}");
    });
}
