//! Agent loop — Stage 0 stub. Stage 2 fills in the per-turn driver, the
//! tool execution modes (sequential / parallel), and the event emission.

use pi_ai::SharedStreamFn;
use pi_protocol::{AssistantMessage, Message};
use thiserror::Error;

use crate::state::{AgentConfig, AgentState};

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
}

/// Outcome of a single [`AgentLoop::run`] invocation.
#[derive(Debug, Clone)]
pub struct TurnOutcome {
    /// Final assistant message.
    pub message: AssistantMessage,
    /// True if the loop asked the model to call a tool and that tool ran.
    pub tool_executed: bool,
}

/// Single agent turn entry point. Stage 0 stub returns the empty message.
#[derive(Clone)]
pub struct AgentLoop {
    config: AgentConfig,
    state: AgentState,
}

impl AgentLoop {
    /// Construct a loop from a configuration and initial state.
    pub fn new(config: AgentConfig, state: AgentState) -> Self {
        Self { config, state }
    }

    /// Borrow the current state.
    pub fn state(&self) -> &AgentState {
        &self.state
    }

    /// Borrow the configuration.
    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Add a user message to the log. Real draining happens in Stage 2.
    pub fn push_user(&mut self, message: Message) {
        self.state.messages.push(message);
    }

    /// Run a single turn. Stage 0 stub — Stage 2 wires streaming, tool
    /// execution, and queue draining.
    pub async fn run(&mut self, _stream: SharedStreamFn) -> Result<TurnOutcome, AgentError> {
        Ok(TurnOutcome {
            message: AssistantMessage {
                model: self.state.model(&self.config).id.clone(),
                content: Vec::new(),
                stop_reason: pi_protocol::StopReason::Empty,
                usage: pi_protocol::Usage::default(),
            },
            tool_executed: false,
        })
    }
}
