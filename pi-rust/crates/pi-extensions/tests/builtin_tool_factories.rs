//! JS `create*Tool` factories — LUM-1175.
//!
//! Upstream extensions import `createReadTool` / `createWriteTool` /
//! `createEditTool` / `createBashTool` / `createFindTool` / `createGrepTool`
//! / `createLsTool` from `@earendil-works/pi-coding-agent` (for example
//! `examples/extensions/built-in-tool-renderer.ts` re-registers a built-in
//! tool with custom rendering but the original `execute`). The shim cannot
//! reach the concrete Rust tool bundle — that would be a dependency cycle —
//! so the host injects it as a [`BuiltinToolRunner`] and the shim bridges
//! through `host_builtin_tool_definition` (sync) / `host_builtin_tool`
//! (async).
//!
//! These tests drive the factories through a fake runner, so the bridge
//! contract is covered without a dependency on `pi-coding-agent`:
//!
//! 1. the factories import, are callable and expose the runner's schema;
//! 2. both call conventions reach the runner (`execute(id, params)` likes
//!    upstream's `AgentTool`, and `execute(args)` like `pi.registerTool`);
//! 3. a tool that failed comes back as `{ isError: true }` with content;
//! 4. without a runner the factories still import, but `execute` rejects
//!    with a named error instead of evaluating to `undefined`.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use pi_extensions::{
    BuiltinToolDefinition, BuiltinToolOutcome, BuiltinToolRunner, ExtensionEntry, HostOptions,
    JsExtensionHost, ToolContext,
};
use serde_json::{json, Value};

static SCRATCH_COUNTER: AtomicU32 = AtomicU32::new(0);

const TOOL_NAMES: [&str; 7] = ["read", "write", "edit", "bash", "find", "grep", "ls"];

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime")
}

/// A unique scratch directory, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let unique = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "pi_builtin_tool_factories/{}-{name}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Self(dir)
    }

    fn as_str(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn entry_at(id: &str, path: &str) -> ExtensionEntry {
    ExtensionEntry {
        source: PathBuf::from(path),
        id: id.to_string(),
        label: None,
    }
}

/// A runner standing in for the real built-in bundle.
///
/// Every known tool returns a text block naming the tool, the `cwd` it was
/// created with and the arguments it received, with the same payload under
/// `details` so pass-through is observable.
struct FakeRunner {
    /// Name that should come back as a *tool-level* failure (`isError`).
    failing: Option<String>,
}

impl FakeRunner {
    fn new() -> Self {
        Self { failing: None }
    }

    fn failing(name: &str) -> Self {
        Self {
            failing: Some(name.to_string()),
        }
    }
}

impl BuiltinToolRunner for FakeRunner {
    fn definition(&self, name: &str) -> Option<BuiltinToolDefinition> {
        if !TOOL_NAMES.contains(&name) {
            return None;
        }
        Some(BuiltinToolDefinition {
            name: name.to_string(),
            label: format!("{name} label"),
            description: format!("{name} description"),
            parameters: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
            }),
        })
    }

    fn run<'a>(
        &'a self,
        name: String,
        args: Value,
        cwd: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<BuiltinToolOutcome, String>> + Send + 'a>> {
        let failing = self.failing.clone();
        Box::pin(async move {
            let cwd = cwd.unwrap_or_default();
            let args_text = args.to_string();
            if failing.as_deref() == Some(name.as_str()) {
                return Ok(BuiltinToolOutcome {
                    content: vec![json!({"type": "text", "text": format!("unknown tool: {name}")})],
                    is_error: true,
                    details: None,
                });
            }
            Ok(BuiltinToolOutcome {
                content: vec![json!({
                    "type": "text",
                    "text": format!("ran {name} cwd={cwd} args={args_text}"),
                })],
                is_error: false,
                details: Some(json!({ "tool": name, "args": args })),
            })
        })
    }
}

async fn host_with_runner(cwd: &str, runner: Arc<dyn BuiltinToolRunner>) -> JsExtensionHost {
    JsExtensionHost::with_options(
        HostOptions {
            tool_context: ToolContext {
                mode: "print".to_string(),
                has_ui: false,
                cwd: cwd.to_string(),
            },
            ..HostOptions::default()
        }
        .with_builtin_tool_runner(runner),
    )
    .await
    .expect("host")
}

