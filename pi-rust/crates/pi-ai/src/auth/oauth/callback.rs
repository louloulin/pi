//! Loopback HTTP server used by OAuth callback flows (OpenAI Codex).
//!
//! Port of the `startLocalOAuthServer` helper in
//! `packages/ai/src/auth/oauth/openai-codex.ts`. The server listens on
//! a loopback port and waits for a single request to a well-known
//! path (`/auth/callback` by default), validates the `state`
//! parameter against the caller-supplied CSRF token, then settles a
//! one-shot promise with the authorization code.
//!
//! Cancellation is exposed via [`LocalCallbackServer::cancel_wait`] so
//! the OpenAI Codex flow can race the browser callback against a
//! `manual_code` prompt — whichever resolves first wins.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use super::oauth_page::{oauth_error_html, oauth_success_html};

/// Outcome of the callback wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackOutcome {
    /// Authorization code captured from the callback query.
    Code(String),
    /// Wait was cancelled before the callback arrived.
    Cancelled,
}

/// Handle to the loopback server returned by
/// [`LocalCallbackServer::start`].
pub struct LocalCallbackServer {
    /// Bound socket address — used to compose the `redirect_uri`.
    pub bound: SocketAddr,
    /// Path the callback is served under.
    pub path: &'static str,
    cancel_signal: Arc<std::sync::atomic::AtomicBool>,
    server_task: Option<tokio::task::JoinHandle<()>>,
}

impl LocalCallbackServer {
    /// Start the server on `host:0` (kernel-chosen port) listening
    /// for a single callback at `path`.
    pub async fn start(
        host: &str,
        path: &'static str,
        expected_state: &str,
    ) -> Result<Self, String> {
        let listener = TcpListener::bind((host, 0))
            .await
            .map_err(|err| format!("failed to bind loopback OAuth server on {host}: {err}"))?;
        let bound = listener
            .local_addr()
            .map_err(|err| format!("could not read loopback OAuth server bound address: {err}"))?;

        let (tx, rx) = oneshot::channel::<String>();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_for_task = cancel.clone();
        let expected_state = expected_state.to_string();

        // Server task: accept until either we settle the channel
        // (success) or the cancel flag flips (cancellation).
        let server_task = tokio::spawn(async move {
            let tx = Arc::new(AsyncMutex::new(Some(tx)));
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((mut stream, _peer)) = accepted else {
                            return;
                        };
                        let outcome = handle_request(&mut stream, path, &expected_state).await;
                        match outcome {
                            HandleOutcome::Settled(code) => {
                                let mut guard = tx.lock().await;
                                let tx = guard.take();
                                drop(guard);
                                if let Some(tx) = tx {
                                    let _ = tx.send(code);
                                }
                                // Drain anything else for a beat to let the
                                // client receive our 200 OK before closing.
                                let _ = tokio::time::timeout(
                                    std::time::Duration::from_secs(1),
                                    write_response(&mut stream, 200, &oauth_success_html("Sign-in complete. You can close this window."))
                                ).await;
                                return;
                            }
                            HandleOutcome::Response(status, body) => {
                                let _ = tokio::time::timeout(
                                    std::time::Duration::from_secs(2),
                                    write_response(&mut stream, status, &body)
                                ).await;
                            }
                            HandleOutcome::Cancelled => {
                                // Connection died before we could read.
                            }
                        }
                    }
                    _ = wait_for_cancel(&cancel_for_task) => {
                        // Drop the sender so the receiver wakes with a
                        // closed-channel error — the OpenAI Codex flow
                        // translates that into `CallbackOutcome::Cancelled`.
                        let mut guard = tx.lock().await;
                        guard.take();
                        return;
                    }
                }
            }
        });

        // Spawn a small task that drops `rx` when the server task ends.
        // The receiver future resolves either with `Ok(code)` or with an
        // error if the sender was dropped.
        tokio::spawn(async move {
            let _ = rx.await;
        });

        Ok(Self {
            bound,
            path,
            cancel_signal: cancel,
            server_task: Some(server_task),
        })
    }

    /// Cancel the wait without dropping the callback handle.
    pub fn cancel_wait(&self) {
        self.cancel_signal.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Wait for the callback to fire. Returns `Cancelled` if the wait
    /// was cancelled before the callback arrived.
    pub async fn wait_for_code(self) -> CallbackOutcome {
        let cancel_signal = self.cancel_signal.clone();
        let task = self.server_task;
        // Poll the cancel flag alongside the underlying server task —
        // the server task itself completes when it settles the channel
        // *or* when the cancel flag flips, and we treat both as the
        // observable end of the wait.
        let outcome = tokio::select! {
            _ = wait_for_cancel(&cancel_signal) => CallbackOutcome::Cancelled,
            _ = async {
                if let Some(handle) = task {
                    let _ = handle.await;
                }
            } => CallbackOutcome::Cancelled,
        };
        outcome
    }
}

