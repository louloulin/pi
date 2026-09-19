//! `node:*` builtin virtual module tests — LUM-1100.
//!
//! Upstream extensions run on Node, so they import `node:fs` /
//! `node:fs/promises` / `node:os` / `node:buffer` / `node:crypto` and read
//! the `process` global directly. These tests drive that surface through
//! the real host: an extension registered from source, executed through
//! [`JsExtensionHost::execute_tool`], with a scratch directory standing in
//! for the session working tree.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use pi_extensions::{ExtensionEntry, HostOptions, JsExtensionHost, ToolContext};
use serde_json::json;

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
            "pi_node_builtins/{}-{name}-{unique}",
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

fn entry_at(id: &str, path: &str) -> ExtensionEntry {
    ExtensionEntry {
        source: PathBuf::from(path),
        id: id.to_string(),
        label: None,
    }
}

/// Build a host whose session cwd is the scratch directory, so
/// `process.cwd()` / `path.resolve` resolve relative paths the way a real
/// session would.
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

/// The sync `node:fs` / `node:buffer` / `node:os` / `node:crypto` /
/// `node:process` surface, exercised by one ESM extension and asserted as a
/// single details object.
#[test]
fn node_builtins_sync_surface_round_trips_files() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("fs-sync");
        let host = host_with_cwd(&scratch.as_str()).await;

        let source = r#"
            import { appendFileSync, copyFileSync, existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, realpathSync, renameSync, rmSync, statSync, unlinkSync, writeFileSync } from "node:fs";
            import { join } from "node:path";
            import { EOL, arch, homedir, platform, release, tmpdir, type } from "node:os";
            import { Buffer } from "node:buffer";
            import { randomBytes, randomInt, randomUUID } from "node:crypto";
            import process from "node:process";

            export default function (pi) {
                pi.registerTool({
                    name: "fs_probe",
                    label: "fs probe",
                    description: "exercises the node:* builtin virtual modules",
                    parameters: { type: "object", properties: { dir: { type: "string" } } },
                    execute: (args, ctx) => {
                        const dir = args.dir;
                        const nested = join(dir, "nested", "deep");
                        mkdirSync(nested, { recursive: true });

                        const file = join(nested, "hello.txt");
                        writeFileSync(file, "héllo\n");
                        appendFileSync(file, Buffer.from(" world"));

                        const text = readFileSync(file, "utf8");
                        const raw = readFileSync(file);

                        const copy = join(dir, "copy.txt");
                        copyFileSync(file, copy);
                        const moved = join(dir, "moved.txt");
                        renameSync(copy, moved);

                        const bytes = Buffer.from("héllo\n world", "utf8");
                        const expectedEol = process.platform === "win32" ? "\r\n" : "\n";
                        const entriesBeforeUnlink = readdirSync(dir).sort();
                        unlinkSync(moved);
                        const entriesAfterUnlink = readdirSync(dir).sort();

                        return {
                            content: [{ type: "text", text }],
                            details: {
                                text,
                                byteLength: raw.length,
                                isBuffer: Buffer.isBuffer(raw),
                                utf8Length: Buffer.byteLength("héllo", "utf8"),
                                hexRoundTrip: Buffer.from(bytes.toString("hex"), "hex").toString("utf8") === text,
                                base64RoundTrip: Buffer.from(bytes.toString("base64"), "base64").equals(bytes),
                                latin1: Buffer.from("abc").toString("latin1"),
                                concat: Buffer.concat([Buffer.from("a"), Buffer.from("b")]).toString("utf8"),
                                heap: Buffer.alloc(3, 7).toString("hex"),
                                compare: Buffer.compare(Buffer.from("a"), Buffer.from("b")),
                                dirent: readdirSync(nested, { withFileTypes: true }).map(
                                    (entry) => entry.name + ":" + entry.isFile(),
                                ),
                                rootEntriesBeforeUnlink: entriesBeforeUnlink,
                                rootEntriesAfterUnlink: entriesAfterUnlink,
                                entryIsFile: statSync(file).isFile(),
                                entryIsDir: statSync(nested).isDirectory(),
                                size: statSync(file).size,
                                lstatIsFile: (() => {
                                    const meta = lstatSync(file);
                                    return meta.isFile() && !meta.isSymbolicLink();
                                })(),
                                realpathResolved: realpathSync(file).endsWith("hello.txt"),
                                existsBefore: existsSync(file),
                                unlinkedGone: !existsSync(moved),
                                os: {
                                    tmpOk: tmpdir().length > 0,
                                    homeOk: homedir().length > 0,
                                    platformMatchesProcess: platform() === process.platform,
                                    arch: arch(),
                                    type: type(),
                                    releaseOk: release().length > 0,
                                    eolMatchesProcess: EOL === expectedEol,
                                },
                                process: {
                                    cwdMatchesCtx: process.cwd() === ctx.cwd,
                                    pidPositive: process.pid > 0,
                                    version: process.version,
                                    hasEnv: Object.keys(process.env).length > 0,
                                    exitCode: process.exitCode,
                                },
                                crypto: {
                                    uuidShape: /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(
                                        randomUUID(),
                                    ),
                                    randomBytesLength: randomBytes(8).length,
                                    randomIntInRange: (() => {
                                        const value = randomInt(1, 4);
                                        return value >= 1 && value < 4;
                                    })(),
                                },
                                rmDir: (() => {
                                    rmSync(join(dir, "nested"), { recursive: true, force: true });
                                    return !existsSync(nested);
                                })(),
                            },
                        };
                    },
                });
            }
        "#;

        host.load(
            entry_at("fs_probe", "/tmp/pi_node_builtins/fs_probe.mjs"),
            source,
        )
        .await
        .expect("load fs probe extension");

        let outcome = host
            .execute_tool("fs_probe", &json!({ "dir": scratch.as_str() }).to_string())
            .await
            .expect("execute fs probe");
        assert!(!outcome.is_error, "{outcome:?}");

        let details = outcome.details.expect("details");
        assert_eq!(details["text"], "héllo\n world");
        assert_eq!(details["byteLength"], 13);
        assert_eq!(details["utf8Length"], 6);
        assert_eq!(details["isBuffer"], true);
        assert_eq!(details["hexRoundTrip"], true);
        assert_eq!(details["base64RoundTrip"], true);
        assert_eq!(details["latin1"], "abc");
        assert_eq!(details["concat"], "ab");
        assert_eq!(details["heap"], "070707");
        assert_eq!(details["compare"], -1);
        assert_eq!(details["dirent"], json!(["hello.txt:true"]));
        assert_eq!(details["rootEntriesBeforeUnlink"], json!(["moved.txt", "nested"]));
        assert_eq!(details["rootEntriesAfterUnlink"], json!(["nested"]));
        assert_eq!(details["entryIsFile"], true);
        assert_eq!(details["entryIsDir"], true);
        assert_eq!(details["size"], 13);
        assert_eq!(details["lstatIsFile"], true);
        assert_eq!(details["realpathResolved"], true);
        assert_eq!(details["existsBefore"], true);
        assert_eq!(details["unlinkedGone"], true);
        assert_eq!(details["os"]["tmpOk"], true);
        assert_eq!(details["os"]["homeOk"], true);
        assert_eq!(details["os"]["platformMatchesProcess"], true);
        assert_eq!(details["os"]["releaseOk"], true);
        assert_eq!(details["os"]["eolMatchesProcess"], true);
        assert_eq!(details["process"]["cwdMatchesCtx"], true);
        assert_eq!(details["process"]["pidPositive"], true);
        assert_eq!(details["process"]["version"], "v0.0.0-pi-rust");
        assert_eq!(details["process"]["hasEnv"], true);
        assert_eq!(details["process"]["exitCode"], 0);
        assert_eq!(details["crypto"]["uuidShape"], true);
        assert_eq!(details["crypto"]["randomBytesLength"], 8);
        assert_eq!(details["crypto"]["randomIntInRange"], true);
        assert_eq!(details["rmDir"], true);

        // The probe wrote real files, so the session tree reflects exactly
        // what the assertions above observed: the nested tree was removed
        // recursively, the copy was renamed then unlinked.
        let leftover: Vec<String> = std::fs::read_dir(scratch.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftover, Vec::<String>::new());
    });
}

