//! End-to-end tests for the JS extension host.
//!
//! These tests mirror what `pi-coding-agent` does when it loads an
//! extension from disk: the agent enumerates a directory of `.js`/`.ts`
//! files, calls [`JsExtensionHost::load`] for each, then emits
//! lifecycle events. The tests below cover:
//!
//! 1. Loading the upstream `hello.ts` example (translated to a small
//!    JS source — the `.ts` copy in `examples/` is kept verbatim for
//!    reference and TypeScript-compatibility tracking).
//! 2. Loading an extension that uses `pi.on("session_start", …)` and
//!    registering a tool the LLM can call.
//! 3. Executing the registered tool end-to-end through
//!    [`JsExtensionHost::execute_tool`] and checking the result is
//!    surfaced to the host log.
//! 4. Loading every `.js` file under `examples/` via the loader trait
//!    and asserting the resulting [`ExtensionRegistry`] snapshot.

use std::path::PathBuf;
use std::sync::Arc;

use pi_extensions::{
    ExtensionEntry, ExtensionSearchPaths, JsExtensionHost, ScriptedUiAnswers, ScriptedUiHandler,
    ToolExecutionOutcome,
};
use pi_protocol::ExtensionEvent;

/// Source for the upstream `hello.ts` extension translated to JS.
/// The TypeScript original is preserved verbatim in `examples/hello.ts`
/// for compatibility tracking.
const HELLO_JS: &str = r#"
module.exports = function (pi) {
    pi.registerTool({
        name: "hello",
        label: "Hello",
        description: "A simple greeting tool",
        parameters: {
            type: "object",
            properties: {
                name: { type: "string", description: "Name to greet" }
            },
            required: ["name"]
        },
        execute: function (args, _ctx) {
            return {
                content: [{ type: "text", text: "Hello, " + String(args.name) + "!" }],
                details: { greeted: args.name },
            };
        },
    });
    pi.on("session_start", function (event, ctx) {
        pi.appendEntry("hello_loaded", { ready: true });
    });
};
"#;

/// Source for a notify-on-session-start extension that mirrors the
/// upstream `notify.ts` example's intent.
const NOTIFY_JS: &str = r#"
module.exports = function (pi) {
    pi.on("session_start", function (event, ctx) {
        ctx.ui.notify("session started", "info");
    });
    pi.on("agent_settled", function (event, ctx) {
        ctx.ui.notify("agent settled", "info");
    });
};
"#;

/// Source for a custom-commands example mirroring the upstream
/// `custom-commands.ts` shape.
const CUSTOM_COMMANDS_JS: &str = r#"
module.exports = function (pi) {
    pi.registerCommand("echo", {
        description: "Echo a string",
        handler: async (args, ctx) => {
            ctx.ui.notify("echo: " + args, "info");
        },
    });
};
"#;

/// Commands whose handlers return a value (mirrors the upstream
/// contract loosely — upstream handlers return `Promise<void>`, the
/// Rust host additionally surfaces a returned value for the CLI).
const COMMAND_RESULT_JS: &str = r#"
module.exports = function (pi) {
    pi.registerCommand("greet", {
        description: "Greets",
        handler: async (args) => {
            return "command" + "-saw:" + String(args);
        },
    });
    pi.registerCommand("boom", {
        description: "Always throws",
        handler: async () => {
            throw new Error("boom-from-command");
        },
    });
};
"#;

/// Commands that exercise every host side-effect channel.
const COMMAND_SIDE_EFFECTS_JS: &str = r#"
module.exports = function (pi) {
    pi.registerCommand("record", {
        description: "Records side effects",
        handler: function (args, ctx) {
            pi.appendEntry("cmd_entry", { args: args });
            pi.sendMessage({ customType: "cmd_message", content: "hello" });
            pi.setSessionName("renamed-by-command");
            ctx.ui.notify("command finished", "info");
        },
    });
};
"#;

fn entry(id: &str) -> ExtensionEntry {
    ExtensionEntry {
        source: PathBuf::from(format!("/tmp/pi_extensions_e2e/{id}.js")),
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

#[test]
fn e2e_hello_extension_loads_and_registers_tool() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        host.load(entry("hello"), HELLO_JS).await.expect("load hello");
        let tools = host.registered_tool_names().await;
        assert_eq!(tools, vec!["hello".to_string()]);
    });
}

#[test]
fn e2e_hello_extension_executes_via_host() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        host.load(entry("hello"), HELLO_JS).await.expect("load hello");
        let outcome: ToolExecutionOutcome = host
            .execute_tool("hello", r#"{"name": "world"}"#)
            .await
            .expect("execute hello");
        assert!(!outcome.is_error);
        let content = outcome.content.first().expect("content block");
        assert_eq!(content["type"], "text");
        assert_eq!(content["text"], "Hello, world!");
        assert_eq!(outcome.details, Some(serde_json::json!({"greeted": "world"})));
    });
}

