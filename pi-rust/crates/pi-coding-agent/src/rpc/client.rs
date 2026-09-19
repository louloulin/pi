//! RPC **client** for the `pi --rpc` stdio service.
//!
//! This is the Rust counterpart of the upstream TypeScript
//! `packages/coding-agent/src/modes/rpc/rpc-client.ts`: it spawns the
//! real `pi --rpc` child process and speaks the *same* NDJSON stdio
//! protocol [`super::server`] implements — one JSON-RPC 2.0 request per
//! line on stdin, one JSON object per line on stdout.
//!
//! It deliberately does **not** touch the `pi-client` / `pi-protocol`
//! socket transport (a different protocol on a different transport) —
//! this client exists so integration tests and future embedders share
//! one implementation of the `--rpc` client side.
//!
//! ```no_run
//! # use pi_coding_agent::rpc::client::{ClientMessage, RpcClient, RpcClientOptions};
//! # use std::time::Duration;
//! let options = RpcClientOptions::new("/path/to/pi");
//! let mut client = RpcClient::spawn(&options).expect("spawn pi --rpc");
//! // Fire-and-observe: events stream while the turn runs.
//! client.prompt("hello").expect("prompt");
//! // Or inspect the wire directly (raw lines, unmatched frames, …).
//! let message = client.recv_message(Duration::from_secs(5)).expect("frame");
//! # let _ = message;
//! ```
//!
//! # Lifecycle
//!
//! * [`RpcClient::spawn`] starts the child with piped stdio and a reader
//!   thread per stdout/stderr.
//! * [`RpcClient::call`] sends a request, waits for the response whose
//!   `id` matches, and buffers every event notification it walks past in
//!   [`RpcClient::events`] while dispatching it to
//!   [`RpcClient::on_event`] listeners. Events are therefore observable
//!   *before* the response returns — that is the streaming guarantee the
//!   integration tests assert.
//! * A timeout, a closed stdout, or a non-JSON stdout line produces an
//!   explicit [`RpcClientError`] carrying the child's stderr so far;
//!   dropping the client kills the process.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use super::protocol::{Response, JSONRPC_VERSION};

/// Default per-request / per-read timeout.
///
/// Generous enough for a cold debug build's first turn, small enough to
/// fail a hang fast. Matches the integration tests' budget.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// How the child's stdin is wired up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdinMode {
    /// A pipe the client writes JSON-RPC requests to (the default).
    Piped,
    /// `/dev/null` — immediate EOF, nothing can be written. Useful for
    /// the "`--rpc` with no stdin must not fall back to a TUI" probe.
    Null,
}

/// How to launch the `pi --rpc` child process.
///
/// Use the builder methods; [`RpcClientOptions::new`] already adds the
/// `--rpc` flag, so the common case is `RpcClientOptions::new(binary)`.
#[derive(Debug, Clone)]
pub struct RpcClientOptions {
    program: PathBuf,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    env: Vec<(OsString, OsString)>,
    stdin: StdinMode,
    timeout: Duration,
}

impl RpcClientOptions {
    /// Options for `program` with the `--rpc` flag already appended.
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: vec![OsString::from("--rpc")],
            cwd: None,
            env: Vec::new(),
            stdin: StdinMode::Piped,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Append one CLI argument (after `--rpc`).
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Append several CLI arguments.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Working directory for the child.
    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// Set one environment variable for the child (inherits the rest).
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// How the child's stdin is wired up.
    pub fn stdin(mut self, stdin: StdinMode) -> Self {
        self.stdin = stdin;
        self
    }

    /// Default timeout for [`RpcClient::call`] and friends.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl StdinMode {
    /// The [`Stdio`] this mode configures.
    fn into_stdio(self) -> Stdio {
        match self {
            StdinMode::Piped => Stdio::piped(),
            StdinMode::Null => Stdio::null(),
        }
    }
}

/// One decoded stdout frame from `pi --rpc`.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    /// A JSON-RPC response (`{"jsonrpc","id","result"|"error"}`).
    Response(Response),
    /// An agent event notification. Holds the `params` payload — the
    /// object carrying the event's `type` tag.
    Event(Value),
    /// Any other notification (unknown `method`).
    Notification {
        /// The notification's `method`.
        method: String,
        /// Its `params`, if any.
        params: Option<Value>,
    },
    /// A stdout line that was not valid JSON. The server never writes
    /// one; it is surfaced verbatim so a test can assert on it.
    Unparsed(String),
}

