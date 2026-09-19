//! Integration tests for `pi --rpc` (Stage 12).
//!
//! These tests spawn the real binary and drive it over stdio — the same
//! way an editor / host process would. Every read is bounded by a
//! timeout so a regression that stops flushing (or a deadlock) fails the
//! test instead of hanging CI.
//!
//! The binary always drives the faux provider (no API key needed), so a
//! `prompt` deterministically produces `"(faux) hello"`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Per-read / per-wait timeout. Generous enough for a cold debug build's
/// first turn, small enough to fail fast on a real hang.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Upper bound on the number of stdout lines a single test will consume
/// before declaring the stream wrong (protects the `recv_until` loops).
const MAX_LINES: usize = 200;

struct RpcHarness {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Option<thread::JoinHandle<Vec<String>>>,
}

impl RpcHarness {
    fn spawn(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_pi"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn pi");

        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let stderr = child.stderr.take().expect("child stderr");

        let (tx, rx) = mpsc::channel::<String>();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let stderr_handle = thread::spawn(move || {
            BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
                .collect::<Vec<_>>()
        });

        Self {
            child,
            stdin: Some(stdin),
            lines: rx,
            stderr: Some(stderr_handle),
        }
    }

    fn send_line(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin already closed");
        stdin.write_all(line.as_bytes()).expect("write stdin");
        stdin.write_all(b"\n").expect("write newline");
        stdin.flush().expect("flush stdin");
    }

    /// Send a raw request line built from a JSON value.
    fn send_json(&mut self, value: Value) {
        self.send_line(&value.to_string());
    }

    /// Drop stdin, which the server observes as EOF.
    fn close_stdin(&mut self) {
        self.stdin.take();
    }

    fn recv_line(&mut self) -> String {
        match self.lines.recv_timeout(TIMEOUT) {
            Ok(line) => line,
            Err(err) => {
                let stderr = self.finish_stderr().join("\n");
                panic!(
                    "timed out waiting for a stdout line from pi --rpc: {err:?}\n\
                     --- child stderr ---\n{stderr}"
                );
            }
        }
    }

    fn recv_json(&mut self) -> Value {
        let line = self.recv_line();
        serde_json::from_str(&line).unwrap_or_else(|err| {
            let stderr = self.finish_stderr().join("\n");
            panic!(
                "pi --rpc wrote a non-JSON stdout line ({err}): {line}\n\
                 --- child stderr ---\n{stderr}"
            );
        })
    }

    /// Read stdout until `predicate` matches, panicking after a timeout.
    fn recv_until<F>(&mut self, mut predicate: F, label: &str) -> Value
    where
        F: FnMut(&Value) -> bool,
    {
        let deadline = Instant::now() + TIMEOUT;
        for _ in 0..MAX_LINES {
            if Instant::now() >= deadline {
                let stderr = self.finish_stderr().join("\n");
                panic!(
                    "timed out waiting for {label} on pi --rpc stdout\n\
                     --- child stderr ---\n{stderr}"
                );
            }
            let value = self.recv_json();
            if predicate(&value) {
                return value;
            }
        }
        let stderr = self.finish_stderr().join("\n");
        panic!(
            "gave up waiting for {label} after {MAX_LINES} stdout lines\n\
             --- child stderr ---\n{stderr}"
        );
    }

    fn wait_for_exit(&mut self) -> ExitStatus {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                panic!("pi --rpc did not exit after stdin EOF");
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Kill the process and return everything it wrote to stderr.
    fn finish_stderr(&mut self) -> Vec<String> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        match self.stderr.take() {
            Some(handle) => handle.join().unwrap_or_default(),
            None => Vec::new(),
        }
    }
}

impl Drop for RpcHarness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn is_event(value: &Value, event_type: &str) -> bool {
    value.get("method").and_then(Value::as_str) == Some("event")
        && value.get("params").and_then(|p| p.get("type")) == Some(&json!(event_type))
}

