//! Stage 28 integration tests: `pi.exec` from an extension, driven through
//! the real `pi` binary in print mode.
//!
//! The host bridge (`host_exec` → `pi.exec`) landed in LUM-1107 (commit
//! `8062c7996`) and is covered at the library level by
//! `pi-extensions/tests/pi_exec.rs`, which calls
//! [`JsExtensionHost::execute_tool`] directly. What those tests cannot see
//! is the path a user actually exercises: `pi --print` loading an extension
//! from a file, the model calling the extension's tool, and the child
//! process' stdout / stderr / exit code travelling back through the tool
//! result to the provider.
//!
//! The harness mirrors `cli_extensions.rs`: a loopback server plays the
//! OpenAI chat-completions API, the first request must advertise the
//! extension's tool, and the second must carry its result. Every marker is
//! assembled *inside the extension's JavaScript* from the `pi.exec` return
//! value — a literal in the fixture would be a false positive, since the
//! `tools` array on every request already contains the fixture's strings.
//!
//! The tests spawn real POSIX tools (`sh`, `printf`, `pwd`), so the file is
//! Unix-only; the `pi.exec` bridge itself is platform-neutral.

#![cfg(unix)]

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

/// An extension whose every observable value is derived from a `pi.exec`
/// result at runtime.
///
/// * `ext_exec` runs `printf` in a shell and returns
///   `ext-exec:<stdout>:code=<code>` — stdio plus exit code in one marker.
///   The shell expands its own pid into the marker, so the value cannot be
///   hard-coded in the fixture.
/// * `ext_exec_fail` runs a command that writes its own pid plus `boom` to
///   stderr and exits 7, then reports `code`, `stderr` and `killed`, so a
///   non-zero exit is proven to be data rather than a host-level failure.
/// * `ext_exec_pwd` reports the cwd `pi.exec` defaulted to.
/// * The `session_start` handler is the event-dispatch path: it awaits
///   `pi.exec` and records the result through `pi.appendEntry`.
const EXEC_EXTENSION: &str = r#"
module.exports = function (pi) {
  pi.registerTool({
    name: "ext_exec",
    label: "Exec",
    description: "Runs printf through pi.exec and reports stdout and exit code.",
    parameters: { type: "object", properties: {} },
    execute: async function () {
      var result = await pi.exec("sh", ["-c", "printf 'ext-exec-%s' \"$$\""]);
      var marker = "ext" + "-exec:" + result.stdout + ":code=" + result.code;
      return { content: [{ type: "text", text: marker }] };
    },
  });

  pi.registerTool({
    name: "ext_exec_fail",
    label: "Exec fail",
    description: "Runs a failing command through pi.exec and reports code and stderr.",
    parameters: { type: "object", properties: {} },
    execute: async function () {
      var result = await pi.exec("sh", ["-c", "echo boom-$$ >&2; exit 7"]);
      var marker =
        "ext" + "-exec-fail:code=" + result.code +
        ":stderr=" + result.stderr.trim() +
        ":killed=" + result.killed;
      return { content: [{ type: "text", text: marker }] };
    },
  });

  pi.registerTool({
    name: "ext_exec_pwd",
    label: "Exec pwd",
    description: "Reports the working directory pi.exec defaults to.",
    parameters: { type: "object", properties: {} },
    execute: async function () {
      var result = await pi.exec("pwd", []);
      return {
        content: [{
          type: "text",
          text: "ext" + "-exec-pwd:" + result.stdout.trim() + ":code=" + result.code,
        }],
      };
    },
  });

  pi.on("session_start", async function () {
    var result = await pi.exec("sh", ["-c", "printf 'ext-entry-%s' \"$$\""]);
    pi.appendEntry("exec_entry", { stdout: result.stdout, code: result.code });
  });
};
"#;

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
        "id": "chatcmpl-exec",
        "object": "chat.completion.chunk",
        "created": 1_700_000_000u64,
        "model": "gpt-4o-mini",
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }],
    });
    format!("data: {payload}\n\n")
}

