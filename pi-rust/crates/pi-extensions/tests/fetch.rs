//! `fetch` global tests — the HTTP bridge behind the extension shim.
//!
//! Upstream extensions run in Node/Bun and therefore get the platform `fetch`;
//! the repo's own `.pi/extensions/import-repro.ts` uses it to read GitHub
//! gists and issue comments. These tests drive the whole path — shim
//! `fetch`/`Headers`/`Response` → `host_fetch` → `reqwest` → a real loopback
//! HTTP server — through the real host, exactly like `pi_exec.rs` does for
//! `pi.exec`.
//!
//! The server is a hand-rolled HTTP/1.1 responder on a `TcpListener`, so the
//! test suite gains no dependency and binds only to `127.0.0.1:<ephemeral>`.
//!
//! The in-flight abort test drives a real delay with `pi.exec("sleep", …)`, so
//! the file is Unix-only like `pi_exec.rs`; the bridge itself is portable.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use pi_extensions::{ExtensionEntry, JsExtensionHost};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime")
}

async fn load(host: &JsExtensionHost, source: &str) {
    host.load(
        ExtensionEntry {
            source: PathBuf::from("/tmp/pi_extensions_fetch/probe.js"),
            id: "fetch_probe".to_string(),
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

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn content_length(headers: &str) -> usize {
    for line in headers.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            return value.trim().parse().unwrap_or(0);
        }
    }
    0
}

/// Read one request (headers + `Content-Length` bytes) off the socket.
async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        let read = socket.read(&mut chunk).await.unwrap_or(0);
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
        if let Some(end) = find_subsequence(&buf, b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&buf).into_owned();
            let want = end + 4 + content_length(&headers);
            while buf.len() < want {
                let read = socket.read(&mut chunk).await.unwrap_or(0);
                if read == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..read]);
            }
            break;
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn http_response(status: u16, reason: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// A loopback server that routes by path:
///
/// * `/json` → `200 application/json` with `{"hello":"world"}`
/// * `/missing` → `404 application/json` with `{"error":"not found"}`
/// * `/echo` → `200 text/plain`, body = request body
/// * `/slow` → `200 text/plain` after a 2 s delay (for the abort test)
///
/// Returns the base URL.
async fn spawn_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback server");
    let addr = listener.local_addr().expect("local addr");
    let listener = Arc::new(listener);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let request = read_request(&mut socket).await;
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();
                let body = request
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body.to_string())
                    .unwrap_or_default();
                let response = match path.as_str() {
                    "/missing" => http_response(404, "Not Found", "application/json", r#"{"error":"not found"}"#),
                    "/echo" => http_response(200, "OK", "text/plain; charset=utf-8", &body),
                    "/slow" => {
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        http_response(200, "OK", "text/plain", "late")
                    }
                    _ => http_response(200, "OK", "application/json", r#"{"hello":"world"}"#),
                };
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

/// A plain GET: status / ok / statusText / response header / text body.
#[test]
fn fetch_get_reads_status_headers_and_text() {
    let runtime = rt();
    runtime.block_on(async {
        let base = spawn_server().await;
        let host = JsExtensionHost::new().await.expect("host");
        load(
            &host,
            &format!(
                r#"
                module.exports = function (pi) {{
                    pi.registerTool({{
                        name: "fetch_probe",
                        label: "fetch probe",
                        description: "runs fetch probes",
                        parameters: {{ type: "object", properties: {{}} }},
                        execute: async () => {{
                            const res = await fetch("{base}/json", {{
                                headers: {{ Accept: "application/json" }},
                            }});
                            const text = await res.text();
                            return {{
                                content: [{{ type: "text", text: text }}],
                                details: {{
                                    status: res.status,
                                    ok: res.ok,
                                    statusText: res.statusText,
                                    contentType: res.headers.get("Content-Type"),
                                    text: text,
                                    bodyUsed: res.bodyUsed,
                                }},
                            }};
                        }},
                    }});
                }};
                "#
            ),
        )
        .await;

        let details = run_probe(&host, "fetch_probe").await;
        assert_eq!(details["status"], json!(200));
        assert_eq!(details["ok"], json!(true));
        assert_eq!(details["statusText"], json!("OK"));
        assert_eq!(details["contentType"], json!("application/json"));
        assert_eq!(details["text"], json!(r#"{"hello":"world"}"#));
        assert_eq!(details["bodyUsed"], json!(true));
    });
}

/// `json()` parses the body, and a 4xx response resolves (never rejects) with
/// `ok: false` — the upstream `fetch` contract, not an exception.
#[test]
fn fetch_json_and_error_status() {
    let runtime = rt();
    runtime.block_on(async {
        let base = spawn_server().await;
        let host = JsExtensionHost::new().await.expect("host");
        load(
            &host,
            &format!(
                r#"
                module.exports = function (pi) {{
                    pi.registerTool({{
                        name: "fetch_probe",
                        label: "fetch probe",
                        description: "runs fetch probes",
                        parameters: {{ type: "object", properties: {{}} }},
                        execute: async () => {{
                            const ok = await fetch("{base}/json");
                            const missing = await fetch("{base}/missing");
                            return {{
                                content: [{{ type: "text", text: "ok" }}],
                                details: {{
                                    hello: (await ok.json()).hello,
                                    status: missing.status,
                                    ok: missing.ok,
                                    error: (await missing.json()).error,
                                }},
                            }};
                        }},
                    }});
                }};
                "#
            ),
        )
        .await;

        let details = run_probe(&host, "fetch_probe").await;
        assert_eq!(details["hello"], json!("world"));
        assert_eq!(details["status"], json!(404));
        assert_eq!(details["ok"], json!(false));
        assert_eq!(details["error"], json!("not found"));
    });
}

/// A POST body round-trips byte-for-byte (UTF-8, sent to the host as base64).
#[test]
fn fetch_post_body_round_trips() {
    let runtime = rt();
    runtime.block_on(async {
        let base = spawn_server().await;
        let host = JsExtensionHost::new().await.expect("host");
        load(
            &host,
            &format!(
                r#"
                module.exports = function (pi) {{
                    pi.registerTool({{
                        name: "fetch_probe",
                        label: "fetch probe",
                        description: "runs fetch probes",
                        parameters: {{ type: "object", properties: {{}} }},
                        execute: async () => {{
                            const res = await fetch("{base}/echo", {{
                                method: "post",
                                headers: {{ "Content-Type": "text/plain" }},
                                body: "héllo fetch",
                            }});
                            return {{
                                content: [{{ type: "text", text: "ok" }}],
                                details: {{ echoed: await res.text() }},
                            }};
                        }},
                    }});
                }};
                "#
            ),
        )
        .await;

        let details = run_probe(&host, "fetch_probe").await;
        assert_eq!(details["echoed"], json!("héllo fetch"));
    });
}

/// An already-aborted signal rejects before any request is sent.
#[test]
fn fetch_honours_a_pre_aborted_signal() {
    let runtime = rt();
    runtime.block_on(async {
        let base = spawn_server().await;
        let host = JsExtensionHost::new().await.expect("host");
        load(
            &host,
            &format!(
                r#"
                module.exports = function (pi) {{
                    pi.registerTool({{
                        name: "fetch_probe",
                        label: "fetch probe",
                        description: "runs fetch probes",
                        parameters: {{ type: "object", properties: {{}} }},
                        execute: async () => {{
                            const controller = new AbortController();
                            controller.abort();
                            let name = null;
                            let threw = false;
                            try {{
                                await fetch("{base}/json", {{ signal: controller.signal }});
                            }} catch (error) {{
                                threw = true;
                                name = error.name;
                            }}
                            return {{
                                content: [{{ type: "text", text: "ok" }}],
                                details: {{ threw: threw, name: name }},
                            }};
                        }},
                    }});
                }};
                "#
            ),
        )
        .await;

        let details = run_probe(&host, "fetch_probe").await;
        assert_eq!(details["threw"], json!(true));
        assert_eq!(details["name"], json!("AbortError"));
    });
}

/// Aborting mid-flight rejects with `AbortError` and cancels the request at
/// the transport layer instead of waiting for the (2 s) response. The delay is
/// a real `sleep` child, so the id is live by the time the cancel lands — the
/// `notify` path, not the pre-registration one.
#[test]
fn fetch_abort_in_flight_rejects() {
    let runtime = rt();
    runtime.block_on(async {
        let base = spawn_server().await;
        let host = JsExtensionHost::new().await.expect("host");
        load(
            &host,
            &format!(
                r#"
                module.exports = function (pi) {{
                    pi.registerTool({{
                        name: "fetch_probe",
                        label: "fetch probe",
                        description: "runs fetch probes",
                        parameters: {{ type: "object", properties: {{}} }},
                        execute: async () => {{
                            const controller = new AbortController();
                            const started = Date.now();
                            const pending = fetch("{base}/slow", {{ signal: controller.signal }});
                            await pi.exec("sleep", ["0.3"]);
                            controller.abort();
                            let name = null;
                            try {{
                                await pending;
                            }} catch (error) {{
                                name = error.name;
                            }}
                            return {{
                                content: [{{ type: "text", text: "ok" }}],
                                details: {{
                                    name: name,
                                    elapsedMs: Date.now() - started,
                                }},
                            }};
                        }},
                    }});
                }};
                "#
            ),
        )
        .await;

        let details = run_probe(&host, "fetch_probe").await;
        assert_eq!(details["name"], json!("AbortError"));
        assert!(
            details["elapsedMs"].as_f64().expect("elapsed number") < 1500.0,
            "abort should not wait for the 2 s response: {details}"
        );
    });
}
