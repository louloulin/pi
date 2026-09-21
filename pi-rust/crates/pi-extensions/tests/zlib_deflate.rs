//! `node:zlib` gzip/deflate interop tests — LUM-1131.
//!
//! The upstream repository needs the gzip/deflate family, not just zstd:
//! `tool-result-images.test.ts` builds PNG `IDAT` chunks with
//! `deflateSync` + `crc32`, and `doom-overlay/wad-finder.ts` inflates a
//! downloaded WAD with `gunzipSync`. The workspace bundles `zstd` but
//! neither `flate2` nor `miniz_oxide`, so `crates/pi-extensions/src/deflate.rs`
//! implements RFC 1951/1950/1952 directly.
//!
//! These tests drive the real host ([`JsExtensionHost::execute_tool`]) like
//! `tests/zlib.rs`, and lean on **fixtures produced by Python's zlib**
//! (zlib 1.3, the library Node vendors) rather than only on self-consistent
//! Rust↔Rust loops. The fixtures cover all three DEFLATE block types
//! (stored / fixed Huffman / dynamic Huffman), a multi-block stream with
//! `Z_SYNC_FLUSH` boundaries, and gzip headers with optional fields. In the
//! other direction the encoder's output is locked to golden bytes that were
//! verified with `zlib.decompress` / `gzip.decompress` while this round was
//! authored — for `TEXT` it is byte-identical to Python's
//! `zlib.compress(TEXT, 6)`.

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

/// Decode the hex fixtures. Hand-rolled so the test needs no extra crate.
fn hex_decode(text: &str) -> Vec<u8> {
    assert!(text.len() % 2 == 0, "hex literal must have even length");
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("hex"))
        .collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    // `clippy::format_collect`: build the string with `write!` instead of
    // allocating a `String` per byte and then concatenating them.
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

// ---------------------------------------------------------------------------
// Payloads (built here so only the compressed fixtures need embedding)
// ---------------------------------------------------------------------------

const TEXT: &str = "pi-rust node:zlib gzip/deflate interop fixture — 1234567890";

fn repeat_payload() -> Vec<u8> {
    "compressible ".repeat(40).into_bytes()
}

fn big_payload() -> Vec<u8> {
    let mut payload = "The quick brown fox jumps over the lazy dog. "
        .repeat(40)
        .into_bytes();
    payload.extend_from_slice(b"tail");
    payload
}

fn ab_payload() -> Vec<u8> {
    "ab".repeat(2000).into_bytes()
}

fn lorem_payload() -> Vec<u8> {
    "lorem ipsum dolor sit amet consectetur adipiscing elit "
        .repeat(30)
        .into_bytes()
}

fn multi_payload() -> Vec<u8> {
    let mut payload = "first chunk of text ".repeat(20).into_bytes();
    payload.extend_from_slice(&"second chunk of text ".repeat(20).into_bytes());
    payload.extend_from_slice(&"third chunk of text ".repeat(20).into_bytes());
    payload
}

// ---------------------------------------------------------------------------
// zlib fixtures (Python `zlib.compress` / `compressobj`)
// ---------------------------------------------------------------------------

