//! RPC server: stdin line loop, method dispatch, stdout event pump.
//!
//! The server owns two directions:
//!
//! * **stdin** is read one line at a time. Each line is a JSON-RPC
//!   request or notification; blank lines are ignored so a caller can
//!   safely send a trailing newline. Reading continues while a turn is
//!   in flight — that is what makes `abort` possible.
//! * **stdout** gets exactly one JSON object per line: a [`Response`] for
//!   every request, and a `Notification` for every agent event. Every
//!   write flushes immediately. Diagnostics go to stderr.
//!
//! Concurrency model: one turn at a time. A second `prompt` while a turn
//! is in flight is rejected with `-32000` (`busy`) rather than queued —
//! see [`run_rpc_server`] for the rationale. `abort` is a no-op when no
//! turn is running. stdin EOF lets the in-flight turn drain, then exits 0.
//!
//! Cancellation is turn-level: the Rust agent core does not expose a
//! cancellation token yet (Stage 10 wires that through to tool
//! execution), so `abort` sets a flag *and* aborts the owning task at its
//! next await point. The `tokio::sync::Mutex` guard around the agent is
//! released when the task is dropped, so the next turn can start.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use pi_agent_core::{Agent, AgentEvent, AgentOptions};
use pi_ai::models::Models;
use pi_protocol::{Content, Message, Model, ProviderId, Role, StopReason, Usage};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Stdout};
use tokio::sync::{mpsc, Mutex as AsyncMutex};
use tokio::task::AbortHandle;
use thiserror::Error;

use super::error::JsonRpcError;
use super::events::agent_event_to_json;
use super::protocol::{parse_incoming, Incoming, Response, JSONRPC_VERSION};
use super::{RpcOutcome, RpcServerOptions};
use crate::prompt_templates::{expand_prompt_template, PromptTemplate};

/// Errors that terminate the RPC server.
#[derive(Debug, Error)]
pub enum RpcServerError {
    /// An I/O operation failed for a reason other than the consumer
    /// going away.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// stdout was closed by the consumer (`EPIPE` / reset). Treated as a
    /// clean shutdown — the server must never panic on a broken pipe.
    #[error("stdout closed")]
    Disconnected,
}

impl RpcServerError {
    /// Classify a stdout write failure. Broken-pipe style errors become
    /// [`RpcServerError::Disconnected`].
    fn from_stdout(err: std::io::Error) -> Self {
        use std::io::ErrorKind;
        match err.kind() {
            ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted => {
                Self::Disconnected
            }
            _ => Self::Io(err),
        }
    }
}

/// Run the RPC server until stdin reaches EOF (or stdout closes).
///
/// A normal EOF shutdown — the in-flight turn (if any) is drained so its
/// events reach stdout before the process exits — returns `Ok`.
pub async fn run_rpc_server(options: RpcServerOptions) -> Result<RpcOutcome, RpcServerError> {
    match run_inner(options).await {
        Err(RpcServerError::Disconnected) => Ok(RpcOutcome::default()),
        other => other,
    }
}

