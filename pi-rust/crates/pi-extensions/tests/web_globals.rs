//! Web platform globals in the extension host — `atob` / `btoa` /
//! `crypto` (WebCrypto subset) / `URLSearchParams`. (`URL` is the fourth
//! sibling global, but its probe lives with the builtin-surface tests in
//! `tests/node_builtins.rs` because the frontier table there names it.)
//!
//! QuickJS ships none of them, but upstream extensions treat them as
//! ambient: `packages/coding-agent/examples/extensions/custom-provider-anthropic/index.ts`
//! runs an OAuth PKCE exchange whose only inputs are `crypto.getRandomValues`,
//! `btoa` / `atob`, `crypto.subtle.digest("SHA-256", …)` and
//! `new URLSearchParams({…})`. These tests drive that surface through the
//! real host (`JsExtensionHost::load` + `execute_tool`) so the shim, the
//! `crypto.digest` bridge op and the Rust digest module are all covered end
//! to end.

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
            "pi_web_globals/{}-{name}-{unique}",
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

/// Run one probe extension and return its `details` object.
async fn probe(name: &str, source: &str) -> serde_json::Value {
    let scratch = Scratch::new(name);
    let host = host_with_cwd(&scratch.as_str()).await;
    host.load(
        entry_at(name, &format!("/tmp/pi_web_globals/{name}.mjs")),
        source,
    )
    .await
    .expect("load probe extension");

    let outcome = host
        .execute_tool(name, &json!({ "dir": scratch.as_str() }).to_string())
        .await
        .expect("execute probe");
    assert!(!outcome.is_error, "{outcome:?}");
    outcome.details.expect("details")
}

/// `atob` / `btoa` — the Latin-1 binary-string contract, including the
/// errors the spec requires.
#[test]
fn base64_globals_follow_the_binary_string_contract() {
    let details = rt().block_on(probe(
        "base64_globals",
        r#"
            export default function (pi) {
                pi.registerTool({
                    name: "base64_globals",
                    label: "base64 globals",
                    description: "exercises atob / btoa",
                    parameters: { type: "object", properties: {} },
                    execute: () => {
                        const bytes = new Uint8Array([0, 1, 254, 255]);
                        const binary = String.fromCharCode(...bytes);
                        const encoded = btoa(binary);
                        const decoded = atob(encoded);
                        const errorName = (fn) => {
                            try {
                                fn();
                                return "no-throw";
                            } catch (error) {
                                return error.name;
                            }
                        };
                        return {
                            content: [{ type: "text", text: encoded }],
                            details: {
                                encoded,
                                decodedCodes: [...decoded].map((c) => c.charCodeAt(0)),
                                decodedLength: decoded.length,
                                roundTrip: btoa(decoded) === encoded,
                                hello: atob("aGVsbG8="),
                                whitespaceTolerant: atob("aGVs\nbG8=") === "hello",
                                latin1UpperBound: btoa("\u00ff") === "/w==",
                                unicodeThrows: errorName(() => btoa("\u20ac")),
                                badLengthThrows: errorName(() => atob("aGVsbG8")),
                                badAlphabetThrows: errorName(() => atob("aGVsbG8*")),
                            },
                        };
                    },
                });
            }
        "#,
    ));

    assert_eq!(details["encoded"], "AAH+/w==");
    assert_eq!(details["decodedCodes"], json!([0, 1, 254, 255]));
    assert_eq!(details["decodedLength"], 4);
    assert_eq!(details["roundTrip"], true);
    assert_eq!(details["hello"], "hello");
    assert_eq!(details["whitespaceTolerant"], true);
    // 0xFF is the top of the Latin-1 range `btoa` accepts; a code point above
    // it has no single byte to encode and must fail loudly.
    assert_eq!(details["latin1UpperBound"], true);
    assert_eq!(details["unicodeThrows"], "InvalidCharacterError");
    assert_eq!(details["badLengthThrows"], "InvalidCharacterError");
    assert_eq!(details["badAlphabetThrows"], "InvalidCharacterError");
}

