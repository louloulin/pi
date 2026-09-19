//! Stage 16 integration tests: the `pi` binary executes *real* tools.
//!
//! Before this stage `main.rs` built every agent without a
//! [`ToolExecutor`](pi_agent_core::tools::ToolExecutor), so the loop fell
//! back to the Stage 2 stub and answered each model tool call with the
//! fabricated string `"(stub) executed <name>"` — the model never saw
//! real file or shell output. These tests drive the real binary the way a
//! user does and prove the built-in bundle ran:
//!
//! 1. A loopback server pretends to be the OpenAI chat-completions API
//!    and scripts a `bash` tool call followed by a plain-text reply.
//! 2. The binary must dial that server twice: the second request is the
//!    agent loop feeding the *tool result* back to the model, so its body
//!    is where the evidence lives — it has to contain the marker string
//!    the scripted shell command printed, and must not contain `(stub)`.
//! 3. The `json-events` stream / RPC notifications must report a
//!    `tool_execution_end` for `bash` with `is_error: false`.
//!
//! No network access and no API key are needed: the transport is loopback
//! and the credential is the literal `test-key`.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{json, Value};

/// Marker the scripted `bash` tool call prints. Its presence in the
/// *second* provider request can only come from a real shell execution:
/// the command reads it from the environment, so the literal never
/// appears in the request's tool-call arguments.
const MARKER: &str = "pi-stage16-tool-marker";

/// Shell command the scripted tool call runs.
const MARKER_COMMAND: &str = r#"printf '%s' "$PI_CLI_TOOLS_MARKER""#;

// ---------------------------------------------------------------------------
// Scripted OpenAI-compatible loopback server
// ---------------------------------------------------------------------------

/// One scripted model response.
#[derive(Clone)]
enum Reply {
    /// Reply with a single tool call.
    ToolCall { name: String, arguments: Value },
    /// Reply with plain assistant text (ends the turn).
    Text(String),
}

impl Reply {
    fn sse(&self) -> String {
        match self {
            Reply::ToolCall { name, arguments } => sse_tool_call(name, arguments),
            Reply::Text(text) => sse_text(text),
        }
    }
}

fn sse_chunk(delta: Value, finish_reason: Option<&str>) -> String {
    let payload = json!({
        "id": "chatcmpl-stage16",
        "object": "chat.completion.chunk",
        "created": 1_700_000_000u64,
        "model": "gpt-4o-mini",
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason,
        }],
    });
    format!("data: {payload}\n\n")
}

fn sse_usage() -> String {
    let payload = json!({
        "id": "chatcmpl-stage16",
        "object": "chat.completion.chunk",
        "created": 1_700_000_000u64,
        "model": "gpt-4o-mini",
        "choices": [],
        "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18},
    });
    format!("data: {payload}\n\n")
}

/// Tool-call stream: announce the call, then stream its arguments, then
/// finish with `tool_calls`.
fn sse_tool_call(name: &str, arguments: &Value) -> String {
    let encoded = arguments.to_string();
    let mut body = String::new();
    body.push_str(&sse_chunk(json!({"role": "assistant", "content": null}), None));
    body.push_str(&sse_chunk(
        json!({"tool_calls": [{
            "index": 0,
            "id": "call_stage16",
            "type": "function",
            "function": {"name": name, "arguments": ""},
        }]}),
        None,
    ));
    body.push_str(&sse_chunk(
        json!({"tool_calls": [{"index": 0, "function": {"arguments": encoded}}]}),
        None,
    ));
    body.push_str(&sse_chunk(json!({}), Some("tool_calls")));
    body.push_str(&sse_usage());
    body.push_str("data: [DONE]\n\n");
    body
}

fn sse_text(text: &str) -> String {
    let mut body = String::new();
    body.push_str(&sse_chunk(json!({"role": "assistant", "content": ""}), None));
    body.push_str(&sse_chunk(json!({"content": text}), None));
    body.push_str(&sse_chunk(json!({}), Some("stop")));
    body.push_str(&sse_usage());
    body.push_str("data: [DONE]\n\n");
    body
}