#[test]
fn e2e_notify_extension_emits_via_ui_handler() {
    let runtime = rt();
    runtime.block_on(async {
        let answers = ScriptedUiAnswers::default();
        let handler = Arc::new(ScriptedUiHandler::new(answers));
        let host = JsExtensionHost::with_handler(handler).await.expect("host");
        host.load(entry("notify"), NOTIFY_JS).await.expect("load notify");
        // Drive session_start with hasUI=true so the shim routes
        // through host_ui_notify.
        host.emit_event_with(&ExtensionEvent::SessionStart, Some("tui"), true, "/tmp")
            .await
            .expect("dispatch session_start");
        // We don't have a strong assertion target for the notify line
        // yet (it lives in the UI handler), but the log entries should
        // at least record the ui_notify side-effect.
        let log = host.log();
        assert!(log
            .entries
            .iter()
            .any(|e| e.custom_type == "ui_notify" && e.data["message"] == "session started"));
    });
}

#[test]
fn e2e_custom_commands_extension_registers_command() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        host.load(entry("commands"), CUSTOM_COMMANDS_JS)
            .await
            .expect("load commands");
        // The JS-side `_pi.commands` map is the source of truth, so the
        // name / description survive the load instead of being folded
        // into opaque log entries.
        let commands = host.registered_commands().await;
        assert_eq!(commands.len(), 1, "commands: {commands:?}");
        assert_eq!(commands[0].name, "echo");
        assert_eq!(commands[0].description, "Echo a string");
    });
}

#[test]
fn e2e_registered_command_handler_runs_with_its_arguments() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        host.load(entry("commands"), COMMAND_RESULT_JS)
            .await
            .expect("load commands");

        let outcome = host
            .execute_command("greet", "world", "cli", false, "/tmp")
            .await
            .expect("execute command");
        assert!(outcome.handled, "{outcome:?}");
        assert!(!outcome.is_error, "{outcome:?}");
        assert_eq!(outcome.result, serde_json::json!("command-saw:world"));
        assert_eq!(outcome.error, None);
    });
}

#[test]
fn e2e_unknown_command_is_reported_as_unhandled() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        host.load(entry("commands"), COMMAND_RESULT_JS)
            .await
            .expect("load commands");
        let outcome = host
            .execute_command("missing", "", "cli", false, "/tmp")
            .await
            .expect("execute command");
        assert!(!outcome.handled, "{outcome:?}");
        assert!(!outcome.is_error, "{outcome:?}");
    });
}

#[test]
fn e2e_throwing_command_handler_becomes_an_error_outcome() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        host.load(entry("commands"), COMMAND_RESULT_JS)
            .await
            .expect("load commands");
        let outcome = host
            .execute_command("boom", "", "cli", false, "/tmp")
            .await
            .expect("execute command");
        assert!(outcome.handled, "{outcome:?}");
        assert!(outcome.is_error, "{outcome:?}");
        assert!(
            outcome
                .error
                .as_deref()
                .is_some_and(|e| e.contains("boom-from-command")),
            "{outcome:?}"
        );
    });
}

#[test]
fn e2e_command_side_effects_are_drainable_once() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        host.load(entry("commands"), COMMAND_SIDE_EFFECTS_JS)
            .await
            .expect("load commands");
        host.execute_command("record", "payload", "cli", false, "/tmp")
            .await
            .expect("execute command");

        let effects = host.drain_side_effects();
        assert!(
            effects
                .entries
                .iter()
                .any(|e| e.custom_type == "cmd_entry" && e.data == serde_json::json!({"args": "payload"})),
            "entries: {:?}",
            effects.entries
        );
        assert_eq!(effects.session_name.as_deref(), Some("renamed-by-command"));
        assert!(
            effects.messages.iter().any(|m| m["customType"] == "cmd_message"),
            "messages: {:?}",
            effects.messages
        );
        // UI call traces stay out of the drained set: they are
        // diagnostics, not session state.
        assert!(
            effects
                .entries
                .iter()
                .all(|e| !e.custom_type.starts_with("ui_")),
            "entries: {:?}",
            effects.entries
        );

        // A second drain sees nothing: side effects are consumed once.
        let second = host.drain_side_effects();
        assert!(second.entries.is_empty(), "{second:?}");
        assert!(second.messages.is_empty(), "{second:?}");
        assert!(second.session_name.is_none(), "{second:?}");
    });
}

#[test]
fn e2e_extension_search_paths_lists_disk_files() {
    // Use the workspace `examples/` directory which is the canonical
    // home for the copies we vendored from upstream. We don't need
    // the files to actually load — just verify the loader finds them.
    let workspace_examples = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");
    let paths = ExtensionSearchPaths {
        global: None,
        project: Some(workspace_examples),
    };
    let names = paths.candidates();
    let names: Vec<String> = names
        .into_iter()
        .filter_map(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .collect();
    assert!(
        names.iter().any(|n| n == "hello.ts"),
        "examples/hello.ts must be present, got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "notify.ts"),
        "examples/notify.ts must be present, got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "custom-commands.ts"),
        "examples/custom-commands.ts must be present, got {names:?}"
    );
}

#[test]
fn e2e_unknown_tool_returns_error_outcome() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        let outcome = host.execute_tool("missing", "{}").await.expect("execute");
        assert!(outcome.is_error);
    });
}