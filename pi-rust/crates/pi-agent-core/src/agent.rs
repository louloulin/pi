//! High-level `Agent` facade. Mirrors the `Agent` class exported from
//! `packages/agent/src/agent.ts`.

use parking_lot::Mutex;
use pi_ai::stream::SharedStreamFn;
use pi_protocol::{Content, Message, Model};
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;
use tokio::sync::mpsc;

use crate::agent_loop::{AgentLoop, TurnOutcome};
use crate::events::{AgentEvent, AssistantMessageUpdate};
use crate::hooks::{AgentHookAdapter, PrepareHookFn, ShouldStopHookFn};
use crate::queue::{MessageQueue, QueueMode};
use crate::state::{AgentConfig, AgentState};
use crate::tools::ToolExecutor;

/// User-facing options for constructing an [`Agent`].
///
/// Mirrors `AgentOptions` from `packages/agent/src/agent.ts`.
#[derive(Clone)]
pub struct AgentOptions {
    /// Default model the agent picks when none is supplied at call time.
    pub model: Model,
    /// Streaming implementation. `Models::stream_simple` from `pi-ai`
    /// satisfies this.
    pub stream_fn: SharedStreamFn,
    /// System prompt prepended to every turn.
    pub system_prompt: String,
    /// Optional `should_stop_after_turn` callback. When unset, the loop
    /// uses its default (`false`).
    pub should_stop_after_turn: Option<ShouldStopHookFn>,
    /// Optional `prepare_next_turn` callback. When unset, the loop uses
    /// its default (`None`).
    pub prepare_next_turn: Option<PrepareHookFn>,
    /// Optional tool executor. When set, the loop advertises its definitions
    /// to the model and dispatches tool calls to it.
    pub tool_executor: Option<Arc<dyn ToolExecutor>>,
}

impl std::fmt::Debug for AgentOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentOptions")
            .field("model", &self.model)
            .field("stream_fn", &"<dyn StreamFn>")
            .field("system_prompt", &self.system_prompt)
            .field(
                "should_stop_after_turn",
                &self.should_stop_after_turn.as_ref().map(|_| "…"),
            )
            .field(
                "prepare_next_turn",
                &self.prepare_next_turn.as_ref().map(|_| "…"),
            )
            .field(
                "tool_executor",
                &self.tool_executor.as_ref().map(|_| "…"),
            )
            .finish()
    }
}

impl AgentOptions {
    /// Construct options with only the required fields filled in.
    pub fn new(model: Model, stream_fn: SharedStreamFn, system_prompt: impl Into<String>) -> Self {
        Self {
            model,
            stream_fn,
            system_prompt: system_prompt.into(),
            should_stop_after_turn: None,
            prepare_next_turn: None,
            tool_executor: None,
        }
    }

    /// Builder-style setter for [`should_stop_after_turn`](Self::should_stop_after_turn).
    pub fn with_should_stop_after_turn(mut self, hook: ShouldStopHookFn) -> Self {
        self.should_stop_after_turn = Some(hook);
        self
    }

    /// Builder-style setter for [`prepare_next_turn`](Self::prepare_next_turn).
    pub fn with_prepare_next_turn(mut self, hook: PrepareHookFn) -> Self {
        self.prepare_next_turn = Some(hook);
        self
    }

    /// Builder-style setter for [`tool_executor`](Self::tool_executor).
    pub fn with_tool_executor(mut self, executor: Arc<dyn ToolExecutor>) -> Self {
        self.tool_executor = Some(executor);
        self
    }

    /// Build the [`AgentHookAdapter`] the [`AgentLoop`] consumes.
    pub(crate) fn hook_adapter(&self) -> AgentHookAdapter {
        let mut adapter = AgentHookAdapter::new();
        adapter.should_stop_after_turn = self.should_stop_after_turn.clone();
        adapter.prepare_next_turn = self.prepare_next_turn.clone();
        adapter
    }
}

/// Subscriber handle — every `subscribe()` call returns one and every
/// event is fanned out to all live subscribers.
pub type SubscriberSender = mpsc::UnboundedSender<AgentEvent>;

/// User-facing agent handle.
pub struct Agent {
    inner: AgentLoop,
    queue: MessageQueue,
    /// Shared sink for events fanned out from the loop's observer
    /// hook. New senders registered via [`Agent::subscribe`] are added
    /// here; subscribers that fall behind stay in lockstep with the
    /// loop because the observer is synchronous (no awaits between
    /// events).
    subscribers: Arc<Mutex<Vec<SubscriberSender>>>,
}