/// `node:fs/promises` and the callback forms, reached from CommonJS via the
/// `require("node:fs")` shim, with Node-shaped error codes on failure.
#[test]
fn node_fs_promises_and_callbacks_reach_the_same_bridge() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("fs-promises");
        let host = host_with_cwd(&scratch.as_str()).await;

        let source = r#"
            const fs = require("node:fs");
            const fsp = require("node:fs/promises");
            const { join } = require("node:path");

            module.exports = function (pi) {
                pi.registerTool({
                    name: "async_probe",
                    label: "async probe",
                    description: "exercises fs/promises + node-style callbacks",
                    parameters: { type: "object", properties: { dir: { type: "string" } } },
                    execute: async (args, ctx) => {
                        const target = join(args.dir, "async.txt");
                        await fsp.writeFile(target, "async payload", "utf8");
                        const read = await fsp.readFile(target, "utf8");
                        const viaCallback = await new Promise((resolve, reject) => {
                            fs.readFile(target, "utf8", (err, data) => (err ? reject(err) : resolve(data)));
                        });
                        const meta = await fsp.stat(target);
                        const listing = (await fsp.readdir(args.dir)).sort();
                        const missingDir = join(args.dir, "missing-dir");

                        let asyncCode = null;
                        try {
                            await fsp.readFile(target + ".missing");
                        } catch (err) {
                            asyncCode = err.code;
                        }

                        let syncCode = null;
                        let syncSyscall = null;
                        try {
                            fs.readFileSync(target + ".missing");
                        } catch (err) {
                            syncCode = err.code;
                            syncSyscall = err.syscall;
                        }

                        let mkdirCode = null;
                        try {
                            await fsp.readFile(missingDir);
                        } catch (err) {
                            mkdirCode = err.code;
                        }

                        await fsp.mkdir(missingDir, { recursive: true });
                        const mkdirCreated = fs.existsSync(missingDir);
                        await fsp.copyFile(target, join(missingDir, "copy.txt"));
                        const copyRead = await fsp.readFile(join(missingDir, "copy.txt"), "utf8");
                        await fsp.rename(join(missingDir, "copy.txt"), join(missingDir, "renamed.txt"));
                        const renamed = await fsp.readdir(missingDir);
                        await fsp.unlink(join(missingDir, "renamed.txt"));
                        const afterUnlink = await fsp.readdir(missingDir);
                        await fsp.rm(missingDir, { recursive: true, force: true });
                        const stillExists = fs.existsSync(missingDir);
                        await fsp.rm(target);

                        return {
                            content: [{ type: "text", text: read }],
                            details: {
                                read,
                                viaCallback,
                                readIsString: typeof read === "string",
                                size: meta.size,
                                isFile: meta.isFile(),
                                listing,
                                asyncCode,
                                syncCode,
                                syncSyscall,
                                mkdirCode,
                                mkdirCreated,
                                copyRead,
                                renamed,
                                afterUnlink,
                                stillExists,
                                existsAfterRm: fs.existsSync(target),
                                realpathIsFunction: typeof fsp.realpath === "function",
                            },
                        };
                    },
                });
            };
        "#;

        host.load(
            entry_at("async_probe", "/tmp/pi_node_builtins/async_probe.cjs"),
            source,
        )
        .await
        .expect("load async probe extension");

        let outcome = host
            .execute_tool("async_probe", &json!({ "dir": scratch.as_str() }).to_string())
            .await
            .expect("execute async probe");
        assert!(!outcome.is_error, "{outcome:?}");

        let details = outcome.details.expect("details");
        assert_eq!(details["read"], "async payload");
        assert_eq!(details["viaCallback"], "async payload");
        assert_eq!(details["readIsString"], true);
        assert_eq!(details["size"], 13);
        assert_eq!(details["isFile"], true);
        assert_eq!(details["listing"], json!(["async.txt"]));
        assert_eq!(details["asyncCode"], "ENOENT");
        assert_eq!(details["syncCode"], "ENOENT");
        assert_eq!(details["syncSyscall"], "open");
        assert_eq!(details["mkdirCode"], "ENOENT");
        assert_eq!(details["mkdirCreated"], true);
        assert_eq!(details["copyRead"], "async payload");
        assert_eq!(details["renamed"], json!(["renamed.txt"]));
        assert_eq!(details["afterUnlink"], json!([]));
        assert_eq!(details["stillExists"], false);
        assert_eq!(details["existsAfterRm"], false);
        assert_eq!(details["realpathIsFunction"], true);

        let leftover: Vec<String> = std::fs::read_dir(scratch.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftover, Vec::<String>::new());
    });
}