/// `zlib.compress(TEXT, 6)` — a single **fixed-Huffman** block.
const TEXT_DEFAULT_HEX: &str = "789c2bc8d42d2a2d2e51c8cb4f49b5aacac94c5248afca2cd04f494dcb492c4955c8cc2b492dca2f5048cbac28292d4a5578d43045c1d0c8d8c4d4ccdcc2d20000ca5415ef";
/// `zlib.compress(TEXT, 0)` — a **stored** block (`BTYPE=00`).
const TEXT_STORED_HEX: &str = "7801013d00c2ff70692d72757374206e6f64653a7a6c696220677a69702f6465666c61746520696e7465726f70206669787475726520e280942031323334353637383930ca5415ef";
/// `compressobj(6, DEFLATED, 15, 9, Z_FIXED)` — fixed Huffman, forced.
const TEXT_FIXED_HEX: &str = "78012bc8d42d2a2d2e51c8cb4f49b5aacac94c5248afca2cd04f494dcb492c4955c8cc2b492dca2f5048cbac28292d4a5578d43045c1d0c8d8c4d4ccdcc2d20000ca5415ef";
/// `zlib.compress("compressible " * 40, 6)`.
const REPEAT_ZLIB_HEX: &str = "789c4bcecf2d284a2d2ece4cca4955481ee58c540e003ce6ce41";
/// `zlib.compress(BIG, 6)`.
const BIG_ZLIB_HEX: &str = "789c0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb29848c2a1e553caa7854f1a8e251c5c34b714962660e00a53587e1";
/// `zlib.compress("", 6)` — an empty fixed-Huffman block.
const EMPTY_ZLIB_HEX: &str = "789c030000000001";
/// `zlib.compress("ab" * 2000, 6)` — a single **dynamic-Huffman** block.
const AB_ZLIB_HEX: &str = "789cedc2310d00000002a0acda3f84317c18a401000000ce069a1df3bc";
/// `zlib.compress(LOREM, 6)` — dynamic Huffman with real code-length runs.
const LOREM_ZLIB_HEX: &str = "789cedcbc10dc0300804b0556eb588a00a094214c8feed0afddfdff63c1ab05d3730d3f3a0ac31421b92ab545afb1e8c69db4a6c3d50ff80b3b1b1b1b1b1fd6b2fea7671e7";
/// Three `Z_SYNC_FLUSH`-separated blocks — the decoder must walk several
/// blocks and skip the empty stored blocks a flush emits.
const MULTI_ZLIB_HEX: &str = "789c4acb2c2a2e5148ce28cdcb56c84f532849ad2851481b15a3480c000000ffffa244ac3835393f2f6554105d10000000ffff1b4c8225199945a36283470c003c02bef0";
/// The same three-chunk stream as a **raw** DEFLATE stream (`wbits=-15`).
const MULTI_RAW_HEX: &str = "4acb2c2a2e5148ce28cdcb56c84f532849ad2851481b15a3480c000000ffffa244ac3835393f2f6554105d10000000ffff1b4c8225199945a36283470c00";
/// Raw DEFLATE of `TEXT` (fixed Huffman).
const TEXT_RAW_HEX: &str = "2bc8d42d2a2d2e51c8cb4f49b5aacac94c5248afca2cd04f494dcb492c4955c8cc2b492dca2f5048cbac28292d4a5578d43045c1d0c8d8c4d4ccdcc2d20000";
/// Raw DEFLATE of `TEXT` as a stored block.
const TEXT_RAW_STORED_HEX: &str = "013d00c2ff70692d72757374206e6f64653a7a6c696220677a69702f6465666c61746520696e7465726f70206669787475726520e280942031323334353637383930";
/// Raw DEFLATE of `"ab" * 2000` (dynamic Huffman).
const AB_RAW_HEX: &str = "edc2310d00000002a0acda3f84317c18a401000000ce06";

// ---------------------------------------------------------------------------
// gzip fixtures (Python `gzip.compress` / `GzipFile` / hand-built headers)
// ---------------------------------------------------------------------------

const TEXT_GZIP_HEX: &str = "1f8b08006fd4ae6a00ff2bc8d42d2a2d2e51c8cb4f49b5aacac94c5248afca2cd04f494dcb492c4955c8cc2b492dca2f5048cbac28292d4a5578d43045c1d0c8d8c4d4ccdcc2d20000fa7ed8023d000000";
const AB_GZIP_HEX: &str =
    "1f8b08006fd4ae6a00ffedc2310d00000002a0acda3f84317c18a401000000ce060fcdb0f5a00f0000";
