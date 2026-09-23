//! Agent loop — Stage 2 driver. Mirrors the loop in
//! `packages/agent/src/agent-loop.ts`, including the `should_stop_after_turn`
//! and `prepare_next_turn` hook points.
//!
//! Stage 2 keeps the surface tight: the loop streams a single assistant
//! turn per iteration, executes inline tool calls through the registered
//! [`ToolExecutor`](crate::tools::ToolExecutor) (falling back to a stub when
//! none is configured), and forwards `should_stop_after_turn` /
//! `prepare_next_turn` decisions to the user-registered hooks. Stage 4 fills
//! in queue draining and steering / follow-up message sources.
//!
//! # Cancellation
//!
//! Tool batches honour the loop's [`CancellationToken`] the way upstream
//! honours its `AbortSignal` (`packages/agent/src/agent-loop.ts:409-545`):
//! the sequential path stops after the call that observes the abort, and the
//! parallel path stops preparing once aborted and finalizes a queued-but-not-
//! yet-started call as an `Operation aborted` error result. The abort check
//! is *not* a pre-flight guard in the sequential path — the first call still
//! runs with the token it was handed, exactly as upstream does.

use futures::StreamExt;
use pi_ai::stream::SharedStreamFn;
use pi_protocol::{
    AssistantMessage, AssistantMessageEvent, Content, Context as AgentContext, Message, Role,
    ToolCall, ToolExecutionMode, ToolResult,
};
use pi_telemetry::{SpanOptions, SpanRef, SpanStatus, TelemetryContextExt};
use std::sync::Arc;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::events::{AgentEvent, AssistantMessageUpdate};
use crate::hooks::{
    AgentHookAdapter, AgentLoopTurnUpdate, PrepareNextTurnContext, ShouldStopAfterTurnContext,
};
use crate::retry::{retry_assistant_call, RetryPolicy};
use crate::state::{AgentConfig, AgentState};
use crate::telemetry::{
    attribute_name, request_error_attributes, response_attributes, span_name, tool_attributes,
};
use crate::tools::ToolExecutor;

/// Errors the agent loop can surface to its caller.
#[derive(Debug, Error)]
pub enum AgentError {
    /// Streaming layer returned an error.
    #[error("stream error: {0}")]
    Stream(String),
    /// Tool execution failed.
    #[error("tool error in {tool}: {message}")]
    Tool {
        /// Tool name.
        tool: String,
        /// Error message.
        message: String,
    },
    /// Provider produced an `AssistantMessageEvent::Error`.
    #[error("provider error: {0}")]
    Provider(String),
}

/// Outcome of a single [`AgentLoop::run`] invocation.
#[derive(Debug, Clone)]
pub struct TurnOutcome {
    /// Final assistant message.
    pub message: AssistantMessage,
    /// Tool results produced by this invocation (one per tool call).
    pub tool_results: Vec<Message>,
    /// True if the loop asked the model to call a tool and that tool ran.
    pub tool_executed: bool,
}

/// Snapshot of the live loop configuration — `model` plus the optional
/// `thinking_level` override a `prepare_next_turn` hook can install.
#[derive(Debug, Clone)]
pub struct LoopConfig {
    /// Active model. Mutated in place by `prepare_next_turn`.
    pub model: pi_protocol::Model,
    /// Optional thinking level override.
    pub thinking_level: Option<crate::hooks::ThinkingLevel>,
    /// How a tool batch without any `Sequential` tool is dispatched.
    /// Copied from [`AgentConfig::tool_execution`].
    pub tool_execution: ToolExecutionMode,
    /// Agent-level retry budget for the assistant call. Copied from
    /// [`AgentConfig::retry`] — `prepare_next_turn` cannot change it.
    pub retry: RetryPolicy,
}

impl From<&AgentConfig> for LoopConfig {
    fn from(config: &AgentConfig) -> Self {
        Self {
            model: config.model.clone(),
            thinking_level: None,
            tool_execution: config.tool_execution,
            retry: config.retry,
        }
    }
}

/// One assistant response plus the tool batch it triggered.
struct TurnBatch {
    message: AssistantMessage,
    tool_results: Vec<ToolResult>,
    continue_loop: bool,
}

/// Sink the loop calls **synchronously at every real event point** — the
/// moment a turn starts, a provider delta arrives, a tool call is dispatched
/// or finishes.
///
/// The observer is a plain `Fn` (no `async`): events are emitted in the order
/// the loop produces them, and a fan-out that pushes onto channels cannot
/// reorder them. `Agent` installs one that fans every event out to its
/// [`subscribe`](crate::Agent::subscribe) channels; the TUI, the RPC pump and
/// the WASM bridge all consume that single feed.
///
/// A loop without an observer behaves exactly as before — the events are
/// simply dropped, not buffered.
pub type EventObserver = Arc<dyn Fn(AgentEvent) + Send + Sync>;

/// Forward one event to an optional observer.
fn emit_event(observer: Option<&EventObserver>, event: AgentEvent) {
    if let Some(observer) = observer {
        observer(event);
    }
}

/// Single agent turn entry point.
#[derive(Clone)]
pub struct AgentLoop {
    config: AgentConfig,
    state: AgentState,
    hooks: AgentHookAdapter,
    follow_up: Vec<Message>,
    signal: CancellationToken,
    observer: Option<EventObserver>,
    /// Session thinking level applied to the next provider call.
    ///
    /// Carried on the loop (not on [`AgentConfig`]) so the value survives the
    /// per-run `LoopConfig` rebuild and is overridable per turn by a
    /// `prepare_next_turn` hook. `None` until the session sets one; the
    /// provider call then keeps its previous default.
    thinking_level: Option<crate::hooks::ThinkingLevel>,
}

