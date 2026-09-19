//! `node:module` / `node:readline` virtual module tests — LUM-1129.
//!
//! Both modules are pure JS in `runtime/pi-ext-shim.mjs` (they add no host
//! op), so these tests drive them the way an extension would: registered
//! from source through the real [`JsExtensionHost`], executed as a tool,
//! with the results asserted as `details`.
//!
//! `node:module` is checked against the contract upstream depends on —
//! `createRequire` returns *real* bridged modules, `require.cache` is the
//! shared `Module._cache`, and an unresolvable specifier throws a real
//! `Error` carrying `code === "MODULE_NOT_FOUND"`. `node:readline` is checked
//! against the sync subset (`on("line")`, `[Symbol.asyncIterator]`,
//! `question`), plus the branches that must fail loudly rather than hang:
//! terminal/raw mode, no input at all, and a closed interface.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use pi_extensions::{
    ExtensionEntry, HostOptions, JsExtensionHost, ScriptedUiAnswers, ScriptedUiHandler, ToolContext,
};
use serde_json::{json, Value};

static SCRATCH_COUNTER: AtomicU32 = AtomicU32::new(0);

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
            "pi_module_readline/{}-{name}-{unique}",
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

/// Build a host whose session cwd is the scratch directory.
async fn host_with(has_ui: bool, cwd: &str) -> JsExtensionHost {
    JsExtensionHost::with_options(HostOptions {
        tool_context: ToolContext {
            mode: "print".to_string(),
            has_ui,
            cwd: cwd.to_string(),
        },
        ..HostOptions::default()
    })
    .await
    .expect("host")
}

/// Load `source`, run the tool it registers, and return `details`.
async fn probe(host: &JsExtensionHost, name: &str, source: &str, tool: &str) -> Value {
    host.load(
        entry_at(name, &format!("/tmp/pi_module_readline/{name}.mjs")),
        source,
    )
    .await
    .unwrap_or_else(|e| panic!("load {name}: {e}"));
    let outcome = host
        .execute_tool(tool, "{}")
        .await
        .unwrap_or_else(|e| panic!("execute {tool}: {e}"));
    assert!(!outcome.is_error, "{tool} failed: {outcome:?}");
    outcome.details.expect("details")
}

const MODULE_SOURCE: &str = r#"
    import * as moduleNs from "node:module";
    import { createRequire, isBuiltin, builtinModules, Module } from "module";

    export default function (pi) {
        pi.registerTool({
            name: "module_probe",
            label: "module probe",
            description: "exercises node:module",
            parameters: { type: "object" },
            execute: (args, ctx) => {
                const out = {};
                const require = createRequire(import.meta.url);

                // `node:module` and the bare `module` alias are one module.
                out.sameModule = moduleNs.createRequire === createRequire
                    && moduleNs.Module === Module
                    && moduleNs.isBuiltin === isBuiltin;
                out.fromHrefString = createRequire(import.meta.url).resolve("node:fs");
                out.fromHrefObject = typeof createRequire({ href: "file:///tmp/x.mjs" }) === "function";

                // `createRequire` resolves the *bridged* builtins for real.
                out.resolvedFs = require.resolve("node:fs");
                out.resolvedBarePath = require.resolve("path");
                out.hasReadFileSync = typeof require("node:fs").readFileSync === "function";
                out.bareAndPrefixedAgree = require("fs") === require("node:fs");
                out.hasBuffer = typeof require("node:buffer").Buffer === "function";
                // Not just "the object exists": call a real bridged op
                // through the `createRequire` result.
                out.crc32 = require("node:zlib").crc32("123456789");
                out.requireMain = require.main === undefined;

                // Unresolvable specifiers throw real errors with a `code`.
                try { require("node:stream"); out.stream = "no-throw"; }
                catch (e) { out.stream = { name: e.name, code: e.code, message: e.message }; }
                try { require("./nope.js"); out.relative = "no-throw"; }
                catch (e) { out.relative = { code: e.code, message: e.message, stack: e.requireStack }; }
                try { require.resolve("node:net"); out.resolveError = "no-throw"; }
                catch (e) { out.resolveError = e.code; }

                // `Module` + the shared `require.cache`.
                const p = ctx.cwd + "/fake-native.node";
                const mod = new Module(p);
                mod.exports = { mocked: true };
                Module._cache[p] = mod;
                out.moduleFields = mod.id === p && mod.filename === p
                    && mod.loaded === false && Array.isArray(mod.children) && Array.isArray(mod.paths);
                out.cacheShared = require.cache === Module._cache;
                out.mockLoaded = require(p).mocked === true;
                out.modRequire = mod.require("node:os").EOL === "\n";
                out.statics = Module.createRequire === createRequire
                    && Module.isBuiltin === isBuiltin
                    && Array.isArray(Module.builtinModules);

                // `builtinModules` / `isBuiltin` describe what this host bridges.
                out.builtins = {
                    fs: builtinModules.includes("fs"),
                    module: builtinModules.includes("module"),
                    readline: builtinModules.includes("readline"),
                    path: builtinModules.includes("path"),
                    zlib: builtinModules.includes("zlib"),
                    stream: builtinModules.includes("stream"),
                };
                out.isBuiltin = {
                    prefixed: isBuiltin("node:fs"),
                    bare: isBuiltin("fs"),
                    zlib: isBuiltin("node:zlib"),
                    stream: isBuiltin("node:stream"),
                    sdk: isBuiltin("@earendil-works/pi-tui"),
                };
                try { isBuiltin(5); out.isBuiltinBad = "no-throw"; }
                catch (e) { out.isBuiltinBad = e.code; }
                try { createRequire(7); out.createRequireBad = "no-throw"; }
                catch (e) { out.createRequireBad = e.code; }

                return { content: [{ type: "text", text: JSON.stringify(out) }], details: out };
            },
        });
    }
