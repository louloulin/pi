//! Agent-event → extension-event mapping.
//!
//! The agent loop emits [`AgentEvent`]s to its subscribers; extensions
//! subscribe to the *upstream* event names (`turn_start`,
//! `tool_execution_start`, …). This module owns the translation so both
//! the interactive fan-out
//! ([`interactive`](crate::interactive)) and the tests share one table
//! instead of re-deriving it at each call site.
//!
//! ## Fidelity rules
//!
//! * One agent event may map to **several** extension events. A tool call
//!   is delivered as both the Rust-native `tool_call` and upstream's
//!   `tool_execution_start`; a user message is delivered as
//!   `user_message` *and* upstream's `input`.
//! * An event with no upstream counterpart maps to nothing rather than to
//!   an invented name. `AgentEvent::Error` is the only such case: upstream
//!   has no agent-level error event, so the runtime reports it through the
//!   TUI banner only.
//! * `turn_index` is tracked here because [`AgentEvent::TurnStart`] does
//!   not carry one; upstream's `turnIndex` is zero-based and counts turns
//!   within the process, which matches what a plugin sees upstream.

use std::collections::HashMap;

use pi_agent_core::{AgentEvent, AssistantMessageUpdate};
use pi_protocol::{Content, ExtensionEvent, InputSource, Message, Role};

/// Stateful translator from [`AgentEvent`] to [`ExtensionEvent`].
///
/// The state is the turn counter plus the tool calls currently in flight
/// (needed because the agent's `ToolExecutionEnd` / `ToolExecutionUpdate`
/// carry the call id but not the tool name or arguments that upstream's
/// payloads include).
#[derive(Debug, Default)]
pub struct ExtensionEventMapper {
    turn_index: usize,
    active_tools: HashMap<String, (String, serde_json::Value)>,
}

impl ExtensionEventMapper {
    /// Fresh mapper with a zeroed turn counter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Zero-based index of the turn being mapped.
    pub fn turn_index(&self) -> usize {
        self.turn_index
    }

    /// Translate one agent event.
    ///
    /// The `now_ms` argument supplies the timestamp for `turn_start`;
    /// callers pass wall-clock milliseconds so the mapper stays free of
    /// clock dependencies in tests.
    pub fn map(&mut self, event: &AgentEvent, now_ms: i64) -> Vec<ExtensionEvent> {
        match event {
            AgentEvent::AgentStart => vec![ExtensionEvent::AgentStart],
            AgentEvent::AgentEnd { messages } => vec![ExtensionEvent::AgentEnd {
                messages: messages.clone(),
            }],
            AgentEvent::TurnStart => {
                let turn_index = self.turn_index;
                self.turn_index = self.turn_index.saturating_add(1);
                vec![ExtensionEvent::TurnStart {
                    turn_index,
                    timestamp: now_ms,
                }]
            }
            AgentEvent::MessageStart { model } => vec![ExtensionEvent::MessageStart {
                message: Message {
                    role: Role::Assistant,
                    content: Vec::new(),
                    model: Some(model.clone()),
                },
            }],
            AgentEvent::MessageUpdate(update) => vec![ExtensionEvent::MessageUpdate {
                assistant_message_event: assistant_message_event_json(update),
            }],
            AgentEvent::MessageEnd { message } => vec![ExtensionEvent::MessageEnd {
                message: assistant_to_message(message),
            }],
            AgentEvent::ToolExecutionStart { call } => {
                self.active_tools
                    .insert(call.id.clone(), (call.name.clone(), call.arguments.clone()));
                vec![
                    // Rust-native tag kept for Stage-3 subscribers.
                    ExtensionEvent::ToolCall { call: call.clone() },
                    ExtensionEvent::ToolExecutionStart {
                        tool_call_id: call.id.clone(),
                        tool_name: call.name.clone(),
                        args: call.arguments.clone(),
                    },
                ]
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                delta,
            } => {
                let (tool_name, args) = self
                    .active_tools
                    .get(tool_call_id)
                    .cloned()
                    .unwrap_or_else(|| (String::new(), serde_json::Value::Null));
                vec![ExtensionEvent::ToolExecutionUpdate {
                    tool_call_id: tool_call_id.clone(),
                    tool_name,
                    args,
                    partial_result: delta.clone(),
                }]
            }
            AgentEvent::ToolExecutionEnd { result, .. } => {
                let (tool_name, _) = self
                    .active_tools
                    .remove(&result.tool_call_id)
                    .unwrap_or_else(|| (String::new(), serde_json::Value::Null));
                vec![
                    ExtensionEvent::ToolResult {
                        result: result.clone(),
                    },
                    ExtensionEvent::ToolExecutionEnd {
                        tool_call_id: result.tool_call_id.clone(),
                        tool_name,
                        result: result.clone(),
                        is_error: result.is_error,
                    },
                ]
            }
            AgentEvent::TurnEnd {
                message,
                tool_results,
            } => vec![ExtensionEvent::TurnEnd {
                turn_index: self.turn_index.saturating_sub(1),
                message: assistant_to_message(message),
                tool_results: tool_results.clone(),
            }],
            AgentEvent::UserMessage(message) => vec![
                ExtensionEvent::UserMessage {
                    message: message.clone(),
                },
                ExtensionEvent::Input {
                    text: message_text(message),
                    source: InputSource::Interactive,
                },
            ],
            // Upstream has no agent-level error event; the TUI banner is the
            // only surface for it (see the module docs).
            AgentEvent::Error(_) => Vec::new(),
        }
    }
}

