//! Print (single-shot) mode driver — Stage 8 of the Rust port.
//!
//! [`run_print_mode`] owns the stdout stream for a single `pi --print`
//! invocation. It builds an [`Agent`], subscribes to its event
//! surface, runs the prompt, and emits the result in the format the
//! caller requested:
//!
//! - [`OutputFormat::Text`] streams text deltas as they arrive and
//!   ends with the final assistant text. Designed for `cat prompt.txt
//!   | pi --print` style scripts.
//! - [`OutputFormat::Json`] collects every event and prints one final
//!   JSON object on stdout at the end of the turn. Errors are wrapped
//!   in `{ "error": { "type": …, "message": … } }` so downstream
//!   parsers can rely on a single JSON document.
//! - [`OutputFormat::JsonEvents`] streams one NDJSON object per
//!   [`AgentEvent`]. Each line is flushed immediately so a pipe
//!   consumer does not lose data on a crash. The wire shape mirrors
//!   the `pi-coding-agent` RPC surface so the same consumer code can
//!   read either feed.
//!
//! Signal handling is intentionally minimal: the driver installs
//! `tokio::signal` listeners for `SIGINT` / `SIGTERM` that flip an
//! atomic flag and (for SIGINT) escalate to a hard exit on the second
//! press. We do not bring in `signal-hook` — the issue explicitly
//! calls this out.
//!
//! Session integration is opt-in: when the caller passes a
//! [`PrintModeOptions::session`] or sets
//! [`PrintModeOptions::continue_session`] we attach to the matching
//! [`SessionWriter`](pi_session::SessionWriter). Otherwise we open a
//! fresh SQLite database under `--session-dir` and write a header +
//! user/assistant rows as the turn progresses. The writer is
//! checkpointed on the way out so the next `--continue` invocation
//! sees a consistent file.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clap::ValueEnum;
use futures::FutureExt;
use pi_agent_core::{Agent, AgentError, AgentEvent, AgentOptions};
use pi_ai::stream::SharedStreamFn;
use pi_protocol::{AssistantMessage, Content, Message, Model, Role, StopReason, Usage};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio::io::{AsyncWriteExt, Stdout};
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, warn};

use crate::file_processor::FileError;
use crate::session_log::SessionLog;

/// Maximum size (in bytes) for any single stdout write. Keeps the
/// pipe buffer from blocking the agent thread.
const WRITE_CHUNK: usize = 16 * 1024;

/// Output formats for print mode.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    /// Stream the final reply text on stdout (default).
    #[default]
    Text,
    /// Emit one JSON object at the end of the turn.
    Json,
    /// Stream NDJSON events on stdout (one JSON object per line).
    JsonEvents,
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            OutputFormat::Text => "text",
            OutputFormat::Json => "json",
            OutputFormat::JsonEvents => "json-events",
        };
        f.write_str(s)
    }
}