/// `Buffer` / `process` are globals on Node, so an extension that never
/// imports them must still find them — and a builtin that is *not* bridged
/// must fail loudly, naming what is available.
#[test]
fn node_globals_are_installed_and_unsupported_builtins_are_reported() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");

        host.load(
            entry_at("global_probe", "/tmp/pi_node_builtins/global_probe.cjs"),
            r#"
                module.exports = function (pi) {
                    pi.registerTool({
                        name: "global_probe",
                        label: "global probe",
                        description: "reads Buffer / process without importing them",
                        parameters: { type: "object" },
                        execute: () => ({
                            content: [{ type: "text", text: String(Buffer.byteLength("héllo", "utf8")) }],
                            details: {
                                byteLength: Buffer.byteLength("héllo", "utf8"),
                                isBuffer: Buffer.isBuffer(Buffer.from([1, 2])),
                                fromHex: Buffer.from("c3a9", "hex").toString("latin1"),
                                platform: process.platform,
                                hasEnv: typeof process.env === "object" && Object.keys(process.env).length > 0,
                                version: process.version,
                                cwdIsString: typeof process.cwd() === "string",
                                stdoutWrite: process.stdout.write("global probe wrote\n"),
                            },
                        }),
                    });
                };
            "#,
        )
        .await
        .expect("load global probe extension");

        let outcome = host
            .execute_tool("global_probe", "{}")
            .await
            .expect("execute global probe");
        assert!(!outcome.is_error, "{outcome:?}");

        let details = outcome.details.expect("details");
        assert_eq!(details["byteLength"], 6);
        assert_eq!(details["isBuffer"], true);
        assert_eq!(details["fromHex"], "Ã©");
        assert!(details["platform"].as_str().is_some(), "{details}");
        assert_eq!(details["hasEnv"], true);
        assert_eq!(details["version"], "v0.0.0-pi-rust");
        assert_eq!(details["cwdIsString"], true);
        assert_eq!(details["stdoutWrite"], true);

        // `process.stdout.write` never touches the real stdout — the `pi`
        // process owns that stream (JSON-RPC) — so it is reported through
        // the host log; the write returning `true` is what extension code
        // observes.
        // `node:stream` is still unbridged (see KNOWN_UNBRIDGED in the
        // compatibility gate below), so the import has to fail loudly and
        // name the modules that do exist.
        let err = host
            .load(
                entry_at("stream", "/tmp/pi_node_builtins/stream.mjs"),
                r#"
                    import { Readable } from "node:stream";
                    export default function (pi) {
                        pi.appendEntry("loaded", { ok: true });
                    }
                "#,
            )
            .await
            .expect_err("node:stream is not bridged yet");
        let message = err.to_string();
        assert!(message.contains("node:stream"), "{message}");
        assert!(message.contains("node:fs"), "{message}");
        assert!(message.contains("node:fs/promises"), "{message}");

        // `node:readline` and `node:module` are bridged (LUM-1129), so the
        // same imports have to load instead of failing.
        host
            .load(
                entry_at("module_readline", "/tmp/pi_node_builtins/module_readline.mjs"),
                r#"
                    import { createRequire, isBuiltin } from "node:module";
                    import { createInterface } from "node:readline";
                    export default function (pi) {
                        pi.appendEntry("loaded", {
                            require: typeof createRequire,
                            builtin: isBuiltin("node:fs"),
                            interface: typeof createInterface,
                        });
                    }
                "#,
            )
            .await
            .expect("node:module / node:readline are bridged");

        // `node:child_process` is bridged now (LUM-1110), so the same import
        // has to load instead of failing.
        host.load(
            entry_at("child_process", "/tmp/pi_node_builtins/child_process.mjs"),
            r#"
                import { execSync } from "node:child_process";
                export default function (pi) {
                    pi.registerTool({
                        name: "child_process_probe",
                        label: "child process probe",
                        description: "imports node:child_process",
                        parameters: { type: "object" },
                        execute: () => ({
                            content: [{ type: "text", text: execSync("printf ok", { encoding: "utf8" }) }],
                            details: { ok: true },
                        }),
                    });
                }
            "#,
        )
        .await
        .expect("node:child_process is bridged");

        let outcome = host
            .execute_tool("child_process_probe", "{}")
            .await
            .expect("execute child_process probe");
        assert!(!outcome.is_error, "{outcome:?}");
        assert_eq!(outcome.details.expect("details")["ok"], true);
    });
}

