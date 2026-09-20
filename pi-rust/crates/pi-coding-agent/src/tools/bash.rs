//! `BashTool` — execute a shell command and capture stdout/stderr.
//!
//! Mirrors `createBashTool` from
//! `packages/coding-agent/src/core/tools/bash.ts`. The first cut uses a
//! synchronous [`std::process::Command`] so the implementation is easy to
//! audit; the agent loop bridges it onto a `tokio::task::spawn_blocking`
//! thread when needed. Permission checks land in Stage 2; for now the tool
//! runs whatever command the model asks for, gated only by cooperative
//! cancellation through the [`AbortLike`] handle.
//!
//! Output handling mirrors upstream: the captured text is tail-truncated to
//! [`DEFAULT_MAX_LINES`](super::truncate::DEFAULT_MAX_LINES) lines /
//! [`DEFAULT_MAX_BYTES`] bytes, and when that fires the *untruncated* output
//! is spilled to `$TMPDIR/pi-output-<16 hex>.log` so the notice can point the
//! model at the full log.
//!
//! Known gaps against upstream: stdout and stderr are labelled separately
//! instead of being merged at the OS level (so interleaving is lost), and the
//! `AbortLike` handle is only polled before spawning — an abort mid-command
//! does not kill the child. Reading the drained pipes can also block on a
//! grandchild that inherited them, which upstream shares.

#![cfg(not(target_arch = "wasm32"))]

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::truncate::{
    format_size, split_lines_for_counting, truncate_tail, TruncatedBy, TruncationOptions,
    TruncationResult, DEFAULT_MAX_BYTES,
};
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
         a timeout error. Output is truncated to the last 2000 lines or 50KB \
         (whichever is hit first); when that happens the full output is saved \
         to a temp file and its path is included in the result."
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

        let parsed: BashArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArguments(e.to_string()))?;

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&parsed.command);
        if let Some(cwd) = &parsed.cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        let timeout = parsed.timeout.unwrap_or(120).min(MAX_TIMEOUT_SECS);

        // Block on the child process on a blocking thread so we don't
        // stall the Tokio runtime, and so we can race a deadline against
        // the child without holding the runtime up.
        let mut child = cmd
            .spawn()
            .map_err(|e| ToolError::Execution(format!("failed to spawn shell: {}", e)))?;
        let timeout = Duration::from_secs(timeout);
        let start = Instant::now();

        // Drain both pipes on dedicated threads so a command that writes more
        // than the OS pipe buffer (64 KiB on Linux) can still exit, and so a
        // killed child's partial output is available.
        let stdout_rx = child.stdout.take().map(spawn_pipe_reader);
        let stderr_rx = child.stderr.take().map(spawn_pipe_reader);

        let result = tokio::task::spawn_blocking(move || {
            wait_with_timeout(child, timeout, stdout_rx, stderr_rx)
        })
        .await
        .map_err(|e| ToolError::Execution(format!("shell join error: {}", e)))?;

        match result {
            ChildOutcome::Finished {
                stdout,
                stderr,
                status,
                stdout_bytes,
                stderr_bytes,
            } => {
                let combined = combine_output(&stdout, &stderr);
                let formatted = format_output(&combined)?;

                let mut details = json!({
                    "exit_code": status.code(),
                    "elapsed_ms": start.elapsed().as_millis() as u64,
                    "timed_out": false,
                    "stdout_bytes": stdout_bytes,
                    "stderr_bytes": stderr_bytes,
                });
                if let Some(truncation) = &formatted.truncation {
                    details["truncation"] = serde_json::to_value(truncation)
                        .map_err(|e| ToolError::Execution(format!("details encode: {}", e)))?;
                    details["full_output_path"] = json!(formatted
                        .full_output_path
                        .as_ref()
                        .map(|p| p.display().to_string()));
                }

                let mut text = formatted.text;
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
            ChildOutcome::TimedOut { stdout, stderr } => {
                let combined = combine_output(&stdout, &stderr);
                let formatted = format_output(&combined)?;
                let status = format!("command exceeded timeout of {}s", timeout.as_secs());
                // Upstream's `appendStatus` skips the blank-line separator when
                // there is no partial output to show.
                Err(ToolError::Execution(if formatted.text.is_empty() {
                    status
                } else {
                    format!("{}\n\n{}", formatted.text, status)
                }))
            }
        }
    }
}

/// Merge captured stdout/stderr into the single text block the model sees.
///
/// Upstream merges both streams at the OS level so the relative order is
/// preserved; this port reads them separately and labels the stderr section.
fn combine_output(stdout: &str, stderr: &str) -> String {
    let mut text = String::new();
    if !stdout.is_empty() {
        text.push_str(stdout);
        if !stdout.ends_with('\n') {
            text.push('\n');
        }
    }
    if !stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str("[stderr]\n");
        text.push_str(stderr);
        if !stderr.ends_with('\n') {
            text.push('\n');
        }
    }
    text
}

/// Model-facing text plus the metadata produced by truncating it.
struct FormattedOutput {
    text: String,
    truncation: Option<TruncationResult>,
    full_output_path: Option<PathBuf>,
}

