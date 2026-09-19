//! Agent state — system prompt, model, and message log.

use pi_ai::stream::SharedStreamFn;
use pi_protocol::{Context, Message, Model};

/// Runtime configuration that is fixed for the lifetime of an [`Agent`](crate::Agent).
#[derive(Clone)]
pub struct AgentConfig {
    /// Streaming implementation. `Models::stream_simple` from `pi-ai` satisfies this.
    pub stream_fn: SharedStreamFn,
    /// Default model the agent picks when none is supplied at call time.
    pub model: Model,
}

/// Mutable agent state — what changes between turns.
#[derive(Debug, Clone, Default)]
pub struct AgentState {
    /// System prompt.
    pub system_prompt: String,
    /// Conversation log.
    pub messages: Vec<Message>,
    /// Optional per-turn override.
    pub model_override: Option<Model>,
}

impl AgentState {
    /// Build the [`Context`] snapshot fed to the streaming layer.
    pub fn context(&self, _config: &AgentConfig) -> Context {
        Context {
            system_prompt: self.system_prompt.clone(),
            messages: self.messages.clone(),
            tools: Vec::new(),
        }
    }

    /// Resolve the model for the next turn.
    pub fn model<'a>(&'a self, config: &'a AgentConfig) -> &'a Model {
        self.model_override.as_ref().unwrap_or(&config.model)
    }
}