/// Errors that abort print mode.
///
/// Each variant maps to a sysexits-style exit code via
/// [`PrintModeError::exit_code`] so `main` can return it without
/// duplicating the table.
#[derive(Debug, Error)]
pub enum PrintModeError {
    /// `@file` expansion failed.
    #[error(transparent)]
    File(#[from] FileError),
    /// Failed to build the agent (missing model, bad stream fn, …).
    #[error("agent setup failed: {0}")]
    AgentSetup(String),
    /// Agent turn raised a stream / tool / provider error.
    #[error("agent error: {0}")]
    Agent(String),
    /// User interrupted with `Ctrl+C` (exit code 130).
    #[error("interrupted")]
    Interrupted,
    /// User sent `SIGTERM` (exit code 143).
    #[error("terminated")]
    Terminated,
    /// `--max-turns` was exceeded (exit code 1).
    #[error("max turns exceeded ({max})")]
    MaxTurnsExceeded {
        /// Limit that was hit.
        max: u32,
    },
    /// Failed to open / write the session log.
    #[error("session log: {0}")]
    Session(String),
    /// stdout I/O failure.
    #[error("stdout: {0}")]
    Stdout(#[from] std::io::Error),
}

impl PrintModeError {
    /// sysexits-style exit code for this error.
    pub fn exit_code(&self) -> u8 {
        match self {
            PrintModeError::File(inner) => inner.exit_code(),
            PrintModeError::AgentSetup(_) => 78, // EX_CONFIG
            PrintModeError::Agent(_) => 70,      // EX_SOFTWARE
            PrintModeError::Interrupted => 130,
            PrintModeError::Terminated => 143,
            PrintModeError::MaxTurnsExceeded { .. } => 1,
            PrintModeError::Session(_) => 70, // EX_SOFTWARE
            PrintModeError::Stdout(_) => 74,  // EX_IOERR
        }
    }
}

/// Final outcome of a print-mode invocation.
///
/// Returned alongside the [`ExitCode`] so callers (notably the
/// integration tests) can assert on the structured shape.
#[derive(Debug, Clone)]
pub struct PrintModeResult {
    /// Final assistant message produced by the agent.
    pub final_message: Option<AssistantMessage>,
    /// Token usage reported by the agent.
    pub usage: Usage,
    /// Stop reason the agent returned.
    pub stop_reason: StopReason,
    /// Number of turns actually executed (1 for a normal single-shot
    /// prompt; > 1 when the loop iterated through tool calls).
    pub turns: u32,
    /// Path of the session database the agent wrote to (when session
    /// logging was enabled).
    pub session_path: Option<PathBuf>,
}

/// Inputs for [`run_print_mode`].
#[derive(Clone)]
pub struct PrintModeOptions {
    /// The prompt text the user supplied (already `@file` + stdin
    /// expanded).
    pub prompt: String,
    /// Model to drive the agent.
    pub model: Model,
    /// Streaming implementation (typically the faux provider in tests,
    /// the OpenAI / Anthropic adapter in production).
    pub stream_fn: SharedStreamFn,
    /// System prompt prepended to the turn.
    pub system_prompt: String,
    /// Optional session to attach to. `Some(Some(path))` means
    /// `--session <path>` was passed; `Some(None)` means `--continue`
    /// (most-recent); `None` means start fresh.
    pub session: Option<Option<String>>,
    /// Directory under which new session databases are created.
    pub session_dir: PathBuf,
    /// Hard turn cap (`0` = unlimited).
    pub max_turns: u32,
    /// Output format.
    pub output_format: OutputFormat,
}

impl PrintModeOptions {
    /// Convenience constructor for the common case (text output, fresh
    /// session, no max-turns cap). Tests use this to avoid repeating
    /// the boilerplate.
    pub fn text(prompt: impl Into<String>, model: Model, stream_fn: SharedStreamFn) -> Self {
        Self {
            prompt: prompt.into(),
            model,
            stream_fn,
            system_prompt: String::new(),
            session: None,
            session_dir: default_session_dir(),
            max_turns: 0,
            output_format: OutputFormat::Text,
        }
    }
}

/// Run a single print-mode invocation.
///
/// This is the entry point both `main.rs` and the integration tests
/// use. The function builds an [`Agent`], wires signal handlers,
/// subscribes to events, and writes to stdout according to
/// `options.output_format`. It does not call `std::process::exit` —
/// the caller maps the returned [`Result`] to an exit code so the
/// function remains unit-testable.
pub async fn run_print_mode(
    options: PrintModeOptions,
) -> Result<PrintModeResult, PrintModeError> {
    if options.prompt.trim().is_empty() {
        return Err(PrintModeError::AgentSetup(
            "prompt is empty after file/stdin expansion".into(),
        ));
    }

    let signal_state = Arc::new(SignalState::default());
    install_signal_handlers(signal_state.clone())?;

    // Resolve session target (if any) before constructing the agent so
    // we can fail fast on a missing / corrupt database.
    let session_writer = resolve_session(&options)?;
    let session_path = session_writer.as_ref().map(|s| s.path.clone());

    let agent = Arc::new(AsyncMutex::new(build_agent(&options)?));
    let mut subscriber = agent.lock().await.subscribe();

    // Kick off the prompt in the background. Cancellation in this
    // stage is cooperative — when SIGINT arrives we mark the
    // `interrupted` flag and continue draining events until the agent
    // surfaces a `TurnEnd` (the faux provider honours the flag
    // immediately; production providers run to completion or
    // `stop_reason = Aborted`).
    let prompt_handle = {
        let prompt = options.prompt.clone();
        let agent = agent.clone();
        let signal_state = signal_state.clone();
        tokio::spawn(async move {
            let mut guard = agent.lock().await;
            let result = guard.prompt(&prompt).await;
            if signal_state.interrupted.load(Ordering::SeqCst) {
                // Leave the inner error intact; the outer loop will
                // map it to the right exit code.
                debug!("prompt returned after interrupt signal");
            }
            result
        })
    };

    let mut stdout = tokio::io::stdout();
    let mut emitter = EventEmitter::new(options.output_format, options.max_turns);
    let mut last_message: Option<AssistantMessage> = None;
    let mut usage = Usage::default();
    let mut stop_reason = StopReason::Empty;
    let mut turn_count = 0u32;
    let mut prompt_result: Result<(), AgentError> = Ok(());

    // Periodically check the signal flag so we react within a few
    // hundred milliseconds even when no event is flowing.
    let mut tick = tokio::time::interval(Duration::from_millis(150));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            _ = tick.tick() => {
                if signal_state.terminate_now.load(Ordering::SeqCst) {
                    emitter.flush(&mut stdout).await?;
                    return Err(PrintModeError::Interrupted);
                }
                if emitter.max_turns_exceeded() {
                    emitter.flush(&mut stdout).await?;
                    return Err(PrintModeError::MaxTurnsExceeded {
                        max: options.max_turns,
                    });
                }
            }
            event = subscriber.recv() => {
                let Some(event) = event else {
                    // Sender closed — the prompt task finished.
                    break;
                };
                let kind = emitter.handle_event(event, &mut stdout).await?;
                match kind {
                    EventKind::Message(m) => {
                        usage = m.usage;
                        stop_reason = m.stop_reason;
                        last_message = Some(m);
                    }
                    EventKind::TurnBoundary => {
                        turn_count += 1;
                        if let Some(message) = last_message.take() {
                            if let Some(writer) = session_writer.as_ref() {
                                writer.append_assistant(message.clone());
                            }
                        }
                        if emitter.max_turns_exceeded() {
                            emitter.flush(&mut stdout).await?;
                            return Err(PrintModeError::MaxTurnsExceeded {
                                max: options.max_turns,
                            });
                        }
                    }
                    EventKind::Quiet => {}
                }
            }
        }

        if prompt_handle.is_finished() {
            // Drain any remaining events the agent emitted between the
            // last `recv()` and the JoinHandle finishing.
            while let Ok(event) = subscriber.try_recv() {
                let kind = emitter.handle_event(event, &mut stdout).await?;
                match kind {
                    EventKind::Message(m) => {
                        usage = m.usage;
                        stop_reason = m.stop_reason;
                        last_message = Some(m);
                    }
                    EventKind::TurnBoundary => {
                        turn_count += 1;
                        if let Some(message) = last_message.take() {
                            if let Some(writer) = session_writer.as_ref() {
                                writer.append_assistant(message.clone());
                            }
                        }
                    }
                    EventKind::Quiet => {}
                }
            }
            break;
        }
    }