const EMPTY_GZIP_HEX: &str = "1f8b08006fd4ae6a00ff03000000000000000000";
/// `gzip.GzipFile(filename="pi-fixture.txt")` — sets `FNAME`.
const GZIP_NAMED_HEX: &str = "1f8b08080000000002ff70692d666978747572652e747874002bc8d42d2a2d2e51c8cb4f49b5aacac94c5248afca2cd04f494dcb492c4955c8cc2b492dca2f5048cbac28292d4a5578d43045c1d0c8d8c4d4ccdcc2d20000fa7ed8023d000000";
/// Hand-built header with `FEXTRA | FNAME | FCOMMENT | FHCRC` set; the
/// header CRC checks out under `zlib.decompress(..., 16 + 15)`.
const GZIP_FULL_HEX: &str = "1f8b081e0000000000ff04007069000370692d666978747572652e747874007069207275737420666978747572650027632bc8d42d2a2d2e51c8cb4f49b5aacac94c5248afca2cd04f494dcb492c4955c8cc2b492dca2f5048cbac28292d4a5578d43045c1d0c8d8c4d4ccdcc2d20000fa7ed8023d000000";

// ---------------------------------------------------------------------------
// Encoder goldens (this encoder's bytes, verified with Python zlib/gzip)
// ---------------------------------------------------------------------------

/// Our encoder at the default level. Byte-identical to [`TEXT_DEFAULT_HEX`],
/// i.e. to Python's `zlib.compress(TEXT, 6)` — it picks the same matches.
const GOLDEN_TEXT_DEFAULT_HEX: &str = TEXT_DEFAULT_HEX;
/// Our encoder at `{ level: 0 }`. Byte-identical to [`TEXT_STORED_HEX`].
const GOLDEN_TEXT_STORED_HEX: &str = TEXT_STORED_HEX;
/// `gzipSync(TEXT)` — same body, reproducible header (MTIME 0, OS unknown).
const GOLDEN_TEXT_GZIP_HEX: &str = "1f8b08000000000000ff2bc8d42d2a2d2e51c8cb4f49b5aacac94c5248afca2cd04f494dcb492c4955c8cc2b492dca2f5048cbac28292d4a5578d43045c1d0c8d8c4d4ccdcc2d20000fa7ed8023d000000";
/// 25 bytes vs zlib's 26 for the same payload — greedy LZ77 with a fixed
/// Huffman table can beat zlib's level 6 on this input.
const GOLDEN_REPEAT_DEFAULT_HEX: &str = "789c4bcecf2d284a2d2ece4cca495518e58c580e003ce6ce41";
/// 71 bytes vs zlib's 72.
const GOLDEN_BIG_DEFAULT_HEX: &str = "789c0bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a7eb298c2a1e553caa7854f1a8e251c5c34c714962660e00a53587e1";

/// Decodes a batch of `{ kind, hex }` fixtures; `kind` picks the entry point.
const DECODE_PROBE: &str = r#"
    import { inflateSync, inflateRawSync, gunzipSync } from "node:zlib";
    import { Buffer } from "node:buffer";

    const decoders = { zlib: inflateSync, gzip: gunzipSync, raw: inflateRawSync };

    export default function (pi) {
        pi.registerTool({
            name: "deflate_decode",
            label: "deflate decode",
            description: "decodes zlib/gzip/raw DEFLATE fixtures",
            parameters: { type: "object", properties: { cases: { type: "array" } } },
            execute: (args) => {
                const results = args.cases.map((entry) => {
                    const decoded = decoders[entry.kind](Buffer.from(entry.hex, "hex"));
                    return { hex: decoded.toString("hex"), isBuffer: Buffer.isBuffer(decoded) };
                });
                return { content: [{ type: "text", text: "ok" }], details: { results: results } };
            },
        });
    }
"#;

async fn decode_cases(cases: &[(&str, &str)]) -> Vec<Value> {
    let host = host().await;
    host.load(
        entry_at("deflate_decode", "/tmp/pi_deflate/decode.mjs"),
        DECODE_PROBE,
    )
    .await
    .expect("load decode probe");

    let cases: Vec<Value> = cases
        .iter()
        .map(|(kind, hex)| json!({ "kind": kind, "hex": hex }))
        .collect();
    let outcome = host
        .execute_tool("deflate_decode", &json!({ "cases": cases }).to_string())
        .await
        .expect("execute decode probe");
    assert!(!outcome.is_error, "{outcome:?}");
    let details = outcome.details.expect("details");
    details["results"]
        .as_array()
        .expect("results array")
        .clone()
}

