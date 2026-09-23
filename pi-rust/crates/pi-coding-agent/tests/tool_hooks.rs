//! End-to-end `tool_call` / `tool_result` hooks (LUM-1330).
//!
//! These tests drive the **real `pi` binary** in print mode against a
//! loopback OpenAI-compatible server, with a project-local extension that
//! uses both hooks. They exist because the unit tests around
//! `ToolCallHookOutcome` prove the *folding* rules, not that the agent
//! loop, the executor and the QuickJS host are wired together:
//!
//! * `blocked_bash_never_runs_and_the_model_sees_the_reason` — the blocked
//!   command must leave **no side effect on disk**, and the model must get
//!   the handler's `reason` back as the tool result.
//! * `patched_arguments_are_the_ones_the_executor_runs` — a handler that
//!   mutates `event.input` decides what actually executes.
//! * `tool_result_handler_replaces_what_the_model_sees` — the secret the
//!   tool read must not reach the second request; the replacement must.
//!
//! No network access and no API key are needed: the transport is loopback
//! and the credential is the literal `test-key`.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{json, Value};

/// One extension that blocks some `bash` calls, patches the rest, and
/// redacts every `read` result.
const HOOKS_EXTENSION: &str = r#"
module.exports = function (pi) {
  pi.on("tool_call", function (event) {
    if (event.toolName === "bash") {
      var command = String(event.input && event.input.command);
      if (command.indexOf("forbidden") !== -1) {
        return { block: true, reason: "blocked-by-extension:" + command };
      }
      // Upstream contract: mutate `event.input` in place to patch the call.
      event.input.command = "echo PATCHED > patched.txt";
    }
    return null;
  });

  pi.on("tool_result", function (event) {
    if (event.toolName === "read") {
      return {
        content: [{ type: "text", text: "REDACTED-BY-EXTENSION" }],
        details: { redacted: true },
      };
    }
    return undefined;
  });
};
"#;

const REDACTED: &str = "REDACTED-BY-EXTENSION";
const SECRET: &str = "SUPER_SECRET_VALUE";

// ---------------------------------------------------------------------------
// Scripted OpenAI-compatible loopback server
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Reply {
    ToolCall { name: String, arguments: Value },
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
        "id": "chatcmpl-hooks",
        "object": "chat.completion.chunk",
        "created": 1_700_000_000u64,
        "model": "gpt-4o-mini",
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }],
    });
    format!("data: {payload}\n\n")
}

fn sse_usage() -> String {
    let payload = json!({
        "id": "chatcmpl-hooks",
        "object": "chat.completion.chunk",
        "created": 1_700_000_000u64,
        "model": "gpt-4o-mini",
        "choices": [],
        "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18},
    });
    format!("data: {payload}\n\n")
}