"#;

#[test]
fn node_module_create_require_resolves_bridged_builtins() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("module");
        let host = host_with(false, &scratch.as_str()).await;
        let out = probe(&host, "module", MODULE_SOURCE, "module_probe").await;

        assert_eq!(out["sameModule"], true, "{out}");
        assert_eq!(out["fromHrefString"], "node:fs", "{out}");
        assert_eq!(out["fromHrefObject"], true, "{out}");
        // `require.resolve` echoes the specifier: there is no on-disk
        // resolution to do for a virtual module.
        assert_eq!(out["resolvedFs"], "node:fs", "{out}");
        assert_eq!(out["resolvedBarePath"], "path", "{out}");
        assert_eq!(out["hasReadFileSync"], true, "{out}");
        assert_eq!(out["bareAndPrefixedAgree"], true, "{out}");
        assert_eq!(out["hasBuffer"], true, "{out}");
        // The known CRC-32/ISO-HDLC vector, computed through the require
        // `createRequire` handed out (LUM-1125's `zlib.crc32` op).
        assert_eq!(out["crc32"], 3421780262u32, "{out}");
        assert_eq!(out["requireMain"], true, "{out}");
    });
}

#[test]
fn node_module_unresolvable_specifiers_throw_real_errors() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("module-errors");
        let host = host_with(false, &scratch.as_str()).await;
        let out = probe(&host, "module_errors", MODULE_SOURCE, "module_probe").await;

        assert_eq!(out["stream"]["name"], "Error", "{out}");
        assert_eq!(out["stream"]["code"], "MODULE_NOT_FOUND", "{out}");
        let message = out["stream"]["message"].as_str().expect("message");
        assert!(
            message.contains("Cannot find module 'node:stream'"),
            "{message}"
        );
        assert!(message.contains("does not bridge"), "{message}");

        assert_eq!(out["relative"]["code"], "MODULE_NOT_FOUND", "{out}");
        let relative = out["relative"]["message"].as_str().expect("message");
        assert!(
            relative.contains("Cannot find module './nope.js'"),
            "{relative}"
        );
        assert!(
            relative.contains("Require stack:"),
            "a relative miss should point at the requester: {relative}"
        );
        // `import.meta.url` is a `file://` URL, so that is the requester the
        // stack reports.
        assert_eq!(
            out["relative"]["stack"][0], "file:///tmp/pi_module_readline/module_errors.mjs",
            "{out}"
        );

        assert_eq!(out["resolveError"], "MODULE_NOT_FOUND", "{out}");
    });
}

#[test]
fn node_module_and_require_cache_share_one_store() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("module-cache");
        let host = host_with(false, &scratch.as_str()).await;
        let out = probe(&host, "module_cache", MODULE_SOURCE, "module_probe").await;

        assert_eq!(out["moduleFields"], true, "{out}");
        assert_eq!(out["cacheShared"], true, "{out}");
        // A `require.cache` entry is a *real* resolution: the mocked native
        // addon the tui tests install is what `require()` returns.
        assert_eq!(out["mockLoaded"], true, "{out}");
        assert_eq!(out["modRequire"], true, "{out}");
        assert_eq!(out["statics"], true, "{out}");
    });
}