/// The pure-JS `node:util` surface — LUM-1105. No host op backs it, so a
/// single extension exercises every export and the assertions read the
/// returned details object. The expected strings were captured from Node
/// v22.23.2 (`util.format` / `util.inspect` / `util.styleText`), so a shim
/// regression that drifts from upstream output fails here.
#[test]
fn node_util_surface_matches_node() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("util");
        let host = host_with_cwd(&scratch.as_str()).await;

        let source = r#"
            import util from "node:util";
            import {
                format, formatWithOptions, inspect, isDeepStrictEqual,
                promisify, callbackify, inherits, deprecate,
                stripVTControlCharacters, styleText, types,
                TextEncoder, TextDecoder,
            } from "node:util";

            export default function (pi) {
                pi.registerTool({
                    name: "util_probe",
                    label: "util probe",
                    description: "exercises the node:util virtual module",
                    parameters: { type: "object" },
                    execute: async () => {
                        const double = (value, callback) => {
                            Promise.resolve().then(() => callback(null, value * 2));
                        };
                        const doubled = await promisify(double)(21);

                        const fail = (callback) => {
                            Promise.resolve().then(() => callback(new Error("boom")));
                        };
                        let rejected = false;
                        try {
                            await promisify(fail)();
                        } catch (err) {
                            rejected = err.message === "boom";
                        }

                        const custom = (callback) => callback(null, "plain");
                        custom[util.promisify.custom] = () => Promise.resolve("custom");
                        const customValue = await promisify(custom)();

                        const asyncFn = async (value) => value + 1;
                        const viaCallback = await new Promise((resolve, reject) => {
                            callbackify(asyncFn)(41, (err, value) =>
                                err ? reject(err) : resolve(value),
                            );
                        });

                        const circular = {};
                        circular.self = circular;

                        function Base() {}
                        Base.prototype.kind = "base";
                        function Child() {}
                        inherits(Child, Base);
                        const child = new Child();

                        let calls = 0;
                        const deprecated = deprecate(
                            (value) => {
                                calls += 1;
                                return value + 1;
                            },
                            "old",
                            "DEP0001",
                        );

                        const encoded = new TextEncoder().encode("héllo");

                        return {
                            content: [{ type: "text", text: "util probe" }],
                            details: {
                                doubled,
                                rejected,
                                customValue,
                                viaCallback,
                                formatted: format("%s|%d|%i|%f|%j|%%", "hi", "42", "3.9", "2.5", { a: 1 }),
                                extra: format("plain", "arg", 7),
                                withOptions: formatWithOptions({ depth: 0 }, "%O", { a: { b: 1 } }),
                                inspected: inspect({ a: [1, 2], b: "x" }),
                                inspectedDepth: inspect({ a: { b: { c: { d: 1 } } } }),
                                inspectedCircular: inspect(circular),
                                inspectedMaxArray: inspect([1, 2, 3, 4, 5], { maxArrayLength: 2 }),
                                inspectedColors: inspect({ a: 1 }, { colors: true }),
                                inspectedCustom: inspect({ [util.inspect.custom]: () => "<custom>" }),
                                typeChecks: [
                                    types.isDate(new Date(0)),
                                    types.isRegExp(/x/),
                                    types.isNativeError(new TypeError("x")),
                                    !types.isNativeError({}),
                                    types.isPromise(Promise.resolve()),
                                    types.isUint8Array(new Uint8Array(1)),
                                    types.isTypedArray(new Float64Array(1)),
                                    !types.isTypedArray(new DataView(new ArrayBuffer(4))),
                                    types.isMap(new Map()),
                                    types.isSet(new Set()),
                                    types.isBoxedPrimitive(new Number(1)),
                                    !types.isBoxedPrimitive(1),
                                    types.isAnyArrayBuffer(new ArrayBuffer(1)),
                                    types.isInt32Array(new Int32Array(1)),
                                    types.isAsyncFunction(async () => {}),
                                ],
                                deepEqual: [
                                    isDeepStrictEqual({ a: [1, { b: 2 }] }, { a: [1, { b: 2 }] }),
                                    isDeepStrictEqual(
                                        new Map([[1, 2], [3, 4]]),
                                        new Map([[3, 4], [1, 2]]),
                                    ),
                                    isDeepStrictEqual(new Set([1, 1, 2]), new Set([2, 2, 1])),
                                    isDeepStrictEqual(new Date(0), new Date(0)),
                                    isDeepStrictEqual(NaN, NaN),
                                    isDeepStrictEqual(new Uint8Array([1, 2]), new Uint8Array([1, 2])),
                                    !isDeepStrictEqual({ a: 1 }, { a: 2 }),
                                    !isDeepStrictEqual({ a: 1 }, { a: 1, b: 2 }),
                                    !isDeepStrictEqual(0, -0),
                                    !isDeepStrictEqual(Object.create(null), {}),
                                    !isDeepStrictEqual([1], Object.assign([1], { extra: 2 })),
                                ],
                                stripped: stripVTControlCharacters("\u001b[31mred\u001b[39m plain"),
                                styled: styleText(["bold", "red"], "x", { validateStream: false }),
                                styledBg: styleText("bgBlue", "x", { validateStream: false }),
                                inherited:
                                    child.kind === "base" &&
                                    child instanceof Base &&
                                    Child.super_ === Base,
                                deprecatedValue: deprecated(1) + deprecated(1),
                                deprecationCalls: calls,
                                decoded: new TextDecoder().decode(encoded),
                                encodedBytes: Array.from(encoded).join(","),
                                latin1: new TextDecoder("latin1").decode(new Uint8Array([0x68, 0xe9])),
                                utf16le: new TextDecoder("utf-16le").decode(
                                    new Uint8Array([0x68, 0x00, 0xe9, 0x00]),
                                ),
                                encodeInto: new TextEncoder().encodeInto("héllo", new Uint8Array(4)),
                                globals:
                                    typeof globalThis.TextEncoder === "function" &&
                                    typeof globalThis.TextDecoder === "function",
                                defaultExport: util.default === util && typeof util.inspect === "function",
                            },
                        };
                    },
                });
            }
        "#;

        host.load(
            entry_at("util_probe", "/tmp/pi_node_builtins/util_probe.mjs"),
            source,
        )
        .await
        .expect("load util probe extension");

        let outcome = host
            .execute_tool("util_probe", "{}")
            .await
            .expect("execute util probe");
        assert!(!outcome.is_error, "{outcome:?}");

        let details = outcome.details.expect("details");
        assert_eq!(details["doubled"], 42);
        assert_eq!(details["rejected"], true);
        assert_eq!(details["customValue"], "custom");
        assert_eq!(details["viaCallback"], 42);
        assert_eq!(details["formatted"], "hi|42|3|2.5|{\"a\":1}|%");
        assert_eq!(details["extra"], "plain arg 7");
        assert_eq!(details["withOptions"], "{ a: [Object] }");
        assert_eq!(details["inspected"], "{ a: [ 1, 2 ], b: 'x' }");
        assert_eq!(details["inspectedDepth"], "{ a: { b: { c: [Object] } } }");
        assert_eq!(details["inspectedCircular"], "<ref *1> { self: [Circular *1] }");
        assert_eq!(details["inspectedMaxArray"], "[ 1, 2, ... 3 more items ]");
        assert_eq!(details["inspectedColors"], "{ a: \u{1b}[33m1\u{1b}[39m }");
        assert_eq!(details["inspectedCustom"], "<custom>");
        for check in details["typeChecks"].as_array().expect("typeChecks") {
            assert_eq!(*check, true, "typeChecks: {details}");
        }
        for check in details["deepEqual"].as_array().expect("deepEqual") {
            assert_eq!(*check, true, "deepEqual: {details}");
        }
        assert_eq!(details["stripped"], "red plain");
        assert_eq!(details["styled"], "\u{1b}[1m\u{1b}[31mx\u{1b}[39m\u{1b}[22m");
        assert_eq!(details["styledBg"], "\u{1b}[44mx\u{1b}[49m");
        assert_eq!(details["inherited"], true);
        assert_eq!(details["deprecatedValue"], 4);
        assert_eq!(details["deprecationCalls"], 2);
        assert_eq!(details["decoded"], "héllo");
        assert_eq!(details["encodedBytes"], "104,195,169,108,108,111");
        assert_eq!(details["latin1"], "hé");
        assert_eq!(details["utf16le"], "hé");
        assert_eq!(details["encodeInto"], json!({"read": 3, "written": 4}));
        assert_eq!(details["globals"], true);
        assert_eq!(details["defaultExport"], true);
    });
}