impl AgentLoop {
    /// Construct a loop from a configuration, initial state, and hook
    /// adapter.
    pub fn new(config: AgentConfig, state: AgentState, hooks: AgentHookAdapter) -> Self {
        Self {
            config,
            state,
            hooks,
            follow_up: Vec::new(),
            signal: CancellationToken::new(),
            observer: None,
            thinking_level: None,
        }
    }

    /// Install (or clear) the [`EventObserver`] the loop reports to.
    ///
    /// [`Agent::prompt`](crate::Agent::prompt) sets a fan-out observer for the
    /// duration of the call and clears it afterwards; standalone
    /// [`AgentLoop::run`] callers may install their own.
    pub fn set_event_observer(&mut self, observer: Option<EventObserver>) {
        self.observer = observer;
    }

    /// Borrow the currently-installed observer, if any.
    pub fn event_observer(&self) -> Option<&EventObserver> {
        self.observer.as_ref()
    }

    /// Borrow the current state.
    pub fn state(&self) -> &AgentState {
        &self.state
    }

    /// Mutable borrow of the state — used by [`Agent`] for
    /// per-turn overrides and by the session layer to inspect /
    /// mutate the message log.
    pub fn state_mut(&mut self) -> &mut AgentState {
        &mut self.state
    }

    /// Borrow the configuration.
    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Mutable borrow of the configuration — used by [`Agent::set_model`]
    /// to swap the active model between turns.
    pub fn config_mut(&mut self) -> &mut AgentConfig {
        &mut self.config
    }

    /// The session thinking level applied to the next provider call.
    ///
    /// `None` means no level has been chosen; a `prepare_next_turn` hook can
    /// still install one for a single turn via
    /// [`AgentLoopTurnUpdate::thinking_level`](crate::hooks::AgentLoopTurnUpdate::thinking_level).
    pub fn thinking_level(&self) -> Option<crate::hooks::ThinkingLevel> {
        self.thinking_level
    }

    /// Set the session thinking level for every following turn. The value is
    /// copied into each run's [`LoopConfig`] before the provider call is
    /// assembled.
    pub fn set_thinking_level(&mut self, level: crate::hooks::ThinkingLevel) {
        self.thinking_level = Some(level);
    }

    /// Borrow the hook adapter driving this loop.
    pub fn hooks(&self) -> &AgentHookAdapter {
        &self.hooks
    }

    /// Mutable borrow of the hook adapter — tests use this to swap hooks
    /// in mid-run.
    pub fn hooks_mut(&mut self) -> &mut AgentHookAdapter {
        &mut self.hooks
    }

    /// Cooperative cancellation handle handed to tool executors.
    pub fn cancellation_token(&self) -> CancellationToken {
        self.signal.clone()
    }

    /// Replace the cancellation handle used for tool execution. A caller can
    /// cancel the returned token to ask the active executor to stop.
    pub fn set_cancellation_token(&mut self, signal: CancellationToken) {
        self.signal = signal;
    }

    /// Add a user message to the log. Real draining happens during
    /// [`AgentLoop::run`].
    pub fn push_user(&mut self, message: Message) {
        self.state.messages.push(message);
    }

    /// Push a follow-up message that the next [`AgentLoop::run`] will
    /// process after the loop's initial prompt messages.
    pub fn push_follow_up(&mut self, message: Message) {
        self.follow_up.push(message);
    }

    /// Number of queued follow-up messages waiting to be processed.
    pub fn follow_up_len(&self) -> usize {
        self.follow_up.len()
    }

    /// Run the loop until the agent would stop, calling
    /// `should_stop_after_turn` and `prepare_next_turn` at the right
    /// points and forwarding their decisions to the live configuration.
    ///
    /// Stage 2 returns the outcome of the *final* turn — multiple turns
    /// are driven internally so a `prepare_next_turn` hook can swap
    /// `context` / `model` between turns. The per-turn outcomes are
    /// emitted via the `on_turn` callback (Stage 4 wires the public
    /// event stream).
    ///
    /// When [`AgentConfig::telemetry`] is set, the whole invocation runs
    /// inside a `pi.harness.run` span so every turn / request / tool span
    /// nests underneath it.
    pub async fn run(
        &mut self,
        prompts: Vec<Message>,
        on_turn: impl FnMut(&TurnOutcome) + Send,
    ) -> Result<TurnOutcome, AgentError> {
        let root = self.config.telemetry.clone();
        match root {
            None => self.run_inner(prompts, on_turn, None).await,
            Some(root) => {
                let options = SpanOptions::new(span_name::HARNESS_RUN)
                    .with_attribute(attribute_name::OPERATION_KIND, "run");
                root.start_span_with(options, |span| async move {
                    let outcome = self.run_inner(prompts, on_turn, Some(span.clone())).await;
                    let mut attributes = pi_telemetry::SpanAttributes::new();
                    attributes.insert(
                        attribute_name::OPERATION_OUTCOME.to_owned(),
                        if outcome.is_ok() {
                            "completed"
                        } else {
                            "failed"
                        }
                        .into(),
                    );
                    span.set_attributes(attributes);
                    outcome
                })
                .await
            }
        }
    }

