//! High-level `Agent` facade. Mirrors the `Agent` class exported from
//! `packages/agent/src/agent.ts`.

use pi_ai::SharedStreamFn;
use pi_protocol::{Message, Model};
use tokio::sync::mpsc;

use crate::agent_loop::{AgentLoop, TurnOutcome};
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

/// User-facing agent handle.
pub struct Agent {
    inner: AgentLoop,
    queue: MessageQueue,
    events: mpsc::UnboundedSender<AgentEvent>,
}

impl Agent {
    /// Construct an agent from a model, stream implementation, and system prompt.
    pub fn new(model: Model, stream_fn: SharedStreamFn, system_prompt: impl Into<String>) -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        let state = AgentState {
            system_prompt: system_prompt.into(),
            messages: Vec::new(),
            model_override: None,
        };
        let config = AgentConfig { stream_fn, model };
        Self {
            inner: AgentLoop::new(config, state),
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