/// `crypto` — `getRandomValues`, `randomUUID`, `subtle.digest` and the Node
/// `createHash` form, all through the host's `crypto.digest` bridge.
///
/// The PKCE assertion is the RFC 7636 appendix B vector, which is the exact
/// computation `custom-provider-anthropic/index.ts` performs before it can
/// build an authorize URL.
#[test]
fn crypto_globals_hash_and_fill_typed_arrays() {
    let details = rt().block_on(probe(
        "crypto_globals",
        r#"
            import { createHash } from "node:crypto";

            export default function (pi) {
                pi.registerTool({
                    name: "crypto_globals",
                    label: "crypto globals",
                    description: "exercises the WebCrypto globals",
                    parameters: { type: "object", properties: {} },
                    execute: async () => {
                        const challengeMap = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
                        const toBase64Url = (bytes) =>
                            btoa(String.fromCharCode(...new Uint8Array(bytes)))
                                .replace(/\+/g, "-")
                                .replace(/\//g, "_")
                                .replace(/=+$/, "");

                        const verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
                        const data = new TextEncoder().encode(verifier);
                        const hash = await crypto.subtle.digest("SHA-256", data);
                        const challenge = toBase64Url(hash);

                        const random = new Uint8Array(32);
                        const filled = crypto.getRandomValues(random);
                        const distinct = new Set(random).size > 1;

                        const errorName = (fn) => {
                            try {
                                fn();
                                return "no-throw";
                            } catch (error) {
                                return error.name + ": " + error.message.slice(0, 40);
                            }
                        };
                        // `subtle.digest` is async, so its argument validation
                        // only surfaces through the rejected promise.
                        const asyncErrorName = async (fn) => {
                            try {
                                await fn();
                                return "no-throw";
                            } catch (error) {
                                return error.name + ": " + error.message.slice(0, 40);
                            }
                        };

                        const digestView = new DataView(new ArrayBuffer(8));
                        return {
                            content: [{ type: "text", text: challenge }],
                            details: {
                                challenge,
                                challengeLength: challenge.length,
                                challengeAlphabet: [...challenge].every((c) => challengeMap.includes(c)),
                                algorithmObject: toBase64Url(
                                    await crypto.subtle.digest({ name: "SHA-256" }, data),
                                ),
                                sha1: toBase64Url(await crypto.subtle.digest("SHA-1", data)),
                                digestIsArrayBuffer: hash instanceof ArrayBuffer,
                                hashByteLength: hash.byteLength,
                                filledInPlace: filled === random,
                                randomLength: random.length,
                                randomLooksRandom: distinct,
                                uuidShape: /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(
                                    crypto.randomUUID(),
                                ),
                                scopeIsGlobal: typeof crypto === "object" && crypto === globalThis.crypto,
                                createHashHex: createHash("sha256").update("abc").digest("hex"),
                                createHashBuffer: Buffer.isBuffer(createHash("sha256").update("abc").digest()),
                                createHashChained: createHash("SHA-256")
                                    .update("a")
                                    .update(Buffer.from("b"))
                                    .update(new TextEncoder().encode("c"))
                                    .digest("hex"),
                                createHashBase64: createHash("sha256").update("abc").digest("base64"),
                                sha1Hex: createHash("sha1").update("abc").digest("hex"),
                                unknownAlgorithm: errorName(() =>
                                    createHash("md5").update("abc").digest("hex"),
                                ),
                                quotaExceeded: errorName(() =>
                                    crypto.getRandomValues(new Uint8Array(65537)),
                                ),
                                floatViewRejected: errorName(() =>
                                    crypto.getRandomValues(new Float64Array(1)),
                                ),
                                dataViewRejected: errorName(() => crypto.getRandomValues(digestView)),
                                stringDataRejected: await asyncErrorName(() =>
                                    crypto.subtle.digest("SHA-256", "abc"),
                                ),
                            },
                        };
                    },
                });
            }
        "#,
    ));

    // RFC 7636 appendix B.
    assert_eq!(
        details["challenge"],
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    assert_eq!(details["challengeLength"], 43);
    assert_eq!(details["challengeAlphabet"], true);
    assert_eq!(details["algorithmObject"], details["challenge"]);
    assert_eq!(details["digestIsArrayBuffer"], true);
    assert_eq!(details["hashByteLength"], 32);
    assert_eq!(details["filledInPlace"], true);
    assert_eq!(details["randomLength"], 32);
    assert_eq!(details["randomLooksRandom"], true);
    assert_eq!(
        details["sha1"].as_str().unwrap().len(),
        27,
        "20 SHA-1 bytes render as 27 unpadded base64url characters"
    );
    assert_eq!(details["uuidShape"], true);
    assert_eq!(details["scopeIsGlobal"], true);

    assert_eq!(
        details["createHashHex"],
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(details["createHashBuffer"], true);
    assert_eq!(details["createHashChained"], details["createHashHex"]);
    assert_eq!(
        details["createHashBase64"],
        "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0="
    );
    assert_eq!(
        details["sha1Hex"],
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert!(
        details["unknownAlgorithm"]
            .as_str()
            .unwrap()
            .starts_with("Error: unsupported digest"),
        "{details}"
    );
    assert!(
        details["quotaExceeded"]
            .as_str()
            .unwrap()
            .starts_with("Error: crypto.getRandomValues: quota"),
        "{details}"
    );
    assert!(
        details["floatViewRejected"]
            .as_str()
            .unwrap()
            .starts_with("TypeError: crypto.getRandomValues"),
        "{details}"
    );
    assert!(
        details["dataViewRejected"]
            .as_str()
            .unwrap()
            .starts_with("TypeError: crypto.getRandomValues"),
        "{details}"
    );
    assert!(
        details["stringDataRejected"]
            .as_str()
            .unwrap()
            .starts_with("TypeError: crypto.subtle.digest"),
        "{details}"
    );
}

/// `URLSearchParams` — construction from string / record / sequence, the
/// `application/x-www-form-urlencoded` codec, and the accessor set the
/// standard defines.
#[test]
fn url_search_params_implements_the_urlencoded_codec() {
    let details = rt().block_on(probe(
        "url_search_params",
        r#"
            export default function (pi) {
                pi.registerTool({
                    name: "url_search_params",
                    label: "url search params",
                    description: "exercises URLSearchParams",
                    parameters: { type: "object", properties: {} },
                    execute: () => {
                        const fromRecord = new URLSearchParams({
                            code: "true",
                            client_id: "abc",
                            scope: "org:create_api_key user:profile",
                        });
                        const authorizeUrl = "https://claude.ai/oauth/authorize?" + fromRecord.toString();

                        const parsed = new URLSearchParams("?a=1&a=2&b=x%20y&c&d=1+2&e=%E6%97%A5");
                        const collect = (iterable) => [...iterable];
                        const seen = [];
                        parsed.forEach((value, name) => seen.push(name + "=" + value));

                        const mutable = new URLSearchParams([["b", "2"], ["a", "1"], ["b", "1"]]);
                        mutable.sort();
                        const sorted = mutable.toString();
                        mutable.set("b", "9");
                        const afterSet = mutable.toString();
                        mutable.delete("b");
                        const afterDelete = mutable.toString();
                        mutable.append("b", "1");
                        mutable.append("b", "2");
                        mutable.delete("b", "1");
                        const afterSelectiveDelete = mutable.toString();

                        const unicode = new URLSearchParams([["q", "日本語 & ~ +=/"]]);
                        const unicodeRoundTrip = new URLSearchParams(unicode.toString()).get("q");
                        const missing = parsed.get("zzz");

                        return {
                            content: [{ type: "text", text: authorizeUrl }],
                            details: {
                                authorizeUrl,
                                toStringTag: Object.prototype.toString.call(fromRecord),
                                getAllA: parsed.getAll("a"),
                                getB: parsed.get("b"),
                                missingIsNull: missing === null,
                                getDPlusDecoded: parsed.get("d"),
                                getEUtf8Decoded: parsed.get("e"),
                                bareKey: parsed.get("c"),
                                hasValue: parsed.has("a", "2") && !parsed.has("a", "3"),
                                size: parsed.size,
                                keys: collect(parsed.keys()),
                                values: collect(parsed.values()),
                                entries: collect(parsed.entries()),
                                forEach: seen,
                                sorted,
                                afterSet,
                                afterDelete,
                                afterSelectiveDelete,
                                unicodeString: unicode.toString(),
                                unicodeRoundTrip,
                                empty: new URLSearchParams().toString(),
                                emptySize: new URLSearchParams().size,
                            },
                        };
                    },
                });
            }
        "#,
    ));

    assert_eq!(
        details["authorizeUrl"],
        "https://claude.ai/oauth/authorize?code=true&client_id=abc&scope=org%3Acreate_api_key+user%3Aprofile"
    );
    assert_eq!(details["toStringTag"], "[object URLSearchParams]");
    assert_eq!(details["getAllA"], json!(["1", "2"]));
    assert_eq!(details["getB"], "x y");
    assert_eq!(details["missingIsNull"], true);
    assert_eq!(details["getDPlusDecoded"], "1 2");
    assert_eq!(details["getEUtf8Decoded"], "日");
    assert_eq!(details["bareKey"], "");
    assert_eq!(details["hasValue"], true);
    assert_eq!(details["size"], 6);
    assert_eq!(details["keys"], json!(["a", "a", "b", "c", "d", "e"]));
    assert_eq!(details["values"], json!(["1", "2", "x y", "", "1 2", "日"]));
    assert_eq!(
        details["entries"],
        json!([
            ["a", "1"],
            ["a", "2"],
            ["b", "x y"],
            ["c", ""],
            ["d", "1 2"],
            ["e", "日"]
        ])
    );
    assert_eq!(
        details["forEach"],
        json!(["a=1", "a=2", "b=x y", "c=", "d=1 2", "e=日"])
    );
    assert_eq!(details["sorted"], "a=1&b=2&b=1");
    assert_eq!(details["afterSet"], "a=1&b=9");
    assert_eq!(details["afterDelete"], "a=1");
    assert_eq!(details["afterSelectiveDelete"], "a=1&b=2");
    assert_eq!(
        details["unicodeString"],
        "q=%E6%97%A5%E6%9C%AC%E8%AA%9E+%26+%7E+%2B%3D%2F"
    );
    assert_eq!(details["unicodeRoundTrip"], "日本語 & ~ +=/");
    assert_eq!(details["empty"], "");
    assert_eq!(details["emptySize"], 0);
}

/// The globals must not shadow an implementation the engine already provides,
/// and the shim must be safe to evaluate twice (the host installs it per
/// runtime). A second `load` in the same host re-runs the shim.
#[test]
fn globals_survive_a_second_extension_load() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("double-load");
        let host = host_with_cwd(&scratch.as_str()).await;
        let source = r#"
            export default function (pi) {
                pi.registerTool({
                    name: "double_load",
                    label: "double load",
                    description: "reads the web globals",
                    parameters: { type: "object", properties: {} },
                    execute: () => ({
                        content: [{ type: "text", text: "ok" }],
                        details: {
                            atob: atob("aGk="),
                            params: new URLSearchParams({ a: "1" }).get("a"),
                            cryptoDigest: typeof crypto.subtle.digest,
                        },
                    }),
                });
            }
        "#;

        for index in 0..2 {
            host.load(
                entry_at(
                    &format!("double_load_{index}"),
                    "/tmp/pi_web_globals/double.mjs",
                ),
                source,
            )
            .await
            .expect("load probe extension");

            let outcome = host
                .execute_tool(
                    "double_load",
                    &json!({ "dir": scratch.as_str() }).to_string(),
                )
                .await
                .expect("execute probe");
            assert!(!outcome.is_error, "{outcome:?}");
            let details = outcome.details.expect("details");
            assert_eq!(details["atob"], "hi");
            assert_eq!(details["params"], "1");
            assert_eq!(details["cryptoDigest"], "function");
        }

        assert_eq!(
            std::fs::read_dir(scratch.path()).unwrap().count(),
            0,
            "the probes must not write files"
        );
    });
}