impl Agent {
    /// Construct an agent from [`AgentOptions`].
    pub fn new(options: AgentOptions) -> Self {
        let state = AgentState {
            system_prompt: options.system_prompt.clone(),
            messages: Vec::new(),
            model_override: None,
        };
        let config = AgentConfig {
            stream_fn: options.stream_fn.clone(),
            model: options.model.clone(),
            tool_executor: options.tool_executor.clone(),
        };
        let hooks = options.hook_adapter();
        Self {
            inner: AgentLoop::new(config, state, hooks),
            queue: MessageQueue::new(QueueMode::default()),
            subscribers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Subscribe to agent events.
    ///
    /// Every subscriber receives every event — the TUI and any
    /// extension bridge share the same fan-out. Subscribers whose
    /// channel is closed are pruned by the fan-out loop the next time
    /// an event is emitted.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<AgentEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subscribers.lock().push(tx);
        rx
    }

    /// Borrow the underlying [`AgentLoop`]. Stage 2 unit tests use this
    /// to drive scripted prompts without going through the queue.
    pub fn loop_ref(&self) -> &AgentLoop {
        &self.inner
    }

    /// Mutable borrow of the underlying [`AgentLoop`].
    pub fn loop_mut(&mut self) -> &mut AgentLoop {
        &mut self.inner
    }

    /// Borrow the [`AgentHookAdapter`] driving this agent's hooks.
    pub fn hooks(&self) -> &AgentHookAdapter {
        self.inner.hooks()
    }

    /// Mutable borrow of the hook adapter — tests use this to swap
    /// hooks in mid-run.
    pub fn hooks_mut(&mut self) -> &mut AgentHookAdapter {
        self.inner.hooks_mut()
    }

    /// Active model — the one the loop will dispatch to on the next
    /// turn unless overridden via [`Agent::set_model_override`].
    pub fn model(&self) -> &Model {
        &self.inner.config().model
    }

    /// Mutable borrow of the configured model. Used by the
    /// `/model` slash command to swap the active model.
    pub fn set_model(&mut self, model: Model) {
        self.inner.config_mut().model = model;
    }

    /// Set a per-turn model override. Pass `None` to clear.
    pub fn set_model_override(&mut self, model: Option<Model>) {
        self.inner.state_mut().model_override = model;
    }

    /// Mutable borrow of the inner state — used by tests and by the
    /// session layer to inspect / mutate the message log.
    pub fn state_mut(&mut self) -> &mut AgentState {
        self.inner.state_mut()
    }

    /// Immutable borrow of the inner state.
    pub fn state(&self) -> &AgentState {
        self.inner.state()
    }

    /// Borrow the message queue (used by tests to inspect queued
    /// messages between turns).
    pub fn queue(&self) -> &MessageQueue {
        &self.queue
    }

    /// Enqueue a user message, drain the queue, and run a turn.
    ///
    /// The loop drives each turn through the streaming layer, executes
    /// any tool calls the model emits, and emits the full event union
    /// on every subscriber channel. The call returns when the loop
    /// exits (either because the model stopped, the user hook
    /// requested an early exit, or the loop surface returned an
    /// error).
    pub async fn prompt(&mut self, text: &str) -> Result<(), crate::agent_loop::AgentError> {
        let user_message = Message {
            role: pi_protocol::Role::User,
            content: vec![Content::text(text)],
            model: None,
        };
        self.queue.push(user_message.clone());
        self.emit(AgentEvent::UserMessage(user_message));

        let drained = self.queue.drain();
        if drained.is_empty() {
            return Ok(());
        }

        // Build an observer that fans events out to all subscribers
        // for the duration of this turn. Drop the Arc when the call
        // returns so the closure captures a weak handle that does not
        // extend the Agent's lifetime.
        let subscribers = self.subscribers.clone();
        self.inner
            .run(drained, |turn: &TurnOutcome| {
                fan_turn_to_subscribers(&subscribers, turn);
            })
            .await?;

        Ok(())
    }

    /// Emit an event to every subscriber. Subscribers whose channel is
    /// closed are silently skipped. Public so the TUI / extension
    /// bridge can re-emit events they want to surface (e.g. when the
    /// `Agent::prompt` call returns an error and the TUI wants to
    /// surface it).
    pub fn emit(&self, event: AgentEvent) {
        let mut guard = self.subscribers.lock();
        guard.retain(|tx| tx.send(event.clone()).is_ok());
    }
}

/// Translate a finished `TurnOutcome` into the AgentEvent sequence the
/// TUI consumes and fan it out to every subscriber.
///
/// The translator covers the three event groups the TUI depends on:
/// `TurnStart` → `MessageStart` → `MessageUpdate(*)` → `MessageEnd` →
/// `ToolExecutionStart`(* per tool call) → `ToolExecutionEnd`(*) →
/// `TurnEnd`.
fn fan_turn_to_subscribers(subscribers: &Arc<Mutex<Vec<SubscriberSender>>>, turn: &TurnOutcome) {
    emit_to(subscribers, AgentEvent::TurnStart);
    emit_to(
        subscribers,
        AgentEvent::MessageStart {
            model: turn.message.model.clone(),
        },
    );

    for block in &turn.message.content {
        match block {
            Content::Text(text) => {
                emit_to(
                    subscribers,
                    AgentEvent::MessageUpdate(AssistantMessageUpdate::TextDelta {
                        delta: text.text.clone(),
                    }),
                );
            }
            Content::ToolCall(call) => {
                emit_to(
                    subscribers,
                    AgentEvent::MessageUpdate(AssistantMessageUpdate::ToolCallDelta {
                        index: 0,
                        id: Some(call.id.clone()),
                        name: Some(call.name.clone()),
                        arguments_delta: Some(call.arguments.to_string()),
                    }),
                );
            }
            _ => {}
        }
    }

    emit_to(
        subscribers,
        AgentEvent::MessageEnd {
            message: turn.message.clone(),
        },
    );

    let started = monotonic_now();
    for tool_message in &turn.tool_results {
        let result = tool_message.content.iter().find_map(|c| match c {
            Content::ToolResult(r) => Some(r.clone()),
            _ => None,
        });
        if let Some(result) = result {
            let call = pi_protocol::ToolCall {
                id: result.tool_call_id.clone(),
                name: String::new(),
                arguments: serde_json::Value::Null,
            };
            emit_to(subscribers, AgentEvent::ToolExecutionStart { call });
            emit_to(
                subscribers,
                AgentEvent::ToolExecutionEnd {
                    result,
                    duration_ms: monotonic_ms_since(started),
                },
            );
        }
    }

    emit_to(
        subscribers,
        AgentEvent::TurnEnd {
            message: turn.message.clone(),
            tool_results: turn.tool_results.clone(),
        },
    );
}

/// Monotonic timestamp — wraps [`Instant::now`] on native targets and
/// returns a `SystemTime`-based fallback on `wasm32-unknown-unknown`
/// where `Instant::now` panics (no monotonic clock source is
/// available).
fn monotonic_now() -> Monotonic {
    Monotonic::now()
}

/// Elapsed milliseconds since the given monotonic timestamp.
///
/// Always returns `0` on wasm32 because the only timestamp source
/// available there is wall-clock — it does not satisfy the
/// `Instant` monotonicity contract.
fn monotonic_ms_since(started: Monotonic) -> u64 {
    started.elapsed_ms()
}

#[derive(Copy, Clone)]
struct Monotonic {
    #[cfg(not(target_arch = "wasm32"))]
    instant: Instant,
    #[cfg(target_arch = "wasm32")]
    millis: u64,
}

impl Monotonic {
    fn now() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self {
                instant: Instant::now(),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            // `js_sys::Date::now()` returns wall-clock milliseconds since
            // the Unix epoch. We only need *elapsed* milliseconds so the
            // absolute origin does not matter — wall clock is good
            // enough for a tool-execution duration.
            let now = js_sys::Date::now() as u64;
            Self { millis: now }
        }
    }

    fn elapsed_ms(&self) -> u64 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.instant.elapsed().as_millis() as u64
        }
        #[cfg(target_arch = "wasm32")]
        {
            // Clamp to zero so a clock that runs slightly backwards
            // (e.g. NTP correction) does not surface as a negative
            // duration.
            let now = js_sys::Date::now() as u64;
            now.saturating_sub(self.millis)
        }
    }
}

fn emit_to(subscribers: &Arc<Mutex<Vec<SubscriberSender>>>, event: AgentEvent) {
    let mut guard = subscribers.lock();
    guard.retain(|tx| tx.send(event.clone()).is_ok());
}