/// Recursively collect files under `dir` (the example trees are shallow).
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// Quoted `"node:…"` / `'node:…'` specifiers in a source file.
///
/// Quote-anchored on purpose: minified bundles contain text like
/// `nextInode:1` or `node:current`, which is not an import.
fn node_specifiers(source: &str) -> std::collections::BTreeSet<String> {
    let mut found = std::collections::BTreeSet::new();
    let bytes = source.as_bytes();
    for (index, _) in source.match_indices("node:") {
        let Some(quote) = index
            .checked_sub(1)
            .map(|i| bytes[i])
            .filter(|b| *b == b'"' || *b == b'\'')
        else {
            continue;
        };
        let rest = &source[index + "node:".len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '/')
            .collect();
        let closes = rest.as_bytes().get(name.len()).copied() == Some(quote);
        if closes && name.contains(|c: char| c.is_ascii_alphabetic()) {
            found.insert(format!("node:{name}"));
        }
    }
    found
}

/// Compatibility gate: every `node:*` specifier the upstream extension
/// examples import is either bridged by the shim or explicitly listed as a
/// known, documented frontier gap. A new upstream import therefore fails
/// here instead of silently outrunning the bridge.
///
/// The bridged set is read out of `runtime/pi-ext-shim.mjs` (the source of
/// truth) rather than restated, and the scan is skipped when the upstream
/// example directories are not part of the checkout.
#[test]
fn upstream_node_imports_are_all_bridged_or_documented() {
    use std::collections::BTreeSet;

    /// Builtins the upstream examples reach for that are not bridged yet.
    /// Every entry must appear in `docs/NODE_BUILTINS.md` under
    /// "Not bridged", and must *not* be in the shim's map.
    ///
    /// Empty since LUM-1129 bridged `node:module` / `node:readline`, the last
    /// two specifiers the scanned upstream examples import. The list stays as
    /// the gate's escape hatch: the next upstream import that lands in the
    /// checkout fails the test below until it is either bridged or listed and
    /// documented here.
    const KNOWN_UNBRIDGED: [&str; 0] = [];

    fn shim_specifiers() -> BTreeSet<String> {
        let shim = include_str!("../runtime/pi-ext-shim.mjs");
        let start = shim
            .find("globalThis.__pi_virtual_modules = Object.freeze({")
            .expect("virtual module map in the shim");
        let block = &shim[start..];
        let end = block.find("});").expect("end of the virtual module map");
        let mut specifiers = BTreeSet::new();
        for line in block[..end].lines().skip(1) {
            let line = line.trim();
            // Keys are either quoted (`"node:fs": …`) or bare (`fs: …`).
            let (key, _) = if let Some(rest) = line.strip_prefix('"') {
                match rest.split_once("\": ") {
                    Some(pair) => pair,
                    None => continue,
                }
            } else {
                match line.split_once(": ") {
                    Some(pair) => pair,
                    None => continue,
                }
            };
            if !key.is_empty() {
                specifiers.insert(key.to_string());
            }
        }
        specifiers
    }

    // `crates/pi-extensions` → the repository root, where the upstream
    // examples live.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut scanned_files = 0usize;
    let mut upstream = BTreeSet::new();
    for dir in [
        ".pi/extensions",
        "packages/coding-agent/examples/extensions",
    ] {
        let dir = root.join(dir);
        if !dir.is_dir() {
            continue;
        }
        for entry in walk(&dir) {
            let is_source = entry
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| matches!(ext, "ts" | "mts" | "js" | "mjs"));
            if !is_source {
                continue;
            }
            scanned_files += 1;
            let source = std::fs::read_to_string(&entry).expect("read upstream example");
            upstream.extend(node_specifiers(&source));
        }
    }
    if scanned_files == 0 {
        // Nothing to compare against in this checkout.
        return;
    }

    eprintln!("SCANNED {} files upstream={:?}", scanned_files, upstream);
    let bridged = shim_specifiers();
    let documented = include_str!("../docs/NODE_BUILTINS.md");
    let known: BTreeSet<&str> = KNOWN_UNBRIDGED.into_iter().collect();

    for specifier in &upstream {
        if bridged.contains(specifier) {
            // The bare alias has to exist as well, for `import … from "fs"`.
            if let Some(bare) = specifier.strip_prefix("node:") {
                assert!(
                    bridged.contains(bare),
                    "`{specifier}` is bridged but its bare alias `{bare}` is not"
                );
            }
        } else {
            assert!(
                known.contains(specifier.as_str()),
                "upstream examples import `{specifier}`, which is neither bridged nor listed in \
                 KNOWN_UNBRIDGED — bridge it or document it in docs/NODE_BUILTINS.md"
            );
            assert!(
                documented.contains(specifier.as_str()),
                "`{specifier}` is a known gap but docs/NODE_BUILTINS.md does not name it"
            );
        }
    }

    // The documented gap list must not rot: once a builtin is bridged, its
    // entry leaves KNOWN_UNBRIDGED (and the doc table) in the same commit.
    for specifier in KNOWN_UNBRIDGED {
        assert!(
            !bridged.contains(specifier),
            "`{specifier}` is now bridged: drop it from KNOWN_UNBRIDGED and the frontier table"
        );
    }

    assert!(
        upstream.len() >= 5,
        "expected several node builtins in the upstream examples, found {upstream:?}"
    );
}