impl ClientMessage {
    /// The event payload if this frame is an event notification.
    pub fn as_event(&self) -> Option<&Value> {
        match self {
            ClientMessage::Event(value) => Some(value),
            _ => None,
        }
    }
}

/// Errors produced by [`RpcClient`].
#[derive(Debug, thiserror::Error)]
pub enum RpcClientError {
    /// Spawning the child or writing its stdin failed.
    #[error("rpc client io error: {0}")]
    Io(#[from] std::io::Error),
    /// The child's stdin was never piped (or was already dropped).
    #[error("pi --rpc stdin is not writable")]
    StdinClosed,
    /// No matching response arrived within the timeout.
    #[error("timed out waiting for `{awaiting}`. child stderr so far:\n{stderr}")]
    Timeout {
        /// What the client was waiting for.
        awaiting: String,
        /// Everything the child wrote to stderr so far.
        stderr: String,
    },
    /// stdout closed (normally: the child exited) before the awaited
    /// response arrived.
    #[error("pi --rpc stdout closed before `{awaiting}` arrived{detail}\n--- child stderr ---\n{stderr}")]
    Disconnected {
        /// What the client was waiting for.
        awaiting: String,
        /// `", exit status: 1"` — empty while the child is still alive.
        detail: String,
        /// Everything the child wrote to stderr.
        stderr: String,
    },
    /// A stdout line was not valid JSON.
    #[error("pi --rpc wrote a non-JSON stdout line: {line}\n--- child stderr ---\n{stderr}")]
    NonJsonLine {
        /// The offending line.
        line: String,
        /// Everything the child wrote to stderr.
        stderr: String,
    },
    /// The frame did not match the method's expected shape.
    #[error("rpc protocol error: {message}")]
    Protocol {
        /// What was wrong.
        message: String,
    },
    /// The server answered with a JSON-RPC error object.
    #[error("JSON-RPC error {}: {}", .0.code, .0.message)]
    JsonRpc(super::error::JsonRpcError),
}

impl RpcClientError {
    /// The JSON-RPC error code, when the failure was a server-side error.
    pub fn json_rpc_code(&self) -> Option<i64> {
        match self {
            RpcClientError::JsonRpc(error) => Some(error.code),
            _ => None,
        }
    }
}

/// Callback invoked for every event the client observes.
type EventListener = Box<dyn FnMut(&Value) + Send>;

/// A live `pi --rpc` child process plus its stdout/stderr pumps.
pub struct RpcClient {
    child: Child,
    stdin: Option<ChildStdin>,
    messages: Receiver<ClientMessage>,
    /// Frames a `call` walked past that did not match its request id
    /// (late responses, unknown notifications, parse errors).
    unmatched: VecDeque<ClientMessage>,
    /// Every event observed so far, in arrival order.
    events: Vec<Value>,
    listeners: Vec<EventListener>,
    stderr: Arc<Mutex<Vec<String>>>,
    stderr_thread: Option<JoinHandle<()>>,
    next_id: u64,
    last_id: Option<Value>,
    timeout: Duration,
}

impl RpcClient {
    /// Spawn the child described by `options` and start pumping stdio.
    pub fn spawn(options: &RpcClientOptions) -> Result<Self, RpcClientError> {
        let mut command = Command::new(&options.program);
        command
            .args(&options.args)
            .stdin(options.stdin.into_stdio())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &options.cwd {
            command.current_dir(cwd);
        }
        command.envs(options.env.iter().cloned());

        let mut child = command.spawn()?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RpcClientError::Protocol {
                message: "child stdout was not piped".to_string(),
            })?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| RpcClientError::Protocol {
                message: "child stderr was not piped".to_string(),
            })?;