/// Checks a batch of fixtures against their expected payloads.
async fn assert_decodes(cases: &[(&str, &str, Vec<u8>)]) {
    let hexes: Vec<(&str, &str)> = cases.iter().map(|(kind, hex, _)| (*kind, *hex)).collect();
    let results = decode_cases(&hexes).await;
    assert_eq!(results.len(), cases.len());

    for (result, (kind, hex, payload)) in results.iter().zip(cases) {
        assert_eq!(result["isBuffer"], true, "{kind} fixture {hex}");
        assert_eq!(
            result["hex"].as_str().expect("hex"),
            hex_encode(payload),
            "{kind} fixture {hex} decoded to the wrong bytes"
        );
    }
}

/// Every zlib fixture decodes to its original payload through the shim:
/// stored, fixed Huffman, dynamic Huffman, empty, and a multi-block stream.
#[test]
fn zlib_deflate_decodes_python_generated_fixtures() {
    let runtime = rt();
    runtime.block_on(async {
        let text = TEXT.as_bytes().to_vec();
        assert_decodes(&[
            ("zlib", TEXT_DEFAULT_HEX, text.clone()),  // fixed Huffman
            ("zlib", TEXT_STORED_HEX, text.clone()),   // stored
            ("zlib", TEXT_FIXED_HEX, text.clone()),    // fixed, forced
            ("zlib", AB_ZLIB_HEX, ab_payload()),       // dynamic Huffman
            ("zlib", LOREM_ZLIB_HEX, lorem_payload()), // dynamic, longer
            ("zlib", MULTI_ZLIB_HEX, multi_payload()), // multi-block
            ("zlib", REPEAT_ZLIB_HEX, repeat_payload()),
            ("zlib", BIG_ZLIB_HEX, big_payload()),
            ("zlib", EMPTY_ZLIB_HEX, Vec::new()), // empty stream
        ])
        .await;
    });
}

/// Raw DEFLATE fixtures decode through `inflateRawSync`.
#[test]
fn zlib_deflate_decodes_raw_fixtures() {
    let runtime = rt();
    runtime.block_on(async {
        let text = TEXT.as_bytes().to_vec();
        assert_decodes(&[
            ("raw", MULTI_RAW_HEX, multi_payload()),
            ("raw", TEXT_RAW_HEX, text.clone()),
            ("raw", TEXT_RAW_STORED_HEX, text.clone()),
            ("raw", AB_RAW_HEX, ab_payload()),
        ])
        .await;
    });
}

/// `gunzipSync` handles Python's gzip output and the optional header fields
/// (`FNAME`, `FEXTRA`, `FCOMMENT`, `FHCRC`) that real `.gz` files carry.
#[test]
fn zlib_deflate_decodes_gzip_fixtures_with_optional_headers() {
    let runtime = rt();
    runtime.block_on(async {
        let text = TEXT.as_bytes().to_vec();
        assert_decodes(&[
            ("gzip", TEXT_GZIP_HEX, text.clone()),
            ("gzip", AB_GZIP_HEX, ab_payload()),
            ("gzip", EMPTY_GZIP_HEX, Vec::new()),
            ("gzip", GZIP_NAMED_HEX, text.clone()),
            ("gzip", GZIP_FULL_HEX, text.clone()),
        ])
        .await;
    });
}

