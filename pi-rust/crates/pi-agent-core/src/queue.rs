//! Queue draining — `all` vs `one-at-a-time`. Mirrors `QueueMode` in
//! `packages/agent/src/types.ts`.

use pi_protocol::Message;

/// Policy used when the agent loop reaches a queue drain point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QueueMode {
    /// Drain every queued message at the drain point.
    #[default]
    All,
    /// Drain only the oldest queued message; leave the rest for the next drain point.
    OneAtATime,
}

/// Per-session message queue.
#[derive(Debug, Default)]
pub struct MessageQueue {
    mode: QueueMode,
    pending: Vec<Message>,
}

impl MessageQueue {
    /// Construct a queue with the given mode.
    pub fn new(mode: QueueMode) -> Self {
        Self {
            mode,
            pending: Vec::new(),
        }
    }

    /// Enqueue a message at the tail.
    pub fn push(&mut self, message: Message) {
        self.pending.push(message);
    }

    /// True when there is at least one queued message.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Drain according to the configured [`QueueMode`].
    pub fn drain(&mut self) -> Vec<Message> {
        match self.mode {
            QueueMode::All => std::mem::take(&mut self.pending),
            QueueMode::OneAtATime => {
                if self.pending.is_empty() {
                    Vec::new()
                } else {
                    vec![self.pending.remove(0)]
                }
            }
        }
    }
}