    /// Inner loop body — `run` wraps this so telemetry can close the run span
    /// on every `return` path.
    async fn run_inner(
        &mut self,
        prompts: Vec<Message>,
        mut on_turn: impl FnMut(&TurnOutcome) + Send,
        root: Option<SpanRef>,
    ) -> Result<TurnOutcome, AgentError> {
        // Step 1 — clone initial prompts into the state and the new-message list.
        // The observer is read once per `run` so a caller that clears it
        // mid-run (or installs one from another task) cannot interleave two
        // feeds.
        let observer = self.observer.clone();
        let mut new_messages: Vec<Message> = prompts.clone();
        for prompt in &prompts {
            self.state.messages.push(prompt.clone());
        }

        let mut current_context: AgentContext = AgentContext {
            system_prompt: self.state.system_prompt.clone(),
            messages: self.state.messages.clone(),
            tools: self.config.tool_definitions(),
        };
        let mut loop_config: LoopConfig = (&self.config).into();
        // The session-level thinking level seeds every run of the loop; a
        // `prepare_next_turn` hook may still override it per turn through
        // `apply_turn_update`.
        loop_config.thinking_level = self.thinking_level;

        let mut last_completed_turn: Option<ShouldStopAfterTurnContext> = None;
        let mut has_more_tool_calls = false;
        let mut turn_id: u64 = 0;
        // Pending messages injected at the start of the next turn
        // (analogous to the TS `pendingMessages` buffer). The first
        // turn is always allowed because we have prompts to process.
        let mut pending_messages: Vec<Message> = std::mem::take(&mut self.follow_up);
        if !prompts.is_empty() {
            has_more_tool_calls = true;
        }

        loop {
            if has_more_tool_calls || !pending_messages.is_empty() {
                turn_id += 1;
                // Step 2 — call `prepare_next_turn` before the next turn
                // if we have a previously completed turn to hand it.
                if let Some(turn) = last_completed_turn.take() {
                    let prepare_ctx = PrepareNextTurnContext::from(turn);
                    if let Some(update) = self.hooks.invoke_prepare_next_turn(prepare_ctx).await {
                        apply_turn_update(&update, &mut current_context, &mut loop_config);
                    }
                }

                // Step 3 — drain pending messages into the live context.
                for message in pending_messages.drain(..) {
                    current_context.messages.push(message.clone());
                    new_messages.push(message);
                }

                // Step 3b — announce the turn. Upstream emits `turn_start`
                // here: after `prepare_next_turn` / the pending drain and
                // before the provider request.
                emit_event(observer.as_ref(), AgentEvent::TurnStart);

                // Step 4 — stream one assistant response and run the tool
                // batch it produced. Both nest inside a single
                // `pi.harness.turn` span when telemetry is installed.
                // `prepare_next_turn` may have replaced the context
                // wholesale; re-advertise the registered tools unless the
                // hook supplied its own list.
                if current_context.tools.is_empty() {
                    current_context.tools = self.config.tool_definitions();
                }
                // Upstream `context`: fired before each provider call so a
                // plugin can rewrite the message list the request carries
                // (`agent-session.ts` `transformContext` →
                // `ExtensionRunner.emitContext`). The rewrite is applied to
                // the live context, so a handler can also drop messages the
                // model must not see.
                let messages = std::mem::take(&mut current_context.messages);
                current_context.messages = self.hooks.invoke_context(messages).await;
                let batch = match root.as_ref() {
                    None => {
                        run_turn_batch(
                            &self.config.stream_fn,
                            &current_context,
                            &loop_config,
                            self.config.tool_executor.as_ref(),
                            &self.hooks,
                            &self.signal,
                            TurnSinks {
                                observer: observer.as_ref(),
                                telemetry: None,
                            },
                        )
                        .await?
                    }
                    Some(parent) => {
                        let stream_fn = self.config.stream_fn.clone();
                        let executor = self.config.tool_executor.clone();
                        let hooks = self.hooks.clone();
                        let signal = self.signal.clone();
                        let context = current_context.clone();
                        let loop_config = loop_config.clone();
                        // The observer is shared, not cloned per event, so the
                        // telemetry branch only clones the `Arc`.
                        let observer = observer.clone();
                        let options = SpanOptions::new(span_name::HARNESS_TURN)
                            .with_attribute(attribute_name::TURN_ID, turn_id.to_string());
                        parent
                            .start_span_with(options, move |turn_span| async move {
                                run_turn_batch(
                                    &stream_fn,
                                    &context,
                                    &loop_config,
                                    executor.as_ref(),
                                    &hooks,
                                    &signal,
                                    TurnSinks {
                                        observer: observer.as_ref(),
                                        telemetry: Some(&turn_span),
                                    },
                                )
                                .await
                            })
                            .await?
                    }
                };
                let TurnBatch {
                    message: assistant_message,
                    tool_results,
                    continue_loop,
                } = batch;
                let assistant_log_message = Message {
                    role: Role::Assistant,
                    content: assistant_message.content.clone(),
                    model: Some(assistant_message.model.clone()),
                };
                current_context.messages.push(assistant_log_message.clone());
                new_messages.push(assistant_log_message);

                // Step 5 — fold the executed tool results into the context.
                // `BeforeToolCall` may block a call, the executor may fail,
                // and `AfterToolCall` may rewrite the result — none of which
                // aborts the turn.
                let mut tool_result_messages: Vec<Message> = Vec::with_capacity(tool_results.len());
                for result in tool_results {
                    let msg = Message {
                        role: Role::Tool,
                        content: vec![pi_protocol::Content::ToolResult(result.clone())],
                        model: None,
                    };
                    current_context.messages.push(msg.clone());
                    new_messages.push(msg.clone());
                    tool_result_messages.push(msg);
                }
                has_more_tool_calls = continue_loop;

                let outcome = TurnOutcome {
                    message: assistant_message.clone(),
                    tool_results: tool_result_messages.clone(),
                    tool_executed: !tool_result_messages.is_empty(),
                };
                // Step 5b — the tool results are back in the context, so the
                // turn is complete. Upstream emits `turn_end` at this point,
                // before `should_stop_after_turn` runs.
                emit_event(
                    observer.as_ref(),
                    AgentEvent::TurnEnd {
                        message: outcome.message.clone(),
                        tool_results: outcome.tool_results.clone(),
                    },
                );
                on_turn(&outcome);

                // Step 6 — build the completed-turn context the next
                // hook will consume.
                let turn = ShouldStopAfterTurnContext {
                    message: assistant_message,
                    tool_results: tool_result_messages,
                    context: current_context.clone(),
                    new_messages: new_messages.clone(),
                };

                // Step 7 — ask the user hook whether the agent should
                // stop here.
                if self.hooks.invoke_should_stop(turn.clone()).await {
                    // Sync our state with the live context before
                    // exiting so subsequent `state.messages` reads
                    // reflect the run.
                    self.state.messages = current_context.messages.clone();
                    return Ok(TurnOutcome {
                        message: turn.message,
                        tool_results: turn.tool_results,
                        tool_executed: outcome.tool_executed,
                    });
                }

                last_completed_turn = Some(turn);

                // Step 8 — check for newly-arrived follow-up messages.
                if !has_more_tool_calls {
                    let follow_up = std::mem::take(&mut self.follow_up);
                    if follow_up.is_empty() {
                        self.state.messages = current_context.messages.clone();
                        return Ok(TurnOutcome {
                            message: last_completed_turn
                                .as_ref()
                                .map(|t| t.message.clone())
                                .unwrap(),
                            tool_results: last_completed_turn
                                .as_ref()
                                .map(|t| t.tool_results.clone())
                                .unwrap_or_default(),
                            tool_executed: outcome.tool_executed,
                        });
                    }
                    pending_messages = follow_up;
                    has_more_tool_calls = true;
                }
            } else {
                break;
            }
        }

        // Sync state and return.
        self.state.messages = current_context.messages.clone();
        Ok(TurnOutcome {
            message: last_completed_turn
                .as_ref()
                .map(|t| t.message.clone())
                .unwrap_or_else(|| AssistantMessage {
                    model: loop_config.model.id.clone(),
                    content: Vec::new(),
                    stop_reason: pi_protocol::StopReason::Empty,
                    usage: pi_protocol::Usage::default(),
                    error_message: None,
                }),
            tool_results: last_completed_turn
                .as_ref()
                .map(|t| t.tool_results.clone())
                .unwrap_or_default(),
            tool_executed: false,
        })
    }
}

