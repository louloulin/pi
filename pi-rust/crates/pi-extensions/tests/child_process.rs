//! `node:child_process` virtual module tests — LUM-1110.
//!
//! Six upstream examples (`interactive-shell.ts`, `ssh.ts`,
//! `mac-system-theme.ts`, `sandbox/index.ts`, `subagent/index.ts`,
//! `truncated-tool.ts`) are blocked on this one specifier. These tests drive
//! the shim the way those examples do — `execSync` for a one-shot command,
//! `promisify(exec)` for the theme poller, `spawn` + `on("data")` /
//! `on("close")` for the streaming wrappers — through the real host:
//! an extension registered from source, executed through
//! [`JsExtensionHost::execute_tool`].
//!
//! The tests spawn real POSIX tools (`sh`, `printf`, `sleep`, …), so the
//! whole file is Unix-only; the bridge itself is platform-neutral.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use pi_extensions::{ExtensionEntry, HostOptions, JsExtensionHost, ToolContext};
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
            "pi_child_process/{}-{name}-{unique}",
            std::process::id()
        ));
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
            source: PathBuf::from("/tmp/pi_child_process/probe.js"),
            id: "child_process_probe".to_string(),
            label: None,
        },
        source,
    )
    .await
    .expect("load extension");
}

async fn run_probe(host: &JsExtensionHost, tool: &str, args: Value) -> Value {
    let outcome = host
        .execute_tool(tool, &args.to_string())
        .await
        .unwrap_or_else(|error| panic!("execute {tool}: {error}"));
    assert!(!outcome.is_error, "{tool} reported an error: {outcome:?}");
    outcome
        .details
        .unwrap_or_else(|| panic!("{tool} returned no details"))
}

