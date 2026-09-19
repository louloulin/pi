//! `pi.exec` tests — LUM-1107.
//!
//! Upstream extensions shell out through `pi.exec(command, args, options)`
//! (`packages/coding-agent/src/core/exec.ts`) rather than importing
//! `node:child_process` directly — eight of the repo's own examples
//! (`auto-commit-on-exit.ts`, `dirty-repo-guard.ts`, `git-checkpoint.ts`, …)
//! use it for `git`. These tests drive the bridge through the real host:
//! an extension registered from source, executed through
//! [`JsExtensionHost::execute_tool`], with a scratch directory standing in
//! for the session working tree.
//!
//! The tests spawn real POSIX tools (`echo`, `pwd`, `sleep`, `sh`), so the
//! whole file is Unix-only; the bridge itself is platform-neutral.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use pi_extensions::{ExtensionEntry, HostOptions, JsExtensionHost, ToolContext};
use pi_protocol::ExtensionEvent;
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
        let dir =
            std::env::temp_dir().join(format!("pi_exec/{}-{name}-{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Self(dir)
    }

    fn as_str(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Build a host whose session cwd is the scratch directory, so a `pi.exec`
/// call without `options.cwd` runs where the session runs.
async fn host_with_cwd(cwd: &str) -> JsExtensionHost {
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

async fn load(host: &JsExtensionHost, source: &str) {
    host.load(
        ExtensionEntry {
            source: PathBuf::from("/tmp/pi_extensions_exec/probe.js"),
            id: "exec_probe".to_string(),
            label: None,
        },
        source,
    )
    .await
    .expect("load extension");
}

async fn run_probe(host: &JsExtensionHost, tool: &str) -> Value {
    let outcome = host
        .execute_tool(tool, "{}")
        .await
        .unwrap_or_else(|error| panic!("execute {tool}: {error}"));
    assert!(!outcome.is_error, "{tool} reported an error: {outcome:?}");
    outcome
        .details
        .unwrap_or_else(|| panic!("{tool} returned no details"))
}

/// stdout / stderr / exit code / spawn failure in one pass.
#[test]
fn pi_exec_reports_stdout_stderr_and_exit_code() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        load(
            &host,
            r#"
            module.exports = function (pi) {
                pi.registerTool({
                    name: "exec_probe",
                    label: "exec probe",
                    description: "runs pi.exec probes",
                    parameters: { type: "object", properties: {} },
                    execute: async () => {
                        const ok = await pi.exec("echo", ["héllo world"]);
                        const failed = await pi.exec("sh", ["-c", "echo boom >&2; exit 3"]);
                        const missing = await pi.exec("pi-missing-binary-xyz", []);
                        return {
                            content: [{ type: "text", text: ok.stdout }],
                            details: { ok, failed, missing },
                        };
                    },
                });
            };
            "#,
        )
        .await;

        let details = run_probe(&host, "exec_probe").await;
        assert_eq!(details["ok"]["stdout"], json!("héllo world\n"));
        assert_eq!(details["ok"]["stderr"], json!(""));
        assert_eq!(details["ok"]["code"], json!(0));
        assert_eq!(details["ok"]["killed"], json!(false));

        assert_eq!(details["failed"]["stdout"], json!(""));
        assert_eq!(details["failed"]["code"], json!(3));
        assert!(
            details["failed"]["stderr"]
                .as_str()
                .expect("stderr string")
                .contains("boom"),
            "stderr should carry the child's stderr: {details}"
        );

        // A missing binary resolves (never rejects) with code 1 and the OS
        // error in stderr — the documented divergence from Node, which
        // drops the message on the `error` event.
        assert_eq!(details["missing"]["code"], json!(1));
        assert!(
            !details["missing"]["stderr"]
                .as_str()
                .expect("stderr string")
                .is_empty(),
            "spawn failure should explain itself: {details}"
        );
    });
}

/// Arguments are passed verbatim (no shell), and output larger than the OS
/// pipe buffer must not deadlock the host.
#[test]
fn pi_exec_passes_arguments_verbatim_and_drains_large_output() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        load(
            &host,
            r#"
            module.exports = function (pi) {
                pi.registerTool({
                    name: "exec_probe",
                    label: "exec probe",
                    description: "runs pi.exec probes",
                    parameters: { type: "object", properties: {} },
                    execute: async () => {
                        const spaced = await pi.exec("printf", ["%s|%s", "a b", "*"]);
                        const big = await pi.exec("sh", ["-c", "yes x | head -c 200000"]);
                        return {
                            content: [{ type: "text", text: spaced.stdout }],
                            details: {
                                spaced: spaced.stdout,
                                bigLen: big.stdout.length,
                                bigCode: big.code,
                            },
                        };
                    },
                });
            };
            "#,
        )
        .await;

        let details = run_probe(&host, "exec_probe").await;
        assert_eq!(details["spaced"], json!("a b|*"));
        assert_eq!(details["bigLen"], json!(200000));
        assert_eq!(details["bigCode"], json!(0));
    });
}