/// Apply an [`AgentLoopTurnUpdate`] to the live loop configuration. Each
/// `Some(_)` field overrides the previous value; `None` leaves the field
/// untouched.
fn apply_turn_update(
    update: &AgentLoopTurnUpdate,
    context: &mut AgentContext,
    config: &mut LoopConfig,
) {
    if let Some(replacement) = &update.context {
        *context = replacement.clone();
    }
    if let Some(model) = &update.model {
        config.model = model.clone();
    }
    if let Some(level) = update.thinking_level {
        config.thinking_level = Some(level);
    }
}

/// The two sinks a turn writes its observations to: the live event observer
/// (when a consumer is attached) and the telemetry span the turn runs inside
/// (when telemetry is enabled). Bundled so the turn plumbing keeps one
/// "where do the observations go" argument instead of one per sink.
#[derive(Clone, Copy)]
struct TurnSinks<'a> {
    observer: Option<&'a EventObserver>,
    telemetry: Option<&'a SpanRef>,
}

/// Run one turn: stream the assistant response, then execute the tool batch
/// it produced.
async fn run_turn_batch(
    stream_fn: &SharedStreamFn,
    context: &AgentContext,
    config: &LoopConfig,
    executor: Option<&Arc<dyn ToolExecutor>>,
    hooks: &AgentHookAdapter,
    signal: &CancellationToken,
    sinks: TurnSinks<'_>,
) -> Result<TurnBatch, AgentError> {
    let assistant_message =
        stream_assistant_response_with_retry(stream_fn, context, config, hooks, signal, sinks)
            .await?;
    let (tool_results, continue_loop) = execute_tool_calls(
        executor,
        hooks,
        &assistant_message,
        config.tool_execution,
        signal,
        sinks.observer,
        sinks.telemetry,
    )
    .await;
    Ok(TurnBatch {
        message: assistant_message,
        tool_results,
        continue_loop,
    })
}

/// Stream one assistant response, retrying transient provider failures when
/// the loop's [`RetryPolicy`] allows it.
///
/// The retry loop wraps the *whole* `pi.ai.request` span, so a retried attempt
/// opens its own span and re-emits `MessageStart` / deltas after the failed
/// attempt's truncated sequence — a failed attempt is not rolled back, exactly
/// like upstream (which surfaces the failed message and then restarts the
/// turn). A policy that is disabled returns the first response unchanged.
async fn stream_assistant_response_with_retry(
    stream_fn: &SharedStreamFn,
    context: &AgentContext,
    config: &LoopConfig,
    hooks: &AgentHookAdapter,
    signal: &CancellationToken,
    sinks: TurnSinks<'_>,
) -> Result<AssistantMessage, AgentError> {
    let policy = config.retry;
    if !policy.enabled {
        return stream_assistant_response(stream_fn, context, config, hooks, sinks).await;
    }
    retry_assistant_call(
        || stream_assistant_response(stream_fn, context, config, hooks, sinks),
        Some(&policy),
        &config.model.id,
        signal,
        None,
    )
    .await
}

