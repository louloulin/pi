//! Stage 17 integration tests: the `pi` binary loads and runs JS
//! extensions.
//!
//! The extension host, shim and loader landed in earlier stages but were
//! only reachable from library tests — `main.rs` parsed `-e/--extension`
//! and never used it, so a user's `.pi/extensions/*.js` was dead weight.
//! These tests drive the real binary the way a user does and prove the
//! whole path works end to end:
//!
//! 1. A loopback server pretends to be the OpenAI chat-completions API.
//! 2. The first request's `tools` array must advertise the extension's
//!    tool — i.e. the model can actually see it.
//! 3. The next request carries the tool result, so a marker produced
//!    *inside the extension's JavaScript* proves the QuickJS execute
//!    path really ran (a literal in the extension's `description` would
//!    be a false positive, so the marker is assembled at runtime).
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

/// Prefix the extension tool's result text starts with. The full string
/// (`ext-echoed:<arg>`) only exists after the JS `execute` ran.
const ECHO_PREFIX: &str = "ext-echoed:";

/// A JS extension that registers `ext_echo`. The `session_start` handler
/// exercises the lifecycle dispatch the CLI performs after loading.
const ECHO_EXTENSION: &str = r#"
module.exports = function (pi) {
  pi.registerTool({
    name: "ext_echo",
    label: "Echo",
    description: "Echoes the text argument back to the model.",
    parameters: {
      type: "object",
      properties: { text: { type: "string" } },
      required: ["text"],
    },
    execute: async function (args) {
      var text = args && typeof args.text === "string" ? args.text : "";
      return { content: [{ type: "text", text: "ext-" + "echoed:" + text }] };
    },
  });

  pi.registerTool({
    name: "ext_boom",
    label: "Boom",
    description: "Always throws.",
    parameters: { type: "object" },
    execute: function () {
      throw new Error("boom-from-extension");
    },
  });

  pi.on("session_start", function () {
    pi.appendEntry("ext-fixture", { started: true });
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
        "id": "chatcmpl-ext",
        "object": "chat.completion.chunk",
        "created": 1_700_000_000u64,
        "model": "gpt-4o-mini",
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }],
    });
    format!("data: {payload}\n\n")
}