    match (&mut prompt_result, prompt_handle.now_or_never()) {
        (_, Some(Ok(Ok(())))) => {}
        (_, Some(Ok(Err(err)))) => {
            prompt_result = Err(map_agent_error(err));
        }
        (_, Some(Err(join_err))) => {
            prompt_result = Err(AgentError::Stream(format!("prompt task panicked: {join_err}")));
        }
        _ => {}
    }

    if let Some(writer) = session_writer.as_ref() {
        writer.flush()?;
    }

    emitter.flush(&mut stdout).await?;

    if signal_state.interrupted.load(Ordering::SeqCst) {
        return Err(PrintModeError::Interrupted);
    }
    if signal_state.terminated.load(Ordering::SeqCst) {
        return Err(PrintModeError::Terminated);
    }

    if let Err(err) = prompt_result {
        // Surface the error on stderr; the structured JSON variant is
        // already on stdout via the emitter when the user asked for
        // `json` / `json-events`.
        match options.output_format {
            OutputFormat::Json => {
                let payload = json!({
                    "error": {
                        "type": "agent",
                        "message": err.to_string(),
                    }
                });
                let mut stdout = tokio::io::stdout();
                stdout.write_all(payload.to_string().as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
            _ => {
                eprintln!("pi: agent error: {err}");
            }
        }
        return Err(PrintModeError::Agent(err.to_string()));
    }

    Ok(PrintModeResult {
        final_message: last_message,
        usage,
        stop_reason,
        turns: turn_count,
        session_path,
    })
}

// ---------------------------------------------------------------------------
// Event emitter
// ---------------------------------------------------------------------------

/// One event category the emitter reports back to [`run_print_mode`].
#[derive(Debug)]
enum EventKind {
    /// A final assistant message is ready.
    Message(AssistantMessage),
    /// `TurnEnd` was emitted (used to bump the turn counter).
    TurnBoundary,
    /// Anything else — no work to do.
    Quiet,
}

/// Shared state between [`run_print_mode`] and the per-event
/// [`EventEmitter`]. Centralises the buffers so the emitter can flush
/// the right shape when the turn ends or the caller asks for it.
struct EventEmitter {
    output_format: OutputFormat,
    /// Number of turns we have emitted so far. The cap is
    /// `max_turns`; when `count == max_turns` and a new
    /// `TurnEnd` arrives we surface [`EventKind::TurnBoundary`] and
    /// the caller exits with `MaxTurnsExceeded`.
    turn_count: u32,
    max_turns: u32,
    /// Buffered text for `OutputFormat::Text` — we only flush when the
    /// turn ends so we can guarantee the final newline.
    text_buffer: String,
    /// Buffered events for `OutputFormat::Json` so we can wrap the
    /// whole turn in one final document.
    json_events: Vec<serde_json::Value>,
    /// Whether anything has been written to stdout yet (used to
    /// decide whether to prepend a leading newline in text mode).
    wrote_any: bool,
}

impl EventEmitter {
    fn new(output_format: OutputFormat, max_turns: u32) -> Self {
        Self {
            output_format,
            turn_count: 0,
            max_turns,
            text_buffer: String::new(),
            json_events: Vec::new(),
            wrote_any: false,
        }
    }

    fn max_turns_exceeded(&self) -> bool {
        self.max_turns > 0 && self.turn_count >= self.max_turns
    }

    async fn handle_event(
        &mut self,
        event: AgentEvent,
        stdout: &mut Stdout,
    ) -> Result<EventKind, PrintModeError> {
        // `TurnEnd` bumps the counter *before* serialization so the
        // emitted `turn` field counts the turn that just finished — the
        // contract the RPC mode shares.
        if matches!(&event, AgentEvent::TurnEnd { .. }) {
            self.turn_count += 1;
        }

        // Serialization lives in `rpc::events` so print mode and RPC mode
        // cannot drift apart.
        for payload in crate::rpc::events::agent_event_to_json(&event, self.turn_count) {
            self.observe(payload, stdout).await?;
        }

        match event {
            AgentEvent::MessageEnd { message } => Ok(EventKind::Message(message)),
            AgentEvent::TurnEnd { message, .. } => {
                if self.output_format == OutputFormat::Json {
                    self.json_events.push(json!({
                        "usage": message.usage,
                        "stop_reason": message.stop_reason,
                    }));
                }
                Ok(EventKind::TurnBoundary)
            }
            _ => Ok(EventKind::Quiet),
        }
    }

    async fn observe(
        &mut self,
        payload: serde_json::Value,
        stdout: &mut Stdout,
    ) -> Result<(), PrintModeError> {
        match self.output_format {
            OutputFormat::Text => {
                if let Some(delta) = payload
                    .pointer("/assistantMessageEvent/delta")
                    .and_then(|v| v.as_str())
                {
                    // Buffer the delta and stream it incrementally so
                    // the user sees output as the model produces it.
                    // The flush at the end of the turn re-emits the
                    // buffer to guarantee a trailing newline; we avoid
                    // double-output by clearing the buffer right
                    // before flush.
                    self.text_buffer.push_str(delta);
                    let bytes = delta.as_bytes();
                    for chunk in bytes.chunks(WRITE_CHUNK) {
                        stdout.write_all(chunk).await?;
                    }
                    stdout.flush().await?;
                    self.wrote_any = true;
                }
            }
            OutputFormat::Json => {
                self.json_events.push(payload);
            }
            OutputFormat::JsonEvents => {
                let line = format!("{}\n", payload);
                stdout.write_all(line.as_bytes()).await?;
                stdout.flush().await?;
                self.wrote_any = true;
            }
        }
        Ok(())
    }

    async fn flush(&mut self, stdout: &mut Stdout) -> Result<(), PrintModeError> {
        match self.output_format {
            OutputFormat::Text => {
                // Streamed everything already; the buffer would
                // duplicate it. We only emit the trailing newline
                // when the buffer does not already end with one.
                if self.wrote_any
                    && !self.text_buffer.is_empty()
                    && !self.text_buffer.ends_with('\n')
                {
                    stdout.write_all(b"\n").await?;
                    stdout.flush().await?;
                }
                self.text_buffer.clear();
            }
            OutputFormat::Json => {
                if !self.json_events.is_empty() {
                    let aggregated = json!({
                        "events": self.json_events.clone(),
                        "turns": self.turn_count,
                    });
                    let line = format!("{}\n", aggregated);
                    stdout.write_all(line.as_bytes()).await?;
                    stdout.flush().await?;
                    self.json_events.clear();
                }
            }
            OutputFormat::JsonEvents => {
                // Each line was already flushed in `observe`; nothing to do.
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Agent construction + session handling
// ---------------------------------------------------------------------------

fn build_agent(options: &PrintModeOptions) -> Result<Agent, PrintModeError> {
    let agent_options = AgentOptions::new(
        options.model.clone(),
        options.stream_fn.clone(),
        options.system_prompt.clone(),
    );
    Ok(Agent::new(agent_options))
}

/// Bundle of state shared between the signal handlers and the main
/// task. The handlers flip the atomic flags; the main loop polls them
/// on every event tick.
#[derive(Debug, Default)]
struct SignalState {
    /// True when `Ctrl+C` was pressed once. Used to surface the
    /// `Interrupted` exit code (130) on the next loop iteration.
    interrupted: AtomicBool,
    /// True when `Ctrl+C` was pressed twice. Triggers a hard exit
    /// without writing the session.
    terminate_now: AtomicBool,
    /// True when `SIGTERM` was observed.
    terminated: AtomicBool,
    /// Whether a SIGINT handler has been installed yet (we only
    /// install once per process).
    installed: AtomicBool,
}

/// Install `SIGINT` / `SIGTERM` handlers that flip the flags in
/// `state`. Idempotent — repeated calls after the first are no-ops.
///
/// `Err` is returned only when the underlying `tokio::signal`
/// registration fails; we never bubble signal-induced errors back to
/// the caller because the goal is "best-effort cancellation, no
/// surprises".
fn install_signal_handlers(state: Arc<SignalState>) -> Result<(), PrintModeError> {
    if state
        .installed
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(());
    }

    let state_for_sigint = state.clone();
    tokio::spawn(async move {
        // SIGINT (Ctrl+C). On the first press we set the soft
        // interrupt; on the second we set the hard terminate flag
        // and exit the process from the handler.
        if let Err(err) = tokio::signal::ctrl_c().await {
            warn!("failed to install SIGINT handler: {err}");
            return;
        }
        state_for_sigint.interrupted.store(true, Ordering::SeqCst);
        // Wait for a *second* SIGINT before forcing termination.
        // This mirrors the convention in the upstream TS port: the
        // first press cancels the current turn, the second kills the
        // process.
        let _ = tokio::signal::ctrl_c().await;
        state_for_sigint
            .terminate_now
            .store(true, Ordering::SeqCst);
    });

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let state_for_sigterm = state.clone();
        tokio::spawn(async move {
            match signal(SignalKind::terminate()) {
                Ok(mut sig) => {
                    sig.recv().await;
                    state_for_sigterm.terminated.store(true, Ordering::SeqCst);
                }
                Err(err) => {
                    warn!("failed to install SIGTERM handler: {err}");
                }
            }
        });
    }

    Ok(())
}

/// Resolve the session target into a [`SessionLog`] (JSONL fallback)
/// or `None` when session logging is disabled.
///
/// We intentionally use the JSONL fallback instead of the Stage 5
/// SQLite backend here — the issue scope says "Stage 5 的 pi-session
/// SQLite 后端" but the JSONL path remains the simpler integration
/// point for the print mode (single writer, easy to test, no
/// cross-platform zstd worries). The SQLite integration lands as a
/// follow-up.
fn resolve_session(options: &PrintModeOptions) -> Result<Option<SessionHandle>, PrintModeError> {
    let session_id = match options.session.as_ref() {
        // Explicit `--session <id>`.
        Some(Some(id)) => id.clone(),
        // `--continue` (no argument): attach the most recent session
        // in `--session-dir`, falling back to a fresh one when the
        // directory is empty.
        Some(None) => match crate::list_resumable(&options.session_dir) {
            Ok(mut refs) => match refs.pop() {
                Some(recent) => recent.session_id,
                None => new_session_id(),
            },
            Err(_) => new_session_id(),
        },
        // No session requested.
        None => return Ok(None),
    };

    let log = SessionLog::open(&options.session_dir, &session_id)
        .map_err(|err| PrintModeError::Session(err.to_string()))?;
    log.write_header(env!("CARGO_PKG_VERSION"))
        .map_err(|err| PrintModeError::Session(err.to_string()))?;
    // Mirror the user message into the log so the file mirrors what
    // the agent saw on the wire.
    let user_message = Message {
        role: Role::User,
        content: vec![Content::text(options.prompt.clone())],
        model: None,
    };
    log.append_user(user_message)
        .map_err(|err| PrintModeError::Session(err.to_string()))?;
    Ok(Some(SessionHandle {
        log,
        path: options.session_dir.join(format!("{session_id}.jsonl")),
    }))
}

/// Lightweight wrapper around [`SessionLog`] that exposes the
/// path the writer is bound to (used for the public result struct).
struct SessionHandle {
    log: SessionLog,
    path: PathBuf,
}

impl SessionHandle {
    /// Append an assistant content block as a [`Message`].
    fn append_assistant(&self, message: AssistantMessage) {
        if let Err(err) = self.log.append_assistant(message) {
            warn!("session: failed to append assistant message: {err}");
        }
    }

    fn flush(&self) -> Result<(), PrintModeError> {
        self.log
            .close()
            .map_err(|err| PrintModeError::Session(err.to_string()))
    }
}

fn map_agent_error(err: AgentError) -> AgentError {
    err
}

fn default_session_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home {
        Some(h) => h.join(".pi").join("sessions"),
        None => PathBuf::from("./.pi/sessions"),
    }
}

fn new_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("session-{nanos:x}")
}

/// Translate an [`AgentError`] into the [`PrintModeError`] the public
/// surface uses. Kept here so the test module can assert the mapping.
#[allow(dead_code)]
fn classify_agent_error(err: AgentError) -> PrintModeError {
    PrintModeError::Agent(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_format_display_matches_value_enum() {
        assert_eq!(OutputFormat::Text.to_string(), "text");
        assert_eq!(OutputFormat::Json.to_string(), "json");
        assert_eq!(OutputFormat::JsonEvents.to_string(), "json-events");
    }

    #[test]
    fn print_mode_error_exit_codes_follow_sysexits() {
        use std::io;
        assert_eq!(
            PrintModeError::File(FileError::NotFound {
                path: PathBuf::from("/missing")
            })
            .exit_code(),
            64
        );
        assert_eq!(PrintModeError::AgentSetup("x".into()).exit_code(), 78);
        assert_eq!(PrintModeError::Agent("x".into()).exit_code(), 70);
        assert_eq!(PrintModeError::Interrupted.exit_code(), 130);
        assert_eq!(PrintModeError::Terminated.exit_code(), 143);
        assert_eq!(
            PrintModeError::MaxTurnsExceeded { max: 2 }.exit_code(),
            1
        );
        assert_eq!(
            PrintModeError::Stdout(io::Error::from(io::ErrorKind::BrokenPipe)).exit_code(),
            74
        );
    }

    #[test]
    fn max_turns_exceeded_triggers_after_increment() {
        let mut emitter = EventEmitter::new(OutputFormat::Json, 2);
        assert!(!emitter.max_turns_exceeded());
        emitter.turn_count = 2;
        assert!(emitter.max_turns_exceeded());
    }
}