/// Stream a single assistant response, wrapping it in a `pi.ai.request` span
/// when a telemetry parent is installed.
async fn stream_assistant_response(
    stream_fn: &SharedStreamFn,
    context: &AgentContext,
    config: &LoopConfig,
    hooks: &AgentHookAdapter,
    sinks: TurnSinks<'_>,
) -> Result<AssistantMessage, AgentError> {
    let Some(parent) = sinks.telemetry else {
        return stream_assistant_events(stream_fn, context, config, hooks, sinks.observer).await;
    };
    let options = SpanOptions::new(span_name::AI_REQUEST)
        .with_attribute(attribute_name::AI_OPERATION, "stream")
        .with_attribute(
            attribute_name::AI_PROVIDER,
            config.model.provider.to_string(),
        )
        .with_attribute(attribute_name::AI_MODEL, config.model.id.clone())
        .with_attribute(
            attribute_name::AI_API,
            crate::telemetry::api_name(config.model.api),
        )
        .with_attribute(attribute_name::AI_STREAMING, true);
    parent
        .start_span_with(options, |span| async move {
            match stream_assistant_events(stream_fn, context, config, hooks, sinks.observer).await {
                Ok(message) => {
                    span.set_attributes(response_attributes(&message));
                    Ok(message)
                }
                Err(error) => {
                    span.set_attributes(request_error_attributes(&error));
                    Err(error)
                }
            }
        })
        .await
}

/// Stream a single assistant response from the provider, forwarding each
/// provider event to the observer at the moment it arrives and folding the
/// stream into the final [`AssistantMessage`].
///
/// The observer sees `MessageStart` on `AssistantMessageEvent::Start`, one
/// `MessageUpdate` per `TextDelta` / `ThinkingDelta` / `ToolCallDelta`, and a
/// single `MessageEnd` once the message is assembled.
///
/// **Failure paths truncate the sequence.** A `Stream` / `Provider` error
/// returns `Err` after `TurnStart` (and possibly `MessageStart` / some
/// `MessageUpdate`s) have already been emitted — there is then no `MessageEnd`
/// or `TurnEnd`. Consumers must tolerate a truncated event sequence; this is
/// deliberate (upstream behaves the same way) and replaces the old behaviour
/// where a failed turn emitted no events at all.
async fn stream_assistant_events(
    stream_fn: &SharedStreamFn,
    context: &AgentContext,
    config: &LoopConfig,
    hooks: &AgentHookAdapter,
    observer: Option<&EventObserver>,
) -> Result<AssistantMessage, AgentError> {
    let mut options = pi_ai::SimpleStreamOptions::default();
    // Stage 67 — the session thinking level reaches the provider call here.
    //
    // The Rust port cannot yet encode a thinking budget on the wire:
    // `pi_ai::SimpleStreamOptions` has no `reasoning`/`thinking` field
    // (`pi-ai/src/types.rs:11`), and the descriptor the provider receives has
    // no `reasoning` flag either (`pi-ai/src/providers/registry.rs:585` drops
    // upstream's `Model.reasoning`). Both live in the frozen `pi-ai` crate, so
    // this is the deepest seam available: an extended level is consumed while
    // the request is assembled and suppresses the temperature override, which
    // is upstream's rule for thinking-enabled requests
    // (`pi-ai/src/providers/anthropic.rs:263`).
    if config
        .thinking_level
        .is_some_and(|level| level.is_reasoning())
    {
        options.temperature = None;
    }
    // Upstream `before_provider_request` / `before_provider_headers`: fired
    // around the provider call so a plugin can rewrite the wire payload or
    // inject headers (`packages/coding-agent/src/core/sdk.ts:320-360`).
    //
    // Fidelity gap (documented in `docs/LUM1432_EXTENSION_EVENTS.md`): the
    // Rust `StreamFn` takes a typed `Context`, and the `pi-ai` adapters build
    // their own HTTP request from a credential + base URL, so there is no
    // seam to hand a rewritten payload or header map back to. The events are
    // therefore constructed and delivered with the data this port actually
    // has — the request descriptor the adapter receives and an empty header
    // map — and the returned values are intentionally ignored at this seam.
    let payload = serde_json::json!({
        "model": {
            "id": config.model.id,
            "provider": config.model.provider.to_string(),
        },
        "systemPrompt": context.system_prompt,
        "messages": serde_json::to_value(&context.messages).unwrap_or(serde_json::Value::Null),
        "temperature": options.temperature,
        "maxTokens": options.max_tokens,
    });
    let _ = hooks.invoke_before_provider_request(payload).await;
    let _ = hooks
        .invoke_before_provider_headers(serde_json::Map::new())
        .await;
    let mut stream = match stream_fn
        .stream_simple(&config.model, context, &options)
        .await
    {
        Ok(stream) => {
            hooks.invoke_after_provider_response(true).await;
            stream
        }
        Err(err) => {
            hooks.invoke_after_provider_response(false).await;
            return Err(AgentError::Stream(err.to_string()));
        }
    };

    let mut final_message: Option<AssistantMessage> = None;
    // `Done` closes the message; a stream that ends without one (e.g. after
    // `Aborted`) still owes consumers a `MessageEnd`.
    let mut message_ended = false;
    // Which delta kinds the provider actually streamed — see
    // [`emit_unstreamed_content`].
    let mut saw_text_delta = false;
    let mut saw_tool_call_delta = false;
    while let Some(event) = stream.next().await {
        match event.map_err(|err| AgentError::Stream(err.to_string()))? {
            AssistantMessageEvent::Start { model } => {
                final_message = Some(AssistantMessage {
                    model: model.clone(),
                    content: Vec::new(),
                    stop_reason: pi_protocol::StopReason::Empty,
                    usage: pi_protocol::Usage::default(),
                    error_message: None,
                });
                emit_event(observer, AgentEvent::MessageStart { model });
            }
            AssistantMessageEvent::Done {
                content,
                stop_reason,
                usage,
            } => {
                if let Some(message) = final_message.as_mut() {
                    message.content = content;
                    message.stop_reason = stop_reason;
                    message.usage = usage;
                } else {
                    final_message = Some(AssistantMessage {
                        model: config.model.id.clone(),
                        content,
                        stop_reason,
                        usage,
                        error_message: None,
                    });
                }
                if let Some(message) = final_message.as_ref() {
                    emit_unstreamed_content(observer, message, saw_text_delta, saw_tool_call_delta);
                    emit_event(
                        observer,
                        AgentEvent::MessageEnd {
                            message: message.clone(),
                        },
                    );
                    message_ended = true;
                }
            }
            AssistantMessageEvent::Error { message } => {
                return Err(AgentError::Provider(message));
            }
            AssistantMessageEvent::Aborted => {
                if let Some(message) = final_message.as_mut() {
                    message.stop_reason = pi_protocol::StopReason::Aborted;
                }
            }
            // Deltas are forwarded verbatim — the loop no longer collapses
            // the stream into the final message only (Stage 40).
            AssistantMessageEvent::TextDelta { delta } => {
                saw_text_delta = true;
                emit_event(
                    observer,
                    AgentEvent::MessageUpdate(AssistantMessageUpdate::TextDelta { delta }),
                );
            }
            AssistantMessageEvent::ThinkingDelta { delta } => {
                emit_event(
                    observer,
                    AgentEvent::MessageUpdate(AssistantMessageUpdate::ThinkingDelta { delta }),
                );
            }
            AssistantMessageEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => {
                saw_tool_call_delta = true;
                emit_event(
                    observer,
                    AgentEvent::MessageUpdate(AssistantMessageUpdate::ToolCallDelta {
                        index,
                        id,
                        name,
                        arguments_delta,
                    }),
                );
            }
        }
    }

    let message = final_message.ok_or_else(|| {
        AgentError::Stream("provider stream closed without a Start or Done event".into())
    })?;
    if !message_ended {
        emit_event(
            observer,
            AgentEvent::MessageEnd {
                message: message.clone(),
            },
        );
    }
    Ok(message)
}