/// `cwd` defaults to the session cwd and `options.cwd` overrides it.
#[test]
fn pi_exec_defaults_to_session_cwd_and_honours_override() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("cwd");
        let nested = scratch.path().join("nested");
        std::fs::create_dir_all(&nested).expect("nested dir");
        let host = host_with_cwd(&scratch.as_str()).await;
        load(
            &host,
            r#"
            module.exports = function (pi) {
                pi.registerTool({
                    name: "exec_probe",
                    label: "exec probe",
                    description: "runs pi.exec probes",
                    parameters: { type: "object", properties: { dir: { type: "string" } } },
                    execute: async (args) => {
                        const here = await pi.exec("pwd", []);
                        const there = await pi.exec("pwd", [], { cwd: args.dir });
                        return {
                            content: [{ type: "text", text: here.stdout }],
                            details: { here: here.stdout.trim(), there: there.stdout.trim() },
                        };
                    },
                });
            };
            "#,
        )
        .await;

        let outcome = host
            .execute_tool("exec_probe", &json!({"dir": nested}).to_string())
            .await
            .expect("execute");
        let details = outcome.details.expect("details");
        assert_eq!(details["here"], json!(scratch.as_str()));
        assert_eq!(details["there"], json!(nested.to_string_lossy()));
    });
}

/// `options.timeout` kills the child and reports `killed: true` with `code:
/// -1`, well inside the host's own per-call timeout.
#[test]
fn pi_exec_timeout_kills_the_child() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        load(
            &host,
            r#"
            module.exports = function (pi) {
                pi.registerTool({
                    name: "exec_probe",
                    label: "exec probe",
                    description: "runs pi.exec probes",
                    parameters: { type: "object", properties: {} },
                    execute: async () => {
                        const slow = await pi.exec("sleep", ["5"], { timeout: 400 });
                        return {
                            content: [{ type: "text", text: JSON.stringify(slow) }],
                            details: slow,
                        };
                    },
                });
            };
            "#,
        )
        .await;

        let started = Instant::now();
        let details = run_probe(&host, "exec_probe").await;
        let elapsed = started.elapsed();

        assert_eq!(details["killed"], json!(true));
        assert_eq!(details["code"], json!(-1));
        assert!(
            elapsed < Duration::from_secs(4),
            "the timeout should fire long before `sleep 5` ends; took {elapsed:?}"
        );
    });
}

/// Bad arguments fail fast with a `TypeError` — upstream's contract is a
/// resolved `ExecResult` for *command* failures, not for a malformed call.
#[test]
fn pi_exec_validates_its_arguments() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        load(
            &host,
            r#"
            module.exports = function (pi) {
                pi.registerTool({
                    name: "exec_probe",
                    label: "exec probe",
                    description: "runs pi.exec probes",
                    parameters: { type: "object", properties: {} },
                    execute: async () => {
                        const capture = async (fn) => {
                            try { await fn(); return "no-error"; }
                            catch (error) { return error.name + ": " + error.message; }
                        };
                        return {
                            content: [{ type: "text", text: "validation" }],
                            details: {
                                nonStringArgs: await capture(() => pi.exec("echo", [1])),
                                emptyCommand: await capture(() => pi.exec("", [])),
                            },
                        };
                    },
                });
            };
            "#,
        )
        .await;

        let details = run_probe(&host, "exec_probe").await;
        let non_string = details["nonStringArgs"].as_str().expect("message");
        let empty = details["emptyCommand"].as_str().expect("message");
        assert!(
            non_string.starts_with("TypeError:") && non_string.contains("array of strings"),
            "{non_string}"
        );
        assert!(
            empty.starts_with("TypeError:") && empty.contains("non-empty string"),
            "{empty}"
        );
    });
}

/// `pi.exec` also works from an event handler (the `_pi_dispatch` path), and
/// its awaited result reaches the host log through `pi.appendEntry`.
#[test]
fn pi_exec_is_awaitable_from_event_handlers() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        load(
            &host,
            r#"
            module.exports = function (pi) {
                pi.on("session_start", async () => {
                    const result = await pi.exec("echo", ["from-event"]);
                    pi.appendEntry("exec_from_event", {
                        stdout: result.stdout,
                        code: result.code,
                    });
                });
            };
            "#,
        )
        .await;

        host.emit_event_with(&ExtensionEvent::SessionStart, Some("print"), false, "/tmp")
            .await
            .expect("dispatch session_start");

        let log = host.log();
        let entry = log
            .entries
            .iter()
            .find(|entry| entry.custom_type == "exec_from_event")
            .unwrap_or_else(|| panic!("no exec_from_event entry in {:?}", log.entries));
        assert_eq!(entry.data["stdout"], json!("from-event\n"));
        assert_eq!(entry.data["code"], json!(0));
    });
}