fn is_response_with_result(value: &Value, id: i64) -> bool {
    value.get("id") == Some(&json!(id)) && value.get("result").is_some()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn prompt_returns_response_and_streams_events() {
    let mut h = RpcHarness::spawn(&["--rpc"]);
    h.send_json(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "prompt",
        "params": {"text": "hello"},
    }));

    let mut response = None;
    let mut saw_text_delta = false;
    let mut saw_message_end = false;
    let mut saw_turn_end = false;

    for _ in 0..MAX_LINES {
        let value = h.recv_json();
        if is_response_with_result(&value, 1) {
            response = Some(value);
        } else if is_event(&value, "message_end") {
            saw_message_end = true;
        } else if is_event(&value, "turn_end") {
            saw_turn_end = true;
        } else if value.get("method").and_then(Value::as_str) == Some("event")
            && value["params"]["assistantMessageEvent"]["type"] == json!("text_delta")
        {
            saw_text_delta = true;
        }
        if response.is_some() && saw_message_end && saw_turn_end && saw_text_delta {
            break;
        }
    }

    let response = response.expect("prompt response");
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["stopReason"], "stop");
    assert!(response["result"]["turn"].is_number(), "{response}");

    assert!(saw_text_delta, "no text_delta event notification arrived");
    assert!(saw_message_end, "no message_end event notification arrived");
    assert!(saw_turn_end, "no turn_end event notification arrived");

    // getState reflects the turn that just ran.
    h.send_json(json!({"jsonrpc": "2.0", "id": 2, "method": "getState"}));
    let state = h.recv_until(|v| is_response_with_result(v, 2), "getState response");
    let state = &state["result"];
    assert!(state["model"]["id"].is_string(), "{state}");
    assert!(
        state["sessionId"].as_str().is_some_and(|id| !id.is_empty()),
        "getState must report a session id: {state}"
    );
    let messages = state["messages"].as_array().expect("messages array");
    assert!(
        messages.len() >= 2,
        "expected user + assistant messages, got {messages:?}"
    );
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[1]["role"], "assistant");

    h.close_stdin();
    let status = h.wait_for_exit();
    assert_eq!(status.code(), Some(0), "stdin EOF must exit 0");
}

#[tokio::test(flavor = "current_thread")]
async fn prompt_template_expands_slash_invocations() {
    // `--prompt-template` loads a markdown template; a `prompt` whose
    // text is `/greet <arg>` must reach the agent as the expanded body,
    // proving RPC mode shares the prompt-template pipeline with the TUI.
    let dir = tempfile::TempDir::with_prefix("pi-rpc-prompts-").expect("tempdir");
    let template = dir.path().join("greet.md");
    std::fs::write(
        &template,
        "---\ndescription: Greet someone.\n---\nhello-template:$1",
    )
    .expect("write template");

    let mut h = RpcHarness::spawn(&[
        "--rpc",
        "--prompt-template",
        template.to_str().expect("utf-8 path"),
    ]);
    h.send_json(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "prompt",
        "params": {"text": "/greet world"},
    }));
    let _ = h.recv_until(|v| is_response_with_result(v, 1), "prompt response");

    h.send_json(json!({"jsonrpc": "2.0", "id": 2, "method": "getState"}));
    let state = h.recv_until(|v| is_response_with_result(v, 2), "getState response");
    let serialized = state.to_string();
    assert!(
        serialized.contains("hello-template:world"),
        "template body was not expanded into the prompt: {serialized}"
    );
    assert!(
        !serialized.contains("/greet world"),
        "the raw slash invocation leaked into the transcript: {serialized}"
    );

    h.close_stdin();
    let status = h.wait_for_exit();
    assert_eq!(status.code(), Some(0));
}

#[test]
fn get_state_without_prompt_returns_empty_state() {
    let mut h = RpcHarness::spawn(&["--rpc"]);
    h.send_json(json!({"jsonrpc": "2.0", "id": 7, "method": "getState"}));

    let response = h.recv_until(|v| is_response_with_result(v, 7), "getState response");
    let result = &response["result"];
    assert_eq!(result["messages"], json!([]));
    assert_eq!(result["model"]["id"], "faux-model");
    assert!(
        result["sessionId"].as_str().is_some_and(|id| !id.is_empty()),
        "getState must report a session id: {result}"
    );

    h.close_stdin();
    assert_eq!(h.wait_for_exit().code(), Some(0));
}

