//! `node:zlib` virtual module tests — LUM-1125, extended in LUM-1131.
//!
//! The zstd family plus `crc32` is what the upstream repository actually
//! calls: `packages/ai/src/api/openai-codex-responses.ts` zstd-compresses
//! Codex request bodies, `packages/ai/test/openai-codex-stream.test.ts`
//! decompresses captured bodies, `tool-result-images.test.ts` builds PNG
//! chunks from `crc32`, and `doom-overlay/wad-finder.ts` gunzips a WAD.
//! LUM-1131 added the gzip/deflate half of the module (pure-Rust codec, no
//! `flate2`); `tests/zlib_deflate.rs` holds the interop fixtures.
//!
//! These tests drive the real host ([`JsExtensionHost::execute_tool`]) the
//! same way `tests/node_builtins.rs` does, and — crucially — cross-check
//! against bytes produced by Node's own `zlib` (Node v22.23.2), both
//! directions, so the suite is not a self-consistent "compress with Rust,
//! decompress with Rust" empty loop.

use pi_extensions::{ExtensionEntry, HostOptions, JsExtensionHost};
use serde_json::{json, Value};

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime")
}

fn entry_at(id: &str, path: &str) -> ExtensionEntry {
    ExtensionEntry {
        source: std::path::PathBuf::from(path),
        id: id.to_string(),
        label: None,
    }
}

async fn host() -> JsExtensionHost {
    JsExtensionHost::with_options(HostOptions::default())
        .await
        .expect("host")
}

/// Bytes → `Vec<u8>`, for `details` fields the extension returns as a number
/// array (the bridge is JSON, so binary has to cross as JSON too).
fn bytes_of(value: &Value) -> Vec<u8> {
    value
        .as_array()
        .expect("byte array")
        .iter()
        .map(|byte| u8::try_from(byte.as_u64().expect("byte")).expect("byte in range"))
        .collect()
}

/// Decode the hex literals below. Hand-rolled so the test has no base64/hex
/// dependency beyond the workspace.
fn hex_decode(text: &str) -> Vec<u8> {
    assert!(text.len() % 2 == 0, "hex literal must have even length");
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("hex"))
        .collect()
}

/// Payload of the Node-generated fixture. Kept identical to the string used
/// by `zlib_crc32_vectors_and_module_aliases_match_node`.
const NODE_FIXTURE_PAYLOAD: &str = "pi-rust node:zlib interop fixture — 1234567890";
/// `require("zlib").zstdCompressSync(NODE_FIXTURE_PAYLOAD)` on Node v22.23.2,
/// hex-encoded. Decompressing it proves the Rust bridge understands frames
/// produced by Node's zstd, not just its own.
const NODE_ZSTD_FIXTURE_HEX: &str = "28b52ffd203081010070692d72757374206e6f64653a7a6c696220696e7465726f70206669787475726520e280942031323334353637383930";
/// `require("zlib").crc32(NODE_FIXTURE_PAYLOAD) >>> 0`.
const NODE_FIXTURE_CRC32: u64 = 2412760136;

/// `zstdCompressSync` / `zstdDecompressSync` round-trip both ways: the
/// extension's frame is decoded with Rust's `zstd::decode_all`, and the
/// extension decodes a frame Rust produced with `zstd::stream::encode_all`.
#[test]
fn zlib_zstd_round_trips_between_rust_and_js() {
    let runtime = rt();
    runtime.block_on(async {
        let host = host().await;

        let source = r#"
            import { zstdCompressSync, zstdDecompressSync } from "node:zlib";
            import { Buffer } from "node:buffer";

            export default function (pi) {
                pi.registerTool({
                    name: "zstd_probe",
                    label: "zstd probe",
                    description: "round-trips zstd frames across the bridge",
                    parameters: { type: "object", properties: { payload: { type: "string" } } },
                    execute: (args) => {
                        const text = args.payload;
                        const compressed = zstdCompressSync(text);
                        // A `Uint8Array` (not a `Buffer`) must be accepted too.
                        const viaUint8 = zstdDecompressSync(new Uint8Array(compressed));
                        const viaArrayBuffer = zstdDecompressSync(compressed.buffer.slice(0));
                        const fromRust = zstdDecompressSync(Buffer.from(args.rustBytes));
                        return {
                            content: [{ type: "text", text: "ok" }],
                            details: {
                                isBuffer: Buffer.isBuffer(compressed),
                                isUint8: compressed instanceof Uint8Array,
                                compressed: Array.prototype.slice.call(compressed),
                                compressedLength: compressed.length,
                                emptyCompressedLength: zstdCompressSync("").length,
                                viaUint8Text: viaUint8.toString("utf8"),
                                viaArrayBufferText: viaArrayBuffer.toString("utf8"),
                                fromRustText: fromRust.toString("utf8"),
                                fromRustIsBuffer: Buffer.isBuffer(fromRust),
                            },
                        };
                    },
                });
            }
        "#;

        host.load(
            entry_at("zstd_probe", "/tmp/pi_zlib/zstd_probe.mjs"),
            source,
        )
        .await
        .expect("load zstd probe extension");

        // A payload big enough that zstd actually has to work for it.
        let payload = format!("héllo zstd 世界 {}", "compressible ".repeat(400));
        let rust_compressed = zstd::stream::encode_all(std::io::Cursor::new(payload.as_bytes()), 3)
            .expect("rust zstd encode");

        let outcome = host
            .execute_tool(
                "zstd_probe",
                &json!({
                    "payload": payload,
                    "rustBytes": rust_compressed.clone(),
                })
                .to_string(),
            )
            .await
            .expect("execute zstd probe");
        assert!(!outcome.is_error, "{outcome:?}");

        let details = outcome.details.expect("details");
        assert_eq!(details["isBuffer"], true);
        assert_eq!(details["isUint8"], true);
        assert_eq!(details["viaUint8Text"], payload);
        // This is the "Node would accept the output" direction: the JS frame
        // decompresses with the Rust zstd library, byte for byte.
        let from_js = bytes_of(&details["compressed"]);
        let decoded = zstd::stream::decode_all(std::io::Cursor::new(from_js)).expect("rust decode");
        assert_eq!(decoded, payload.as_bytes());
        // ...and the frame Rust produced is understood by the shim.
        assert_eq!(details["fromRustText"], payload);
        assert_eq!(details["fromRustIsBuffer"], true);
        assert_eq!(details["viaArrayBufferText"], payload);
        // Empty input still produces a valid, non-empty zstd frame.
        assert!(details["emptyCompressedLength"].as_u64().unwrap() > 0);
    });
}