/// The synchronous surface: `execSync` / `execFileSync` / `spawnSync`,
/// their default (Buffer) and explicit encodings, and the non-zero-exit
/// shape `truncated-tool.ts` branches on (`err.status === 1`).
#[test]
fn child_process_sync_surface_matches_node() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        load(
            &host,
            r#"
            import { execSync, execFileSync, spawnSync } from "node:child_process";
            import { Buffer } from "node:buffer";

            export default function (pi) {
                pi.registerTool({
                    name: "cp_sync",
                    label: "child_process sync probe",
                    description: "exercises execSync / execFileSync / spawnSync",
                    parameters: { type: "object", properties: {} },
                    execute: () => {
                        const defaultIsBuffer = execSync("printf hello");
                        const utf8 = execSync("printf hello", { encoding: "utf8" });

                        const split = spawnSync("sh", ["-c", "printf a; printf b >&2; exit 3"]);
                        const splitUtf8 = spawnSync("sh", ["-c", "printf a; printf b >&2; exit 3"], {
                            encoding: "utf8",
                        });

                        const missing = spawnSync("pi-missing-binary-xyz");
                        const execFileDefault = execFileSync("printf", ["%s", "x"]);

                        let failure = null;
                        try {
                            execSync("printf oops >&2; exit 7", { encoding: "utf8" });
                        } catch (error) {
                            failure = {
                                status: error.status,
                                signal: error.signal,
                                stdout: error.stdout,
                                stderr: error.stderr,
                                message: error.message,
                            };
                        }

                        return {
                            content: [{ type: "text", text: "sync" }],
                            details: {
                                defaultIsBuffer: Buffer.isBuffer(defaultIsBuffer),
                                defaultBytes: Array.from(defaultIsBuffer),
                                utf8: utf8,
                                split: {
                                    status: split.status,
                                    signal: split.signal,
                                    stdout: split.stdout.toString(),
                                    stderr: split.stderr.toString(),
                                    pidIsNumber: typeof split.pid === "number",
                                    outputShape: [
                                        split.output[0] === null,
                                        split.output[1].toString(),
                                        split.output[2].toString(),
                                    ],
                                },
                                splitUtf8: {
                                    stdout: splitUtf8.stdout,
                                    stderr: splitUtf8.stderr,
                                },
                                missing: {
                                    pid: missing.pid,
                                    status: missing.status,
                                    signal: missing.signal,
                                    code: missing.error && missing.error.code,
                                },
                                execFileDefault: {
                                    isBuffer: Buffer.isBuffer(execFileDefault),
                                    text: execFileDefault.toString(),
                                },
                                failure: failure,
                            },
                        };
                    },
                });
            }
            "#,
        )
        .await;

        let details = run_probe(&host, "cp_sync", json!({})).await;

        assert_eq!(details["defaultIsBuffer"], json!(true), "{details}");
        assert_eq!(details["defaultBytes"], json!([104, 101, 108, 108, 111]));
        assert_eq!(details["utf8"], json!("hello"));

        assert_eq!(details["split"]["status"], json!(3), "{details}");
        assert_eq!(details["split"]["signal"], json!(null));
        assert_eq!(details["split"]["stdout"], json!("a"));
        assert_eq!(details["split"]["stderr"], json!("b"));
        assert_eq!(details["split"]["pidIsNumber"], json!(true));
        assert_eq!(details["split"]["outputShape"], json!([true, "a", "b"]));
        assert_eq!(details["splitUtf8"]["stdout"], json!("a"));
        assert_eq!(details["splitUtf8"]["stderr"], json!("b"));

        // A missing binary is a *value* for `spawnSync`: `error.code` set,
        // `status` / `signal` null. The upstream examples rely on this.
        assert_eq!(details["missing"]["status"], json!(null));
        assert_eq!(details["missing"]["signal"], json!(null));
        assert_eq!(details["missing"]["code"], json!("ENOENT"));

        assert_eq!(details["execFileDefault"]["isBuffer"], json!(true));
        assert_eq!(details["execFileDefault"]["text"], json!("x"));

        assert_eq!(details["failure"]["status"], json!(7), "{details}");
        assert_eq!(details["failure"]["signal"], json!(null));
        assert_eq!(details["failure"]["stdout"], json!(""));
        assert_eq!(details["failure"]["stderr"], json!("oops"));
        assert!(
            details["failure"]["message"]
                .as_str()
                .expect("message")
                .contains("Command failed"),
            "{details}"
        );
    });
}