/// Encoder output is locked to bytes that were round-tripped through
/// Python's `zlib.decompress` / `gzip.decompress` while this round was
/// authored. Two of them are additionally asserted **byte-identical** to
/// Python's own compressor, which is the strongest cross-check available
/// without linking a DEFLATE implementation into the test binary.
#[test]
fn zlib_deflate_encoder_matches_python_golden_bytes() {
    let runtime = rt();
    runtime.block_on(async {
        let host = host().await;
        host.load(
            entry_at("deflate_encode", "/tmp/pi_deflate/encode.mjs"),
            ENCODE_PROBE,
        )
        .await
        .expect("load encode probe");

        let text = encode_payload(&host, TEXT.as_bytes()).await;
        assert_eq!(text["minusOne"], GOLDEN_TEXT_DEFAULT_HEX);
        assert_eq!(text["gzip"], GOLDEN_TEXT_GZIP_HEX);
        // Levels 1 and 9 only differ in the zlib FLEVEL byte for these
        // payloads, so the golden body is the default body.
        assert_eq!(
            text["byLevel"]["1"],
            GOLDEN_TEXT_DEFAULT_HEX.replacen("789c", "7801", 1)
        );
        assert_eq!(
            text["byLevel"]["9"],
            GOLDEN_TEXT_DEFAULT_HEX.replacen("789c", "78da", 1)
        );

        let repeat = encode_payload(&host, &repeat_payload()).await;
        let big = encode_payload(&host, &big_payload()).await;

        for (label, encoded, golden) in [
            ("text", &text, GOLDEN_TEXT_DEFAULT_HEX),
            ("repeat", &repeat, GOLDEN_REPEAT_DEFAULT_HEX),
            ("big", &big, GOLDEN_BIG_DEFAULT_HEX),
        ] {
            let actual = encoded["default"].as_str().expect("default hex");
            // `text`'s default output is byte-identical to Python's
            // `zlib.compress(TEXT, 6)` (`TEXT_DEFAULT_HEX`).
            assert_eq!(actual, golden, "{label} default-level golden");
            assert_eq!(hex_decode(actual)[2] & 0x06, 0x02, "{label} fixed block");
        }

        // Also assert the two byte-identical cases against Python's own
        // output, not just against our recorded golden.
        assert_eq!(text["byLevel"]["0"], GOLDEN_TEXT_STORED_HEX);
        assert_eq!(GOLDEN_TEXT_STORED_HEX, TEXT_STORED_HEX);
        assert_eq!(text["default"], TEXT_DEFAULT_HEX);

        // Level 0 must emit stored blocks (BTYPE=00).
        assert_eq!(
            hex_decode(text["byLevel"]["0"].as_str().expect("level 0"))[2] & 0x06,
            0,
            "level 0 should use a stored block"
        );
    });
}

/// Encode `payload` at every supported level through the real host.
async fn encode_payload(host: &JsExtensionHost, payload: &[u8]) -> Value {
    let outcome = host
        .execute_tool(
            "deflate_encode",
            &json!({ "hex": hex_encode(payload), "levels": [0, 1, 6, 9] }).to_string(),
        )
        .await
        .expect("execute encode probe");
    assert!(!outcome.is_error, "{outcome:?}");
    outcome.details.expect("details")
}

/// Round-trips through the shim, including the PNG-`IDAT`-shaped workload
/// `tool-result-images.test.ts` performs, and the input shapes Node accepts.
#[test]
fn zlib_deflate_round_trips_through_the_shim() {
    let runtime = rt();
    runtime.block_on(async {
        let host = host().await;
        host.load(
            entry_at("deflate_round_trip", "/tmp/pi_deflate/round_trip.mjs"),
            ROUND_TRIP_PROBE,
        )
        .await
        .expect("load round-trip probe");

        let outcome = host
            .execute_tool("deflate_round_trip", "{}")
            .await
            .expect("execute round-trip probe");
        assert!(!outcome.is_error, "{outcome:?}");

        let details = outcome.details.expect("details");
        // Every level, plus gzip/raw, restores the payload byte for byte.
        for level in ["0", "1", "6", "9"] {
            assert_eq!(details["matches"][level], true, "zlib level {level}");
        }
        assert_eq!(details["gzipMatches"], true);
        assert_eq!(details["rawMatches"], true);
        assert_eq!(details["minusOneMatches"], true);
        // `Uint8Array` / `ArrayBuffer` inputs are accepted like Node's.
        assert_eq!(details["uint8Matches"], true);
        assert_eq!(details["arrayBufferMatches"], true);
        // The PNG-`IDAT`-shaped payload really does shrink, and the empty
        // payload round-trips too.
        assert!(
            details["idatCompressedLength"].as_u64().expect("length")
                < details["idatLength"].as_u64().expect("length") / 2,
            "expected IDAT-shaped data to compress, got {details:?}"
        );
        assert_eq!(details["emptyMatches"], true);
        assert!(details["gzipIsBuffer"].as_bool().expect("bool"));
        assert!(details["crc32OfDeflatedIdat"].as_u64().expect("crc") > 0);
    });
}