/// Interop fixture: a frame captured from Node's `zstdCompressSync` must
/// decode to the original string through the shim, and the same bytes must
/// decode through Rust's `zstd::decode_all` (so the fixture itself is known
/// good on both sides).
#[test]
fn zlib_zstd_decodes_a_node_generated_fixture() {
    let runtime = rt();
    runtime.block_on(async {
        let host = host().await;

        let source = r#"
            import { zstdDecompressSync } from "node:zlib";
            import { Buffer } from "node:buffer";

            export default function (pi) {
                pi.registerTool({
                    name: "zstd_fixture",
                    label: "zstd fixture",
                    description: "decodes a Node-generated zstd frame",
                    parameters: { type: "object", properties: { hex: { type: "string" } } },
                    execute: (args) => {
                        const frame = Buffer.from(args.hex, "hex");
                        const decoded = zstdDecompressSync(frame);
                        return {
                            content: [{ type: "text", text: decoded.toString("utf8") }],
                            details: {
                                text: decoded.toString("utf8"),
                                bytes: Array.prototype.slice.call(decoded),
                                isBuffer: Buffer.isBuffer(decoded),
                            },
                        };
                    },
                });
            }
        "#;

        host.load(
            entry_at("zstd_fixture", "/tmp/pi_zlib/zstd_fixture.mjs"),
            source,
        )
        .await
        .expect("load fixture extension");

        let outcome = host
            .execute_tool(
                "zstd_fixture",
                &json!({ "hex": NODE_ZSTD_FIXTURE_HEX }).to_string(),
            )
            .await
            .expect("execute fixture probe");
        assert!(!outcome.is_error, "{outcome:?}");

        let details = outcome.details.expect("details");
        assert_eq!(details["text"], NODE_FIXTURE_PAYLOAD);
        assert_eq!(details["isBuffer"], true);
        assert_eq!(bytes_of(&details["bytes"]), NODE_FIXTURE_PAYLOAD.as_bytes());

        // The fixture is genuine zstd: the Rust decoder agrees.
        let fixture = hex_decode(NODE_ZSTD_FIXTURE_HEX);
        let decoded = zstd::stream::decode_all(std::io::Cursor::new(fixture)).expect("rust decode");
        assert_eq!(decoded, NODE_FIXTURE_PAYLOAD.as_bytes());
    });
}