fn sse_tool_call(name: &str, arguments: &Value) -> String {
    let encoded = arguments.to_string();
    let mut body = String::new();
    body.push_str(&sse_chunk(
        json!({"role": "assistant", "content": null}),
        None,
    ));
    body.push_str(&sse_chunk(
        json!({"tool_calls": [{
            "index": 0,
            "id": "call_hook",
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
    body.push_str(&sse_chunk(
        json!({"role": "assistant", "content": ""}),
        None,
    ));
    body.push_str(&sse_chunk(json!({"content": text}), None));
    body.push_str(&sse_chunk(json!({}), Some("stop")));
    body.push_str(&sse_usage());
    body.push_str("data: [DONE]\n\n");
    body
}

struct ModelServer {
    addr: SocketAddr,
    bodies: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ModelServer {
    fn spawn(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
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

    fn finish(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

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

fn run_print(server: &ModelServer, sessions: &Path, project: &Path) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pi"));
    cmd.env_clear()
        .current_dir(project)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", sessions)
        .env("LANG", "C.UTF-8")
        .env("OPENAI_API_KEY", "test-key")
        .env("OPENAI_BASE_URL", server.url())
        .args(["--model", "openai/gpt-4o-mini", "--print", "use the tool"])
        // `.pi/extensions` is project-local, so the directory has to be
        // trusted before its files load.
        .arg("--approve")
        .arg("--session-dir")
        .arg(sessions);
    cmd.output().expect("failed to spawn pi")
}

fn tempdir(label: &str) -> tempfile::TempDir {
    tempfile::TempDir::with_prefix(format!("pi-tool-hooks-{label}-{}-", std::process::id()))
        .expect("tempdir")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Install `HOOKS_EXTENSION` as the project's only extension.
fn install_extension(project: &Path) {
    let dir = project.join(".pi").join("extensions");
    std::fs::create_dir_all(&dir).expect("mkdir extensions dir");
    std::fs::write(dir.join("hooks.js"), HOOKS_EXTENSION).expect("write extension");
}

/// The request the model made *after* the tool ran — the one that carries
/// the tool result back.
fn second_request(bodies: &[String]) -> &str {
    bodies.get(1).map(String::as_str).unwrap_or("")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn blocked_bash_never_runs_and_the_model_sees_the_reason() {
    let server = ModelServer::spawn(vec![
        Reply::ToolCall {
            name: "bash".into(),
            arguments: json!({"command": "echo forbidden > blocked-marker.txt"}),
        },
        Reply::Text("finished".into()),
    ]);
    let sessions = tempdir("block");
    let project = tempdir("block-project");
    install_extension(project.path());

    let output = run_print(&server, sessions.path(), project.path());
    let bodies = server.bodies();
    server.finish();

    assert!(
        output.status.success(),
        "print mode failed\nstdout: {}\nstderr: {}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        !project.path().join("blocked-marker.txt").exists(),
        "the blocked command must never reach the shell"
    );
    let feedback = second_request(&bodies);
    assert!(
        feedback.contains("blocked-by-extension"),
        "the handler's reason must reach the model:\n{feedback}"
    );
    assert!(
        feedback.contains("tool call blocked"),
        "the loop must report the block as an error tool result:\n{feedback}"
    );
}

#[test]
fn patched_arguments_are_the_ones_the_executor_runs() {
    let server = ModelServer::spawn(vec![
        Reply::ToolCall {
            name: "bash".into(),
            arguments: json!({"command": "echo ORIGINAL > original.txt"}),
        },
        Reply::Text("finished".into()),
    ]);
    let sessions = tempdir("patch");
    let project = tempdir("patch-project");
    install_extension(project.path());

    let output = run_print(&server, sessions.path(), project.path());
    server.finish();

    assert!(
        output.status.success(),
        "print mode failed\nstdout: {}\nstderr: {}",
        stdout(&output),
        stderr(&output)
    );
    let patched = project.path().join("patched.txt");
    assert!(
        patched.exists(),
        "the patched command must be the one that ran (project: {:?})",
        std::fs::read_dir(project.path())
            .map(|entries| entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name())
                .collect::<Vec<_>>())
            .unwrap_or_default()
    );
    assert_eq!(
        std::fs::read_to_string(&patched)
            .expect("read patched.txt")
            .trim(),
        "PATCHED"
    );
    assert!(
        !project.path().join("original.txt").exists(),
        "the model's original command must not run"
    );
}

#[test]
fn tool_result_handler_replaces_what_the_model_sees() {
    let server = ModelServer::spawn(vec![
        Reply::ToolCall {
            name: "read".into(),
            arguments: json!({"path": "secret.txt"}),
        },
        Reply::Text("finished".into()),
    ]);
    let sessions = tempdir("redact");
    let project = tempdir("redact-project");
    install_extension(project.path());
    std::fs::write(project.path().join("secret.txt"), SECRET).expect("write secret");

    let output = run_print(&server, sessions.path(), project.path());
    let bodies = server.bodies();
    server.finish();

    assert!(
        output.status.success(),
        "print mode failed\nstdout: {}\nstderr: {}",
        stdout(&output),
        stderr(&output)
    );
    let feedback = second_request(&bodies);
    assert!(
        feedback.contains(REDACTED),
        "the handler's replacement must be what the model sees:\n{feedback}"
    );
    assert!(
        !feedback.contains(SECRET),
        "the tool's real output must not leak past the handler:\n{feedback}"
    );
}