/// Tail-truncate `combined` and, when anything was dropped, spill the full
/// text to a temp file so the notice can name it.
fn format_output(combined: &str) -> Result<FormattedOutput, ToolError> {
    let truncation = truncate_tail(combined, TruncationOptions::default());
    let mut text = truncation.content.clone();

    if !truncation.truncated {
        return Ok(FormattedOutput {
            text,
            truncation: None,
            full_output_path: None,
        });
    }

    let path = write_full_output(combined)?;
    let start_line = truncation.total_lines - truncation.output_lines + 1;
    let end_line = truncation.total_lines;
    if truncation.last_line_partial {
        // Upstream reads this from its streaming accumulator, which reports 0
        // when the output ends with a newline; report the real length of the
        // last content line instead.
        let last_line_size = split_lines_for_counting(combined)
            .last()
            .map(|line| line.len())
            .unwrap_or(0);
        text.push_str(&format!(
            "\n\n[Showing last {} of line {} (line is {}). Full output: {}]",
            format_size(truncation.output_bytes),
            end_line,
            format_size(last_line_size),
            path.display()
        ));
    } else if truncation.truncated_by == Some(TruncatedBy::Lines) {
        text.push_str(&format!(
            "\n\n[Showing lines {}-{} of {}. Full output: {}]",
            start_line,
            end_line,
            truncation.total_lines,
            path.display()
        ));
    } else {
        text.push_str(&format!(
            "\n\n[Showing lines {}-{} of {} ({} limit). Full output: {}]",
            start_line,
            end_line,
            truncation.total_lines,
            format_size(DEFAULT_MAX_BYTES),
            path.display()
        ));
    }

    Ok(FormattedOutput {
        text,
        truncation: Some(truncation),
        full_output_path: Some(path),
    })
}

/// Write the untruncated output to `$TMPDIR/pi-output-<16 hex>.log`, the same
/// naming scheme as upstream's `defaultTempFilePath`.
fn write_full_output(contents: &str) -> Result<PathBuf, ToolError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "pi-output-{:08x}{:08x}.log",
        seq as u32,
        (nanos ^ (nanos >> 32)) as u32
    ));
    std::fs::write(&path, contents)
        .map_err(|e| ToolError::Execution(format!("failed to spill output to temp file: {}", e)))?;
    Ok(path)
}

/// How long to keep collecting pipe chunks after the child has been reaped
/// (or killed). Everything the reader threads have already sent arrives
/// immediately; the grace period only covers data still in flight. Without it
/// a grandchild that inherited the pipe could keep us waiting forever.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

enum ChildOutcome {
    Finished {
        stdout: String,
        stderr: String,
        status: std::process::ExitStatus,
        stdout_bytes: usize,
        stderr_bytes: usize,
    },
    /// The deadline fired and the child was killed; whatever it managed to
    /// write into the pipes before dying is drained and kept.
    TimedOut { stdout: String, stderr: String },
}

/// Read a child pipe to EOF on a dedicated thread, forwarding each chunk to an
/// unbounded channel. Reading in a separate thread is what stops a
/// large-output command from deadlocking: without a concurrent reader the child
/// fills the pipe buffer and blocks in `write`, so it never exits and
/// `try_wait` never reports the exit.
///
/// The thread ends at EOF, on a read error, or as soon as the receiver is
/// dropped. A grandchild that inherited the pipe can keep it parked in `read`
/// until that process exits; the caller never joins it, so this only leaks one
/// idle thread for as long as the grandchild lives.
fn spawn_pipe_reader(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

/// Collect everything the reader thread sent, giving up `grace` after the last
/// chunk (or immediately once the channel disconnects, which is the normal
/// EOF path).
fn drain_pipe(rx: &mpsc::Receiver<Vec<u8>>, grace: Duration) -> Vec<u8> {
    let deadline = Instant::now() + grace;
    let mut out = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok(chunk) => out.extend_from_slice(&chunk),
            Err(_) => break,
        }
    }
    out
}

/// Block on `child.wait()` until either the child exits or `deadline`
/// elapses. We poll the child rather than rely on platform-specific
/// `wait_timeout` to keep this portable across Unix and Windows; the
/// resolution is 50ms which is fine for command-level timeouts.
fn wait_with_timeout(
    mut child: std::process::Child,
    deadline: Duration,
    stdout_rx: Option<mpsc::Receiver<Vec<u8>>>,
    stderr_rx: Option<mpsc::Receiver<Vec<u8>>>,
) -> ChildOutcome {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout_bytes = stdout_rx
                    .as_ref()
                    .map(|rx| drain_pipe(rx, DRAIN_GRACE))
                    .unwrap_or_default();
                let stderr_bytes = stderr_rx
                    .as_ref()
                    .map(|rx| drain_pipe(rx, DRAIN_GRACE))
                    .unwrap_or_default();
                return ChildOutcome::Finished {
                    stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
                    stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
                    status,
                    stdout_bytes: stdout_bytes.len(),
                    stderr_bytes: stderr_bytes.len(),
                };
            }
            Ok(None) => {
                if start.elapsed() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Keep whatever the command had already flushed so the
                    // timeout error can show partial progress.
                    let stdout = stdout_rx
                        .as_ref()
                        .map(|rx| drain_pipe(rx, DRAIN_GRACE))
                        .unwrap_or_default();
                    let stderr = stderr_rx
                        .as_ref()
                        .map(|rx| drain_pipe(rx, DRAIN_GRACE))
                        .unwrap_or_default();
                    return ChildOutcome::TimedOut {
                        stdout: String::from_utf8_lossy(&stdout).into_owned(),
                        stderr: String::from_utf8_lossy(&stderr).into_owned(),
                    };
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return ChildOutcome::Finished {
                    stdout: String::new(),
                    stderr: format!("wait failed: {}", e),
                    status: exit_status_from_code(1),
                    stdout_bytes: 0,
                    stderr_bytes: 0,
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