/// `crc32` vectors from Node, including the chained second-argument form,
/// plus the bare `zlib` alias and the gzip/deflate family added in LUM-1131.
#[test]
fn zlib_crc32_vectors_and_module_aliases_match_node() {
    let runtime = rt();
    runtime.block_on(async {
        let host = host().await;

        // The bare alias is exercised here (the other tests import
        // `node:zlib`), and `require()` must resolve to the same module.
        let source = r#"
            import { crc32, constants, deflateSync, gunzipSync, gzipSync, inflateSync, zstdCompressSync } from "zlib";
            import { Buffer } from "node:buffer";

            const viaRequire = require("node:zlib");

            export default function (pi) {
                pi.registerTool({
                    name: "crc32_probe",
                    label: "crc32 probe",
                    description: "exercises zlib.crc32 and the module aliases",
                    parameters: { type: "object", properties: { fixture: { type: "string" } } },
                    execute: (args) => {
                        const raw = Buffer.from("123456789", "utf8");
                        return {
                            content: [{ type: "text", text: "ok" }],
                            details: {
                                empty: crc32(""),
                                vector: crc32("123456789"),
                                chained: crc32("456789", crc32("123")),
                                chainedThree: crc32("789", crc32("456", crc32("123"))),
                                bytesInput: crc32(raw),
                                uint8Input: crc32(new Uint8Array(raw)),
                                unsigned: crc32("hello world"),
                                fixture: crc32(args.fixture),
                                sameModuleViaRequire:
                                    viaRequire.zstdCompressSync === zstdCompressSync &&
                                    viaRequire.crc32 === crc32,
                                compressionLevelConstant: constants.ZSTD_c_compressionLevel,
                                noCompressionConstant: constants.Z_NO_COMPRESSION,
                                gzipPresent:
                                    typeof deflateSync === "function" &&
                                    typeof inflateSync === "function" &&
                                    typeof gzipSync === "function" &&
                                    typeof gunzipSync === "function" &&
                                    viaRequire.gunzipSync === gunzipSync,
                                deflateRoundTrip: inflateSync(deflateSync("round trip")).toString("utf8"),
                                gzipRoundTrip: gunzipSync(gzipSync("round trip")).toString("utf8"),
                            },
                        };
                    },
                });
            }
        "#;

        host.load(
            entry_at("crc32_probe", "/tmp/pi_zlib/crc32_probe.mjs"),
            source,
        )
        .await
        .expect("load crc32 probe extension");

        let outcome = host
            .execute_tool(
                "crc32_probe",
                &json!({ "fixture": NODE_FIXTURE_PAYLOAD }).to_string(),
            )
            .await
            .expect("execute crc32 probe");
        assert!(!outcome.is_error, "{outcome:?}");

        let details = outcome.details.expect("details");
        // Node: crc32("") === 0, crc32("123456789") === 3421780262.
        assert_eq!(details["empty"], 0);
        assert_eq!(details["vector"], 3421780262u64);
        assert_eq!(details["chained"], 3421780262u64);
        assert_eq!(details["chainedThree"], 3421780262u64);
        assert_eq!(details["bytesInput"], 3421780262u64);
        assert_eq!(details["uint8Input"], 3421780262u64);
        // Unsigned 32-bit, like Node (`crc32("hello world")` is positive).
        assert_eq!(details["unsigned"], 222957957u64);
        assert_eq!(details["fixture"], NODE_FIXTURE_CRC32);
        assert_eq!(details["sameModuleViaRequire"], true);
        assert_eq!(details["compressionLevelConstant"], 100);
        assert_eq!(details["noCompressionConstant"], 0);
        // LUM-1131: the gzip/deflate family is bridged too (it used to be a
        // documented gap — no `flate2`/`miniz_oxide` in the offline registry).
        assert_eq!(details["gzipPresent"], true);
        assert_eq!(details["deflateRoundTrip"], "round trip");
        assert_eq!(details["gzipRoundTrip"], "round trip");
    });
}

/// A non-zstd input must surface as a Node-shaped `Error` with a `code`
/// instead of taking the QuickJS host down, and the host must keep working
/// afterwards.
#[test]
fn zlib_invalid_input_throws_with_code_and_host_survives() {
    let runtime = rt();
    runtime.block_on(async {
        let host = host().await;

        let source = r#"
            import { zstdCompressSync, zstdDecompressSync } from "node:zlib";
            import { Buffer } from "node:buffer";

            export default function (pi) {
                pi.registerTool({
                    name: "zstd_failure",
                    label: "zstd failure",
                    description: "triggers a zstd decode failure",
                    parameters: { type: "object", properties: {} },
                    execute: () => {
                        let failure = null;
                        try {
                            zstdDecompressSync(Buffer.from("this is definitely not a zstd frame"));
                        } catch (err) {
                            failure = {
                                isError: err instanceof Error,
                                name: err.name,
                                code: typeof err.code === "string" ? err.code : null,
                                message: err.message,
                            };
                        }
                        // The host must still be usable after the throw.
                        const recovery = zstdDecompressSync(zstdCompressSync("still here")).toString("utf8");
                        return {
                            content: [{ type: "text", text: "ok" }],
                            details: { failure: failure, recovery: recovery },
                        };
                    },
                });
            }
        "#;

        host.load(entry_at("zstd_failure", "/tmp/pi_zlib/zstd_failure.mjs"), source)
            .await
            .expect("load failure extension");

        let outcome = host
            .execute_tool("zstd_failure", "{}")
            .await
            .expect("execute failure probe");
        assert!(!outcome.is_error, "{outcome:?}");

        let details = outcome.details.expect("details");
        let failure = &details["failure"];
        assert_eq!(failure["isError"], true, "{failure:?}");
        let code = failure["code"].as_str().expect("err.code is a string");
        assert!(
            code.starts_with("ZSTD"),
            "expected a ZSTD error code, got {code:?} ({failure:?})"
        );
        // Node v22 reports `ZSTD_error_prefix_unknown` for the same input.
        assert_eq!(code, "ZSTD_error_prefix_unknown");
        assert_eq!(details["recovery"], "still here");
    });
}
