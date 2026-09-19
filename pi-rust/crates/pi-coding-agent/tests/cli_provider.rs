//! Integration tests for CLI provider selection (Stage 14).
//!
//! `main.rs` used to hard-code `FauxProvider` in print, RPC and
//! interactive mode, so `--model anthropic/...` still streamed from the
//! faux provider. These tests spawn the real binary and assert, two
//! different ways, that the selected model now picks its own adapter:
//!
//! 1. **Configuration errors** — a remote model without its credential
//!    exits 78 and names the env var to set, instead of silently falling
//!    back to faux.
//! 2. **Wire proof** — with a credential set and the provider's
//!    `*_BASE_URL` pointing at a loopback capture server, the binary
//!    dials that server and issues the provider-specific request path
//!    (`/chat/completions`, `/v1/messages`,
//!    `/models/<id>:streamGenerateContent`). The capture server reads the
//!    request head and closes without responding, so no network access or
//!    real API key is needed.
//!
//! The catalog also lists Google entries, so the Stage 13 `GoogleProvider`
//! adapter that previously had no CLI-reachable model is covered here.

use std::io::Read;
use std::net::{SocketAddr, TcpListener};
use std::process::{Command, Output};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Every credential env var the router understands. Removed before each
/// spawn so the host environment cannot influence the assertions.
const CREDENTIAL_VARS: [&str; 6] = [
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_OAUTH_TOKEN",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
];

/// Spawn `pi` with a cleared credential environment.
///
/// Print-mode invocations get their own `--session-dir`, so tests never
/// touch the developer's `~/.pi/sessions`.
fn pi(args: &[&str]) -> Output {
    let sessions = tempfile::TempDir::with_prefix(format!(
        "pi-provider-test-{}-{}-",
        std::process::id(),
        args.join("_").replace('/', "-")
    ))
    .expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pi"));
    cmd.args(args);
    if args.contains(&"--print") {
        cmd.arg("--session-dir").arg(sessions.path());
    }
    for var in CREDENTIAL_VARS {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("failed to spawn pi");
    drop(sessions);
    output
}

/// Loopback server that captures one request's head and closes.
struct Capture {
    addr: SocketAddr,
    requests: Receiver<String>,
    handle: JoinHandle<()>,
}

impl Capture {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .ok();
            let mut head = Vec::new();
            let mut chunk = [0u8; 1024];
            while let Ok(n) = stream.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                head.extend_from_slice(&chunk[..n]);
                if head.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&head).into_owned());
            // Dropping the stream closes the connection: the provider
            // surfaces a stream error, which is all this test needs.
        });
        Self {
            addr,
            requests: rx,
            handle,
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn request_head(&self) -> String {
        self.requests
            .recv_timeout(Duration::from_secs(30))
            .expect("provider never dialed the loopback capture server")
    }
}

/// Run one provider end-to-end and assert the captured request path.
fn assert_request_path(base_url_var: &str, key_var: &str, model_arg: &str, expected: &str) {
    let capture = Capture::spawn();
    let sessions = tempfile::TempDir::with_prefix("pi-provider-test-").expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pi"));
    cmd.args(["--model", model_arg, "--print", "hello"])
        .arg("--session-dir")
        .arg(sessions.path())
        .env(base_url_var, capture.url())
        .env(key_var, "test-key");
    for var in CREDENTIAL_VARS {
        if var != key_var {
            cmd.env_remove(var);
        }
    }
    let output = cmd.output().expect("failed to spawn pi");
    drop(sessions);
    let head = capture.request_head();
    capture.handle.join().ok();

    let ok = head.contains(expected);
    assert!(
        ok,
        "expected `{expected}` in the request to {model_arg} (stderr: {})",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        head.starts_with("POST "),
        "provider request must be a POST:\n{head}"
    );
}

#[test]
fn missing_anthropic_key_exits_78_and_names_the_env_var() {
    let output = pi(&["--model", "anthropic/claude-sonnet-4-5", "--print", "hello"]);
    assert_eq!(output.status.code(), Some(78), "stderr: {}", stderr(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ANTHROPIC_API_KEY"),
        "stderr should name the env var to set: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "no turn should run: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn missing_openai_key_exits_78_and_names_the_env_var() {
    let output = pi(&["--model", "openai/gpt-4o-mini", "--print", "hello"]);
    assert_eq!(output.status.code(), Some(78), "stderr: {}", stderr(&output));
    assert!(String::from_utf8_lossy(&output.stderr).contains("OPENAI_API_KEY"));
}

#[test]
fn missing_gemini_key_exits_78_and_names_the_env_var() {
    let output = pi(&["--model", "google/gemini-2.5-flash", "--print", "hello"]);
    assert_eq!(output.status.code(), Some(78), "stderr: {}", stderr(&output));
    assert!(String::from_utf8_lossy(&output.stderr).contains("GEMINI_API_KEY"));
}

#[test]
fn print_mode_defaults_to_faux_without_credentials() {
    // The default model stays faux, so print mode still works offline.
    let output = pi(&["--print", "hello"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("(faux) hello"),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn list_models_includes_the_google_catalog() {
    let output = pi(&["list-models"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    for id in ["gemini-2.5-pro", "gemini-2.5-flash", "gemini-2.5-flash-lite"] {
        assert!(stdout.contains(id), "list-models is missing {id}:\n{stdout}");
    }
}

#[test]
fn anthropic_model_dials_the_anthropic_messages_endpoint() {
    assert_request_path(
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_API_KEY",
        "anthropic/claude-sonnet-4-5",
        "POST /v1/messages",
    );
}

#[test]
fn anthropic_auth_token_is_an_accepted_credential() {
    assert_request_path(
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_AUTH_TOKEN",
        "anthropic/claude-opus-4-5",
        "POST /v1/messages",
    );
}

#[test]
fn openai_model_dials_the_chat_completions_endpoint() {
    assert_request_path(
        "OPENAI_BASE_URL",
        "OPENAI_API_KEY",
        "openai/gpt-4o-mini",
        "POST /chat/completions",
    );
}

#[test]
fn google_model_dials_the_stream_generate_content_endpoint() {
    assert_request_path(
        "GEMINI_BASE_URL",
        "GEMINI_API_KEY",
        "google/gemini-2.5-flash",
        "POST /models/gemini-2.5-flash:streamGenerateContent",
    );
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
