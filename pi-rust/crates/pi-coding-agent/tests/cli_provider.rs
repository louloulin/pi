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
//! The catalog also lists Google entries and the OpenAI-compatible family
//! (DeepSeek, Groq, …), so the adapters that previously had no
//! CLI-reachable model are covered here.
//!
//! Stage 15 makes the provider list data-driven
//! (`pi_ai::providers::registry`), so the family members below reach the
//! same `OpenAiProvider` through their own base URL and credential.

use std::io::Read;
use std::net::{SocketAddr, TcpListener};
use std::process::{Command, ExitStatus, Output};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Every credential env var the router understands. Removed before each
/// spawn so the host environment cannot influence the assertions.
const CREDENTIAL_VARS: [&str; 21] = [
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_OAUTH_TOKEN",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "ANT_LING_API_KEY",
    "BASETEN_API_KEY",
    "CEREBRAS_API_KEY",
    "DEEPSEEK_API_KEY",
    "FIREWORKS_API_KEY",
    "GROQ_API_KEY",
    "HF_TOKEN",
    "MOONSHOT_API_KEY",
    "NVIDIA_API_KEY",
    "OPENROUTER_API_KEY",
    "TOGETHER_API_KEY",
    "XAI_API_KEY",
    "XIAOMI_API_KEY",
    "ZAI_API_KEY",
    "ZAI_CODING_CN_API_KEY",
];