async fn host_without_runner(cwd: &str) -> JsExtensionHost {
    JsExtensionHost::with_options(HostOptions {
        tool_context: ToolContext {
            mode: "print".to_string(),
            has_ui: false,
            cwd: cwd.to_string(),
        },
        ..HostOptions::default()
    })
    .await
    .expect("host")
}

/// Every factory imports, is callable and hands out the runner's schema.
const DEFINITIONS_SOURCE: &str = r##"
    import {
        createBashTool, createEditTool, createFindTool, createGrepTool,
        createLsTool, createReadTool, createWriteTool,
    } from "@earendil-works/pi-coding-agent";

    export default function (pi) {
        pi.registerTool({
            name: "definitions_probe",
            label: "definitions probe",
            description: "checks the create*Tool definitions",
            parameters: { type: "object", properties: {} },
            execute: () => {
                const factories = {
                    read: createReadTool,
                    write: createWriteTool,
                    edit: createEditTool,
                    bash: createBashTool,
                    find: createFindTool,
                    grep: createGrepTool,
                    ls: createLsTool,
                };
                const names = {};
                const labels = {};
                const descriptions = {};
                const required = {};
                const executeIsFunction = {};
                for (const key of Object.keys(factories)) {
                    const tool = factories[key]("/tmp/pi-factory-defs");
                    names[key] = tool.name;
                    labels[key] = tool.label;
                    descriptions[key] = tool.description;
                    required[key] = tool.parameters && tool.parameters.required
                        ? tool.parameters.required
                        : null;
                    executeIsFunction[key] = typeof tool.execute === "function";
                }
                return {
                    content: [{ type: "text", text: "ok" }],
                    details: {
                        factoryTypes: Object.keys(factories).map((key) => typeof factories[key]),
                        names, labels, descriptions, required, executeIsFunction,
                    },
                };
            },
        });
    }
"##;

#[test]
fn factories_return_the_runners_definitions() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("definitions");
        let host = host_with_runner(&scratch.as_str(), Arc::new(FakeRunner::new())).await;

        host.load(
            entry_at("definitions_probe", "/tmp/pi_factories/definitions.mjs"),
            DEFINITIONS_SOURCE,
        )
        .await
        .expect("load definitions probe");

        let outcome = host
            .execute_tool("definitions_probe", &json!({}).to_string())
            .await
            .expect("execute definitions probe");
        assert!(!outcome.is_error, "{outcome:?}");
        let d = outcome.details.expect("details");

        for (index, name) in TOOL_NAMES.iter().enumerate() {
            assert_eq!(
                d["factoryTypes"][index],
                "function",
                "`create{}Tool` must be callable",
                capitalize(name)
            );
            assert_eq!(d["names"][name], json!(name));
            assert_eq!(d["labels"][name], json!(format!("{name} label")));
            assert_eq!(
                d["descriptions"][name],
                json!(format!("{name} description"))
            );
            assert_eq!(
                d["required"][name],
                json!(["path"]),
                "the runner's JSON Schema must reach the factory"
            );
            assert_eq!(d["executeIsFunction"][name], json!(true));
        }
    });
}

fn capitalize(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Both call conventions the shim has to support reach the runner.
const EXECUTE_SOURCE: &str = r##"
    import { createReadTool, createBashTool } from "@earendil-works/pi-coding-agent";

    export default function (pi) {
        pi.registerTool({
            name: "execute_probe",
            label: "execute probe",
            description: "checks the create*Tool execute bridge",
            parameters: { type: "object", properties: {} },
            execute: async () => {
                const read = createReadTool("/tmp/pi-factory-exec");
                // Upstream `AgentTool.execute(toolCallId, params, signal, onUpdate)`.
                const upstream = await read.execute("call-1", { path: "a.txt" });
                // The shim's own `pi.registerTool` convention: params first.
                const shimStyle = await createBashTool("/tmp/pi-factory-exec").execute({ command: "ls" });
                return {
                    content: [{ type: "text", text: "ok" }],
                    details: {
                        upstreamContent: upstream.content,
                        upstreamDetails: upstream.details,
                        upstreamIsError: upstream.isError,
                        shimContent: shimStyle.content,
                        shimArgs: shimStyle.details.args,
                    },
                };
            },
        });
    }
"##;

#[test]
fn execute_reaches_the_runner_in_both_call_conventions() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("execute");
        let host = host_with_runner(&scratch.as_str(), Arc::new(FakeRunner::new())).await;

        host.load(
            entry_at("execute_probe", "/tmp/pi_factories/execute.mjs"),
            EXECUTE_SOURCE,
        )
        .await
        .expect("load execute probe");

        let outcome = host
            .execute_tool("execute_probe", &json!({}).to_string())
            .await
            .expect("execute probe");
        assert!(!outcome.is_error, "{outcome:?}");
        let d = outcome.details.expect("details");

        assert_eq!(d["upstreamIsError"], json!(false));
        assert_eq!(
            d["upstreamContent"][0]["text"],
            json!("ran read cwd=/tmp/pi-factory-exec args={\"path\":\"a.txt\"}")
        );
        assert_eq!(
            d["upstreamDetails"],
            json!({ "tool": "read", "args": { "path": "a.txt" } }),
            "details must pass through unchanged"
        );
        assert_eq!(
            d["shimContent"][0]["text"],
            json!("ran bash cwd=/tmp/pi-factory-exec args={\"command\":\"ls\"}")
        );
        assert_eq!(d["shimArgs"], json!({ "command": "ls" }));
    });
}

