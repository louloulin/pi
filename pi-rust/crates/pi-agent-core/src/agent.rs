//! High-level `Agent` facade. Mirrors the `Agent` class exported from
//! `packages/agent/src/agent.ts`.

use parking_lot::Mutex;
use pi_ai::stream::SharedStreamFn;
use pi_protocol::{Content, Message, Model, ToolExecutionMode};
use pi_telemetry::TelemetryContext;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::agent_loop::{AgentLoop, EventObserver, TurnOutcome};
use crate::events::AgentEvent;
use crate::hooks::{AgentHookAdapter, PrepareHookFn, ShouldStopHookFn};
use crate::queue::{MessageQueue, QueueMode};
use crate::retry::RetryPolicy;
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
    /// How a tool batch without any `Sequential` tool is dispatched.
    /// Defaults to [`ToolExecutionMode::Parallel`], matching the upstream
    /// `AgentLoopConfig.toolExecution` default; set it to
    /// [`ToolExecutionMode::Sequential`] to force one call at a time.
    pub tool_execution: ToolExecutionMode,
    /// Optional telemetry context. When set, the loop emits one span per run,
    /// turn, provider request and tool execution. `None` records nothing.
    pub telemetry: Option<Arc<dyn TelemetryContext>>,
    /// Agent-level retry budget for the assistant call. Defaults to
    /// [`RetryPolicy::default`] — the upstream `settings.retry` defaults
    /// (enabled, 3 retries, 2 s base, 60 s cap).
    pub retry: RetryPolicy,
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
            .field("tool_executor", &self.tool_executor.as_ref().map(|_| "…"))
            .field("tool_execution", &self.tool_execution)
            .field("telemetry", &self.telemetry.as_ref().map(|_| "…"))
            .field("retry", &self.retry)
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
            tool_execution: ToolExecutionMode::Parallel,
            telemetry: None,
            retry: RetryPolicy::default(),
        }
    }

    /// Builder-style setter for [`retry`](Self::retry).
    ///
    /// [`RetryPolicy::disabled`] turns the agent-level retry loop off,
    /// matching `settings.retry.enabled: false` upstream.
    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Builder-style setter for [`telemetry`](Self::telemetry).
    pub fn with_telemetry(mut self, telemetry: Arc<dyn TelemetryContext>) -> Self {
        self.telemetry = Some(telemetry);
        self
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

    /// Builder-style setter for [`tool_execution`](Self::tool_execution).
    ///
    /// [`ToolExecutionMode::Sequential`] serializes every tool batch, which
    /// is the escape hatch for hosts whose tools cannot be run concurrently.
    pub fn with_tool_execution(mut self, mode: ToolExecutionMode) -> Self {
        self.tool_execution = mode;
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
            tool_execution: options.tool_execution,
            telemetry: options.telemetry.clone(),
            retry: options.retry,
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
    /// on every subscriber channel **at the moment each event happens** —
    /// provider deltas, per-tool `ToolExecutionStart` / `ToolExecutionEnd`
    /// and the turn boundaries. The call returns when the loop exits
    /// (either because the model stopped, the user hook requested an early
    /// exit, or the loop surface returned an error).
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

        // Install a fan-out observer for the duration of the run. The loop
        // calls it synchronously at every real event point, so subscribers
        // observe deltas and tool execution while the turn is still running.
        // The closure captures an `Arc` clone of the subscriber list (not the
        // `Agent`), so the loop can be driven from outside this borrow.
        let subscribers = self.subscribers.clone();
        let observer: EventObserver = Arc::new(move |event: AgentEvent| {
            emit_to(&subscribers, event);
        });
        self.inner.set_event_observer(Some(observer));
        let result = self.inner.run(drained, |_turn: &TurnOutcome| {}).await;
        // Clear the observer on every path — including the error path — so a
        // later `run` on the same loop starts without a stale sink.
        self.inner.set_event_observer(None);
        result?;

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

/// Send one event to every subscriber. Subscribers whose channel is closed
/// are pruned.
fn emit_to(subscribers: &Arc<Mutex<Vec<SubscriberSender>>>, event: AgentEvent) {
    let mut guard = subscribers.lock();
    guard.retain(|tx| tx.send(event.clone()).is_ok());
}