/// Every base-URL override env var the router understands, for the same
/// environment-isolation reason.
const BASE_URL_VARS: [&str; 20] = [
    "OPENAI_BASE_URL",
    "ANTHROPIC_BASE_URL",
    "GEMINI_BASE_URL",
    "GOOGLE_BASE_URL",
    "ANT_LING_BASE_URL",
    "BASETEN_BASE_URL",
    "CEREBRAS_BASE_URL",
    "DEEPSEEK_BASE_URL",
    "FIREWORKS_BASE_URL",
    "GROQ_BASE_URL",
    "HUGGINGFACE_BASE_URL",
    "MOONSHOT_BASE_URL",
    "MOONSHOT_CN_BASE_URL",
    "NVIDIA_BASE_URL",
    "OPENROUTER_BASE_URL",
    "TOGETHER_BASE_URL",
    "XAI_BASE_URL",
    "XIAOMI_BASE_URL",
    "ZAI_BASE_URL",
    "ZAI_CODING_CN_BASE_URL",
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
    for var in BASE_URL_VARS {
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
            stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
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

    /// Wait for the child's request head.
    ///
    /// The child is already dead by the time this runs (`cmd.output()`
    /// blocked on it), so a timeout means it exited *without* dialing —
    /// which is how the known extension-host crash in LUM-1083 shows up
    /// here: the process dies during startup and the only visible symptom
    /// is this 30s wait. Report the exit status and stderr so a CI failure
    /// names the real cause instead of looking like a provider timeout.
    fn request_head(&self, child: &Output) -> String {
        match self.requests.recv_timeout(Duration::from_secs(30)) {
            Ok(head) => head,
            Err(err) => panic!(
                "provider never dialed the loopback capture server ({err});\n\
                 child exit code: {:?}, signal: {:?},\nchild stderr: {}",
                child.status.code(),
                termination_signal(&child.status),
                String::from_utf8_lossy(&child.stderr)
            ),
        }
    }
}

/// The signal that killed a child, when it did not exit normally. A crash
/// (`SIGSEGV` / `SIGABRT`) is diagnosable only through this — `ExitStatus`
/// has no portable accessor for it.
#[cfg(unix)]
fn termination_signal(status: &ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn termination_signal(_status: &ExitStatus) -> Option<i32> {
    None
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
    for var in BASE_URL_VARS {
        if var != base_url_var {
            cmd.env_remove(var);
        }
    }
    let output = cmd.output().expect("failed to spawn pi");
    drop(sessions);
    let head = capture.request_head(&output);
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

/// Loopback server that accepts up to `max_requests` connections, reads
/// each request head, and closes without responding. Every attempt
/// therefore fails with a transport error, which is exactly the shape the
/// provider retry loop exists to absorb.
struct FlakyCapture {
    addr: SocketAddr,
    requests: Receiver<String>,
}

impl FlakyCapture {
    fn spawn(max_requests: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for _ in 0..max_requests {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
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
                if tx
                    .send(String::from_utf8_lossy(&head).into_owned())
                    .is_err()
                {
                    return;
                }
                // Dropping the stream closes it without a response.
            }
        });
        Self { addr, requests: rx }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Every request head the child dialed, stopping at the first idle gap
    /// (the child has already exited by the time this runs).
    fn requests(&self) -> Vec<String> {
        let mut heads = Vec::new();
        while let Ok(head) = self.requests.recv_timeout(Duration::from_millis(1_500)) {
            heads.push(head);
        }
        heads
    }
}

/// Spawn `pi` against `capture` from a throwaway `$HOME` that holds
/// `settings.json`.
fn pi_with_user_settings(capture: &FlakyCapture, settings: Option<&str>) -> std::process::Output {
    let home = tempfile::TempDir::with_prefix("pi-retry-home-").expect("tempdir");
    if let Some(settings) = settings {
        let agent_dir = home.path().join(".pi").join("agent");
        std::fs::create_dir_all(&agent_dir).expect("mkdir agent dir");
        std::fs::write(agent_dir.join("settings.json"), settings).expect("write settings");
    }
    let sessions = tempfile::TempDir::with_prefix("pi-retry-session-").expect("tempdir");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pi"));
    cmd.args(["--model", "anthropic/claude-sonnet-4-5", "--print", "hello"])
        .arg("--session-dir")
        .arg(sessions.path())
        .env("HOME", home.path())
        .env("ANTHROPIC_BASE_URL", capture.url())
        .env("ANTHROPIC_API_KEY", "test-key");
    for var in CREDENTIAL_VARS {
        if var != "ANTHROPIC_API_KEY" {
            cmd.env_remove(var);
        }
    }
    for var in BASE_URL_VARS {
        if var != "ANTHROPIC_BASE_URL" {
            cmd.env_remove(var);
        }
    }
    let output = cmd.output().expect("failed to spawn pi");
    drop(home);
    drop(sessions);
    output
}

#[test]
fn provider_retry_is_off_by_default() {
    let capture = FlakyCapture::spawn(1);
    let _ = pi_with_user_settings(&capture, None);

    assert_eq!(
        capture.requests().len(),
        1,
        "without `retry.provider` the CLI must not resent the failed request"
    );
}

#[test]
fn provider_retry_settings_retry_a_failed_request() {
    let capture = FlakyCapture::spawn(3);
    let _ = pi_with_user_settings(
        &capture,
        Some(r#"{"retry":{"provider":{"maxRetries":2,"maxRetryDelayMs":1000}}}"#),
    );

    let requests = capture.requests();
    assert_eq!(
        requests.len(),
        3,
        "`retry.provider.maxRetries: 2` must resend twice before giving up"
    );
    assert!(
        requests.iter().all(|head| head.contains("/v1/messages")),
        "every attempt must be a real anthropic request: {requests:?}"
    );
}

#[test]
fn missing_anthropic_key_exits_78_and_names_the_env_var() {
    let output = pi(&["--model", "anthropic/claude-sonnet-4-5", "--print", "hello"]);
    assert_eq!(
        output.status.code(),
        Some(78),
        "stderr: {}",
        stderr(&output)
    );
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
    assert_eq!(
        output.status.code(),
        Some(78),
        "stderr: {}",
        stderr(&output)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("OPENAI_API_KEY"));
}

#[test]
fn missing_gemini_key_exits_78_and_names_the_env_var() {
    let output = pi(&["--model", "google/gemini-2.5-flash", "--print", "hello"]);
    assert_eq!(
        output.status.code(),
        Some(78),
        "stderr: {}",
        stderr(&output)
    );
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
    for id in [
        "gemini-2.5-pro",
        "gemini-2.5-flash",
        "gemini-2.5-flash-lite",
    ] {
        assert!(
            stdout.contains(id),
            "list-models is missing {id}:\n{stdout}"
        );
    }
}

#[test]
fn list_models_reports_gemini_pricing() {
    // JSON output carries the pricing object for models whose catalog
    // entry declares it (Gemini), and omits it for the rest.
    let output = pi(&["list-models", "--output", "json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let payload: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("list-models JSON");
    let models = payload["models"].as_array().expect("models array");
    let flash = models
        .iter()
        .find(|m| m["provider"] == "google" && m["id"] == "gemini-2.5-flash")
        .expect("gemini-2.5-flash entry");
    let pricing = &flash["pricing"];
    assert_eq!(pricing["inputPer1M"], 0.30);
    assert_eq!(pricing["outputPer1M"], 2.50);
    assert_eq!(pricing["cacheReadPer1M"], 0.075);

    let faux = models
        .iter()
        .find(|m| m["provider"] == "faux")
        .expect("faux entry");
    assert!(faux["pricing"].is_null(), "faux has no published pricing");

    // Text output surfaces the same rates inline.
    let text = pi(&["list-models"]);
    let stdout = String::from_utf8_lossy(&text.stdout);
    let line = stdout
        .lines()
        .find(|line| line.contains("google/gemini-2.5-flash "))
        .unwrap_or_else(|| panic!("google/gemini-2.5-flash line missing:\n{stdout}"));
    assert!(line.contains("0.30"), "pricing missing from `{line}`");
    assert!(line.contains("2.50"), "pricing missing from `{line}`");
}

#[test]
fn list_models_includes_the_openai_compatible_family() {
    let output = pi(&["list-models"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    for entry in [
        "deepseek/deepseek-v4-pro",
        "deepseek/deepseek-flash",
        "ant-ling/Ling-2.6-flash",
        "groq/openai/gpt-oss-120b",
        "cerebras/gpt-oss-120b",
        "zai/glm-5.3",
        "zai-coding-cn/glm-5.3",
        "moonshotai/kimi-k2.6",
        "openrouter/moonshotai/kimi-k2.6",
        "together/deepseek-ai/DeepSeek-V4-Pro",
        "fireworks/accounts/fireworks/models/kimi-k2p6",
        "nvidia/nvidia/nemotron-3-super-120b-a12b",
        "huggingface/moonshotai/Kimi-K2.6",
        "baseten/zai-org/GLM-5.2",
        "xiaomi/mimo-v2.5-pro",
        "xai/grok-4.6",
    ] {
        assert!(
            stdout.contains(entry),
            "list-models is missing {entry}:\n{stdout}"
        );
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

#[test]
fn missing_deepseek_key_exits_78_and_names_the_env_var() {
    let output = pi(&["--model", "deepseek/deepseek-v4-pro", "--print", "hello"]);
    assert_eq!(
        output.status.code(),
        Some(78),
        "stderr: {}",
        stderr(&output)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("DEEPSEEK_API_KEY"));
}

#[test]
fn deepseek_model_dials_the_chat_completions_endpoint() {
    assert_request_path(
        "DEEPSEEK_BASE_URL",
        "DEEPSEEK_API_KEY",
        "deepseek/deepseek-v4-pro",
        "POST /chat/completions",
    );
}

#[test]
fn ant_ling_model_dials_the_chat_completions_endpoint() {
    assert_request_path(
        "ANT_LING_BASE_URL",
        "ANT_LING_API_KEY",
        "ant-ling/Ling-2.6-flash",
        "POST /chat/completions",
    );
}

#[test]
fn xai_model_dials_the_responses_endpoint() {
    // `xai` is not `openai-responses`: it reuses the Responses adapter but
    // must dial its own host and credential.
    assert_request_path(
        "XAI_BASE_URL",
        "XAI_API_KEY",
        "xai/grok-4.6",
        "POST /responses",
    );
}

#[test]
fn openrouter_model_dials_the_chat_completions_endpoint() {
    assert_request_path(
        "OPENROUTER_BASE_URL",
        "OPENROUTER_API_KEY",
        "openrouter/moonshotai/kimi-k2.6",
        "POST /chat/completions",
    );
}

#[test]
fn family_providers_use_their_own_base_url_override() {
    // Same adapter as OpenAI, but a distinct override var: proves the
    // lookup is data-driven rather than a switch on the provider id.
    assert_request_path(
        "ZAI_BASE_URL",
        "ZAI_API_KEY",
        "zai/glm-5.3",
        "POST /chat/completions",
    );
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