async fn run_inner(options: RpcServerOptions) -> Result<RpcOutcome, RpcServerError> {
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<TurnFinished>();
    let mut server = Server::new(options, done_tx);
    let mut lines = BufReader::new(tokio::io::stdin()).lines();

    loop {
        let line = tokio::select! {
            biased;
            // A finished turn first: it frees the slot and writes the
            // deferred `prompt` response.
            finished = done_rx.recv(), if server.current.is_some() => {
                server.handle_done(finished).await?;
                if server.shared.write_failed() {
                    return Err(RpcServerError::Disconnected);
                }
                continue;
            }
            line = lines.next_line() => line,
        };

        match line {
            Ok(Some(line)) => server.handle_line(&line).await?,
            // stdin EOF → graceful shutdown.
            Ok(None) => break,
            Err(err) => {
                eprintln!("pi: rpc: stdin read error: {err}");
                break;
            }
        }

        // A failed stdout write means the consumer went away; stop even
        // when the failing write belonged to a `prompt` sent as a
        // notification (which owes no response).
        if server.shared.write_failed() {
            return Err(RpcServerError::Disconnected);
        }
    }

    // Let the in-flight turn finish and flush its events.
    server.drain_in_flight(&mut done_rx).await?;

    Ok(RpcOutcome {
        requests: server.requests,
        turns: server.shared.turns(),
    })
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Message the spawned turn task sends back when it finishes.
#[derive(Debug)]
struct TurnFinished {
    /// Turn generation — lets the loop discard completions from a turn it
    /// already abandoned via `abort`.
    generation: u64,
    /// 1-based turn number after this turn completed.
    turn: u32,
    /// Last stop reason observed on the event stream.
    stop_reason: StopReason,
    /// Set when the agent loop returned an error.
    error: Option<String>,
}

/// Handle to the single in-flight turn.
struct TurnTask {
    generation: u64,
    /// Request id to answer with once the turn completes. `None` for a
    /// `prompt` sent as a notification.
    request_id: Option<Value>,
    /// Turn-level cancellation flag (checked by the event pump).
    cancel: Arc<AtomicBool>,
    /// Abort handle for the owning task.
    abort: AbortHandle,
}

/// State shared between the server loop and the spawned turn task.
struct SharedState {
    /// Mirror of the conversation log. Kept here (not read through the
    /// agent mutex) so `getState` answers even while a turn holds the
    /// lock.
    messages: parking_lot::Mutex<Vec<Message>>,
    /// Active model. `setModel` writes here; the next turn applies it to
    /// the agent, so a model switch never blocks on an in-flight turn.
    model: parking_lot::Mutex<Model>,
    /// Completed turn counter.
    turns: AtomicU32,
    /// Set by the turn worker when a stdout write fails, so the main loop
    /// exits even if no response was owed.
    write_failed: AtomicBool,
}

impl SharedState {
    fn new(model: Model) -> Self {
        Self {
            messages: parking_lot::Mutex::new(Vec::new()),
            model: parking_lot::Mutex::new(model),
            turns: AtomicU32::new(0),
            write_failed: AtomicBool::new(false),
        }
    }

    fn model(&self) -> Model {
        self.model.lock().clone()
    }

    fn set_model(&self, model: Model) {
        *self.model.lock() = model;
    }

    fn messages(&self) -> Vec<Message> {
        self.messages.lock().clone()
    }

    fn push_message(&self, message: Message) {
        self.messages.lock().push(message);
    }

    fn turns(&self) -> u32 {
        self.turns.load(Ordering::SeqCst)
    }

    /// Record that stdout is no longer usable.
    fn mark_write_failed(&self) {
        self.write_failed.store(true, Ordering::SeqCst);
    }

    /// Whether a stdout write has failed since startup.
    fn write_failed(&self) -> bool {
        self.write_failed.load(Ordering::SeqCst)
    }

    /// Increment the completed-turn counter and return the new value.
    fn bump_turn(&self) -> u32 {
        self.turns.fetch_add(1, Ordering::SeqCst) + 1
    }
}

/// Serialized stdout sink. Cloned into the turn task so events stream
/// while the main loop keeps reading stdin.
#[derive(Clone)]
struct Writer {
    out: Arc<AsyncMutex<Stdout>>,
}

impl Writer {
    fn new() -> Self {
        Self {
            out: Arc::new(AsyncMutex::new(tokio::io::stdout())),
        }
    }

    async fn write_value(&self, value: &Value) -> Result<(), RpcServerError> {
        let mut line = value.to_string();
        line.push('\n');
        let mut guard = self.out.lock().await;
        guard
            .write_all(line.as_bytes())
            .await
            .map_err(RpcServerError::from_stdout)?;
        guard.flush().await.map_err(RpcServerError::from_stdout)?;
        Ok(())
    }

    async fn write_response(&self, response: &Response) -> Result<(), RpcServerError> {
        let value = serde_json::to_value(response).map_err(|err| {
            RpcServerError::Io(std::io::Error::other(format!(
                "failed to serialize JSON-RPC response: {err}"
            )))
        })?;
        self.write_value(&value).await
    }

    async fn write_notification(&self, method: &str, params: Value) -> Result<(), RpcServerError> {
        self.write_value(&json!({
            "jsonrpc": JSONRPC_VERSION,
            "method": method,
            "params": params,
        }))
        .await
    }
}

struct Server {
    agent: Arc<AsyncMutex<Agent>>,
    writer: Writer,
    shared: Arc<SharedState>,
    models: Arc<Models>,
    session_id: String,
    prompt_templates: Vec<PromptTemplate>,
    done_tx: mpsc::UnboundedSender<TurnFinished>,
    generation: u64,
    current: Option<TurnTask>,
    requests: u64,
}

impl Server {
    fn new(options: RpcServerOptions, done_tx: mpsc::UnboundedSender<TurnFinished>) -> Self {
        let agent = Agent::new(
            AgentOptions::new(
                options.model.clone(),
                options.stream_fn.clone(),
                options.system_prompt.clone(),
            )
            // RPC clients drive the same coding agent the TUI does, so
            // tool calls must execute for real instead of hitting the
            // Stage 2 stub.
            .with_tool_executor(options.tool_executor),
        );
        Self {
            agent: Arc::new(AsyncMutex::new(agent)),
            writer: Writer::new(),
            shared: Arc::new(SharedState::new(options.model)),
            models: Arc::new(options.models),
            session_id: options.session_id,
            prompt_templates: options.prompt_templates,
            done_tx,
            generation: 0,
            current: None,
            requests: 0,
        }
    }

    // -- stdin handling ---------------------------------------------------

    async fn handle_line(&mut self, line: &str) -> Result<(), RpcServerError> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        self.requests += 1;

        let value: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(err) => {
                return self
                    .writer
                    .write_response(&Response::error(None, JsonRpcError::parse_error(err.to_string())))
                    .await;
            }
        };

        let incoming = match parse_incoming(value.clone()) {
            Ok(incoming) => incoming,
            Err(err) => {
                let id = value
                    .get("id")
                    .filter(|id| {
                        matches!(id, Value::String(_) | Value::Number(_) | Value::Null)
                    })
                    .cloned();
                return self.writer.write_response(&Response::error(id, err)).await;
            }
        };

        self.handle_incoming(incoming).await
    }

    async fn handle_incoming(&mut self, incoming: Incoming) -> Result<(), RpcServerError> {
        let expects_response = matches!(incoming, Incoming::Request(_));
        let id = incoming.id().cloned();
        let method = incoming.method().to_string();
        let params = incoming.params().cloned();

        match method.as_str() {
            "prompt" => self.handle_prompt(expects_response, id, params).await,
            "abort" => self.handle_abort(expects_response, id).await,
            "getState" | "get_state" => self.handle_get_state(expects_response, id).await,
            "setModel" | "set_model" => self.handle_set_model(expects_response, id, params).await,
            _ => {
                self.respond(
                    expects_response,
                    Response::error(id, JsonRpcError::method_not_found(&method)),
                )
                .await
            }
        }
    }

    async fn respond(
        &self,
        expects_response: bool,
        response: Response,
    ) -> Result<(), RpcServerError> {
        if expects_response {
            self.writer.write_response(&response).await?;
        }
        Ok(())
    }

    // -- methods ----------------------------------------------------------

    async fn handle_prompt(
        &mut self,
        expects_response: bool,
        id: Option<Value>,
        params: Option<Value>,
    ) -> Result<(), RpcServerError> {
        if self.current.is_some() {
            return self
                .respond(expects_response, Response::error(id, JsonRpcError::busy()))
                .await;
        }

        let parsed: PromptParams = match serde_json::from_value(params.unwrap_or(Value::Null)) {
            Ok(parsed) => parsed,
            Err(err) => {
                return self
                    .respond(
                        expects_response,
                        Response::error(id, JsonRpcError::invalid_params(format!("prompt: {err}"))),
                    )
                    .await;
            }
        };

        if parsed.text.trim().is_empty() {
            return self
                .respond(
                    expects_response,
                    Response::error(
                        id,
                        JsonRpcError::invalid_params("prompt: \"text\" must be non-empty"),
                    ),
                )
                .await;
        }

        self.generation += 1;
        let generation = self.generation;
        let expanded = expand_prompt_template(&parsed.text, &self.prompt_templates);
        self.current = Some(self.spawn_turn(generation, id, expanded));
        Ok(())
    }

    async fn handle_abort(
        &mut self,
        expects_response: bool,
        id: Option<Value>,
    ) -> Result<(), RpcServerError> {
        match self.current.take() {
            Some(task) => {
                task.cancel.store(true, Ordering::SeqCst);
                task.abort.abort();
                self.respond(
                    expects_response,
                    Response::success(id, json!({"aborted": true})),
                )
                .await?;
                // Tell the client the turn boundary it is waiting for.
                let turn = self.shared.bump_turn();
                self.writer
                    .write_notification(
                        "event",
                        json!({
                            "type": "turn_end",
                            "turn": turn,
                            "usage": Usage::default(),
                            "stop_reason": StopReason::Aborted,
                            "tool_results": 0,
                        }),
                    )
                    .await
            }
            None => {
                // Idempotent: aborting with nothing running is a success.
                self.respond(
                    expects_response,
                    Response::success(id, json!({"aborted": false})),
                )
                .await
            }
        }
    }

    async fn handle_get_state(
        &mut self,
        expects_response: bool,
        id: Option<Value>,
    ) -> Result<(), RpcServerError> {
        let state = json!({
            "model": self.shared.model(),
            "messages": self.shared.messages(),
            "sessionId": &self.session_id,
        });
        self.respond(expects_response, Response::success(id, state))
            .await
    }

    async fn handle_set_model(
        &mut self,
        expects_response: bool,
        id: Option<Value>,
        params: Option<Value>,
    ) -> Result<(), RpcServerError> {
        let parsed: SetModelParams = match serde_json::from_value(params.unwrap_or(Value::Null)) {
            Ok(parsed) => parsed,
            Err(err) => {
                return self
                    .respond(
                        expects_response,
                        Response::error(
                            id,
                            JsonRpcError::invalid_params(format!("setModel: {err}")),
                        ),
                    )
                    .await;
            }
        };

        let Some(model) = resolve_model(&self.models, &parsed) else {
            return self
                .respond(
                    expects_response,
                    Response::error(
                        id,
                        JsonRpcError::invalid_params(format!(
                            "setModel: unknown model ({})",
                            parsed.describe()
                        )),
                    ),
                )
                .await;
        };

        self.shared.set_model(model.clone());
        self.respond(
            expects_response,
            Response::success(id, json!({"model": model})),
        )
        .await
    }

    // -- turn lifecycle ---------------------------------------------------

    fn spawn_turn(&self, generation: u64, request_id: Option<Value>, text: String) -> TurnTask {
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = tokio::spawn(run_turn(
            self.agent.clone(),
            self.writer.clone(),
            self.shared.clone(),
            self.done_tx.clone(),
            generation,
            cancel.clone(),
            text,
        ));
        TurnTask {
            generation,
            request_id,
            cancel,
            abort: handle.abort_handle(),
        }
    }

    /// Handle a turn completion (or the disappearance of the turn task).
    async fn handle_done(
        &mut self,
        finished: Option<TurnFinished>,
    ) -> Result<(), RpcServerError> {
        let Some(finished) = finished else {
            // Task vanished without reporting (panic or abort).
            self.current = None;
            return Ok(());
        };

        // Discard completions from a turn we already abandoned.
        if self.current.as_ref().map(|task| task.generation) != Some(finished.generation) {
            return Ok(());
        }
        let task = self.current.take().expect("current checked above");

        let Some(id) = task.request_id else {
            return Ok(());
        };
        let response = match finished.error {
            Some(message) => Response::error(Some(id), JsonRpcError::internal(message)),
            None => Response::success(
                Some(id),
                json!({
                    "turn": finished.turn,
                    "stopReason": finished.stop_reason,
                }),
            ),
        };
        self.writer.write_response(&response).await
    }

    /// After EOF, wait for the in-flight turn so its events reach stdout.
    async fn drain_in_flight(
        &mut self,
        done_rx: &mut mpsc::UnboundedReceiver<TurnFinished>,
    ) -> Result<(), RpcServerError> {
        let Some(generation) = self.current.as_ref().map(|task| task.generation) else {
            return Ok(());
        };
        while let Some(finished) = done_rx.recv().await {
            if finished.generation == generation {
                self.handle_done(Some(finished)).await?;
                break;
            }
        }
        self.current = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Turn worker
// ---------------------------------------------------------------------------

/// Drive exactly one turn: subscribe, prompt, pump events, report back.
async fn run_turn(
    agent: Arc<AsyncMutex<Agent>>,
    writer: Writer,
    shared: Arc<SharedState>,
    done_tx: mpsc::UnboundedSender<TurnFinished>,
    generation: u64,
    cancel: Arc<AtomicBool>,
    text: String,
) {
    shared.push_message(Message {
        role: Role::User,
        content: vec![Content::text(text.clone())],
        model: None,
    });

    let mut guard = agent.lock().await;
    // Apply any model switch queued by `setModel` before this turn.
    guard.set_model(shared.model());
    let mut events = guard.subscribe();

    let mut last_stop = StopReason::Empty;
    let mut error: Option<String> = None;
    // When a stdout write fails the consumer is gone: stop pumping, mark
    // the shared flag, and fall through to report so the server loop can
    // terminate instead of hanging on the drain channel.
    let mut write_failed = false;

    {
        let prompt = guard.prompt(&text);
        tokio::pin!(prompt);

        loop {
            tokio::select! {
                biased;
                result = &mut prompt => {
                    if let Err(err) = result {
                        error = Some(err.to_string());
                    }
                    break;
                }
                event = events.recv() => {
                    match event {
                        Some(event) => {
                            if cancel.load(Ordering::SeqCst) {
                                break;
                            }
                            if emit_agent_event(&writer, &shared, &event, &mut last_stop)
                                .await
                                .is_err()
                            {
                                write_failed = true;
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }

        // Drain anything the agent queued before returning.
        if !write_failed {
            while let Ok(event) = events.try_recv() {
                if emit_agent_event(&writer, &shared, &event, &mut last_stop)
                    .await
                    .is_err()
                {
                    write_failed = true;
                    break;
                }
            }
        }
    }
    drop(guard);

    if write_failed {
        shared.mark_write_failed();
    } else if let Some(message) = error.as_ref() {
        if writer
            .write_notification("event", json!({"type": "error", "message": message}))
            .await
            .is_err()
        {
            shared.mark_write_failed();
        }
    }

    // Always report, even after a write failure: the server loop keys the
    // end of the turn off this message, and must never wait forever.
    let _ = done_tx.send(TurnFinished {
        generation,
        turn: shared.turns(),
        stop_reason: last_stop,
        error,
    });
}

/// Mirror one agent event into the shared log and push it to stdout as an
/// `event` notification.
async fn emit_agent_event(
    writer: &Writer,
    shared: &SharedState,
    event: &AgentEvent,
    last_stop: &mut StopReason,
) -> Result<(), RpcServerError> {
    let turn = match event {
        AgentEvent::TurnEnd {
            message,
            tool_results,
        } => {
            let turn = shared.bump_turn();
            *last_stop = message.stop_reason;
            for result in tool_results {
                shared.push_message(result.clone());
            }
            turn
        }
        AgentEvent::MessageEnd { message } => {
            *last_stop = message.stop_reason;
            shared.push_message(Message {
                role: Role::Assistant,
                content: message.content.clone(),
                model: Some(message.model.clone()),
            });
            shared.turns()
        }
        _ => shared.turns(),
    };

    for payload in agent_event_to_json(event, turn) {
        writer.write_notification("event", payload).await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Params
// ---------------------------------------------------------------------------

/// `prompt` params. The issue's canonical key is `text`; `message` is
/// accepted as an alias for parity with the upstream TypeScript RPC
/// command shape.
#[derive(Debug, Deserialize)]
struct PromptParams {
    #[serde(alias = "message")]
    text: String,
}

/// `setModel` params. Accepts either `{ "model": "provider/id" | "id" }`
/// or the upstream `{ "provider", "modelId" }` pair.
#[derive(Debug, Deserialize)]
struct SetModelParams {
    #[serde(default)]
    model: Option<String>,
    #[serde(default, alias = "providerId")]
    provider: Option<String>,
    #[serde(default, rename = "modelId")]
    model_id: Option<String>,
}

impl SetModelParams {
    /// Human-readable rendering used in error messages.
    fn describe(&self) -> String {
        if let Some(model) = self.model.as_deref() {
            model.to_string()
        } else {
            format!(
                "{}/{}",
                self.provider.as_deref().unwrap_or("?"),
                self.model_id.as_deref().unwrap_or("?")
            )
        }
    }
}

/// Resolve a model reference against the catalog.
fn resolve_model(models: &Models, params: &SetModelParams) -> Option<Model> {
    if let Some(raw) = params.model.as_deref() {
        if let Some((provider, id)) = raw.split_once('/') {
            return models.get_model(&ProviderId::new(provider), id).cloned();
        }
        return models
            .iter()
            .find(|(_, model)| model.id == raw)
            .map(|(_, model)| model.clone());
    }

    match (params.provider.as_deref(), params.model_id.as_deref()) {
        (Some(provider), Some(model_id)) => models
            .get_model(&ProviderId::new(provider), model_id)
            .cloned(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_agent_core::AgentError;
    use pi_protocol::Api;
    use serde_json::json;

    fn fake_model(provider: &str, id: &str) -> Model {
        Model {
            provider: ProviderId::new(provider),
            id: id.into(),
            api: Api::Faux,
            label: None,
            context_window: 8192,
            max_output_tokens: 1024,
        }
    }

    fn catalog() -> Models {
        let mut models = Models::new();
        models.set_provider(
            ProviderId::new("faux"),
            vec![fake_model("faux", "faux-model")],
        );
        models.set_provider(
            ProviderId::new("anthropic"),
            vec![fake_model("anthropic", "claude-sonnet-4-5")],
        );
        models
    }

    #[test]
    fn resolves_provider_qualified_model_ids() {
        let params: SetModelParams =
            serde_json::from_value(json!({"model": "anthropic/claude-sonnet-4-5"})).unwrap();
        let model = resolve_model(&catalog(), &params).expect("resolved");
        assert_eq!(model.provider, ProviderId::new("anthropic"));
        assert_eq!(model.id, "claude-sonnet-4-5");
    }

    #[test]
    fn resolves_bare_model_ids() {
        let params: SetModelParams = serde_json::from_value(json!({"model": "faux-model"})).unwrap();
        assert!(resolve_model(&catalog(), &params).is_some());
    }

    #[test]
    fn resolves_upstream_provider_model_id_pair() {
        let params: SetModelParams =
            serde_json::from_value(json!({"provider": "anthropic", "modelId": "claude-sonnet-4-5"}))
                .unwrap();
        assert!(resolve_model(&catalog(), &params).is_some());
    }

    #[test]
    fn unknown_models_do_not_resolve() {
        let params: SetModelParams = serde_json::from_value(json!({"model": "nope/nope"})).unwrap();
        assert!(resolve_model(&catalog(), &params).is_none());
    }

    #[test]
    fn prompt_params_accept_text_and_message_aliases() {
        let from_text: PromptParams = serde_json::from_value(json!({"text": "hi"})).unwrap();
        assert_eq!(from_text.text, "hi");
        let from_message: PromptParams = serde_json::from_value(json!({"message": "hi"})).unwrap();
        assert_eq!(from_message.text, "hi");
    }

    #[tokio::test]
    async fn abort_without_a_turn_is_idempotent() {
        // No runtime stdin/stdout is touched: `handle_abort` only writes
        // when `expects_response` is true (it is false here).
        let (done_tx, _done_rx) = mpsc::unbounded_channel();
        let mut server = Server::new(
            RpcServerOptions {
                model: fake_model("faux", "faux-model"),
                models: catalog(),
                stream_fn: pi_ai::stream::SharedStreamFn::from(
                    std::sync::Arc::new(pi_ai::providers::faux::FauxProvider::default())
                        as std::sync::Arc<dyn pi_ai::stream::StreamFn>,
                ),
                system_prompt: String::new(),
                session_id: "session-test".into(),
                prompt_templates: Vec::new(),
                tool_executor: crate::tool_executor::default_executor(),
            },
            done_tx,
        );
        server
            .handle_abort(false, None)
            .await
            .expect("idempotent abort");
        assert!(server.current.is_none());
    }

    #[test]
    fn agent_errors_map_to_internal_error() {
        let err: AgentError = AgentError::Stream("boom".into());
        let mapped = JsonRpcError::internal(err.to_string());
        assert_eq!(mapped.code, super::super::error::codes::INTERNAL_ERROR);
    }
}