        let (tx, messages) = mpsc::channel::<ClientMessage>();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                // Blank lines are framing noise, not a frame.
                if line.trim().is_empty() {
                    continue;
                }
                if tx.send(classify(line)).is_err() {
                    break;
                }
            }
        });

        let stderr_lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&stderr_lines);
        let stderr_thread = thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                sink.lock().push(line);
            }
        });

        Ok(Self {
            child,
            stdin,
            messages,
            unmatched: VecDeque::new(),
            events: Vec::new(),
            listeners: Vec::new(),
            stderr: stderr_lines,
            stderr_thread: Some(stderr_thread),
            next_id: 0,
            last_id: None,
            timeout: options.timeout,
        })
    }

    /// The default per-call timeout these options configured.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// The id of the most recently sent request.
    pub fn last_request_id(&self) -> Option<&Value> {
        self.last_id.as_ref()
    }

    // -- sending ----------------------------------------------------------

    /// Write one raw line to the child's stdin (LF appended).
    ///
    /// The low-level escape hatch: use it to send deliberately malformed
    /// bytes that the typed helpers cannot express. The server answers a
    /// bad JSON line with a `-32700` response carrying `"id": null`, which
    /// [`RpcClient::recv_message`] will return.
    pub fn send_raw_line(&mut self, line: &str) -> Result<(), RpcClientError> {
        let stdin = self.stdin.as_mut().ok_or(RpcClientError::StdinClosed)?;
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    /// Send a JSON-RPC notification (no response expected).
    pub fn notify(&mut self, method: &str, params: Option<Value>) -> Result<(), RpcClientError> {
        let mut frame = Map::new();
        frame.insert("jsonrpc".to_string(), json!(JSONRPC_VERSION));
        frame.insert("method".to_string(), json!(method));
        if let Some(params) = params {
            frame.insert("params".to_string(), params);
        }
        self.send_raw_line(&Value::Object(frame).to_string())
    }

    fn send_request(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, RpcClientError> {
        self.next_id += 1;
        let id = json!(self.next_id);
        let mut frame = Map::new();
        frame.insert("jsonrpc".to_string(), json!(JSONRPC_VERSION));
        frame.insert("id".to_string(), id.clone());
        frame.insert("method".to_string(), json!(method));
        if let Some(params) = params {
            frame.insert("params".to_string(), params);
        }
        self.send_raw_line(&Value::Object(frame).to_string())?;
        self.last_id = Some(id.clone());
        Ok(id)
    }

    // -- calling ----------------------------------------------------------

    /// Send a request and wait for its response using the default timeout.
    pub fn call(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Response, RpcClientError> {
        self.call_with_timeout(method, params, self.timeout)
    }

    /// Send a request and wait for the response whose `id` matches.
    ///
    /// Event notifications seen while waiting are dispatched to the
    /// listeners and retained in [`RpcClient::events`]. Frames that do not
    /// answer this request (late responses, parse-error responses with a
    /// null id, unknown notifications) land in the unmatched queue and are
    /// retrievable through [`RpcClient::recv_message`].
    pub fn call_with_timeout(
        &mut self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Response, RpcClientError> {
        let id = self.send_request(method, params)?;
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(self.timeout_error(method));
            }
            match self.recv_channel(method, remaining)? {
                ClientMessage::Response(response) if response.id.as_ref() == Some(&id) => {
                    return Ok(response)
                }
                other => self.unmatched.push_back(other),
            }
        }
    }

    /// Like [`RpcClient::call`] but unwraps the `result` and turns a
    /// JSON-RPC error object into [`RpcClientError::JsonRpc`].
    pub fn call_ok(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, RpcClientError> {
        self.call_ok_with_timeout(method, params, self.timeout)
    }

    /// [`RpcClient::call_ok`] with an explicit timeout.
    pub fn call_ok_with_timeout(
        &mut self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, RpcClientError> {
        let response = self.call_with_timeout(method, params, timeout)?;
        if let Some(error) = response.error {
            return Err(RpcClientError::JsonRpc(error));
        }
        response.result.ok_or_else(|| RpcClientError::Protocol {
            message: format!("`{method}` response carried neither `result` nor `error`"),
        })
    }

    // -- receiving --------------------------------------------------------

    /// The next frame, unmatched-queue first, then the live stdout pump.
    pub fn recv_message(&mut self, timeout: Duration) -> Result<ClientMessage, RpcClientError> {
        if let Some(message) = self.unmatched.pop_front() {
            return Ok(message);
        }
        self.recv_channel("<any message>", timeout)
    }

    /// Wait for the first event matching `predicate` that arrives from now
    /// on. Frames that are not events are skipped.
    pub fn wait_for_event<F>(
        &mut self,
        mut predicate: F,
        timeout: Duration,
    ) -> Result<Value, RpcClientError>
    where
        F: FnMut(&Value) -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(self.timeout_error("event"));
            }
            if let ClientMessage::Event(value) = self.recv_channel("event", remaining)? {
                if predicate(&value) {
                    return Ok(value);
                }
            }
        }
    }

    /// Every event observed so far, in arrival order.
    pub fn events(&self) -> &[Value] {
        &self.events
    }

    /// Drain and return the retained event log.
    pub fn take_events(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.events)
    }

    /// Register a callback invoked for every event notification. Callbacks
    /// run on the client's thread, inside the call that observes the event.
    pub fn on_event<F>(&mut self, listener: F)
    where
        F: FnMut(&Value) + Send + 'static,
    {
        self.listeners.push(Box::new(listener));
    }

    /// Remove every [`RpcClient::on_event`] listener.
    pub fn clear_event_listeners(&mut self) {
        self.listeners.clear();
    }

    fn recv_channel(
        &mut self,
        awaiting: &str,
        timeout: Duration,
    ) -> Result<ClientMessage, RpcClientError> {
        match self.messages.recv_timeout(timeout) {
            Ok(ClientMessage::Event(value)) => {
                self.dispatch_event(&value);
                Ok(ClientMessage::Event(value))
            }
            Ok(message) => Ok(message),
            Err(RecvTimeoutError::Timeout) => Err(self.timeout_error(awaiting)),
            Err(RecvTimeoutError::Disconnected) => Err(self.disconnected_error(awaiting)),
        }
    }

    fn dispatch_event(&mut self, value: &Value) {
        self.events.push(value.clone());
        for listener in &mut self.listeners {
            listener(value);
        }
    }

    fn timeout_error(&mut self, awaiting: &str) -> RpcClientError {
        RpcClientError::Timeout {
            awaiting: awaiting.to_string(),
            stderr: self.stderr_snapshot().join("\n"),
        }
    }

    fn disconnected_error(&mut self, awaiting: &str) -> RpcClientError {
        let detail = match self.child.try_wait() {
            Ok(Some(status)) => format!(", {status}"),
            _ => ", child still running".to_string(),
        };
        RpcClientError::Disconnected {
            awaiting: awaiting.to_string(),
            detail,
            stderr: self.stderr_snapshot().join("\n"),
        }
    }

    // -- typed helpers (the `pi --rpc` method set) -------------------------

    /// `prompt` — run one turn and return its `{ "turn", "stopReason" }`
    /// result once the turn ended. Events observed while waiting are
    /// available through [`RpcClient::events`].
    pub fn prompt(&mut self, text: &str) -> Result<Value, RpcClientError> {
        self.call_ok("prompt", Some(json!({"text": text})))
    }

    /// `abort` — cancel the in-flight turn. `false` when none was running.
    pub fn abort(&mut self) -> Result<bool, RpcClientError> {
        let result = self.call_ok("abort", None)?;
        result
            .get("aborted")
            .and_then(Value::as_bool)
            .ok_or_else(|| RpcClientError::Protocol {
                message: format!("abort result is missing `aborted`: {result}"),
            })
    }

    /// `getState` — `{ "model", "messages", "sessionId" }`, or `null`
    /// when the server answered with a JSON-RPC error.
    pub fn get_state(&mut self) -> Result<Value, RpcClientError> {
        self.call_ok("getState", None)
    }

    /// `setModel` with the canonical `{ "model": "provider/id" }` params.
    pub fn set_model(&mut self, model: &str) -> Result<Value, RpcClientError> {
        self.call_ok("setModel", Some(json!({"model": model})))
    }

    /// `setModel` with the upstream `{ "provider", "modelId" }` pair.
    pub fn set_model_parts(
        &mut self,
        provider: &str,
        model_id: &str,
    ) -> Result<Value, RpcClientError> {
        self.call_ok(
            "setModel",
            Some(json!({"provider": provider, "modelId": model_id})),
        )
    }

    // -- lifecycle --------------------------------------------------------

    /// Drop stdin, which the server observes as EOF.
    pub fn close_stdin(&mut self) {
        self.stdin.take();
    }

    /// Wait for the child to exit, then collect its stderr.
    pub fn wait_for_exit(&mut self, timeout: Duration) -> Result<ExitStatus, RpcClientError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.join_stderr();
                return Ok(status);
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                self.join_stderr();
                return Err(self.timeout_error("process exit"));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Everything the child has written to stderr, without blocking on a
    /// still-running child.
    pub fn stderr_snapshot(&self) -> Vec<String> {
        self.stderr.lock().clone()
    }

    /// Everything the child wrote to stderr, joining the pump thread.
    ///
    /// Call it after [`RpcClient::wait_for_exit`]; before the child exits
    /// this blocks until stderr closes.
    pub fn stderr(&mut self) -> Vec<String> {
        self.join_stderr();
        self.stderr_snapshot()
    }

    /// Kill the child without waiting.
    pub fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill()
    }

    fn join_stderr(&mut self) {
        if let Some(handle) = self.stderr_thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for RpcClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Split one stdout line into its frame.
///
/// Only JSON objects are considered; anything else is surfaced as
/// [`ClientMessage::Unparsed`] instead of being silently dropped.
fn classify(line: String) -> ClientMessage {
    let Ok(value) = serde_json::from_str::<Value>(&line) else {
        return ClientMessage::Unparsed(line);
    };
    match value.get("method").and_then(Value::as_str) {
        Some("event") => ClientMessage::Event(value.get("params").cloned().unwrap_or(Value::Null)),
        Some(method) => ClientMessage::Notification {
            method: method.to_string(),
            params: value.get("params").cloned(),
        },
        None => match serde_json::from_value::<Response>(value) {
            Ok(response) => ClientMessage::Response(response),
            Err(_) => ClientMessage::Unparsed(line),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_a_response() {
        let message = classify(r#"{"jsonrpc":"2.0","id":1,"result":{"turn":1}}"#.to_string());
        let ClientMessage::Response(response) = message else {
            panic!("expected a response, got {message:?}");
        };
        assert_eq!(response.id, Some(json!(1)));
        assert_eq!(response.result, Some(json!({"turn": 1})));
    }

    #[test]
    fn classifies_a_null_id_parse_error() {
        let message = classify(
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"bad json"}}"#
                .to_string(),
        );
        let ClientMessage::Response(response) = message else {
            panic!("expected a response, got {message:?}");
        };
        assert_eq!(response.id, None);
        assert_eq!(response.error.expect("error").code, -32700);
    }

    #[test]
    fn classifies_an_event_by_its_params() {
        let message = classify(
            r#"{"jsonrpc":"2.0","method":"event","params":{"type":"turn_end","turn":1}}"#
                .to_string(),
        );
        assert_eq!(
            message.as_event(),
            Some(&json!({"type": "turn_end", "turn": 1}))
        );
    }

    #[test]
    fn classifies_an_unknown_notification() {
        let message = classify(r#"{"jsonrpc":"2.0","method":"log","params":{"x":1}}"#.to_string());
        assert_eq!(
            message,
            ClientMessage::Notification {
                method: "log".to_string(),
                params: Some(json!({"x": 1})),
            }
        );
    }

    #[test]
    fn classifies_a_dirty_line_verbatim() {
        assert_eq!(
            classify("{ this is not json".to_string()),
            ClientMessage::Unparsed("{ this is not json".to_string())
        );
    }

    #[test]
    fn options_default_to_the_rpc_flag() {
        let options = RpcClientOptions::new("/bin/pi")
            .arg("--prompt-template")
            .arg("greet.md")
            .timeout(Duration::from_secs(5));
        assert_eq!(options.args[0], OsString::from("--rpc"));
        assert_eq!(options.args[1], OsString::from("--prompt-template"));
        assert_eq!(options.args[2], OsString::from("greet.md"));
        assert_eq!(options.timeout, Duration::from_secs(5));
    }
}
