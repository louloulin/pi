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
    /// Optional async lifecycle hooks for the extension events that fire
    /// inside a run (`context`, `before_agent_start`, …).
    pub lifecycle: Option<Arc<dyn crate::hooks::LifecycleHooks>>,
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
            .field("lifecycle", &self.lifecycle.as_ref().map(|_| "…"))
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
            lifecycle: None,
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

    /// Builder-style setter for [`lifecycle`](Self::lifecycle).
    ///
    /// The JS extension runtime is installed this way: `pi-coding-agent`
    /// builds its `LifecycleHooks` implementation after the agent exists (the
    /// runtime comes from the extension load pass), so it can also assign
    /// [`Agent::hooks_mut`]`().lifecycle` directly.
    pub fn with_lifecycle(mut self, hook: Arc<dyn crate::hooks::LifecycleHooks>) -> Self {
        self.lifecycle = Some(hook);
        self
    }

    /// Build the [`AgentHookAdapter`] the [`AgentLoop`] consumes.
    pub(crate) fn hook_adapter(&self) -> AgentHookAdapter {
        let mut adapter = AgentHookAdapter::new();
        adapter.should_stop_after_turn = self.should_stop_after_turn.clone();
        adapter.prepare_next_turn = self.prepare_next_turn.clone();
        adapter.lifecycle = self.lifecycle.clone();
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
    /// System prompt the agent was constructed with.
    ///
    /// Upstream `before_agent_start` replaces the system prompt for one run
    /// and resets to the base prompt on the next
    /// (`packages/coding-agent/src/core/agent-session.ts:1285`); keeping the
    /// base here is what makes that reset possible without the host having to
    /// re-supply it.
    base_system_prompt: String,
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
            base_system_prompt: options.system_prompt.clone(),
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

    /// The session thinking level applied to the next provider call.
    ///
    /// `None` until [`Agent::set_thinking_level`] is called; the session
    /// layer seeds it from the persisted `defaultThinkingLevel`.
    pub fn thinking_level(&self) -> Option<crate::hooks::ThinkingLevel> {
        self.inner.thinking_level()
    }

    /// Set the thinking level every following turn requests from the
    /// provider. Backs `app.thinking.cycle`, `/thinking` and the
    /// `app.thinking.save` chord (upstream `AgentSession::setThinkingLevel`).
    pub fn set_thinking_level(&mut self, level: crate::hooks::ThinkingLevel) {
        self.inner.set_thinking_level(level);
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
        self.prompt_content(vec![Content::text(text)]).await
    }

    /// Enqueue a user message built from pre-formed content blocks and run a
    /// turn. [`Agent::prompt`] is the text-only shorthand; the composer uses
    /// this to send a prompt that carries pasted image blocks alongside its
    /// text (upstream `UserMessage.content`, `packages/ai/src/types.ts`).
    ///
    /// The block list is used verbatim, so callers own the block order —
    /// text first, then one `Content::Image` per attachment, mirroring how a
    /// provider request is assembled.
    pub async fn prompt_content(
        &mut self,
        content: Vec<Content>,
    ) -> Result<(), crate::agent_loop::AgentError> {
        let user_message = Message {
            role: pi_protocol::Role::User,
            content,
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
        // Upstream `before_agent_start`: after the prompt was expanded, before
        // the agent loop. A handler may replace the system prompt for this run;
        // the next run resets to the base prompt unless a handler overrides it
        // again (`agent-session.ts:1285`).
        let prompt_text: String = drained
            .iter()
            .map(content_text)
            .collect::<Vec<_>>()
            .join("\n");
        let lifecycle = self.inner.hooks().lifecycle.clone();
        let base_prompt = self.base_system_prompt.clone();
        let override_prompt = match &lifecycle {
            Some(hook) => hook.before_agent_start(&prompt_text, &base_prompt).await,
            None => None,
        };
        self.inner.state_mut().system_prompt = override_prompt.unwrap_or(base_prompt);
        // Bracket the run for extensions (upstream `agent_start`).
        self.emit(AgentEvent::AgentStart);
        let result = self.inner.run(drained, |_turn: &TurnOutcome| {}).await;
        // Clear the observer on every path — including the error path — so a
        // later `run` on the same loop starts without a stale sink.
        self.inner.set_event_observer(None);
        // `agent_end` brackets the run; emit it before propagating an error so
        // subscribers always observe the run close.
        self.emit(AgentEvent::AgentEnd {
            messages: self.inner.state().messages.clone(),
        });
        // Upstream `agent_settled`: the run is over and nothing (retry,
        // auto-compaction, queued continuation) will restart the loop on its
        // own. Emitted on the error path too, so a plugin's `finally`-style
        // cleanup always runs.
        self.inner.hooks().invoke_agent_settled().await;
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

/// Flatten a message's text blocks. Used for `before_agent_start.prompt`,
/// which upstream carries as a plain string.
fn content_text(message: &Message) -> String {
    let mut out = String::new();
    for block in &message.content {
        if let pi_protocol::Content::Text(text) = block {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&text.text);
        }
    }
    out
}