fn sse_usage() -> String {
    let payload = json!({
        "id": "chatcmpl-ext",
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
            "id": "call_ext",
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

fn run_print(server: &ModelServer, sessions: &Path, cwd: &Path, extra_args: &[&str]) -> Output {
    let mut cmd = pi_command(server, sessions, cwd);
    cmd.args(["--model", "openai/gpt-4o-mini", "--print", "use the tool"]);
    cmd.args(extra_args);
    cmd.arg("--session-dir").arg(sessions);
    cmd.output().expect("failed to spawn pi")
}

fn tempdir(label: &str) -> tempfile::TempDir {
    tempfile::TempDir::with_prefix(format!("pi-cli-ext-{label}-{}-", std::process::id()))
        .expect("tempdir")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Write `ECHO_EXTENSION` into `<project>/.pi/extensions/echo.js`.
fn install_project_extension(project: &Path) -> std::path::PathBuf {
    let dir = project.join(".pi").join("extensions");
    std::fs::create_dir_all(&dir).expect("mkdir extensions dir");
    let path = dir.join("echo.js");
    std::fs::write(&path, ECHO_EXTENSION).expect("write extension");
    path
}

fn echo_call_replies() -> Vec<Reply> {
    vec![
        Reply::ToolCall {
            name: "ext_echo".into(),
            arguments: json!({"text": "hello"}),
        },
        Reply::Text("finished".into()),
    ]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn project_extensions_directory_is_discovered_and_its_tool_runs() {
    let server = ModelServer::spawn(echo_call_replies());
    let sessions = tempdir("discover");
    let project = tempdir("discover-project");
    install_project_extension(project.path());

    let output = run_print(
        &server,
        sessions.path(),
        project.path(),
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
    // The model only calls a tool it was offered — advertising it in the
    // first request is what proves the extension is wired in.
    assert!(
        bodies[0].contains("ext_echo") && bodies[0].contains("Echoes the text argument"),
        "the first request must advertise the extension tool:\n{}",
        bodies[0]
    );
    // The result string is assembled inside the JS `execute`, so seeing it
    // fed back to the model proves the QuickJS execution path ran.
    let second = &bodies[1];
    assert!(
        second.contains(&format!("{ECHO_PREFIX}hello")),
        "the extension tool result must reach the model:\n{second}"
    );
    assert!(
        !second.contains("(stub)"),
        "the stub executor's fabricated result must never reach the model:\n{second}"
    );

    let events = stdout(&output);
    assert!(
        events.contains("\"name\":\"ext_echo\"")
            && events.contains("\"type\":\"tool_execution_end\"")
            && events.contains("\"is_error\":false"),
        "expected a successful ext_echo tool_execution_end event:\n{events}"
    );
}

#[test]
fn explicit_extension_flag_loads_a_file_outside_the_project() {
    let server = ModelServer::spawn(echo_call_replies());
    let sessions = tempdir("explicit");
    let project = tempdir("explicit-project");
    let elsewhere = tempdir("explicit-elsewhere");
    let extension = elsewhere.path().join("echo.js");
    std::fs::write(&extension, ECHO_EXTENSION).expect("write extension");

    let flag = extension.to_string_lossy().into_owned();
    let output = run_print(
        &server,
        sessions.path(),
        project.path(),
        &["--extension", &flag, "--output-format", "json-events"],
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
    assert!(
        bodies[0].contains("ext_echo"),
        "`-e <file>` must advertise the extension tool:\n{}",
        bodies[0]
    );
    assert!(
        bodies[1].contains(&format!("{ECHO_PREFIX}hello")),
        "`-e <file>` must execute the extension tool:\n{}",
        bodies[1]
    );
}

#[test]
fn extensions_dir_flag_loads_every_extension_in_a_directory() {
    let server = ModelServer::spawn(echo_call_replies());
    let sessions = tempdir("dir");
    let project = tempdir("dir-project");
    let dir = tempdir("dir-registry");
    std::fs::write(dir.path().join("echo.js"), ECHO_EXTENSION).expect("write extension");

    let flag = dir.path().to_string_lossy().into_owned();
    let output = run_print(
        &server,
        sessions.path(),
        project.path(),
        &["--extensions-dir", &flag, "--output-format", "json-events"],
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
    assert!(
        bodies[0].contains("ext_echo"),
        "`--extensions-dir` must advertise the extension tool:\n{}",
        bodies[0]
    );
}

#[test]
fn no_extensions_flag_skips_discovery() {
    let server = ModelServer::spawn(vec![
        Reply::ToolCall {
            name: "ext_echo".into(),
            arguments: json!({"text": "hello"}),
        },
        Reply::Text("finished".into()),
    ]);
    let sessions = tempdir("no-ext");
    let project = tempdir("no-ext-project");
    install_project_extension(project.path());

    let output = run_print(
        &server,
        sessions.path(),
        project.path(),
        &["--no-extensions", "--output-format", "json-events"],
    );
    let bodies = server.bodies();
    server.finish();

    assert!(
        output.status.success(),
        "a model calling a missing tool must still finish the turn\nstdout: {}\nstderr: {}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(bodies.len(), 2, "requests: {bodies:#?}");
    assert!(
        !bodies[0].contains("ext_echo"),
        "`--no-extensions` must keep extension tools out of the tool list:\n{}",
        bodies[0]
    );
    // The built-in bundle is still advertised and the unknown tool call is
    // reported back as an error result instead of crashing the turn.
    assert!(
        bodies[0].contains("\"bash\""),
        "built-in tools must survive `--no-extensions`:\n{}",
        bodies[0]
    );
    assert!(
        bodies[1].contains("unknown tool"),
        "the missing tool must surface as an error result:\n{}",
        bodies[1]
    );
}

#[test]
fn no_extensions_conflicts_with_extension_flags() {
    let server = ModelServer::spawn(vec![]);
    let sessions = tempdir("conflict");
    let project = tempdir("conflict-project");
    let extension = project.path().join("echo.js");
    std::fs::write(&extension, ECHO_EXTENSION).expect("write extension");

    let flag = extension.to_string_lossy().into_owned();
    let output = run_print(
        &server,
        sessions.path(),
        project.path(),
        &["--no-extensions", "-e", &flag],
    );
    server.finish();

    assert_eq!(
        output.status.code(),
        Some(2),
        "contradictory flags must fail fast\nstdout: {}\nstderr: {}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("cannot be used with"),
        "clap must explain the conflict:\n{}",
        stderr(&output)
    );
}

#[test]
fn a_throwing_extension_tool_becomes_an_error_result() {
    let server = ModelServer::spawn(vec![
        Reply::ToolCall {
            name: "ext_boom".into(),
            arguments: json!({}),
        },
        Reply::Text("recovered".into()),
    ]);
    let sessions = tempdir("boom");
    let project = tempdir("boom-project");
    install_project_extension(project.path());

    let output = run_print(
        &server,
        sessions.path(),
        project.path(),
        &["--output-format", "json-events"],
    );
    let bodies = server.bodies();
    server.finish();

    assert!(
        output.status.success(),
        "an extension exception must not abort the turn\nstdout: {}\nstderr: {}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(bodies.len(), 2, "requests: {bodies:#?}");
    assert!(
        bodies[1].contains("boom-from-extension"),
        "the JS exception must reach the model:\n{}",
        bodies[1]
    );

    let events = stdout(&output);
    assert!(
        events.contains("\"name\":\"ext_boom\"") && events.contains("\"is_error\":true"),
        "the failed extension call must be reported as an error result:\n{events}"
    );
}
