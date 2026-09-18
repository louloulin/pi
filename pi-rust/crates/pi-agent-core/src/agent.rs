//! High-level `Agent` facade. Mirrors the `Agent` class exported from
//! `packages/agent/src/agent.ts`.

use pi_ai::stream::SharedStreamFn;
use pi_protocol::{Message, Model};
use tokio::sync::mpsc;

use crate::agent_loop::{AgentLoop, TurnOutcome};
use crate::hooks::{AgentHookAdapter, PrepareHookFn, ShouldStopHookFn};
use crate::queue::{MessageQueue, QueueMode};
use crate::state::{AgentConfig, AgentState};

/// Public event emitted to subscribers. Stage 2 fills in the full union.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// A user message was enqueued.
    UserMessage(Message),
    /// A turn finished.
    TurnEnd(TurnOutcome),
}

/// User-facing options for constructing an [`Agent`].
///
/// Mirrors `AgentOptions` from `packages/agent/src/agent.ts` (the fields
/// relevant to Stage 2 of the Rust port — Stage 4 fills in queue policies
/// and follow-up / steering APIs).
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

    /// Build the [`AgentHookAdapter`] the [`AgentLoop`] consumes.
    pub(crate) fn hook_adapter(&self) -> AgentHookAdapter {
        let mut adapter = AgentHookAdapter::new();
        adapter.should_stop_after_turn = self.should_stop_after_turn.clone();
        adapter.prepare_next_turn = self.prepare_next_turn.clone();
        adapter
    }
}

/// User-facing agent handle.
pub struct Agent {
    inner: AgentLoop,
    queue: MessageQueue,
    /// Live event sink for [`Agent::subscribe`]. Stage 4 wires the
    /// bridge from `AgentLoop` events to subscribers; today the field
    /// is parked so the struct shape stays stable for downstream work.
    #[allow(dead_code)]
    events: mpsc::UnboundedSender<AgentEvent>,
}

impl Agent {
    /// Construct an agent from [`AgentOptions`].
    pub fn new(options: AgentOptions) -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        let state = AgentState {
            system_prompt: options.system_prompt.clone(),
            messages: Vec::new(),
            model_override: None,
        };
        let config = AgentConfig {
            stream_fn: options.stream_fn.clone(),
            model: options.model.clone(),
        };
        let hooks = options.hook_adapter();
        Self {
            inner: AgentLoop::new(config, state, hooks),
            queue: MessageQueue::new(QueueMode::default()),
            events: tx,
        }
    }

    /// Subscribe to agent events.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<AgentEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        // Forward `tx` into a side-channel — Stage 2 wires the live bridge.
        let _ = tx;
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

    /// Enqueue a user message and trigger a turn.
    pub async fn prompt(&mut self, text: &str) -> Result<(), crate::agent_loop::AgentError> {
        // Stage 0: enqueue only. Stage 2 drains + streams + executes tools.
        self.queue.push(Message {
            role: pi_protocol::Role::User,
            content: vec![pi_protocol::Content::text(text)],
            model: None,
        });
        Ok(())
    }
}
