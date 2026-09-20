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
//! [`PrintModeOptions::session`] or sets it to `Some(None)` for
//! `--continue` we attach to the matching
//! [`SessionWriter`](pi_session::SessionWriter) — the same Stage 5
//! SQLite store the TUI's `/resume` reads. Otherwise we open a fresh
//! database under `--session-dir` and write a header + user/assistant
//! rows as the turn progresses. The writer is checkpointed on the way
//! out so the next `--continue` invocation sees a consistent file.
//!
//! Legacy Stage 4 JSONL sessions found in `--session-dir` are migrated
//! into SQLite in place (best-effort, source preserved) before the
//! writer opens, and a resumed session replays its stored user /
//! assistant messages into the agent context.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clap::ValueEnum;
use futures::FutureExt;
use pi_agent_core::tools::ToolExecutor;
use pi_agent_core::{Agent, AgentError, AgentEvent, AgentOptions, RetryPolicy};
use pi_ai::stream::SharedStreamFn;
use pi_protocol::{AssistantMessage, Content, Message, Model, Role, StopReason, Usage};
use pi_session::{SessionEntry, SessionReader, SessionWriter};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio::io::{AsyncWriteExt, Stdout};
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, warn};

use crate::extensions::wiring::ExtensionRuntime;
use crate::file_processor::FileError;
use crate::tool_executor::default_executor;

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
    /// Tool executor the agent loop dispatches model tool calls to.
    /// The `pi` binary passes [`default_executor`]; passing a custom
    /// executor lets tests script tool traffic.
    pub tool_executor: Arc<dyn ToolExecutor>,
    /// Loaded JS extensions. When a prompt is `/name args` and `name`
    /// is an extension command (`pi.registerCommand`), print mode runs
    /// the JS handler instead of calling the model, and persists the
    /// handler's `pi.appendEntry` / `pi.sendMessage` side effects into
    /// the session.
    pub extensions: Arc<ExtensionRuntime>,
    /// Agent-level retry budget for the assistant call, resolved from
    /// `settings.json` by the caller (`config::load_agent_retry_policy`).
    pub retry: RetryPolicy,
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
            tool_executor: default_executor(),
            extensions: Arc::new(ExtensionRuntime::empty()),
            retry: RetryPolicy::default(),
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
pub async fn run_print_mode(options: PrintModeOptions) -> Result<PrintModeResult, PrintModeError> {
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

    // `/name args` may name an extension command (`pi.registerCommand`).
    // Those run inside the extension host only — no model request is
    // made and the turn counter stays at 0.
    if let Some((name, args)) = extension_command_invocation(&options.prompt) {
        if options.extensions.has_command(&name) {
            return run_extension_command(&options, &name, args, session_writer, session_path)
                .await;
        }
    }

    let agent = Arc::new(AsyncMutex::new(build_agent(&options)?));
    // Replay the stored conversation so `--continue` / `--session <id>`
    // resumes with the same context the TUI would load.
    if let Some(handle) = session_writer.as_ref().filter(|h| !h.history.is_empty()) {
        let mut guard = agent.lock().await;
        guard.state_mut().messages.extend(handle.history.clone());
    }
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
            prompt_result = Err(AgentError::Stream(format!(
                "prompt task panicked: {join_err}"
            )));
        }
        _ => {}
    }

    // Extensions may have appended entries / messages while handling
    // agent events during the turn; persist whatever is pending.
    persist_print_side_effects(session_writer.as_ref(), &options.extensions);
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
    )
    // Advertise the built-in tool schemas to the model and execute the
    // calls it emits for real. Without this the loop falls back to the
    // Stage 2 stub ("(stub) executed <name>") and the model sees a
    // fabricated result for every tool call.
    .with_tool_executor(options.tool_executor.clone())
    // `settings.retry` decides how many times a transient provider failure
    // restarts the assistant call before the run fails.
    .with_retry_policy(options.retry);
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
        state_for_sigint.terminate_now.store(true, Ordering::SeqCst);
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

/// Where a session request points.
enum SessionTarget {
    /// `--session <id-or-path>` / `--continue=<id>`.
    Explicit(String),
    /// Bare `--continue`: the most recent session in the directory.
    MostRecent,
}

