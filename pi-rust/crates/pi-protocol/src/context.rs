//! Conversation context — the message log plus the system prompt and the
//! available tool set.

use serde::{Deserialize, Serialize};

use crate::{Content, ToolDefinition};

/// Role of a message author.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// System prompt (usually only the first message in a context).
    System,
    /// Human-authored user message.
    User,
    /// Assistant turn.
    Assistant,
    /// Tool result returned to the model.
    Tool,
}

/// A single message in the conversation log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Message author.
    pub role: Role,
    /// Message content blocks (text, images, tool calls, tool results).
    pub content: Vec<Content>,
    /// Optional model identifier (assistant messages only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Conversation context — what the model sees on each turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Context {
    /// System prompt.
    pub system_prompt: String,
    /// Message log.
    pub messages: Vec<Message>,
    /// Tools available to the model on this turn.
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
}

impl Context {
    /// Construct an empty context with the given system prompt.
    pub fn new(system_prompt: impl Into<String>) -> Self {
        Self {
            system_prompt: system_prompt.into(),
            messages: Vec::new(),
            tools: Vec::new(),
        }
    }
}