#[test]
fn node_module_builtin_helpers_describe_the_bridged_set() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("module-builtins");
        let host = host_with(false, &scratch.as_str()).await;
        let out = probe(&host, "module_builtins", MODULE_SOURCE, "module_probe").await;

        let builtins = &out["builtins"];
        assert_eq!(builtins["fs"], true, "{out}");
        assert_eq!(builtins["module"], true, "{out}");
        assert_eq!(builtins["readline"], true, "{out}");
        assert_eq!(builtins["path"], true, "{out}");
        assert_eq!(builtins["zlib"], true, "{out}");
        // Documented divergence: `builtinModules` is the bridged set, not
        // Node's full list.
        assert_eq!(builtins["stream"], false, "{out}");

        let is_builtin = &out["isBuiltin"];
        assert_eq!(is_builtin["prefixed"], true, "{out}");
        assert_eq!(is_builtin["bare"], true, "{out}");
        assert_eq!(is_builtin["zlib"], true, "{out}");
        assert_eq!(is_builtin["stream"], false, "{out}");
        assert_eq!(is_builtin["sdk"], false, "{out}");
        assert_eq!(out["isBuiltinBad"], "ERR_INVALID_ARG_TYPE", "{out}");
        assert_eq!(out["createRequireBad"], "ERR_INVALID_ARG_TYPE", "{out}");
    });
}

const READLINE_SOURCE: &str = r#"
    import * as readlineNs from "node:readline";
    import { createInterface } from "readline";

    export default function (pi) {
        pi.registerTool({
            name: "readline_probe",
            label: "readline probe",
            description: "exercises node:readline",
            parameters: { type: "object" },
            execute: async (args, ctx) => {
                const out = {};
                out.sameModule = readlineNs.createInterface === createInterface
                    && typeof readlineNs.Interface === "function";

                // Minimal Readable-ish input: `on("data")` / `on("end")`.
                const makeInput = () => {
                    const handlers = {};
                    return {
                        on(type, fn) {
                            (handlers[type] || (handlers[type] = [])).push(fn);
                            return this;
                        },
                        off(type, fn) {
                            const list = handlers[type] || [];
                            const index = list.indexOf(fn);
                            if (index >= 0) list.splice(index, 1);
                            return this;
                        },
                        push(data) {
                            for (const fn of (handlers.data || []).slice()) fn(data);
                        },
                        end() {
                            for (const fn of (handlers.end || []).slice()) fn();
                        },
                    };
                };

                // 1. `on("line")`, with CRLF split across two chunks.
                const input = makeInput();
                const rl = createInterface({ input });
                const seen = [];
                rl.on("line", (line) => seen.push(line));
                out.emitter = typeof rl.on === "function" && typeof rl.emit === "function";
                input.push("a\r");
                input.push("\nb\n");
                input.push("c\rd");
                out.syncLines = seen.slice();
                input.end();
                out.afterEnd = seen.slice();

                // 2. `for await` drains buffered lines, then finishes.
                const input2 = makeInput();
                const rl2 = createInterface({ input: input2 });
                let closeEvents = 0;
                rl2.on("close", () => { closeEvents += 1; });
                input2.push("one\ntwo\n");
                input2.end();
                const iterated = [];
                for await (const line of rl2) iterated.push(line);
                out.iterated = iterated;
                out.closeEvents = closeEvents;

                // 3. `question` waits for a line that has not arrived yet.
                const input3 = makeInput();
                const rl3 = createInterface({ input: input3 });
                const pending = rl3.question("name? ");
                input3.push("world\n");
                out.question = await pending;

                // 4. `question` resolves `null` when the input ends first.
                const input4 = makeInput();
                const rl4 = createInterface({ input: input4 });
                const pending4 = rl4.question("q ");
                input4.end();
                out.questionOnEnd = await pending4;

                // 5. `close()` settles a pending consumer instead of hanging.
                const input5 = makeInput();
                const rl5 = createInterface({ input: input5 });
                const waiting = rl5[Symbol.asyncIterator]().next();
                rl5.close();
                out.closedIteration = (await waiting).done === true;
                try { rl5.question("late? "); out.afterClose = "no-throw"; }
                catch (e) { out.afterClose = e.code; }

                // 6. Callback form.
                const input6 = makeInput();
                const rl6 = createInterface({ input: input6 });
                const callbackAnswer = new Promise((resolve) => {
                    rl6.question("cb? ", (value) => resolve(value));
                });
                input6.push("cb\n");
                out.callback = await callbackAnswer;

                // 7. `rl.write` feeds the same splitter.
                const input7 = makeInput();
                const rl7 = createInterface({ input: input7 });
                const written = [];
                rl7.on("line", (line) => written.push(line));
                rl7.write("typed\r\n");
                out.written = written;
                out.prompt = rl7.setPrompt("> ").getPrompt();

                // 8. The branches that must throw instead of pretending.
                try { createInterface({ input: makeInput(), terminal: true }); out.tty = "no-throw"; }
                catch (e) { out.tty = { name: e.name, code: e.code }; }
                try { createInterface(); out.noInput = "no-throw"; }
                catch (e) { out.noInput = e.code; }
                try { createInterface({ input: 42 }); out.badInput = "no-throw"; }
                catch (e) { out.badInput = e.code; }

                return { content: [{ type: "text", text: JSON.stringify(out) }], details: out };
            },
        });
    }