/// The callback and promise forms. `mac-system-theme.ts` does
/// `const { stdout } = await promisify(exec)(...)`, so the
/// `util.promisify.custom` hook has to resolve `{stdout, stderr}`.
#[test]
fn child_process_callback_and_promisify_forms() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        load(
            &host,
            r#"
            import { exec, execFile } from "node:child_process";
            import { promisify } from "node:util";
            import { Buffer } from "node:buffer";

            const execAsync = promisify(exec);
            const execFileAsync = promisify(execFile);

            function execCallback(command) {
                return new Promise((resolve) => {
                    exec(command, (error, stdout, stderr) => {
                        resolve({
                            code: error ? error.code : null,
                            cmd: error ? error.cmd : null,
                            killed: error ? error.killed : null,
                            signal: error ? error.signal : null,
                            stdout: stdout,
                            stderr: stderr,
                        });
                    });
                });
            }

            export default function (pi) {
                pi.registerTool({
                    name: "cp_async",
                    label: "child_process async probe",
                    description: "exercises exec / execFile callbacks and promisify",
                    parameters: { type: "object", properties: {} },
                    execute: async () => {
                        const ok = await execCallback("printf héllo");
                        const failed = await execCallback("printf a; printf b >&2; exit 3");
                        const file = await new Promise((resolve) => {
                            execFile("printf", ["%s", "x"], (error, stdout, stderr) => {
                                resolve({ error: error, stdout: stdout, stderr: stderr });
                            });
                        });

                        let promiseOk = null;
                        try {
                            const result = await execAsync("printf héllo");
                            promiseOk = { stdout: result.stdout, stderr: result.stderr };
                        } catch (error) {
                            promiseOk = { rejected: String(error) };
                        }

                        let promiseFail = null;
                        try {
                            await execAsync("printf a; printf b >&2; exit 4");
                        } catch (error) {
                            promiseFail = {
                                code: error.code,
                                stdout: error.stdout,
                                stderr: error.stderr,
                            };
                        }

                        const fileAsync = await execFileAsync("printf", ["%s", "z"]);

                        let missing = null;
                        try {
                            await execFileAsync("pi-missing-binary-xyz");
                        } catch (error) {
                            missing = { code: error.code, syscall: error.syscall };
                        }

                        return {
                            content: [{ type: "text", text: "async" }],
                            details: {
                                ok: ok,
                                failed: failed,
                                file: {
                                    error: file.error,
                                    stdout: file.stdout,
                                    stderr: file.stderr,
                                },
                                promiseOk: promiseOk,
                                promiseFail: promiseFail,
                                fileAsync: fileAsync,
                                missing: missing,
                                execFileHasCustom:
                                    typeof execFile[Symbol.for("nodejs.util.promisify.custom")] ===
                                    "function",
                                execHasCustom:
                                    typeof exec[Symbol.for("nodejs.util.promisify.custom")] ===
                                    "function",
                                defaultEncodingIsUtf8: typeof ok.stdout === "string",
                            },
                        };
                    },
                });
            }
            "#,
        )
        .await;

        let details = run_probe(&host, "cp_async", json!({})).await;

        assert_eq!(details["ok"]["code"], json!(null), "{details}");
        assert_eq!(details["ok"]["stdout"], json!("héllo"));
        assert_eq!(details["ok"]["stderr"], json!(""));
        assert_eq!(details["defaultEncodingIsUtf8"], json!(true));

        assert_eq!(details["failed"]["code"], json!(3), "{details}");
        assert_eq!(details["failed"]["stdout"], json!("a"));
        assert_eq!(details["failed"]["stderr"], json!("b"));
        assert_eq!(details["failed"]["killed"], json!(false));
        assert_eq!(details["failed"]["signal"], json!(null));

        assert_eq!(details["file"]["error"], json!(null));
        assert_eq!(details["file"]["stdout"], json!("x"));

        assert_eq!(details["promiseOk"]["stdout"], json!("héllo"));
        assert_eq!(details["promiseOk"]["stderr"], json!(""));

        assert_eq!(details["promiseFail"]["code"], json!(4), "{details}");
        assert_eq!(details["promiseFail"]["stdout"], json!("a"));
        assert_eq!(details["promiseFail"]["stderr"], json!("b"));

        assert_eq!(details["fileAsync"]["stdout"], json!("z"));
        assert_eq!(details["fileAsync"]["stderr"], json!(""));

        assert_eq!(details["missing"]["code"], json!("ENOENT"));
        assert_eq!(details["execHasCustom"], json!(true));
        assert_eq!(details["execFileHasCustom"], json!(true));
    });
}

