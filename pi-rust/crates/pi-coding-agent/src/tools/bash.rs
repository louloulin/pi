//! `BashTool` — execute a shell command and capture stdout/stderr.
//!
//! Mirrors `createBashTool` from
//! `packages/coding-agent/src/core/tools/bash.ts`. The first cut uses a
//! synchronous [`std::process::Command`] so the implementation is easy to
//! audit; the agent loop bridges it onto a `tokio::task::spawn_blocking`
//! thread when needed. Permission checks land in Stage 2; for now the tool
//! runs whatever command the model asks for, gated only by cooperative
//! cancellation through the [`AbortLike`] handle.

#![cfg(not(target_arch = "wasm32"))]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::{AbortLike, AgentTool, ToolError, ToolOutput};
use pi_protocol::ToolExecutionMode;

/// `bash` tool — execute a shell command.
#[derive(Debug, Default)]
pub struct BashTool;

/// Soft ceiling for `timeout` enforced in this implementation. The model
/// is told 600s (10 minutes) is the max in the schema description; values
/// above this are clamped silently rather than rejected so a stray request
/// from a model that drifts upward still gets a useful answer.
const MAX_TIMEOUT_SECS: u64 = 600;

#[derive(Debug, Deserialize)]
struct BashArgs {
    command: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout: Option<u64>,
}

#[async_trait]
impl AgentTool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn label(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return its combined stdout/stderr. \
         The command runs in the agent's current working directory by default; \
         pass `cwd` to override. `timeout` is a soft kill in seconds (max 600) \
         — past that point the process is sent SIGKILL and the tool reports \
         a timeout error."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["command"],
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute."
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for the command. Defaults to the agent's cwd."
                },
                "timeout": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 600,
                    "description": "Maximum runtime in seconds. Defaults to 120. Hard-capped at 600."
                }
            }
        })
    }

    fn execution_mode(&self) -> Option<ToolExecutionMode> {
        Some(ToolExecutionMode::Sequential)
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        abort: AbortLike,
    ) -> Result<ToolOutput, ToolError> {
        if abort.is_cancelled() {
            return Err(ToolError::Aborted);
        }

        let parsed: BashArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidArguments(e.to_string()))?;

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&parsed.command);
        if let Some(cwd) = &parsed.cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());

        let timeout = parsed
            .timeout
            .unwrap_or(120)
            .min(MAX_TIMEOUT_SECS);

        // Block on the child process on a blocking thread so we don't
        // stall the Tokio runtime, and so we can race a deadline against
        // the child without holding the runtime up.
        let child = cmd
            .spawn()
            .map_err(|e| ToolError::Execution(format!("failed to spawn shell: {}", e)))?;
        let timeout = Duration::from_secs(timeout);
        let start = Instant::now();

        let result = tokio::task::spawn_blocking(move || wait_with_timeout(child, timeout))
            .await
            .map_err(|e| ToolError::Execution(format!("shell join error: {}", e)))?;

        match result {
            ChildOutcome::Finished { stdout, stderr, status } => {
                let details = json!({
                    "exit_code": status.code(),
                    "elapsed_ms": start.elapsed().as_millis() as u64,
                    "timed_out": false,
                    "stdout_bytes": stdout.len(),
                    "stderr_bytes": stderr.len(),
                });

                let mut text = String::new();
                if !stdout.is_empty() {
                    text.push_str(&stdout);
                    if !stdout.ends_with('\n') {
                        text.push('\n');
                    }
                }
                if !stderr.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str("[stderr]\n");
                    text.push_str(&stderr);
                    if !stderr.ends_with('\n') {
                        text.push('\n');
                    }
                }
                if !status.success() {
                    let code = status.code().unwrap_or(-1);
                    text.push_str(&format!("\n[exit code {}]", code));
                    // Non-zero exit is a real failure — surface it to the
                    // model as an execution error with the captured
                    // stdout/stderr so the model can read the output.
                    return Err(ToolError::Execution(text));
                }

                Ok(ToolOutput::text(text).with_details(details))
            }
            ChildOutcome::TimedOut => Err(ToolError::Execution(format!(
                "command exceeded timeout of {}s",
                timeout.as_secs()
            ))),
        }
    }
}

enum ChildOutcome {
    Finished {
        stdout: String,
        stderr: String,
        status: std::process::ExitStatus,
    },
    TimedOut,
}

/// Block on `child.wait()` until either the child exits or `deadline`
/// elapses. We poll the child rather than rely on platform-specific
/// `wait_timeout` to keep this portable across Unix and Windows; the
/// resolution is 50ms which is fine for command-level timeouts.
fn wait_with_timeout(
    mut child: std::process::Child,
    deadline: Duration,
) -> ChildOutcome {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut out) = child.stdout.take() {
                    use std::io::Read;
                    let _ = out.read_to_string(&mut stdout);
                }
                if let Some(mut err) = child.stderr.take() {
                    use std::io::Read;
                    let _ = err.read_to_string(&mut stderr);
                }
                return ChildOutcome::Finished {
                    stdout,
                    stderr,
                    status,
                };
            }
            Ok(None) => {
                if start.elapsed() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return ChildOutcome::TimedOut;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return ChildOutcome::Finished {
                    stdout: String::new(),
                    stderr: format!("wait failed: {}", e),
                    status: exit_status_from_code(1),
                };
            }
        }
    }
}

#[cfg(unix)]
fn exit_status_from_code(code: i32) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(code)
}

#[cfg(not(unix))]
fn exit_status_from_code(code: i32) -> std::process::ExitStatus {
    let _ = code;
    // On non-Unix we don't have a portable way to fabricate an exit
    // status from an integer. Fall back to spawning a process that
    // exits immediately and waiting for it; this branch is only
    // reachable from the rare `try_wait` error path.
    Command::new("cmd")
        .args(["/C", "exit 1"])
        .status()
        .expect("fallback exit probe")
}