/// Resolve the session target into an open [`SessionWriter`], or `None`
/// when session logging is disabled.
///
/// Print mode writes to the Stage 5 `pi-session` SQLite backend, the
/// same store the TUI's `/resume` reads. A legacy Stage 4 JSONL file is
/// migrated in place (best-effort, source preserved) before the writer
/// opens — see [`migrate_legacy_jsonl`].
fn resolve_session(options: &PrintModeOptions) -> Result<Option<SessionHandle>, PrintModeError> {
    let target = match options.session.as_ref() {
        // No session requested.
        None => return Ok(None),
        // Explicit `--session <id-or-path>` / `--continue=<id>`.
        Some(Some(raw)) => SessionTarget::Explicit(raw.clone()),
        // Bare `--continue`: attach the most recent session in
        // `--session-dir`, falling back to a fresh one when the
        // directory is empty.
        Some(None) => SessionTarget::MostRecent,
    };

    let (path, session_id) = match target {
        SessionTarget::Explicit(raw) => resolve_explicit_session(&options.session_dir, &raw)?,
        SessionTarget::MostRecent => match most_recent_session(&options.session_dir)? {
            Some(found) => found,
            None => {
                let id = new_session_id();
                (options.session_dir.join(format!("{id}.sqlite")), id)
            }
        },
    };

    // Load the stored conversation *before* opening the writer so the
    // read-only handle never races the WAL writer. A missing database
    // yields an empty history (the writer below creates the file); a
    // corrupt one is logged and treated as empty rather than aborting
    // the turn.
    let history = if path.exists() {
        match SessionReader::open(&path) {
            Ok(reader) => load_history(&reader, &session_id),
            Err(err) => {
                warn!("session: cannot read {}: {err}", path.display());
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    let writer = SessionWriter::open(&path).map_err(session_error)?;
    writer
        .write_header(SessionEntry::Header {
            id: session_id.clone(),
            created_at: chrono::Utc::now(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        })
        .map_err(session_error)?;
    // Derive `next_seq` from the stored rows so a continued session does
    // not collide with its own history.
    writer.resume(&session_id).map_err(session_error)?;
    // Mirror the user message into the session so the store records what
    // the agent saw on the wire.
    let user_message = Message {
        role: Role::User,
        content: vec![Content::text(options.prompt.clone())],
        model: None,
    };
    writer
        .append(SessionEntry::UserMessage(user_message))
        .map_err(session_error)?;

    Ok(Some(SessionHandle {
        writer,
        path,
        history,
    }))
}

/// Resolve an explicit `--session` / `--continue=<id>` argument into the
/// `(database path, session id)` pair the writer binds to.
///
/// Accepts, in order: a direct `*.sqlite` path, a direct legacy `*.jsonl`
/// path, a session id already present under `directory`, a legacy
/// `<id>.jsonl` file, or a brand-new id (which creates
/// `<directory>/<id>.sqlite`).
fn resolve_explicit_session(
    directory: &Path,
    raw: &str,
) -> Result<(PathBuf, String), PrintModeError> {
    let direct = Path::new(raw);
    if direct.is_file() {
        match direct.extension().and_then(|s| s.to_str()) {
            Some("sqlite") => {
                let id = latest_session_id(direct)?;
                return Ok((direct.to_path_buf(), id));
            }
            Some("jsonl") => {
                let database = direct.with_extension("sqlite");
                migrate_legacy_jsonl(direct, &database)?;
                let id = latest_session_id(&database)?;
                return Ok((database, id));
            }
            _ => {}
        }
    }

    if let Some(found) = find_session_by_id(directory, raw)? {
        return Ok(found);
    }

    let legacy = directory.join(format!("{raw}.jsonl"));
    if legacy.is_file() {
        let database = directory.join(format!("{raw}.sqlite"));
        migrate_legacy_jsonl(&legacy, &database)?;
        return Ok((database, raw.to_string()));
    }

    Ok((directory.join(format!("{raw}.sqlite")), raw.to_string()))
}

/// Session id of the newest session stored in `database`.
fn latest_session_id(database: &Path) -> Result<String, PrintModeError> {
    let reader = SessionReader::open(database).map_err(session_error)?;
    reader
        .latest_session()
        .map_err(session_error)?
        .map(|session| session.id)
        .ok_or_else(|| {
            PrintModeError::Session(format!(
                "session database {} has no sessions",
                database.display()
            ))
        })
}

/// Look for `session_id` across every SQLite database in `directory`.
fn find_session_by_id(
    directory: &Path,
    session_id: &str,
) -> Result<Option<(PathBuf, String)>, PrintModeError> {
    let refs =
        crate::list_resumable(directory).map_err(|err| PrintModeError::Session(err.to_string()))?;
    Ok(refs
        .into_iter()
        .find(|r| r.session_id == session_id)
        .map(|r| (r.database, r.session_id)))
}

/// Most recent SQLite session in `directory`, after migrating any legacy
/// JSONL files. Returns `None` when the directory holds no sessions.
fn most_recent_session(directory: &Path) -> Result<Option<(PathBuf, String)>, PrintModeError> {
    migrate_legacy_sessions(directory);
    let refs =
        crate::list_resumable(directory).map_err(|err| PrintModeError::Session(err.to_string()))?;
    Ok(refs.into_iter().next().map(|r| (r.database, r.session_id)))
}

/// Best-effort one-shot migration of every `<id>.jsonl` in `directory`
/// that has no sibling `<id>.sqlite` yet.
///
/// The JSONL source is never deleted, so the migration is **reversible**:
/// the original bytes stay on disk and `pi session migrate <file>` can be
/// rerun. A failure is logged and skipped — a corrupt legacy file must
/// not abort `--continue`.
fn migrate_legacy_sessions(directory: &Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let database = path.with_extension("sqlite");
        if database.exists() {
            continue;
        }
        // Skip empty files: a zero-length JSONL has no header and would
        // otherwise materialise an empty database.
        if path.metadata().map(|m| m.len()).unwrap_or(0) == 0 {
            continue;
        }
        if let Err(err) = migrate_legacy_jsonl(&path, &database) {
            warn!(
                "session: legacy migration skipped for {}: {err}",
                path.display()
            );
        }
    }
}

/// Migrate a single legacy JSONL session into SQLite, best-effort.
///
/// A no-op when the destination already exists (never overwrite a live
/// database) or when the source is absent. The JSONL is preserved on
/// success and on failure; see [`pi_session::migrate_jsonl`].
fn migrate_legacy_jsonl(source: &Path, destination: &Path) -> Result<(), PrintModeError> {
    if destination.exists() || !source.is_file() {
        return Ok(());
    }
    pi_session::migrate_jsonl(source, destination)
        .map(|_| ())
        .map_err(|err| {
            PrintModeError::Session(format!(
                "migrating {} → {}: {err}",
                source.display(),
                destination.display()
            ))
        })
}

/// Rebuild the agent's message log from the stored session entries.
///
/// Only user / assistant messages participate in the conversation
/// context; tool calls, tool results, and extension entries are skipped
/// (the loop re-derives tool traffic on every turn). A compaction entry
/// resets the log to its summary plus the retained tail, exactly like the
/// original `/compact` did.
fn load_history(reader: &SessionReader, session_id: &str) -> Vec<Message> {
    match reader.iter_entries(session_id) {
        Ok(entries) => {
            let mut history: Vec<Message> = Vec::new();
            for entry in entries {
                match entry.entry {
                    SessionEntry::UserMessage(message) => history.push(message),
                    SessionEntry::AssistantMessage(message) => history.push(Message {
                        role: Role::Assistant,
                        content: message.content,
                        model: Some(message.model),
                    }),
                    SessionEntry::Compaction {
                        summary,
                        retained_tail,
                        ..
                    } => {
                        history =
                            crate::compaction::replace_with_compaction(&retained_tail, &summary);
                    }
                    _ => {}
                }
            }
            history
        }
        Err(err) => {
            warn!("session: cannot read entries for {session_id}: {err}");
            Vec::new()
        }
    }
}

/// Normalise a `pi-session` error into the print-mode error surface.
fn session_error(err: pi_session::SessionError) -> PrintModeError {
    PrintModeError::Session(err.to_string())
}

// ---------------------------------------------------------------------------
// Extension commands
// ---------------------------------------------------------------------------

/// Parse a prompt that is exactly `/name args` into its parts.
///
/// Returns `None` for anything that is not a lone slash-command prompt
/// (plain text, an empty name, a path-like `/usr/bin`).
fn extension_command_invocation(prompt: &str) -> Option<(String, &str)> {
    let rest = prompt.trim().strip_prefix('/')?;
    let (name, args) = match rest.find(char::is_whitespace) {
        Some(idx) => (&rest[..idx], rest[idx..].trim()),
        None => (rest, ""),
    };
    if name.is_empty() || name.contains('/') {
        return None;
    }
    Some((name.to_string(), args))
}

/// Run one extension command in print mode and emit its result.
///
/// The shape mirrors a normal turn: `text` prints the handler's return
/// value, `json` prints one summary object, and `json-events` prints
/// one NDJSON line of the same content. A handler that throws is
/// surfaced on stderr and returned as [`PrintModeError::Agent`] so the
/// process exits non-zero — after the structured result has already
/// been written.
async fn run_extension_command(
    options: &PrintModeOptions,
    name: &str,
    args: &str,
    session: Option<SessionHandle>,
    session_path: Option<PathBuf>,
) -> Result<PrintModeResult, PrintModeError> {
    let outcome = options
        .extensions
        .execute_command(name, args)
        .await
        .map_err(|err| {
            PrintModeError::Agent(format!("extension command `/{name}` failed: {err}"))
        })?;

    // Persist whatever the handler recorded before we emit, so a
    // failed command still leaves its `appendEntry` rows behind.
    persist_print_side_effects(session.as_ref(), &options.extensions);

    let text = match &outcome.result {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    };

    let mut line = match options.output_format {
        OutputFormat::Text => text,
        OutputFormat::Json => json!({
            "command": name,
            "handled": outcome.handled,
            "is_error": outcome.is_error,
            "result": outcome.result,
            "error": outcome.error,
            "turns": 0,
        })
        .to_string(),
        OutputFormat::JsonEvents => json!({
            "type": "extension_command",
            "command": name,
            "handled": outcome.handled,
            "is_error": outcome.is_error,
            "result": outcome.result,
            "error": outcome.error,
        })
        .to_string(),
    };

    let mut stdout = tokio::io::stdout();
    if !line.is_empty() {
        line.push('\n');
        stdout.write_all(line.as_bytes()).await?;
    }
    stdout.flush().await?;

    if let Some(session) = session.as_ref() {
        session.flush()?;
    }

    if outcome.is_error {
        let reason = outcome
            .error
            .clone()
            .unwrap_or_else(|| "extension command failed".into());
        eprintln!("pi: /{name}: {reason}");
        return Err(PrintModeError::Agent(reason));
    }

    Ok(PrintModeResult {
        final_message: None,
        usage: Usage::default(),
        stop_reason: StopReason::Empty,
        turns: 0,
        session_path,
    })
}

/// Persist drained extension side effects into the SQLite session.
///
/// The host buffer is always drained, even when session logging is off,
/// so a later drain cannot see stale effects.
fn persist_print_side_effects(session: Option<&SessionHandle>, runtime: &ExtensionRuntime) {
    let effects = runtime.drain_side_effects();
    let Some(session) = session else {
        return;
    };
    let append = |kind: &str, payload: serde_json::Value| {
        if let Err(err) = session.writer.append(SessionEntry::Extension {
            extension: "extension".into(),
            kind: kind.into(),
            payload,
        }) {
            warn!("session: failed to append extension `{kind}` entry: {err}");
        }
    };
    for entry in &effects.entries {
        append(&entry.custom_type, entry.data.clone());
    }
    for message in &effects.messages {
        append("message", message.clone());
    }
    for user_message in &effects.user_messages {
        append("user_message", json!(user_message));
    }
    if let Some(name) = &effects.session_name {
        append("session_name", json!(name));
    }
}

/// Open SQLite session the driver appends to, plus the history the
/// agent starts from.
struct SessionHandle {
    writer: SessionWriter,
    path: PathBuf,
    history: Vec<Message>,
}

impl SessionHandle {
    /// Append an assistant message to the session. Failures are logged
    /// and swallowed so a session write never aborts the turn (matches
    /// the TS behaviour).
    fn append_assistant(&self, message: AssistantMessage) {
        if let Err(err) = self.writer.append(SessionEntry::AssistantMessage(message)) {
            warn!("session: failed to append assistant message: {err}");
        }
    }

    /// Checkpoint the WAL so the next `--continue` sees a consistent
    /// file.
    fn flush(&self) -> Result<(), PrintModeError> {
        self.writer.checkpoint().map_err(session_error)?;
        Ok(())
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
    fn extension_command_invocation_parses_name_and_args() {
        assert_eq!(
            extension_command_invocation("/echo"),
            Some(("echo".to_string(), ""))
        );
        assert_eq!(
            extension_command_invocation("  /echo  hello world  "),
            Some(("echo".to_string(), "hello world"))
        );
        assert_eq!(
            extension_command_invocation("/echo\targ"),
            Some(("echo".to_string(), "arg"))
        );
    }

    #[test]
    fn extension_command_invocation_ignores_non_commands() {
        for prompt in ["hello", "/", "/usr/bin/ls", "//echo"] {
            assert_eq!(extension_command_invocation(prompt), None, "{prompt}");
        }
    }

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
        assert_eq!(PrintModeError::MaxTurnsExceeded { max: 2 }.exit_code(), 1);
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