/// `spawn` — the ssh.ts / sandbox / subagent shape: `on("data")` on both
/// pipes, `on("exit")` then `on("close")`, `pid`, `exitCode`.
#[test]
fn child_process_spawn_streams_data_and_signals_lifecycle() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        load(
            &host,
            r#"
            import { spawn } from "node:child_process";
            import { Buffer } from "node:buffer";

            export default function (pi) {
                pi.registerTool({
                    name: "cp_spawn",
                    label: "child_process spawn probe",
                    description: "exercises spawn data / exit / close",
                    parameters: { type: "object", properties: {} },
                    execute: () =>
                        new Promise((resolve, reject) => {
                            const child = spawn("sh", [
                                "-c",
                                "printf out; printf err >&2; exit 5",
                            ], { stdio: ["ignore", "pipe", "pipe"] });

                            const out = [];
                            const err = [];
                            const events = [];
                            child.stdout.on("data", (data) => {
                                events.push("stdout-data:" + Buffer.isBuffer(data));
                                out.push(data);
                            });
                            child.stderr.on("data", (data) => err.push(data));
                            child.on("exit", (code, signal) => {
                                events.push("exit:" + code + ":" + signal);
                            });
                            child.on("error", reject);
                            child.on("close", (code, signal) => {
                                events.push("close:" + code + ":" + signal);
                                resolve({
                                    content: [{ type: "text", text: "spawn" }],
                                    details: {
                                        events: events,
                                        stdout: Buffer.concat(out).toString(),
                                        stderr: Buffer.concat(err).toString(),
                                        exitCode: child.exitCode,
                                        signalCode: child.signalCode,
                                        pidIsNumber:
                                            typeof child.pid === "number" && child.pid > 0,
                                        stdinIsNull: child.stdin === null,
                                    },
                                });
                            });
                        }),
                });
            }
            "#,
        )
        .await;

        let details = run_probe(&host, "cp_spawn", json!({})).await;

        assert_eq!(details["stdout"], json!("out"), "{details}");
        assert_eq!(details["stderr"], json!("err"));
        assert_eq!(details["exitCode"], json!(5));
        assert_eq!(details["signalCode"], json!(null));
        assert_eq!(details["pidIsNumber"], json!(true));
        assert_eq!(details["stdinIsNull"], json!(true));

        let events = details["events"].as_array().expect("events");
        assert!(
            events.iter().any(|event| event == "stdout-data:true"),
            "stdout chunks must be Buffers: {details}"
        );
        assert!(
            events.iter().any(|event| event == "exit:5:null"),
            "exit carries (code, signal): {details}"
        );
        // `close` is last, after both streams ended.
        assert_eq!(events.last(), Some(&json!("close:5:null")), "{details}");
    });
}

/// `kill()` terminates a long-running child well inside the host deadline
/// and reports the death through `close`.
#[test]
fn child_process_spawn_kill_terminates_the_child() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        load(
            &host,
            r#"
            import { spawn } from "node:child_process";

            export default function (pi) {
                pi.registerTool({
                    name: "cp_kill",
                    label: "child_process kill probe",
                    description: "kills a sleeping child",
                    parameters: { type: "object", properties: {} },
                    execute: () =>
                        new Promise((resolve, reject) => {
                            const child = spawn("sleep", ["30"], {
                                stdio: ["ignore", "pipe", "pipe"],
                            });
                            let killedReturn = null;
                            child.on("error", reject);
                            child.on("close", (code, signal) => {
                                resolve({
                                    content: [{ type: "text", text: "kill" }],
                                    details: {
                                        killedReturn: killedReturn,
                                        killed: child.killed,
                                        code: code,
                                        signal: signal,
                                        secondKill: child.kill(),
                                    },
                                });
                            });
                            killedReturn = child.kill();
                        }),
                });
            }
            "#,
        )
        .await;

        let started = Instant::now();
        let details = run_probe(&host, "cp_kill", json!({})).await;
        let elapsed = started.elapsed();

        assert_eq!(details["killedReturn"], json!(true), "{details}");
        assert_eq!(details["killed"], json!(true));
        assert_eq!(details["code"], json!(null));
        assert_eq!(details["signal"], json!("SIGKILL"), "{details}");
        assert_eq!(details["secondKill"], json!(false));
        assert!(
            elapsed < Duration::from_secs(4),
            "kill should not wait for `sleep 30`; took {elapsed:?}"
        );
    });
}