/// Failures surface as Node-shaped `Error`s with a `code`, and the host keeps
/// working afterwards.
#[test]
fn zlib_deflate_failures_are_node_shaped_and_host_survives() {
    let runtime = rt();
    runtime.block_on(async {
        // Corrupt the gzip CRC-32 (last 8 bytes are CRC+ISIZE) of a real
        // fixture, and hand the shim a header that demands a preset
        // dictionary (FDICT), which the codec rejects explicitly.
        let mut corrupt = hex_decode(TEXT_GZIP_HEX);
        let crc = corrupt.len() - 8;
        corrupt[crc] ^= 0xFF;

        let host = host().await;
        host.load(
            entry_at("deflate_failures", "/tmp/pi_deflate/failures.mjs"),
            FAILURE_PROBE,
        )
        .await
        .expect("load failure probe");

        let outcome = host
            .execute_tool(
                "deflate_failures",
                &json!({
                    "corruptGzip": hex_encode(&corrupt),
                    "truncatedZlib": "789c",
                    "presetDictHeader": "782000000000",
                })
                .to_string(),
            )
            .await
            .expect("execute failure probe");
        assert!(!outcome.is_error, "{outcome:?}");

        let details = outcome.details.expect("details");
        let failures = details["failures"].clone();
        for (name, code) in [
            ("badCrc", "Z_DATA_ERROR"),
            ("truncatedZlib", "Z_BUF_ERROR"),
            ("badMagic", "Z_DATA_ERROR"),
            ("presetDict", "Z_STREAM_ERROR"),
        ] {
            let failure = &failures[name];
            assert_eq!(failure["isError"], true, "{name}: {failure:?}");
            assert_eq!(failure["code"], code, "{name}: {failure:?}");
            assert!(
                failure["message"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty()),
                "{name}: {failure:?}"
            );
        }
        // Node throws a `RangeError` with `ERR_OUT_OF_RANGE` for bad levels.
        for name in ["badLevel", "badLevelNegative"] {
            let failure = &failures[name];
            assert_eq!(failure["isError"], true, "{name}: {failure:?}");
            assert_eq!(failure["name"], "RangeError", "{name}: {failure:?}");
            assert_eq!(failure["code"], "ERR_OUT_OF_RANGE", "{name}: {failure:?}");
        }
        assert_eq!(
            details["recovery"], "still here",
            "the host must survive a thrown zlib error"
        );
    });
}

/// Encodes payloads at several levels and reports the round-trip results.
const ENCODE_PROBE: &str = r#"
    import { deflateSync, inflateSync, deflateRawSync, inflateRawSync, gzipSync, gunzipSync } from "node:zlib";
    import { Buffer } from "node:buffer";

    export default function (pi) {
        pi.registerTool({
            name: "deflate_encode",
            label: "deflate encode",
            description: "encodes a payload with the gzip/deflate family",
            parameters: {
                type: "object",
                properties: { hex: { type: "string" }, levels: { type: "array" } },
            },
            execute: (args) => {
                const payload = Buffer.from(args.hex, "hex");
                const byLevel = {};
                for (const level of args.levels) {
                    byLevel[String(level)] = deflateSync(payload, { level: level }).toString("hex");
                }
                return {
                    content: [{ type: "text", text: "ok" }],
                    details: {
                        default: deflateSync(payload).toString("hex"),
                        minusOne: deflateSync(payload, { level: -1 }).toString("hex"),
                        byLevel: byLevel,
                        gzip: gzipSync(payload).toString("hex"),
                    },
                };
            },
        });
    }
"#;

