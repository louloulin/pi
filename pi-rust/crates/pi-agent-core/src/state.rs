//! Agent state — system prompt, model, and message log.

use std::sync::Arc;

use pi_ai::stream::SharedStreamFn;
use pi_protocol::{Context, Message, Model, ToolDefinition};

use crate::tools::ToolExecutor;

/// Runtime configuration that is fixed for the lifetime of an [`Agent`](crate::Agent).
#[derive(Clone)]
pub struct AgentConfig {
    /// Streaming implementation. `Models::stream_simple` from `pi-ai` satisfies this.
    pub stream_fn: SharedStreamFn,
    /// Default model the agent picks when none is supplied at call time.
    pub model: Model,
    /// Optional tool executor. When set, the loop advertises
    /// [`ToolExecutor::definitions`] on every turn and dispatches each tool
    /// call to it. `None` keeps the Stage 2 stub behaviour so callers that
    /// predate tool execution still work.
    pub tool_executor: Option<Arc<dyn ToolExecutor>>,
}

impl AgentConfig {
    /// Tool definitions this configuration advertises to the model.
    ///
    /// Returns an empty list when no executor is registered, preserving the
    /// pre-tool-execution behaviour.
    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tool_executor
            .as_ref()
            .map(|executor| executor.definitions())
            .unwrap_or_default()
    }
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
    ///
    /// The tool list comes from the configuration's executor, so the model
    /// sees exactly the tools the loop can actually run.
    pub fn context(&self, config: &AgentConfig) -> Context {
        Context {
            system_prompt: self.system_prompt.clone(),
            messages: self.messages.clone(),
            tools: config.tool_definitions(),
        }
    }

    /// Resolve the model for the next turn.
    pub fn model<'a>(&'a self, config: &'a AgentConfig) -> &'a Model {
        self.model_override.as_ref().unwrap_or(&config.model)
    }
}
