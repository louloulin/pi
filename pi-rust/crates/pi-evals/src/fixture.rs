//! Loopback HTTP fixture server for offline provider evals.
//!
//! The upstream `providers.eval.ts` starts a Node `http` server that speaks
//! the OpenAI Chat Completions SSE protocol, points the provider at it, and
//! asserts both the wire request and the parsed response. This module is the
//! Rust equivalent: a single-threaded HTTP/1.1 server over `std::net` (no new
//! dependency) that serves whatever the case's handler returns and records
//! every request for assertions.
//!
//! The server closes the connection after each response, which keeps the
//! handler simple and lets `reqwest` finish a request without keep-alive
//! bookkeeping.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// A request the fixture server received.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedRequest {
    /// HTTP method (`POST`, `GET`, …).
    pub method: String,
    /// Request path including any query string.
    pub path: String,
    /// Request headers, keys lower-cased.
    pub headers: Vec<(String, String)>,
    /// Request body decoded as UTF-8 (lossy).
    pub body: String,
}

impl RecordedRequest {
    /// Value of a header, case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Parse the body as JSON.
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_str(&self.body).ok()
    }
}

/// Response the handler asks the server to return.
#[derive(Debug, Clone)]
pub struct FixtureResponse {
    /// HTTP status code.
    pub status: u16,
    /// `Content-Type` header value.
    pub content_type: String,
    /// Response body.
    pub body: String,
}

impl FixtureResponse {
    /// A `200 OK` Server-Sent Events response.
    pub fn sse(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream".into(),
            body: body.into(),
        }
    }

    /// A `200 OK` JSON response.
    pub fn json(value: serde_json::Value) -> Self {
        Self {
            status: 200,
            content_type: "application/json".into(),
            body: value.to_string(),
        }
    }

    /// A plain-text response with an explicit status.
    pub fn error(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: "text/plain".into(),
            body: body.into(),
        }
    }
}

/// A loopback fixture server.
///
/// Dropping the server sets its stop flag and unblocks the accept loop, then
/// joins the worker thread.
pub struct FixtureServer {
    origin: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl FixtureServer {
    /// Bind `127.0.0.1:0` and serve requests with `handler` on a worker
    /// thread.
    pub fn start(
        handler: impl Fn(&RecordedRequest) -> FixtureResponse + Send + 'static,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let worker_requests = Arc::clone(&requests);
        let worker_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            for incoming in listener.incoming() {
                if worker_stop.load(Ordering::SeqCst) {
                    break;
                }
                match incoming {
                    Ok(mut stream) => {
                        if let Some(request) = read_request(&mut stream) {
                            worker_requests
                                .lock()
                                .expect("fixture request log poisoned")
                                .push(request.clone());
                            let response = handler(&request);
                            let _ = write_response(&mut stream, &response);
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            origin: format!("http://{address}"),
            requests,
            stop,
            handle: Some(handle),
        })
    }

    /// Server origin, e.g. `http://127.0.0.1:53211` (no trailing slash).
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// OpenAI-compatible base URL (`{origin}/v1`).
    pub fn base_url(&self) -> String {
        format!("{}/v1", self.origin)
    }

    /// Every request received so far, in arrival order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests
            .lock()
            .expect("fixture request log poisoned")
            .clone()
    }

    /// Number of requests received so far.
    pub fn request_count(&self) -> usize {
        self.requests
            .lock()
            .expect("fixture request log poisoned")
            .len()
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking `accept` with a throwaway connection.
        if let Ok(address) = self.origin.trim_start_matches("http://").parse::<SocketAddr>() {
            let _ = TcpStream::connect_timeout(&address, Duration::from_millis(200));
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<RecordedRequest> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .ok()?;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 2048];
    let header_end = loop {
        if let Some(position) = find_subsequence(&buffer, b"\r\n\r\n") {
            break position + 4;
        }
        if buffer.len() > 128 * 1024 {
            return None;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return None,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(_) => return None,
        }
    };

    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    if method.is_empty() {
        return None;
    }
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.push((key.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    let content_length = headers
        .iter()
        .find(|(key, _)| key == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buffer[header_end..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => body.extend_from_slice(&chunk[..read]),
            Err(_) => break,
        }
    }
    body.truncate(content_length);

    Some(RecordedRequest {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}

fn write_response(stream: &mut TcpStream, response: &FixtureResponse) -> std::io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        415 => "Unsupported Media Type",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        response.status,
        reason,
        response.content_type,
        response.body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(response.body.as_bytes())?;
    stream.flush()
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Build an SSE Chat Completions body carrying one text answer.
///
/// The final chunk carries `usage` and `finish_reason: "stop"`, mirroring
/// what OpenAI-compatible endpoints emit with `stream_options.include_usage`.
pub fn sse_text(model: &str, text: &str, prompt_tokens: u32, completion_tokens: u32) -> String {
    let first = serde_json::json!({
        "id": "chatcmpl-fixture",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "content": text },
            "finish_reason": null
        }]
    });
    sse_from_chunks(model, first, "stop", prompt_tokens, completion_tokens)
}

/// Build an SSE Chat Completions body carrying one tool call.
pub fn sse_tool_call(
    model: &str,
    name: &str,
    arguments: &str,
    prompt_tokens: u32,
    completion_tokens: u32,
) -> String {
    let first = serde_json::json!({
        "id": "chatcmpl-fixture",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": "call_fixture_1",
                    "type": "function",
                    "function": { "name": name, "arguments": arguments }
                }]
            },
            "finish_reason": null
        }]
    });
    sse_from_chunks(model, first, "tool_calls", prompt_tokens, completion_tokens)
}

fn sse_from_chunks(
    model: &str,
    delta: serde_json::Value,
    finish_reason: &str,
    prompt_tokens: u32,
    completion_tokens: u32,
) -> String {
    let last = serde_json::json!({
        "id": "chatcmpl-fixture",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": model,
        "choices": [{ "index": 0, "delta": {}, "finish_reason": finish_reason }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens
        }
    });
    format!("data: {delta}\n\ndata: {last}\n\ndata: [DONE]\n\n")
}