/// Loopback HTTP server that answers each request with the next scripted
/// SSE reply and records the request bodies.
struct ModelServer {
    addr: SocketAddr,
    bodies: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ModelServer {
    fn spawn(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener.set_nonblocking(true).expect("nonblocking listener");
        let addr = listener.local_addr().expect("local addr");

        let bodies = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let bodies_thread = bodies.clone();
        let stop_thread = stop.clone();
        let handle = thread::spawn(move || {
            let mut queue: VecDeque<Reply> = replies.into();
            while !stop_thread.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let reply = queue
                            .pop_front()
                            .unwrap_or_else(|| Reply::Text("done".into()));
                        if let Some(body) = serve_one(stream, reply) {
                            if !body.is_empty() {
                                bodies_thread.lock().expect("bodies lock").push(body);
                            }
                        }
                    }
                    Err(ref err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            addr,
            bodies,
            stop,
            handle: Some(handle),
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn bodies(&self) -> Vec<String> {
        self.bodies.lock().expect("bodies lock").clone()
    }

    /// Signal the accept loop to stop and wait for the thread.
    fn finish(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Read one request from `stream`, answer it with `reply`, and return the
/// request body.
fn serve_one(mut stream: TcpStream, reply: Reply) -> Option<String> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let body = read_request(&mut stream)?;
    let sse = reply.sse();
    let head = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        sse.len()
    );
    stream.write_all(head.as_bytes()).ok()?;
    stream.write_all(sse.as_bytes()).ok()?;
    stream.flush().ok()?;
    Some(body)
}

/// Read the request head plus its body (per `content-length`).
fn read_request(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = find(head.as_slice(), b"\r\n\r\n") {
            break pos + 4;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return None,
            Ok(n) => head.extend_from_slice(&chunk[..n]),
            Err(_) => return None,
        }
    };

    let head_text = String::from_utf8_lossy(&head[..head_end]).into_owned();
    let content_length = head_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);

    let mut body = head[head_end..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    Some(String::from_utf8_lossy(&body).into_owned())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Spawn `pi` against `server` with a scrubbed environment.
///
/// `env_clear` keeps the host's `OPENAI_API_KEY` / base-URL overrides from
/// changing what the test measures; only `PATH` (needed to run `sh`),
/// `HOME` and a UTF-8 locale are put back.
fn pi_command(server: &ModelServer, sessions: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pi"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", sessions)
        .env("LANG", "C.UTF-8")
        .env("PI_CLI_TOOLS_MARKER", MARKER)
        .env("OPENAI_API_KEY", "test-key")
        .env("OPENAI_BASE_URL", server.url());
    cmd
}

fn run_print(
    server: &ModelServer,
    sessions: &std::path::Path,
    output_format: &str,
) -> Output {
    pi_command(server, sessions)
        .args([
            "--model",
            "openai/gpt-4o-mini",
            "--print",
            "run the tool",
            "--output-format",
            output_format,
        ])
        .arg("--session-dir")
        .arg(sessions)
        .output()
        .expect("failed to spawn pi")
}

fn tempdir(label: &str) -> tempfile::TempDir {
    tempfile::TempDir::with_prefix(format!(
        "pi-cli-tools-{label}-{}-",
        std::process::id()
    ))
    .expect("tempdir")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// ---------------------------------------------------------------------------
// Print mode
// ---------------------------------------------------------------------------

#[test]
fn print_mode_executes_the_bash_tool_and_feeds_the_result_back() {
    let server = ModelServer::spawn(vec![
        Reply::ToolCall {
            name: "bash".into(),
            arguments: json!({"command": MARKER_COMMAND}),
        },
        Reply::Text("finished".into()),
    ]);
    let sessions = tempdir("print");

    let output = run_print(&server, sessions.path(), "json-events");
    let bodies = server.bodies();
    server.finish();

    assert!(
        output.status.success(),
        "print mode failed\nstdout: {}\nstderr: {}",
        stdout(&output),
        stderr(&output)
    );

    assert_eq!(
        bodies.len(),
        2,
        "the loop must issue a second request carrying the tool result; requests: {bodies:#?}"
    );

    // The first request advertises the built-in tool bundle to the model.
    assert!(
        bodies[0].contains("\"tools\"") && bodies[0].contains("\"bash\""),
        "the first request must advertise the tool schemas:\n{}",
        bodies[0]
    );

    // The second request carries the tool result, so it proves the shell
    // really ran instead of the Stage 2 stub answering.
    let second = &bodies[1];
    assert!(
        second.contains(MARKER),
        "the tool result fed back to the model must contain the marker printed by `bash`:\n{second}"
    );
    assert!(
        !second.contains("(stub)"),
        "the stub executor's fabricated result must never reach the model:\n{second}"
    );

    // The event stream reports a successful bash execution.
    let events = stdout(&output);
    assert!(
        events.contains("\"type\":\"tool_execution_start\"") && events.contains("\"name\":\"bash\""),
        "expected a bash tool_execution_start event:\n{events}"
    );
    assert!(
        events.contains("\"type\":\"tool_execution_end\"") && events.contains("\"is_error\":false"),
        "expected a successful tool_execution_end event:\n{events}"
    );
}

#[test]
fn print_mode_surfaces_real_tool_failures_as_error_results() {
    let server = ModelServer::spawn(vec![
        Reply::ToolCall {
            name: "bash".into(),
            arguments: json!({"command": "exit 3"}),
        },
        Reply::Text("recovered".into()),
    ]);
    let sessions = tempdir("print-failure");

    let output = run_print(&server, sessions.path(), "json-events");
    let bodies = server.bodies();
    server.finish();

    assert!(
        output.status.success(),
        "a failing tool must not abort the turn\nstdout: {}\nstderr: {}",
        stdout(&output),
        stderr(&output)
    );

    let events = stdout(&output);
    assert!(
        events.contains("\"type\":\"tool_execution_end\"") && events.contains("\"is_error\":true"),
        "a non-zero shell exit must surface as an error result:\n{events}"
    );

    assert_eq!(bodies.len(), 2, "requests: {bodies:#?}");
    assert!(
        bodies[1].contains("[exit code 3]"),
        "the real failure message must reach the model:\n{}",
        bodies[1]
    );
}

#[test]
fn print_mode_runs_the_read_tool_against_a_real_file() {
    let sessions = tempdir("print-read");
    let target = sessions.path().join("note.txt");
    std::fs::write(&target, "file-content-marker").expect("write fixture");

    let server = ModelServer::spawn(vec![
        Reply::ToolCall {
            name: "read".into(),
            arguments: json!({"path": target.to_string_lossy()}),
        },
        Reply::Text("read it".into()),
    ]);

    let output = run_print(&server, sessions.path(), "text");
    let bodies = server.bodies();
    server.finish();

    assert!(
        output.status.success(),
        "print mode failed\nstdout: {}\nstderr: {}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        stdout(&output).contains("read it"),
        "the final assistant text must be printed:\n{}",
        stdout(&output)
    );

    assert_eq!(bodies.len(), 2, "requests: {bodies:#?}");
    assert!(
        bodies[1].contains("file-content-marker"),
        "the file contents must reach the model through the tool result:\n{}",
        bodies[1]
    );
    assert!(
        !bodies[1].contains("(stub)"),
        "the stub executor must not answer tool calls:\n{}",
        bodies[1]
    );
}

// ---------------------------------------------------------------------------
// RPC mode
// ---------------------------------------------------------------------------

#[test]
fn rpc_mode_executes_the_bash_tool_and_feeds_the_result_back() {
    let server = ModelServer::spawn(vec![
        Reply::ToolCall {
            name: "bash".into(),
            arguments: json!({"command": MARKER_COMMAND}),
        },
        Reply::Text("finished".into()),
    ]);
    let sessions = tempdir("rpc");

    let mut child = pi_command(&server, sessions.path())
        .args(["--model", "openai/gpt-4o-mini", "--rpc"])
        .arg("--session-dir")
        .arg(sessions.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn pi --rpc");

    {
        let stdin = child.stdin.as_mut().expect("child stdin");
        stdin
            .write_all(
                br#"{"jsonrpc":"2.0","id":1,"method":"prompt","params":{"text":"run the tool"}}
"#,
            )
            .expect("write prompt");
        stdin.flush().expect("flush prompt");
    }
    // Closing stdin makes the server drain the turn and exit 0.
    drop(child.stdin.take());

    let mut stdout_text = String::new();
    let mut stderr_text = String::new();
    {
        let mut out = BufReader::new(child.stdout.take().expect("child stdout"));
        let mut line = String::new();
        while out.read_line(&mut line).unwrap_or(0) > 0 {
            stdout_text.push_str(&line);
            line.clear();
        }
    }
    child
        .stderr
        .take()
        .expect("child stderr")
        .read_to_string(&mut stderr_text)
        .ok();
    let status = child.wait().expect("wait for pi");
    let bodies = server.bodies();
    server.finish();

    assert!(
        status.success(),
        "pi --rpc failed\nstdout: {stdout_text}\nstderr: {stderr_text}"
    );

    assert_eq!(
        bodies.len(),
        2,
        "RPC mode must feed the tool result back; requests: {bodies:#?}"
    );
    assert!(
        bodies[1].contains(MARKER),
        "the RPC tool result must contain the marker printed by `bash`:\n{}",
        bodies[1]
    );
    assert!(
        !bodies[1].contains("(stub)"),
        "the RPC mode must not use the stub executor:\n{}",
        bodies[1]
    );

    assert!(
        stdout_text.contains("\"type\":\"tool_execution_start\"")
            && stdout_text.contains("\"type\":\"turn_end\""),
        "RPC notifications must report the tool execution:\n{stdout_text}"
    );
}