/// Emit `MessageUpdate`s for message content the provider delivered only in
/// its final `Done` event.
///
/// This is a **compatibility backfill, not a streaming path**: real providers
/// stream their deltas, which are forwarded one by one above, and
/// `saw_*_delta` then suppresses the corresponding backfill. It is needed
/// because `pi-ai`'s `FauxProvider` emits `Start` + `Done` only (out of scope
/// this round), while delta-driven consumers render text exclusively from
/// `MessageUpdate` — without it a faux turn would render as empty in
/// `pi-coding-agent`'s print mode and in the TUI.
///
/// The backfill runs immediately before `MessageEnd`, so it never reorders
/// the event sequence, and it only fires for a delta kind the provider never
/// emitted at all — a partially streamed message is never re-emitted in full.
fn emit_unstreamed_content(
    observer: Option<&EventObserver>,
    message: &AssistantMessage,
    saw_text_delta: bool,
    saw_tool_call_delta: bool,
) {
    for (index, block) in message.content.iter().enumerate() {
        let update = match block {
            Content::Text(text) if !saw_text_delta && !text.text.is_empty() => {
                Some(AssistantMessageUpdate::TextDelta {
                    delta: text.text.clone(),
                })
            }
            Content::ToolCall(call) if !saw_tool_call_delta => {
                Some(AssistantMessageUpdate::ToolCallDelta {
                    index: index as u32,
                    id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                    arguments_delta: Some(call.arguments.to_string()),
                })
            }
            _ => None,
        };
        if let Some(update) = update {
            emit_event(observer, AgentEvent::MessageUpdate(update));
        }
    }
}

