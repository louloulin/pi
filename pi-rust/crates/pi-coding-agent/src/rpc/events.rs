//! Shared agent-event → JSON serialization for the `json-events` /
//! RPC surfaces.
//!
//! Stage 8 (`print_mode::OutputFormat::JsonEvents`) defined the wire
//! shape for every [`AgentEvent`]; the RPC mode has to emit the *same*
//! objects on stdout (only the envelope differs — print mode writes the
//! bare object per line, RPC wraps it in a JSON-RPC `Notification`).
//! Keeping the translation in one place means the two feeds cannot
//! drift apart.
//!
//! Every event maps to one JSON object with a `type` discriminant
//! (`turn_start`, `message_start`, `message_update`, …). The one
//! exception is [`AgentEvent::ToolExecutionEnd`], which may produce a
//! second `tool_execution_end_details` object when the tool result
//! carries structured details.

use pi_agent_core::{AgentEvent, AssistantMessageUpdate};
use serde_json::{json, Value};

/// Translate one [`AgentEvent`] into the JSON payload(s) the
/// `json-events` feed and the RPC event pump write on stdout.
///
/// `turn` is the 1-based number of the turn the event belongs to. It is
/// only consumed by [`AgentEvent::TurnEnd`] (every other variant matches
/// the Stage 8 shape exactly). Callers bump their turn counter *before*
/// calling this for `TurnEnd` so the emitted `turn` field counts the
/// turn that just finished.
pub fn agent_event_to_json(event: &AgentEvent, turn: u32) -> Vec<Value> {
    match event {
        AgentEvent::AgentStart => vec![json!({"type": "agent_start"})],
        AgentEvent::AgentEnd { messages } => vec![json!({
            "type": "agent_end",
            "messages": messages.len(),
        })],
        AgentEvent::TurnStart => vec![json!({"type": "turn_start"})],
        AgentEvent::MessageStart { model } => {
            vec![json!({"type": "message_start", "model": model})]
        }
        AgentEvent::MessageUpdate(update) => {
            let assistant_message_event = match update {
                AssistantMessageUpdate::TextDelta { delta } => {
                    json!({"type": "text_delta", "delta": delta})
                }
                AssistantMessageUpdate::ThinkingDelta { delta } => {
                    json!({"type": "thinking_delta", "delta": delta})
                }
                AssistantMessageUpdate::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments_delta,
                } => json!({
                    "type": "toolcall_delta",
                    "index": index,
                    "id": id,
                    "name": name,
                    "arguments_delta": arguments_delta,
                }),
            };
            vec![json!({
                "type": "message_update",
                "assistantMessageEvent": assistant_message_event,
            })]
        }
        AgentEvent::MessageEnd { message } => vec![json!({
            "type": "message_end",
            "stop_reason": message.stop_reason,
            "usage": message.usage,
        })],
        AgentEvent::ToolExecutionStart { call } => vec![json!({
            "type": "tool_execution_start",
            "id": call.id,
            "name": call.name,
            "arguments": call.arguments,
        })],
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            delta,
        } => vec![json!({
            "type": "tool_execution_update",
            "tool_call_id": tool_call_id,
            "delta": delta,
        })],
        AgentEvent::ToolExecutionEnd {
            result,
            duration_ms,
        } => {
            let mut payloads = vec![json!({
                "type": "tool_execution_end",
                "tool_call_id": result.tool_call_id,
                "is_error": result.is_error,
                "duration_ms": duration_ms,
            })];
            if let Some(detail) = result.details.as_ref() {
                payloads.push(json!({
                    "type": "tool_execution_end_details",
                    "tool_call_id": result.tool_call_id,
                    "details": detail,
                }));
            }
            payloads
        }
        AgentEvent::TurnEnd {
            message,
            tool_results,
        } => vec![json!({
            "type": "turn_end",
            "turn": turn,
            "usage": message.usage,
            "stop_reason": message.stop_reason,
            "tool_results": tool_results.len(),
        })],
        AgentEvent::UserMessage(msg) => {
            vec![json!({"type": "user_message", "message": msg})]
        }
        AgentEvent::Error(message) => vec![json!({"type": "error", "message": message})],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_protocol::{AssistantMessage, Content, StopReason, Usage};

    fn assistant_message(text: &str) -> AssistantMessage {
        AssistantMessage {
            model: "faux-model".into(),
            content: vec![Content::text(text)],
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
            error_message: None,
        }
    }

    #[test]
    fn turn_start_uses_stage8_shape() {
        let payloads = agent_event_to_json(&AgentEvent::TurnStart, 0);
        assert_eq!(payloads, vec![json!({"type": "turn_start"})]);
    }

    #[test]
    fn text_delta_wraps_in_message_update() {
        let event =
            AgentEvent::MessageUpdate(AssistantMessageUpdate::TextDelta { delta: "hi".into() });
        let payloads = agent_event_to_json(&event, 1);
        assert_eq!(payloads.len(), 1);
        assert_eq!(
            payloads[0],
            json!({
                "type": "message_update",
                "assistantMessageEvent": {"type": "text_delta", "delta": "hi"},
            })
        );
    }

    #[test]
    fn turn_end_carries_the_supplied_turn_number() {
        let event = AgentEvent::TurnEnd {
            message: assistant_message("done"),
            tool_results: Vec::new(),
        };
        let payloads = agent_event_to_json(&event, 3);
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0]["type"], "turn_end");
        assert_eq!(payloads[0]["turn"], 3);
        assert_eq!(payloads[0]["tool_results"], 0);
    }

    #[test]
    fn tool_execution_end_with_details_emits_two_objects() {
        let event = AgentEvent::ToolExecutionEnd {
            result: pi_protocol::ToolResult {
                tool_call_id: "call-1".into(),
                content: Box::new(Content::text("ok")),
                is_error: false,
                details: Some(json!({"lines": 3})),
                added_tool_names: None,
                images: Vec::new(),
            },
            duration_ms: 12,
        };
        let payloads = agent_event_to_json(&event, 0);
        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[0]["type"], "tool_execution_end");
        assert_eq!(payloads[1]["type"], "tool_execution_end_details");
    }
}