"#;

#[test]
fn node_readline_lines_questions_and_iteration() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("readline");
        let host = host_with(false, &scratch.as_str()).await;
        let out = probe(&host, "readline", READLINE_SOURCE, "readline_probe").await;

        assert_eq!(out["sameModule"], true, "{out}");
        assert_eq!(out["emitter"], true, "{out}");
        // `a\r` + `\nb\n` is one CRLF break; a lone `\r` is another.
        assert_eq!(out["syncLines"], json!(["a", "b", "c"]), "{out}");
        // Input end flushes the trailing line and closes the interface.
        assert_eq!(out["afterEnd"], json!(["a", "b", "c", "d"]), "{out}");
        assert_eq!(out["iterated"], json!(["one", "two"]), "{out}");
        assert_eq!(out["closeEvents"], 1, "{out}");
        assert_eq!(out["question"], "world", "{out}");
        assert!(out["questionOnEnd"].is_null(), "{out}");
        assert_eq!(out["closedIteration"], true, "{out}");
        assert_eq!(out["callback"], "cb", "{out}");
        assert_eq!(out["written"], json!(["typed"]), "{out}");
        assert_eq!(out["prompt"], "> ", "{out}");
    });
}

#[test]
fn node_readline_unsupported_branches_throw() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("readline-errors");
        let host = host_with(false, &scratch.as_str()).await;
        let out = probe(&host, "readline_errors", READLINE_SOURCE, "readline_probe").await;

        assert_eq!(out["afterClose"], "ERR_USE_AFTER_CLOSE", "{out}");
        assert_eq!(out["tty"]["name"], "Error", "{out}");
        assert_eq!(out["tty"]["code"], "ERR_READLINE_TTY_UNSUPPORTED", "{out}");
        assert_eq!(out["noInput"], "ERR_READLINE_NO_INPUT", "{out}");
        assert_eq!(out["badInput"], "ERR_INVALID_ARG_TYPE", "{out}");
    });
}

/// Without an `input` stream the session UI is the only place a human can
/// answer — the host owns stdin. `question` therefore routes through
/// `host_ui_input`, the same channel `ctx.ui.input` uses.
#[test]
fn node_readline_question_without_input_uses_the_ui_dialog() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("readline-ui");
        let answers = ScriptedUiAnswers {
            inputs: [("What is your name? ".to_string(), "alice".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let handler = Arc::new(ScriptedUiHandler::new(answers));
        let host = JsExtensionHost::with_options(HostOptions {
            ui_handler: Some(handler),
            tool_context: ToolContext {
                mode: "tui".to_string(),
                has_ui: true,
                cwd: scratch.as_str(),
            },
            ..HostOptions::default()
        })
        .await
        .expect("host");

        let source = r#"
            import { createInterface } from "node:readline";
            export default function (pi) {
                pi.registerTool({
                    name: "ui_question",
                    label: "ui question",
                    description: "asks the session UI",
                    parameters: { type: "object" },
                    execute: async (args, ctx) => {
                        const rl = createInterface();
                        const answer = await rl.question("What is your name? ");
                        let afterClose = null;
                        rl.close();
                        try { rl.question("late? "); } catch (e) { afterClose = e.code; }
                        return { content: [{ type: "text", text: JSON.stringify({ answer: answer, afterClose: afterClose }) }], details: { answer: answer, hasUI: ctx.hasUI, afterClose: afterClose } };
                    },
                });
            }
        "#;
        let out = probe(&host, "readline_ui", source, "ui_question").await;
        assert_eq!(out["answer"], "alice", "{out}");
        assert_eq!(out["hasUI"], true, "{out}");
        assert_eq!(out["afterClose"], "ERR_USE_AFTER_CLOSE", "{out}");
    });
}