/// Project an [`AssistantMessage`](pi_protocol::AssistantMessage) onto the
/// generic [`Message`] shape the extension payloads use.
fn assistant_to_message(message: &pi_protocol::AssistantMessage) -> Message {
    Message {
        role: Role::Assistant,
        content: message.content.clone(),
        model: Some(message.model.clone()),
    }
}

/// Flatten a message's text blocks. Used for `input.text`, which upstream
/// carries as a plain string.
fn message_text(message: &Message) -> String {
    let mut out = String::new();
    for block in &message.content {
        if let Content::Text(text) = block {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&text.text);
        }
    }
    out
}

/// Render an [`AssistantMessageUpdate`] the way upstream's
/// `AssistantMessageEvent` is shaped: a `type` discriminant plus flat
/// payload fields.
///
/// The Rust type serialises as `{ "kind": "text_delta", "delta": … }`
/// (its own `serde` tag is `kind`), so the only adjustment upstream needs
/// is renaming the discriminant key to `type`.
fn assistant_message_event_json(update: &AssistantMessageUpdate) -> serde_json::Value {
    let mut value = serde_json::to_value(update).unwrap_or(serde_json::Value::Null);
    if let Some(object) = value.as_object_mut() {
        if let Some(kind) = object.remove("kind") {
            object.insert("type".to_string(), kind);
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_protocol::{AssistantMessage, StopReason, ToolCall, ToolResult, Usage};

    fn assistant(text: &str) -> AssistantMessage {
        AssistantMessage {
            model: "faux".into(),
            content: vec![Content::text(text)],
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
            error_message: None,
        }
    }

    fn user(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![Content::text(text)],
            model: None,
        }
    }

    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: serde_json::json!({"path": "src/main.rs"}),
        }
    }

    fn result(id: &str) -> ToolResult {
        ToolResult {
            tool_call_id: id.into(),
            content: Box::new(Content::text("ok")),
            is_error: false,
            details: None,
            added_tool_names: None,
        }
    }

    #[test]
    fn run_brackets_and_turn_index_are_tracked() {
        let mut mapper = ExtensionEventMapper::new();
        assert_eq!(
            mapper.map(&AgentEvent::AgentStart, 1),
            vec![ExtensionEvent::AgentStart]
        );
        let first = mapper.map(&AgentEvent::TurnStart, 1_700_000_000_000);
        assert_eq!(
            first,
            vec![ExtensionEvent::TurnStart {
                turn_index: 0,
                timestamp: 1_700_000_000_000,
            }]
        );
        let second = mapper.map(&AgentEvent::TurnStart, 1);
        assert_eq!(
            second,
            vec![ExtensionEvent::TurnStart {
                turn_index: 1,
                timestamp: 1,
            }]
        );
        let end = mapper.map(
            &AgentEvent::TurnEnd {
                message: assistant("done"),
                tool_results: vec![user("ignored")],
            },
            1,
        );
        assert_eq!(
            end,
            vec![ExtensionEvent::TurnEnd {
                turn_index: 1,
                message: Message {
                    role: Role::Assistant,
                    content: vec![Content::text("done")],
                    model: Some("faux".into()),
                },
                tool_results: vec![user("ignored")],
            }]
        );
        let stop = mapper.map(&AgentEvent::AgentEnd { messages: vec![] }, 1);
        assert_eq!(
            stop,
            vec![ExtensionEvent::AgentEnd { messages: vec![] }],
            "agent_end closes the run and carries the message log"
        );
    }

    #[test]
    fn user_message_maps_to_user_message_and_input() {
        let mut mapper = ExtensionEventMapper::new();
        let mapped = mapper.map(&AgentEvent::UserMessage(user("hello")), 0);
        assert_eq!(mapped.len(), 2);
        assert_eq!(mapped[0].name(), "user_message");
        assert_eq!(
            mapped[1],
            ExtensionEvent::Input {
                text: "hello".into(),
                source: InputSource::Interactive,
            }
        );
    }

    #[test]
    fn tool_lifecycle_remembers_name_and_args() {
        let mut mapper = ExtensionEventMapper::new();
        let mapped = mapper.map(
            &AgentEvent::ToolExecutionStart {
                call: call("t1", "read"),
            },
            0,
        );
        assert_eq!(mapped.len(), 2, "tool_call + tool_execution_start");
        assert_eq!(mapped[0].name(), "tool_call");
        assert_eq!(
            mapped[1],
            ExtensionEvent::ToolExecutionStart {
                tool_call_id: "t1".into(),
                tool_name: "read".into(),
                args: serde_json::json!({"path": "src/main.rs"}),
            }
        );

        let update = mapper.map(
            &AgentEvent::ToolExecutionUpdate {
                tool_call_id: "t1".into(),
                delta: "half".into(),
            },
            0,
        );
        assert_eq!(
            update,
            vec![ExtensionEvent::ToolExecutionUpdate {
                tool_call_id: "t1".into(),
                tool_name: "read".into(),
                args: serde_json::json!({"path": "src/main.rs"}),
                partial_result: "half".into(),
            }]
        );

        let end = mapper.map(
            &AgentEvent::ToolExecutionEnd {
                result: result("t1"),
                duration_ms: 12,
            },
            0,
        );
        assert_eq!(end.len(), 2, "tool_result + tool_execution_end");
        match &end[1] {
            ExtensionEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                is_error,
                ..
            } => {
                assert_eq!(tool_call_id, "t1");
                assert_eq!(tool_name, "read");
                assert!(!is_error);
            }
            other => panic!("unexpected {other:?}"),
        }
        // The cache is cleared, so a stray update degrades to an empty name
        // instead of leaking the previous call's arguments.
        let stray = mapper.map(
            &AgentEvent::ToolExecutionUpdate {
                tool_call_id: "t1".into(),
                delta: "late".into(),
            },
            0,
        );
        match &stray[0] {
            ExtensionEvent::ToolExecutionUpdate { tool_name, .. } => assert_eq!(tool_name, ""),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn message_update_uses_upstream_type_key() {
        let mut mapper = ExtensionEventMapper::new();
        let mapped = mapper.map(
            &AgentEvent::MessageUpdate(AssistantMessageUpdate::TextDelta { delta: "hi".into() }),
            0,
        );
        let payload = match &mapped[0] {
            ExtensionEvent::MessageUpdate {
                assistant_message_event,
            } => assistant_message_event.clone(),
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(payload["type"], "text_delta");
        assert_eq!(payload["delta"], "hi");
        assert!(payload.get("kind").is_none(), "kind is renamed to type");
    }

    #[test]
    fn error_maps_to_nothing() {
        let mut mapper = ExtensionEventMapper::new();
        assert!(mapper.map(&AgentEvent::Error("boom".into()), 0).is_empty());
    }
}