/// A tool that failed is a structured result, not a rejected promise.
const FAILURE_SOURCE: &str = r##"
    import { createLsTool } from "@earendil-works/pi-coding-agent";

    export default function (pi) {
        pi.registerTool({
            name: "failure_probe",
            label: "failure probe",
            description: "checks the isError pass-through",
            parameters: { type: "object", properties: {} },
            execute: async () => {
                const result = await createLsTool("/tmp/pi-factory-fail").execute("call-1", { path: "." });
                return {
                    content: [{ type: "text", text: "ok" }],
                    details: {
                        content: result.content,
                        isError: result.isError,
                        details: result.details,
                    },
                };
            },
        });
    }
"##;

#[test]
fn tool_level_failures_come_back_as_structured_results() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("failure");
        let host = host_with_runner(&scratch.as_str(), Arc::new(FakeRunner::failing("ls"))).await;

        host.load(
            entry_at("failure_probe", "/tmp/pi_factories/failure.mjs"),
            FAILURE_SOURCE,
        )
        .await
        .expect("load failure probe");

        let outcome = host
            .execute_tool("failure_probe", &json!({}).to_string())
            .await
            .expect("execute failure probe");
        assert!(!outcome.is_error, "{outcome:?}");
        let d = outcome.details.expect("details");

        assert_eq!(d["isError"], json!(true));
        assert_eq!(d["content"][0]["text"], json!("unknown tool: ls"));
        assert_eq!(d["details"], Value::Null);
    });
}

/// Without a runner the factories still import — an extension that merely
/// *builds* a tool keeps loading — but executing one rejects with a named
/// error rather than silently returning `undefined`.
const NO_RUNNER_SOURCE: &str = r##"
    import { createReadTool } from "@earendil-works/pi-coding-agent";

    export default function (pi) {
        pi.registerTool({
            name: "missing_runner_probe",
            label: "missing runner probe",
            description: "checks the no-runner error envelope",
            parameters: { type: "object", properties: {} },
            execute: async () => {
                const tool = createReadTool("/tmp/pi-factory-missing");
                const details = {
                    name: tool.name,
                    parametersType: tool.parameters.type,
                };
                try {
                    await tool.execute("call-1", { path: "a.txt" });
                    details.threw = false;
                } catch (err) {
                    details.threw = true;
                    details.code = err.code;
                    details.message = err.message;
                }
                return { content: [{ type: "text", text: "ok" }], details };
            },
        });
    }
"##;

#[test]
fn factories_reject_with_a_named_error_without_a_runner() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("no-runner");
        let host = host_without_runner(&scratch.as_str()).await;

        host.load(
            entry_at("missing_runner_probe", "/tmp/pi_factories/missing.mjs"),
            NO_RUNNER_SOURCE,
        )
        .await
        .expect("the factories must still import without a runner");

        let outcome = host
            .execute_tool("missing_runner_probe", &json!({}).to_string())
            .await
            .expect("execute missing runner probe");
        assert!(!outcome.is_error, "{outcome:?}");
        let d = outcome.details.expect("details");

        assert_eq!(d["name"], json!("read"));
        assert_eq!(
            d["parametersType"],
            json!("object"),
            "a runner-less factory falls back to a permissive schema"
        );
        assert_eq!(d["threw"], json!(true));
        assert_eq!(d["code"], json!("ERR_PI_BUILTIN_TOOL"));
        assert!(
            d["message"]
                .as_str()
                .expect("message string")
                .contains("no built-in tool runner"),
            "error explains the missing runner: {}",
            d["message"]
        );
    });
}