#[test]
fn unknown_method_returns_method_not_found() {
    let mut h = RpcHarness::spawn(&["--rpc"]);
    h.send_json(json!({"jsonrpc": "2.0", "id": 3, "method": "definitely-not-a-method"}));

    let response = h.recv_until(
        |v| v.get("id") == Some(&json!(3)) && v.get("error").is_some(),
        "method-not-found response",
    );
    assert_eq!(response["error"]["code"], -32601);

    h.close_stdin();
    assert_eq!(h.wait_for_exit().code(), Some(0));
}

#[test]
fn invalid_json_line_returns_parse_error_and_keeps_running() {
    let mut h = RpcHarness::spawn(&["--rpc"]);
    h.send_line("{ this is not json");

    let error = h.recv_until(
        |v| v.get("error").is_some() && v["error"]["code"] == json!(-32700),
        "parse-error response",
    );
    assert_eq!(error["id"], Value::Null);

    // The server must still answer a well-formed request afterwards.
    h.send_json(json!({"jsonrpc": "2.0", "id": 4, "method": "getState"}));
    let response = h.recv_until(|v| is_response_with_result(v, 4), "getState after parse error");
    assert!(response["result"]["messages"].is_array());

    h.close_stdin();
    assert_eq!(h.wait_for_exit().code(), Some(0));
}

#[test]
fn abort_without_in_flight_turn_is_idempotent() {
    let mut h = RpcHarness::spawn(&["--rpc"]);
    h.send_json(json!({"jsonrpc": "2.0", "id": 5, "method": "abort"}));

    let response = h.recv_until(|v| is_response_with_result(v, 5), "abort response");
    assert_eq!(response["result"]["aborted"], false);

    h.close_stdin();
    assert_eq!(h.wait_for_exit().code(), Some(0));
}

#[test]
fn set_model_rejects_unknown_model() {
    let mut h = RpcHarness::spawn(&["--rpc"]);
    h.send_json(json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "setModel",
        "params": {"model": "nope/nope-model"},
    }));

    let response = h.recv_until(
        |v| v.get("id") == Some(&json!(6)) && v.get("error").is_some(),
        "setModel error response",
    );
    assert_eq!(response["error"]["code"], -32602);

    // A known model still switches.
    h.send_json(json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "setModel",
        "params": {"model": "anthropic/claude-sonnet-4-5"},
    }));
    let response = h.recv_until(|v| is_response_with_result(v, 7), "setModel response");
    assert_eq!(response["result"]["model"]["id"], "claude-sonnet-4-5");

    h.close_stdin();
    assert_eq!(h.wait_for_exit().code(), Some(0));
}

#[test]
fn rpc_flag_no_longer_prints_the_stage5_stub() {
    let mut h = RpcHarness::spawn(&["--rpc"]);
    // stdin is already at EOF once we drop it; the server should exit 0
    // without printing anything but JSON (nothing, in this case).
    h.close_stdin();
    let status = h.wait_for_exit();
    let stderr = h.finish_stderr();

    assert_eq!(status.code(), Some(0), "stderr: {stderr:?}");
    let stderr = stderr.join("\n");
    assert!(
        !stderr.contains("Stage 5 deliverable"),
        "rpc mode still prints the stub banner: {stderr}"
    );
    assert!(
        !stderr.contains("rpc mode is a"),
        "rpc mode still prints the stub banner: {stderr}"
    );
}

#[test]
fn rpc_flag_without_stdin_exits_zero() {
    // `--rpc` must never fall back to an interactive TUI (which would
    // block forever without a tty). `/dev/null` gives immediate EOF.
    let mut child = Command::new(env!("CARGO_BIN_EXE_pi"))
        .arg("--rpc")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pi --rpc");

    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("pi --rpc with /dev/null stdin did not exit");
        }
        thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(0));
}