fn sse_usage() -> String {
    let payload = json!({
        "id": "chatcmpl-exec",
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
            "id": "call_exec",
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

fn pi_command(server: &ModelServer, sessions: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pi"));
    cmd.env_clear()
        .current_dir(cwd)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", sessions)
        .env("LANG", "C.UTF-8")
        .env("OPENAI_API_KEY", "test-key")
        .env("OPENAI_BASE_URL", server.url());
    cmd
}

/// Run `pi --print` with `EXEC_EXTENSION` loaded through `-e`.
fn run_print(
    server: &ModelServer,
    sessions: &Path,
    cwd: &Path,
    extension: &Path,
    extra_args: &[&str],
) -> Output {
    let flag = extension.to_string_lossy().into_owned();
    let mut cmd = pi_command(server, sessions, cwd);
    cmd.args(["--model", "openai/gpt-4o-mini", "--print", "use the tool"]);
    cmd.args(["--extension", &flag]);
    cmd.args(extra_args);
    cmd.arg("--session-dir").arg(sessions);
    cmd.output().expect("failed to spawn pi")
}

fn tempdir(label: &str) -> tempfile::TempDir {
    tempfile::TempDir::with_prefix(format!("pi-cli-exec-{label}-{}-", std::process::id()))
        .expect("tempdir")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Write `EXEC_EXTENSION` into `<dir>/exec.js` and return its path.
fn install_exec_extension(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("exec.js");
    std::fs::write(&path, EXEC_EXTENSION).expect("write extension");
    path
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The success path: an extension tool awaits `pi.exec`, the child's
/// stdout and exit code are folded into the result string, and that
/// string reaches the provider on the next request.
#[test]
fn extension_tool_pi_exec_result_reaches_the_model() {
    let server = ModelServer::spawn(vec![
        Reply::ToolCall {
            name: "ext_exec".into(),
            arguments: json!({}),
        },
        Reply::Text("finished".into()),
    ]);
    let sessions = tempdir("ok");
    let project = tempdir("ok-project");
    let extension = install_exec_extension(project.path());

    let output = run_print(
        &server,
        sessions.path(),
        project.path(),
        &extension,
        &["--output-format", "json-events"],
    );
    let bodies = server.bodies();
    server.finish();

    assert!(
        output.status.success(),
        "print mode failed\nstdout: {}\nstderr: {}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("loaded 1 extension"),
        "the CLI must report the extension it loaded:\n{}",
        stderr(&output)
    );
    assert_eq!(
        bodies.len(),
        2,
        "the loop must issue a second request carrying the tool result; requests: {bodies:#?}"
    );
    // The model only calls a tool it was offered.
    assert!(
        bodies[0].contains("ext_exec"),
        "the first request must advertise the extension tool:\n{}",
        bodies[0]
    );
    // `ext-exec:ext-exec-<pid>:code=0` is assembled in JS from the
    // `ExecResult`, and the pid is expanded by the shell at runtime — so
    // the marker cannot exist unless a real child ran. It must not leak
    // into the first request either, or the assertion could be met
    // without `pi.exec` ever being called.
    assert!(
        !bodies[0].contains("ext-exec:ext-exec-"),
        "the marker is runtime-only and must not leak into the tool list:\n{}",
        bodies[0]
    );
    let result_body = &bodies[1];
    let prefix = "ext-exec:ext-exec-";
    let start = result_body
        .find(prefix)
        .unwrap_or_else(|| panic!("the pi.exec result must reach the model:\n{result_body}"))
        + prefix.len();
    let tail = &result_body[start..];
    let end = tail
        .find(":code=0")
        .unwrap_or_else(|| panic!("the exit code must reach the model:\n{result_body}"));
    let pid = &tail[..end];
    assert!(
        !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()),
        "the shell must have expanded `$$` into a pid:\n{result_body}"
    );

    let events = stdout(&output);
    assert!(
        events.contains("\"name\":\"ext_exec\"")
            && events.contains("\"type\":\"tool_execution_end\"")
            && events.contains("\"is_error\":false"),
        "expected a successful ext_exec tool_execution_end event:\n{events}"
    );
}

/// A non-zero child exit is data, not a fatal error: the extension reads
/// `code === 7` and the child's stderr, and the `pi` process itself still
/// finishes with exit code 0.
#[test]
fn extension_tool_pi_exec_nonzero_exit_is_reported_not_fatal() {
    let server = ModelServer::spawn(vec![
        Reply::ToolCall {
            name: "ext_exec_fail".into(),
            arguments: json!({}),
        },
        Reply::Text("finished".into()),
    ]);
    let sessions = tempdir("fail");
    let project = tempdir("fail-project");
    let extension = install_exec_extension(project.path());

    let output = run_print(
        &server,
        sessions.path(),
        project.path(),
        &extension,
        &["--output-format", "json-events"],
    );
    let bodies = server.bodies();
    server.finish();

    assert!(
        output.status.success(),
        "a child's non-zero exit must not fail the `pi` process\nstdout: {}\nstderr: {}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(bodies.len(), 2, "requests: {bodies:#?}");
    assert!(
        !bodies[0].contains("ext-exec-fail:code=7"),
        "the failure marker is runtime-only and must not leak into the tool list:\n{}",
        bodies[0]
    );
    // `stderr` carries the child's own pid, so the value must have come
    // from the running process rather than a literal in the fixture.
    let result_body = &bodies[1];
    let prefix = "ext-exec-fail:code=7:stderr=boom-";
    let start = result_body.find(prefix).unwrap_or_else(|| {
        panic!("the extension must see code 7 and the child's stderr:\n{result_body}")
    }) + prefix.len();
    let tail = &result_body[start..];
    let end = tail
        .find(":killed=false")
        .unwrap_or_else(|| panic!("the extension must see `killed: false`:\n{result_body}"));
    let pid = &tail[..end];
    assert!(
        !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()),
        "the child's stderr must carry the expanded `$$`:\n{result_body}"
    );

    let events = stdout(&output);
    assert!(
        events.contains("\"name\":\"ext_exec_fail\"")
            && events.contains("\"type\":\"tool_execution_end\"")
            && events.contains("\"is_error\":false"),
        "a non-zero child exit is a successful tool call:\n{events}"
    );
}

/// The event-handler path: `session_start` awaits `pi.exec` and records
/// the result with `pi.appendEntry`. Print mode drains the side effects
/// into the session store, so the entry (with the child's stdout) has to
/// show up in the session the process produced.
#[test]
fn extension_session_start_exec_is_persisted_as_a_session_entry() {
    let server = ModelServer::spawn(vec![Reply::Text("finished".into())]);
    let sessions = tempdir("entry");
    let project = tempdir("entry-project");
    let extension = install_exec_extension(project.path());
    let session_id = "ext-exec-entry";

    let output = run_print(
        &server,
        sessions.path(),
        project.path(),
        &extension,
        &["--session", session_id, "--output-format", "json-events"],
    );
    let bodies = server.bodies();
    server.finish();

    assert!(
        output.status.success(),
        "print mode failed\nstdout: {}\nstderr: {}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(bodies.len(), 1, "requests: {bodies:#?}");

    let db = sessions.path().join(format!("{session_id}.sqlite"));
    let reader = pi_session::SessionReader::open(&db).expect("open session database");
    let entries = reader.iter_entries(session_id).expect("session entries");
    let payload = entries
        .iter()
        .find_map(|row| match row.entry() {
            pi_session::SessionEntry::Extension { kind, payload, .. } if kind == "exec_entry" => {
                Some(payload.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "the session_start handler's `pi.appendEntry` must be persisted: {:?}",
                entries
                    .iter()
                    .map(|row| row.entry().clone())
                    .collect::<Vec<_>>()
            )
        });
    // The payload is the `pi.exec` result captured in the handler. The
    // child's PID is expanded by the shell at runtime, so a literal in
    // the extension could not have produced it: the awaited result must
    // really have carried the child's stdout.
    let captured = payload["stdout"]
        .as_str()
        .unwrap_or_else(|| panic!("stdout must be a string: {payload}"));
    let pid = captured
        .strip_prefix("ext-entry-")
        .unwrap_or_else(|| panic!("the entry must carry the child's runtime stdout: {payload}"));
    assert!(
        !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()),
        "the shell must have expanded `$$` into a pid: {payload}"
    );
    assert_eq!(payload["code"], json!(0));
}

/// `options.cwd` is optional: without it `pi.exec` runs in the directory
/// `pi` was launched from, which is the project the session opened in.
#[test]
fn extension_pi_exec_defaults_to_the_launch_cwd() {
    let server = ModelServer::spawn(vec![
        Reply::ToolCall {
            name: "ext_exec_pwd".into(),
            arguments: json!({}),
        },
        Reply::Text("finished".into()),
    ]);
    let sessions = tempdir("cwd");
    let project = tempdir("cwd-project");
    let extension = install_exec_extension(project.path());

    let output = run_print(
        &server,
        sessions.path(),
        project.path(),
        &extension,
        &["--output-format", "json-events"],
    );
    let bodies = server.bodies();
    server.finish();

    assert!(
        output.status.success(),
        "print mode failed\nstdout: {}\nstderr: {}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(bodies.len(), 2, "requests: {bodies:#?}");

    // `pwd` prints the physical path, so compare canonical forms to stay
    // independent of a symlinked temp directory.
    let expected = std::fs::canonicalize(project.path()).expect("canonical project path");
    let expected = expected.to_string_lossy();
    let marker = format!("ext-exec-pwd:{expected}:code=0");
    assert!(
        !bodies[0].contains(&marker),
        "the cwd marker is runtime-only and must not leak into the tool list:\n{}",
        bodies[0]
    );
    assert!(
        bodies[1].contains(&marker),
        "pi.exec must default to the launch cwd ({marker}):\n{}",
        bodies[1]
    );
}