/// The round-trip and input-shape checks, all driven from JS.
const ROUND_TRIP_PROBE: &str = r#"
    import { deflateSync, inflateSync, deflateRawSync, inflateRawSync, gzipSync, gunzipSync, crc32 } from "node:zlib";
    import { Buffer } from "node:buffer";

    // A PNG IDAT-shaped payload: a filter byte per scanline plus RGBA rows,
    // the same shape `tool-result-images.test.ts` compresses. The image is
    // built from 16x16 flat blocks so it compresses like a real screenshot.
    function idatPayload() {
        const width = 64;
        const height = 64;
        const bytes = [];
        for (let y = 0; y < height; y++) {
            bytes.push(y % 2 === 0 ? 0 : 1);
            for (let x = 0; x < width; x++) {
                const block = (Math.floor(x / 16) * 16 + Math.floor(y / 16)) % 256;
                bytes.push(block, (block + 32) % 256, (block + 64) % 256, 255);
            }
        }
        return Buffer.from(bytes);
    }

    export default function (pi) {
        pi.registerTool({
            name: "deflate_round_trip",
            label: "deflate round trip",
            description: "round-trips zlib/gzip/raw payloads",
            parameters: { type: "object", properties: {} },
            execute: () => {
                const idat = idatPayload();
                const text = "pi-rust round trip — 世界 " + "compressible ".repeat(200);
                const payload = Buffer.from(text, "utf8");

                const matches = {};
                for (const level of [0, 1, 6, 9]) {
                    const compressed = deflateSync(payload, { level: level });
                    matches[String(level)] =
                        inflateSync(compressed).toString("utf8") === text;
                }

                const small = Buffer.from([1, 2, 3, 4, 5]);
                // `Buffer.from(array)` may be a view into the shared pool, so
                // take the ArrayBuffer from a freshly allocated typed array.
                const exact = new Uint8Array([9, 8, 7, 6, 5]);
                const idatCompressed = deflateSync(idat, { level: 6 });

                return {
                    content: [{ type: "text", text: "ok" }],
                    details: {
                        matches: matches,
                        minusOneMatches:
                            inflateSync(deflateSync(payload, { level: -1 })).toString("utf8") === text,
                        gzipMatches: gunzipSync(gzipSync(payload)).toString("utf8") === text,
                        rawMatches:
                            inflateRawSync(deflateRawSync(payload)).toString("utf8") === text,
                        uint8Matches:
                            inflateSync(deflateSync(new Uint8Array(small))).equals(small),
                        arrayBufferMatches:
                            inflateSync(deflateSync(exact.buffer)).equals(Buffer.from(exact)),
                        emptyMatches: inflateSync(deflateSync(Buffer.alloc(0))).length === 0,
                        gzipIsBuffer: Buffer.isBuffer(gzipSync(payload)),
                        idatLength: idat.length,
                        idatCompressedLength: idatCompressed.length,
                        crc32OfDeflatedIdat: crc32(idatCompressed),
                    },
                };
            },
        });
    }
"#;

/// Triggers each failure mode and reports the thrown error's shape.
const FAILURE_PROBE: &str = r#"
    import { deflateSync, inflateSync, gunzipSync } from "node:zlib";
    import { Buffer } from "node:buffer";

    export default function (pi) {
        pi.registerTool({
            name: "deflate_failures",
            label: "deflate failures",
            description: "triggers gzip/deflate decode failures",
            parameters: { type: "object", properties: {} },
            execute: (args) => {
                const capture = (fn) => {
                    try {
                        fn();
                        return null;
                    } catch (err) {
                        return {
                            isError: err instanceof Error,
                            name: err.name,
                            code: typeof err.code === "string" ? err.code : null,
                            message: err.message,
                        };
                    }
                };
                const failures = {
                    badCrc: capture(() => gunzipSync(Buffer.from(args.corruptGzip, "hex"))),
                    truncatedZlib: capture(() => inflateSync(Buffer.from(args.truncatedZlib, "hex"))),
                    badMagic: capture(() => gunzipSync(Buffer.from("this is not a gzip stream at all", "utf8"))),
                    presetDict: capture(() => inflateSync(Buffer.from(args.presetDictHeader, "hex"))),
                    badLevel: capture(() => deflateSync("data", { level: 10 })),
                    badLevelNegative: capture(() => deflateSync("data", { level: -2 })),
                };
                const recovery = inflateSync(deflateSync("still here")).toString("utf8");
                return {
                    content: [{ type: "text", text: "ok" }],
                    details: { failures: failures, recovery: recovery },
                };
            },
        });
    }
"#;
