//! Shared helpers used by the built-in suites.

use pi_agent_core::{Agent, AgentEvent};
use pi_protocol::{Api, AssistantMessage, Content, Message, Model, ProviderId, Role, ToolResult, Usage};
use serde_json::Value;

use crate::harness::{EvalError, TokenUsage, TranscriptEvent};

/// Read a boolean env flag: set and not `0` / `false` / empty.
pub(crate) fn env_flag(name: &str) -> bool {
    match std::env::var(name) {
        Ok(value) => {
            let value = value.trim().to_ascii_lowercase();
            !value.is_empty() && value != "0" && value != "false"
        }
        Err(_) => false,
    }
}

/// Read a non-empty environment variable.
pub(crate) fn env_string(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Model descriptor for the in-process `faux` provider.
pub(crate) fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: Some("Faux test model".into()),
        context_window: 8_192,
        max_output_tokens: 1_024,
    }
}

/// Model descriptor for a provider / id / API family.
pub(crate) fn model(provider: &str, id: &str, api: Api) -> Model {
    Model {
        provider: ProviderId::new(provider),
        id: id.to_string(),
        api,
        label: None,
        context_window: 32_768,
        max_output_tokens: 4_096,
    }
}

/// Extract the text of a single content block.
pub(crate) fn content_text(content: &Content) -> String {
    match content {
        Content::Text(text) => text.text.clone(),
        Content::Image(image) => format!("[image {}]", image.mime_type),
        Content::ToolCall(call) => format!("[tool_call {}]", call.name),
        Content::ToolResult(result) => content_text(&result.content),
    }
}

/// Concatenate every text block of a message.
pub(crate) fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter(|content| matches!(content, Content::Text(_)))
        .map(content_text)
        .collect::<Vec<_>>()
        .join("")
}

/// Text of the final assistant message, if any.
pub(crate) fn assistant_text(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant)
        .map(message_text)
        .unwrap_or_default()
}

/// Captured result of one [`Agent::prompt`] call.
pub(crate) struct AgentRun {
    /// Full message log after the run.
    pub messages: Vec<Message>,
    /// Assistant messages as the provider produced them (usage + stop reason).
    pub assistant_messages: Vec<AssistantMessage>,
    /// Raw event stream the agent emitted.
    pub events: Vec<AgentEvent>,
}

impl AgentRun {
    /// Final assistant text.
    pub fn output(&self) -> String {
        assistant_text(&self.messages)
    }

    /// Telemetry for the run, stamped with provider / model.
    pub fn usage(&self, provider: &str, model_id: &str) -> TokenUsage {
        usage_from_assistant_messages(&self.assistant_messages, provider, model_id)
    }

    /// Normalized transcript events.
    pub fn transcript(&self) -> Vec<TranscriptEvent> {
        transcript(&self.messages)
    }
}

/// Run one prompt through `agent`, collecting events and the final log.
pub(crate) async fn run_agent(mut agent: Agent, prompt: &str) -> Result<AgentRun, EvalError> {
    let mut receiver = agent.subscribe();
    agent
        .prompt(prompt)
        .await
        .map_err(|error| EvalError::Case(error.to_string()))?;
    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }
    let messages = agent.state().messages.clone();
    let assistant_messages = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::MessageEnd { message } => Some(message.clone()),
            _ => None,
        })
        .collect();
    Ok(AgentRun {
        messages,
        assistant_messages,
        events,
    })
}

/// Normalize a message log into transcript events.
///
/// Mirrors `toTranscriptEvents` in the upstream `pi-harness.ts`: user and
/// assistant text become `message` events, assistant tool calls become
/// `tool_call` events, and tool results become `tool_result` events (with the
/// tool name resolved from the matching call).
pub(crate) fn transcript(messages: &[Message]) -> Vec<TranscriptEvent> {
    let mut events = Vec::new();
    for message in messages {
        match message.role {
            Role::User => {
                let content = message_text(message);
                if !content.is_empty() {
                    events.push(TranscriptEvent::Message {
                        role: "user".into(),
                        content,
                    });
                }
            }
            Role::Assistant => {
                let content = message_text(message);
                if !content.is_empty() {
                    events.push(TranscriptEvent::Message {
                        role: "assistant".into(),
                        content,
                    });
                }
                for block in &message.content {
                    if let Content::ToolCall(call) = block {
                        events.push(TranscriptEvent::ToolCall {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                        });
                    }
                }
            }
            Role::Tool => {
                for block in &message.content {
                    if let Content::ToolResult(result) = block {
                        events.push(tool_result_event(result, &events));
                    }
                }
            }
            Role::System => {}
        }
    }
    events
}

fn tool_result_event(result: &ToolResult, prior: &[TranscriptEvent]) -> TranscriptEvent {
    let name = prior
        .iter()
        .rev()
        .find_map(|event| match event {
            TranscriptEvent::ToolCall { id, name, .. } if *id == result.tool_call_id => {
                Some(name.clone())
            }
            _ => None,
        })
        .unwrap_or_default();
    TranscriptEvent::ToolResult {
        tool_call_id: result.tool_call_id.clone(),
        name,
        content: serde_json::to_value(&result.content).unwrap_or(Value::Null),
        error: result
            .is_error
            .then(|| content_text(&result.content)),
    }
}

/// Sum usage across the assistant messages of a run.
pub(crate) fn usage_from_assistant_messages(
    messages: &[AssistantMessage],
    provider: &str,
    model_id: &str,
) -> TokenUsage {
    let mut usage = Usage::default();
    let mut tool_calls = 0u32;
    for message in messages {
        usage.input = usage.input.saturating_add(message.usage.input);
        usage.output = usage.output.saturating_add(message.usage.output);
        usage.total = usage.total.saturating_add(message.usage.total);
        usage.cache_read = usage.cache_read.saturating_add(message.usage.cache_read);
        usage.cache_write = usage.cache_write.saturating_add(message.usage.cache_write);
        tool_calls = tool_calls.saturating_add(
            message
                .content
                .iter()
                .filter(|block| matches!(block, Content::ToolCall(_)))
                .count() as u32,
        );
    }
    let total = if usage.total == 0 {
        usage.input.saturating_add(usage.output)
    } else {
        usage.total
    };
    TokenUsage {
        provider: Some(provider.to_string()),
        model: Some(model_id.to_string()),
        input: usage.input,
        output: usage.output,
        total,
        tool_calls,
        cache_read: usage.cache_read,
        cache_write: usage.cache_write,
        estimated_cost_usd: None,
    }
}

/// Count tool calls of a given name in a transcript.
pub(crate) fn tool_calls_named<'a>(
    events: &'a [TranscriptEvent],
    name: &str,
) -> Vec<&'a TranscriptEvent> {
    events
        .iter()
        .filter(|event| matches!(event, TranscriptEvent::ToolCall { name: n, .. } if n == name))
        .collect()
}