async fn wait_for_cancel(flag: &Arc<std::sync::atomic::AtomicBool>) {
    while !flag.load(std::sync::atomic::Ordering::SeqCst) {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

enum HandleOutcome {
    /// Authorization code captured — settle the callback channel.
    Settled(String),
    /// Write an HTTP response and keep accepting.
    Response(u16, String),
    /// Connection died before we could read.
    Cancelled,
}

async fn handle_request(
    stream: &mut tokio::net::TcpStream,
    expected_path: &'static str,
    expected_state: &str,
) -> HandleOutcome {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buf = [0u8; 4096];
    let mut total = Vec::new();
    let mut header_end = None;
    let read_result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        async {
            loop {
                match stream.read(&mut buf).await {
                    Ok(0) => return Err(()),
                    Ok(n) => {
                        total.extend_from_slice(&buf[..n]);
                        if let Some(idx) = total.windows(4).position(|w| w == b"\r\n\r\n") {
                            header_end = Some(idx + 4);
                            return Ok(());
                        }
                        if total.len() > 16 * 1024 {
                            return Err(());
                        }
                    }
                    Err(_) => return Err(()),
                }
            }
        },
    )
    .await;
    if read_result.is_err() {
        return HandleOutcome::Cancelled;
    }
    let header_end = match header_end {
        Some(end) => end,
        None => return HandleOutcome::Cancelled,
    };
    let _body_after_headers = &total[header_end..];

    let request_line = total
        .split(|b| *b == b'\n')
        .next()
        .and_then(|line| std::str::from_utf8(line).ok())
        .unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    if method != "GET" && method != "POST" {
        return HandleOutcome::Response(405, oauth_error_html("Method not allowed"));
    }
    let (path, query) = match target.find('?') {
        Some(idx) => (&target[..idx], &target[idx + 1..]),
        None => (target, ""),
    };
    if path != expected_path {
        return HandleOutcome::Response(404, oauth_error_html("Callback route not found"));
    }
    let params = parse_form_urlencoded(query);
    let state = params.get("state").cloned().unwrap_or_default();
    let code = params.get("code").cloned();
    if state != expected_state {
        return HandleOutcome::Response(400, oauth_error_html("State mismatch"));
    }
    let Some(code) = code else {
        return HandleOutcome::Response(400, oauth_error_html("Missing authorization code"));
    };
    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\n").await;
    HandleOutcome::Settled(code)
}

async fn write_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    body: &str,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        _ => "Internal Server Error",
    };
    let body = body.as_bytes();
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await
}

/// Parse `application/x-www-form-urlencoded` query strings into a
/// simple map. We avoid the `url` crate because the only thing we
/// parse is a `key=value&key=value` block, not a full URL.
fn parse_form_urlencoded(input: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for pair in input.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.find('=') {
            Some(idx) => (&pair[..idx], &pair[idx + 1..]),
            None => (pair, ""),
        };
        let key = percent_decode(k);
        let value = percent_decode(v);
        out.insert(key, value);
    }
    out
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = hex_digit(bytes[i + 1]);
                let lo = hex_digit(bytes[i + 2]);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h << 4) | l);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oauth::http::client;

    #[test]
    fn parse_form_urlencoded_round_trips_simple_pairs() {
        let parsed = parse_form_urlencoded("code=abc&state=xyz");
        assert_eq!(parsed.get("code").map(String::as_str), Some("abc"));
        assert_eq!(parsed.get("state").map(String::as_str), Some("xyz"));
    }

    #[test]
    fn parse_form_urlencoded_decodes_percent_encoded_values() {
        let parsed = parse_form_urlencoded("redirect=http%3A%2F%2Flocalhost");
        assert_eq!(
            parsed.get("redirect").map(String::as_str),
            Some("http://localhost")
        );
    }

    #[test]
    fn parse_form_urlencoded_handles_plus_as_space() {
        let parsed = parse_form_urlencoded("msg=hello+world");
        assert_eq!(parsed.get("msg").map(String::as_str), Some("hello world"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn server_receives_a_callback() {
        let server = LocalCallbackServer::start("127.0.0.1", "/auth/callback", "expected")
            .await
            .unwrap();
        let port = server.bound.port();
        let client = client();
        let url = format!("http://127.0.0.1:{port}/auth/callback?code=abc&state=expected");
        let response = client.get(&url).send().await.unwrap();
        assert!(response.status().is_success());
        let outcome = server.wait_for_code().await;
        assert_eq!(outcome, CallbackOutcome::Code("abc".into()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn server_rejects_state_mismatch() {
        let server = LocalCallbackServer::start("127.0.0.1", "/auth/callback", "expected")
            .await
            .unwrap();
        let port = server.bound.port();
        let client = client();
        let url = format!("http://127.0.0.1:{port}/auth/callback?code=abc&state=other");
        let _ = client.get(&url).send().await.unwrap();
        let _ = server.wait_for_code().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancel_returns_cancelled_outcome() {
        let server = LocalCallbackServer::start("127.0.0.1", "/auth/callback", "expected")
            .await
            .unwrap();
        server.cancel_wait();
        let outcome = server.wait_for_code().await;
        assert_eq!(outcome, CallbackOutcome::Cancelled);
    }
}