/// `cwd` / `env` options reach the child, and output larger than an OS pipe
/// buffer neither deadlocks nor truncates.
#[test]
fn child_process_honours_cwd_env_and_drains_large_output() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("spawn-cwd");
        let nested = scratch.path().join("nested");
        std::fs::create_dir_all(&nested).expect("nested dir");
        let host = host_with_cwd(&scratch.as_str()).await;
        load(
            &host,
            r#"
            import { spawn, execSync, execFileSync } from "node:child_process";
            import { Buffer } from "node:buffer";

            function collect(child) {
                return new Promise((resolve, reject) => {
                    const out = [];
                    child.stdout.on("data", (chunk) => out.push(chunk));
                    child.on("error", reject);
                    child.on("close", (code) => {
                        resolve({ code: code, stdout: Buffer.concat(out).toString() });
                    });
                });
            }

            export default function (pi) {
                pi.registerTool({
                    name: "cp_opts",
                    label: "child_process options probe",
                    description: "exercises cwd / env / large output",
                    parameters: { type: "object", properties: { dir: { type: "string" } } },
                    execute: async (args) => {
                        const here = await collect(
                            spawn("pwd", [], { cwd: args.dir, stdio: ["ignore", "pipe", "pipe"] }),
                        );
                        const env = await collect(
                            spawn("sh", ["-c", "printf %s \"$PI_CHILD_ENV\""], {
                                env: { PATH: "/usr/bin:/bin", PI_CHILD_ENV: "from-env" },
                                stdio: ["ignore", "pipe", "pipe"],
                            }),
                        );
                        const big = await collect(
                            spawn("sh", ["-c", "yes x | head -c 200000"], {
                                stdio: ["ignore", "pipe", "pipe"],
                            }),
                        );
                        const bigSync = execSync("sh -c 'yes y | head -c 200000'");
                        const bigFile = execFileSync("sh", ["-c", "printf %s abc"]);
                        return {
                            content: [{ type: "text", text: "opts" }],
                            details: {
                                here: here.stdout.trim(),
                                env: env.stdout,
                                bigCode: big.code,
                                bigLen: big.stdout.length,
                                bigSyncLen: bigSync.length,
                                bigSyncIsBuffer: Buffer.isBuffer(bigSync),
                                bigFileIsBuffer: Buffer.isBuffer(bigFile),
                            },
                        };
                    },
                });
            }
            "#,
        )
        .await;

        let details = run_probe(&host, "cp_opts", json!({"dir": nested})).await;

        assert_eq!(
            details["here"],
            json!(nested.to_string_lossy()),
            "{details}"
        );
        assert_eq!(details["env"], json!("from-env"));
        assert_eq!(details["bigCode"], json!(0));
        assert_eq!(details["bigLen"], json!(200_000));
        assert_eq!(details["bigSyncLen"], json!(200_000));
        assert_eq!(details["bigSyncIsBuffer"], json!(true));
        assert_eq!(details["bigFileIsBuffer"], json!(true));
    });
}