/// Execute the tool calls emitted by an assistant message.
///
/// Mirrors `executeToolCalls` in `packages/agent/src/agent-loop.ts`. A batch
/// is serialized when `mode` is [`ToolExecutionMode::Sequential`] — or when
/// any call in it belongs to a tool whose
/// [`execution_mode`](crate::tools::ToolExecutor::execution_mode) is
/// `Sequential` — and otherwise prepared in source order and executed
/// concurrently. Results are always returned in source order.
///
/// For every call the loop:
///
/// 1. asks the `BeforeToolCall` hook for a decision — a `Block` decision
///    turns into an `is_error: true` result without executing anything;
/// 2. dispatches to the registered [`ToolExecutor`](crate::tools::ToolExecutor)
///    — or, when none is registered, keeps the Stage 2 stub behaviour so
///    callers that predate tool execution still work;
/// 3. asks the `AfterToolCall` hook to rewrite the result, exactly once per
///    executed call.
///
/// Tool failures never abort the turn: they surface as `is_error: true`
/// [`ToolResult`]s so the model can react to them. `continue_loop` mirrors
/// the TypeScript loop's `!terminate` — it is `true` unless *every* tool in
/// the batch requested termination through `BeforeToolCallDecision`.
async fn execute_tool_calls(
    executor: Option<&Arc<dyn ToolExecutor>>,
    hooks: &AgentHookAdapter,
    assistant_message: &AssistantMessage,
    mode: ToolExecutionMode,
    signal: &CancellationToken,
    observer: Option<&EventObserver>,
    telemetry: Option<&SpanRef>,
) -> (Vec<ToolResult>, bool) {
    let tool_calls: Vec<&ToolCall> = assistant_message
        .content
        .iter()
        .filter_map(|c| match c {
            Content::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect();

    if tool_calls.is_empty() {
        return (Vec::new(), false);
    }

    // A missing executor means the Stage 2 stub, which has no per-tool mode
    // to consult and therefore always runs one call at a time.
    let sequential = match executor {
        None => true,
        Some(executor) => {
            mode == ToolExecutionMode::Sequential
                || tool_calls.iter().any(|call| {
                    executor.execution_mode(&call.name) == ToolExecutionMode::Sequential
                })
        }
    };

    if sequential {
        execute_batch_sequential(executor, hooks, &tool_calls, signal, observer, telemetry).await
    } else {
        execute_batch_parallel(executor, hooks, &tool_calls, signal, observer, telemetry).await
    }
}

/// `BeforeToolCall` outcome for a single call.
enum CallPreparation {
    /// The call may be dispatched to the executor.
    Execute,
    /// The call is already complete and must not reach the executor.
    Immediate(ToolResult),
}

/// Ask the `BeforeToolCall` hook about one call. The returned flag is the
/// hook's `terminate` decision for [`TurnBatch::continue_loop`].
async fn prepare_call(hooks: &AgentHookAdapter, call: &ToolCall) -> (CallPreparation, bool) {
    let decision = hooks.invoke_before_tool_call(call).await;
    if decision.block {
        let reason = decision
            .reason
            .unwrap_or_else(|| "blocked by before_tool_call".to_string());
        (
            CallPreparation::Immediate(ToolResult {
                tool_call_id: call.id.clone(),
                content: Box::new(Content::text(format!("tool call blocked: {reason}"))),
                is_error: true,
                details: None,
                added_tool_names: None,
            }),
            decision.terminate,
        )
    } else {
        (CallPreparation::Execute, decision.terminate)
    }
}

/// Serialized dispatch — each call is prepared, executed and finalized
/// before the next one starts.
///
/// `ToolExecutionStart` is emitted **before** the call's `BeforeToolCall` hook
/// and `ToolExecutionEnd` once the result is finalized, so a consumer can
/// render the call as running for its whole real lifetime. `duration_ms`
/// measures that same window for this one call (not the batch).
///
/// When the signal is aborted the loop stops after the call that observed the
/// abort: the remaining calls never reach the executor at all (upstream
/// `executeToolCallsSequential`, `packages/agent/src/agent-loop.ts:476-478`).
async fn execute_batch_sequential(
    executor: Option<&Arc<dyn ToolExecutor>>,
    hooks: &AgentHookAdapter,
    tool_calls: &[&ToolCall],
    signal: &CancellationToken,
    observer: Option<&EventObserver>,
    telemetry: Option<&SpanRef>,
) -> (Vec<ToolResult>, bool) {
    let mut all_terminate = true;
    let mut results = Vec::with_capacity(tool_calls.len());
    for call in tool_calls {
        let started = Monotonic::now();
        emit_tool_start(observer, call);
        let (preparation, terminate) = prepare_call(hooks, call).await;
        all_terminate &= terminate;
        let result = match preparation {
            CallPreparation::Immediate(result) => result,
            CallPreparation::Execute => run_call(executor, hooks, call, signal, telemetry).await,
        };
        emit_tool_end(observer, &result, started);
        results.push(result);
        if signal.is_cancelled() {
            break;
        }
    }
    (results, !all_terminate)
}

/// Concurrent dispatch — every call is prepared in source order (so the
/// `BeforeToolCall` hook still observes the batch in model order), the
/// allowed calls run concurrently, and their results are folded back into
/// source order.
///
/// Every `ToolExecutionStart` is emitted during the preparation pass, i.e.
/// **before any of the batch's calls is awaited for execution**, and each
/// `ToolExecutionEnd` is emitted by the future that ran that call. Two
/// concurrent calls therefore always produce both starts before either end.
///
/// Cancellation mirrors upstream `executeToolCallsParallel`
/// (`packages/agent/src/agent-loop.ts:504-545`): the preparation loop stops
/// as soon as the signal is aborted (calls after it are dropped), and a call
/// that was already queued but has not started yet finalizes as an
/// `Operation aborted` error result instead of reaching the executor.
async fn execute_batch_parallel(
    executor: Option<&Arc<dyn ToolExecutor>>,
    hooks: &AgentHookAdapter,
    tool_calls: &[&ToolCall],
    signal: &CancellationToken,
    observer: Option<&EventObserver>,
    telemetry: Option<&SpanRef>,
) -> (Vec<ToolResult>, bool) {
    let mut all_terminate = true;
    let mut slots: Vec<Option<ToolResult>> = Vec::with_capacity(tool_calls.len());
    let mut prepared: Vec<(usize, &ToolCall, Monotonic)> = Vec::with_capacity(tool_calls.len());

    for call in tool_calls {
        let started = Monotonic::now();
        emit_tool_start(observer, call);
        let (preparation, terminate) = prepare_call(hooks, call).await;
        all_terminate &= terminate;
        match preparation {
            CallPreparation::Immediate(result) => {
                emit_tool_end(observer, &result, started);
                slots.push(Some(result));
            }
            CallPreparation::Execute => {
                prepared.push((slots.len(), call, started));
                slots.push(None);
            }
        }
        if signal.is_cancelled() {
            break;
        }
    }

    // Each queued call checks the signal once more right before it starts, so
    // an abort that lands between the preparation loop and the join still
    // short-circuits execution (the result slot is filled, the executor is not
    // called) — and still emits a matching `ToolExecutionEnd`, because its
    // `ToolExecutionStart` was already emitted in the preparation pass.
    let cancelled = signal.clone();
    let futures = prepared.iter().map(|(slot, call, started)| {
        let signal = cancelled.clone();
        async move {
            let result = if signal.is_cancelled() {
                aborted_tool_result(call)
            } else {
                run_call(executor, hooks, call, &signal, telemetry).await
            };
            emit_tool_end(observer, &result, *started);
            (*slot, result)
        }
    });
    for (slot, result) in futures::future::join_all(futures).await {
        slots[slot] = Some(result);
    }

    let results = slots.into_iter().flatten().collect();
    (results, !all_terminate)
}

/// Emit `ToolExecutionStart` for one dispatchable call.
fn emit_tool_start(observer: Option<&EventObserver>, call: &ToolCall) {
    emit_event(
        observer,
        AgentEvent::ToolExecutionStart { call: call.clone() },
    );
}

/// Emit `ToolExecutionEnd` with the real duration of the call that just
/// finished. Every emitted `ToolExecutionStart` — including one that is
/// finalized as `Operation aborted` before it reaches the executor — gets a
/// matching end.
fn emit_tool_end(observer: Option<&EventObserver>, result: &ToolResult, started: Monotonic) {
    emit_event(
        observer,
        AgentEvent::ToolExecutionEnd {
            result: result.clone(),
            duration_ms: started.elapsed_ms(),
        },
    );
}

/// Error result upstream synthesizes for a queued parallel call that finds
/// the signal aborted before it runs (`createErrorToolResult("Operation
/// aborted")`, `packages/agent/src/agent-loop.ts:524`).
fn aborted_tool_result(call: &ToolCall) -> ToolResult {
    ToolResult {
        tool_call_id: call.id.clone(),
        content: Box::new(Content::text("Operation aborted")),
        is_error: true,
        details: None,
        added_tool_names: None,
    }
}

/// Execute one tool call and finalize it: dispatch to the registered executor
/// (or the Stage 2 stub), convert executor-level failures into error results
/// so the turn continues, then let the `AfterToolCall` hook rewrite the result
/// exactly once. Wraps the dispatch in a `pi.harness.tool` span when telemetry
/// is installed.
///
/// Tool *arguments* and *output* are deliberately never recorded in the span —
/// only the name, the call id and whether the execution produced an error.
async fn run_call(
    executor: Option<&Arc<dyn ToolExecutor>>,
    hooks: &AgentHookAdapter,
    call: &ToolCall,
    signal: &CancellationToken,
    telemetry: Option<&SpanRef>,
) -> ToolResult {
    let mut result = match telemetry {
        None => dispatch_tool(executor, call, signal).await,
        Some(parent) => {
            // One `pi.harness.tool` span per call, parented to the turn.
            let options = SpanOptions::new(span_name::HARNESS_TOOL)
                .with_attribute(attribute_name::TOOL_NAME, call.name.clone())
                .with_attribute(attribute_name::TOOL_CALL_ID, call.id.clone());
            parent
                .start_span_with(options, |span| async move {
                    let result = dispatch_tool(executor, call, signal).await;
                    span.set_attributes(tool_attributes(&result));
                    if result.is_error {
                        span.set_status(SpanStatus::error(
                            "ToolError",
                            "tool call produced an error result",
                        ));
                    }
                    Ok::<ToolResult, AgentError>(result)
                })
                .await
                .unwrap_or_else(|err| ToolResult {
                    tool_call_id: call.id.clone(),
                    content: Box::new(Content::text(err.to_string())),
                    is_error: true,
                    details: None,
                    added_tool_names: None,
                })
        }
    };
    hooks.invoke_after_tool_call(&mut result).await;
    result
}

/// Dispatch one tool call to the registered executor and convert
/// executor-level failures into error results so the turn continues. This is
/// the raw execution step — [`run_call`] adds the `AfterToolCall` hook.
async fn dispatch_tool(
    executor: Option<&Arc<dyn ToolExecutor>>,
    call: &ToolCall,
    signal: &CancellationToken,
) -> ToolResult {
    match executor {
        Some(executor) => match executor.execute(call, signal.clone()).await {
            Ok(result) => result,
            // Executor-level failures become error results; the turn
            // continues so the model can correct course.
            Err(err) => ToolResult {
                tool_call_id: call.id.clone(),
                content: Box::new(Content::text(err.to_string())),
                is_error: true,
                details: None,
                added_tool_names: None,
            },
        },
        // Backward compatibility: no executor registered, so keep the
        // Stage 2 stub result the existing loop tests assert on.
        None => ToolResult {
            tool_call_id: call.id.clone(),
            content: Box::new(Content::text(format!("(stub) executed {}", call.name))),
            is_error: false,
            details: None,
            added_tool_names: None,
        },
    }
}

/// Monotonic timestamp used to measure one tool call's wall-clock duration.
///
/// Wraps [`std::time::Instant`] on native targets and falls back to
/// `js_sys::Date::now()` on `wasm32-unknown-unknown`, where `Instant::now`
/// panics because no monotonic clock source is available. The WASM fallback
/// is wall-clock and therefore only used for an *elapsed* reading, never for
/// ordering.
#[derive(Copy, Clone)]
struct Monotonic {
    #[cfg(not(target_arch = "wasm32"))]
    instant: std::time::Instant,
    #[cfg(target_arch = "wasm32")]
    millis: u64,
}

impl Monotonic {
    fn now() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self {
                instant: std::time::Instant::now(),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            Self {
                millis: js_sys::Date::now() as u64,
            }
        }
    }

    fn elapsed_ms(&self) -> u64 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.instant.elapsed().as_millis() as u64
        }
        #[cfg(target_arch = "wasm32")]
        {
            // Clamp to zero so a wall clock that runs slightly backwards
            // (e.g. an NTP correction) cannot surface as a huge duration.
            let now = js_sys::Date::now() as u64;
            now.saturating_sub(self.millis)
        }
    }
}