/// The host deadline is a ceiling: a `spawn` with a short `timeout` is
/// killed and `close` fires with `SIGKILL` long before the child would
/// have finished. This is what keeps a runaway extension from hanging
/// `pi`.
#[test]
fn child_process_timeout_is_bounded() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        load(
            &host,
            r#"
            import { spawn, spawnSync, execSync } from "node:child_process";

            export default function (pi) {
                pi.registerTool({
                    name: "cp_timeout",
                    label: "child_process timeout probe",
                    description: "checks the timeout paths",
                    parameters: { type: "object", properties: {} },
                    execute: async () => {
                        const spawned = await new Promise((resolve, reject) => {
                            const child = spawn("sleep", ["30"], {
                                timeout: 300,
                                stdio: ["ignore", "pipe", "pipe"],
                            });
                            child.on("error", reject);
                            child.on("close", (code, signal) =>
                                resolve({ code: code, signal: signal }),
                            );
                        });

                        let syncError = null;
                        try {
                            execSync("sleep 30", { timeout: 300, encoding: "utf8" });
                        } catch (error) {
                            syncError = {
                                code: error.code,
                                status: error.status,
                                killed: error.killed,
                                signal: error.signal,
                            };
                        }

                        const spawnSyncTimeout = spawnSync("sleep", ["30"], {
                            timeout: 300,
                            encoding: "utf8",
                        });

                        return {
                            content: [{ type: "text", text: "timeout" }],
                            details: {
                                spawned: spawned,
                                syncError: syncError,
                                spawnSyncTimeout: {
                                    status: spawnSyncTimeout.status,
                                    signal: spawnSyncTimeout.signal,
                                    code: spawnSyncTimeout.error && spawnSyncTimeout.error.code,
                                },
                            },
                        };
                    },
                });
            }
            "#,
        )
        .await;

        let started = Instant::now();
        let details = run_probe(&host, "cp_timeout", json!({})).await;
        let elapsed = started.elapsed();

        assert_eq!(details["spawned"]["code"], json!(null), "{details}");
        assert_eq!(details["spawned"]["signal"], json!("SIGKILL"), "{details}");
        assert_eq!(details["syncError"]["killed"], json!(true), "{details}");
        assert_eq!(details["syncError"]["status"], json!(null));
        assert_eq!(details["syncError"]["signal"], json!("SIGKILL"));
        assert_eq!(details["syncError"]["code"], json!("ETIMEDOUT"));
        // `spawnSync` never throws: the timeout surfaces as `error.code`.
        assert_eq!(
            details["spawnSyncTimeout"]["status"],
            json!(null),
            "{details}"
        );
        assert_eq!(details["spawnSyncTimeout"]["signal"], json!("SIGKILL"));
        assert_eq!(details["spawnSyncTimeout"]["code"], json!("ETIMEDOUT"));
        assert!(
            elapsed < Duration::from_secs(4),
            "timeouts should fire long before `sleep 30`; took {elapsed:?}"
        );
    });
}

/// `maxBuffer` is honoured for the buffered forms, and the exported surface
/// includes the bare-specifier alias the upstream CJS examples import.
#[test]
fn child_process_honours_max_buffer_and_bare_specifier() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        load(
            &host,
            r#"
            import cs from "child_process";
            import { spawnSync } from "node:child_process";
            import { execSync } from "child_process";

            export default function (pi) {
                pi.registerTool({
                    name: "cp_limits",
                    label: "child_process limits probe",
                    description: "checks maxBuffer and the bare alias",
                    parameters: { type: "object", properties: {} },
                    execute: () => {
                        const limited = spawnSync("sh", ["-c", "printf 1234567890"], {
                            maxBuffer: 4,
                            encoding: "utf8",
                        });
                        let syncError = null;
                        try {
                            execSync("printf 1234567890", { maxBuffer: 4, encoding: "utf8" });
                        } catch (error) {
                            syncError = { code: error.code };
                        }
                        return {
                            content: [{ type: "text", text: "limits" }],
                            details: {
                                bareAlias: typeof cs.spawn === "function",
                                hasFork: typeof cs.fork === "function",
                                hasChildProcess:
                                    typeof cs.ChildProcess === "function" ||
                                    typeof cs.ChildProcess === "object",
                                limitedError: limited.error && limited.error.code,
                                limitedStatus: limited.status,
                                syncError: syncError,
                            },
                        };
                    },
                });
            }
            "#,
        )
        .await;

        let details = run_probe(&host, "cp_limits", json!({})).await;
        assert_eq!(details["bareAlias"], json!(true), "{details}");
        assert_eq!(details["hasFork"], json!(true));
        assert_eq!(details["hasChildProcess"], json!(true));
        assert_eq!(details["limitedError"], json!("ENOBUFS"), "{details}");
        assert_eq!(details["limitedStatus"], json!(0));
        assert_eq!(
            details["syncError"]["code"],
            json!("ERR_CHILD_PROCESS_STDIO_MAXBUFFER"),
            "{details}"
        );
    });
}
