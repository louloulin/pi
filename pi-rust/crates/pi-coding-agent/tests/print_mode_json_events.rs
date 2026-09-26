//! Integration tests for print-mode JSON event alignment.
//!
//! Port of the wire-shape contract spelled out by:
//!
//! * `packages/coding-agent/src/modes/print-mode.ts`
//! * `packages/coding-agent/src/modes/json-event.ts`
//!
//! The Rust port keeps the per-event translation in
//! [`pi_coding_agent::rpc::events::agent_event_to_json`] so the print
//! (`--mode json` / `--mode json-events`) feed and the RPC feed cannot
//! drift. These tests pin the exact JSON shape so a downstream tool
//! that consumes the feed stays compatible.

#![cfg(not(target_arch = "wasm32"))]

use pi_agent_core::AgentEvent;
use pi_coding_agent::rpc::events::agent_event_to_json;
use pi_protocol::{AssistantMessage, Content, Message, Role, StopReason, ToolResult, Usage};
use serde_json::json;

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
fn agent_start_emits_a_single_object() {
    let payloads = agent_event_to_json(&AgentEvent::AgentStart, 0);
    assert_eq!(payloads, vec![json!({"type": "agent_start"})]);
}

#[test]
fn agent_end_reports_message_count() {
    let event = AgentEvent::AgentEnd { messages: vec![] };
    let payloads = agent_event_to_json(&event, 1);
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0]["type"], "agent_end");
    assert_eq!(payloads[0]["messages"], 0);
}

#[test]
fn turn_start_round_trip_is_stable() {
    let payloads = agent_event_to_json(&AgentEvent::TurnStart, 0);
    assert_eq!(payloads, vec![json!({"type": "turn_start"})]);
}

#[test]
fn message_start_includes_the_model_name() {
    let event = AgentEvent::MessageStart {
        model: "claude-3-5-sonnet".into(),
    };
    let payloads = agent_event_to_json(&event, 1);
    assert_eq!(
        payloads,
        vec![json!({"type": "message_start", "model": "claude-3-5-sonnet"})]
    );
}

#[test]
fn text_delta_wraps_in_message_update() {
    use pi_agent_core::AssistantMessageUpdate;
    let event = AgentEvent::MessageUpdate(AssistantMessageUpdate::TextDelta {
        delta: "hello".into(),
    });
    let payloads = agent_event_to_json(&event, 1);
    assert_eq!(payloads.len(), 1);
    assert_eq!(
        payloads[0],
        json!({
            "type": "message_update",
            "assistantMessageEvent": {"type": "text_delta", "delta": "hello"},
        })
    );
}

#[test]
fn thinking_delta_round_trips() {
    use pi_agent_core::AssistantMessageUpdate;
    let event = AgentEvent::MessageUpdate(AssistantMessageUpdate::ThinkingDelta {
        delta: "reasoning".into(),
    });
    let payloads = agent_event_to_json(&event, 1);
    assert_eq!(
        payloads[0],
        json!({
            "type": "message_update",
            "assistantMessageEvent": {"type": "thinking_delta", "delta": "reasoning"},
        })
    );
}

#[test]
fn tool_call_delta_includes_id_and_name() {
    use pi_agent_core::AssistantMessageUpdate;
    let event = AgentEvent::MessageUpdate(AssistantMessageUpdate::ToolCallDelta {
        index: 0,
        id: Some("call-1".into()),
        name: Some("read".into()),
        arguments_delta: Some("{\"path\":".into()),
    });
    let payloads = agent_event_to_json(&event, 1);
    assert_eq!(payloads[0]["type"], "message_update");
    assert_eq!(
        payloads[0]["assistantMessageEvent"],
        json!({
            "type": "toolcall_delta",
            "index": 0,
            "id": "call-1",
            "name": "read",
            "arguments_delta": "{\"path\":",
        })
    );
}

#[test]
fn message_end_reports_stop_reason_and_usage() {
    let event = AgentEvent::MessageEnd {
        message: assistant_message("done"),
    };
    let payloads = agent_event_to_json(&event, 1);
    assert_eq!(payloads[0]["type"], "message_end");
    assert_eq!(payloads[0]["stop_reason"], "stop");
    assert!(payloads[0]["usage"].is_object());
}

#[test]
fn tool_execution_start_has_id_name_arguments() {
    let event = AgentEvent::ToolExecutionStart {
        call: pi_protocol::ToolCall {
            id: "call-1".into(),
            name: "read".into(),
            arguments: json!({"path": "/tmp/x"}),
        },
    };
    let payloads = agent_event_to_json(&event, 1);
    assert_eq!(
        payloads[0],
        json!({
            "type": "tool_execution_start",
            "id": "call-1",
            "name": "read",
            "arguments": {"path": "/tmp/x"},
        })
    );
}

#[test]
fn tool_execution_update_carries_tool_call_id_and_delta() {
    let event = AgentEvent::ToolExecutionUpdate {
        tool_call_id: "call-1".into(),
        delta: "partial line".into(),
    };
    let payloads = agent_event_to_json(&event, 1);
    assert_eq!(
        payloads[0],
        json!({
            "type": "tool_execution_update",
            "tool_call_id": "call-1",
            "delta": "partial line",
        })
    );
}

#[test]
fn tool_execution_end_with_no_details_is_a_single_object() {
    let event = AgentEvent::ToolExecutionEnd {
        result: ToolResult {
            tool_call_id: "call-1".into(),
            content: Box::new(Content::text("ok")),
            is_error: false,
            details: None,
            added_tool_names: None,
            images: Vec::new(),
        },
        duration_ms: 7,
    };
    let payloads = agent_event_to_json(&event, 1);
    assert_eq!(payloads.len(), 1);
    assert_eq!(
        payloads[0],
        json!({
            "type": "tool_execution_end",
            "tool_call_id": "call-1",
            "is_error": false,
            "duration_ms": 7,
        })
    );
}

#[test]
fn tool_execution_end_with_details_emits_two_objects() {
    let event = AgentEvent::ToolExecutionEnd {
        result: ToolResult {
            tool_call_id: "call-1".into(),
            content: Box::new(Content::text("ok")),
            is_error: false,
            details: Some(json!({"lines": 3})),
            added_tool_names: None,
            images: Vec::new(),
        },
        duration_ms: 12,
    };
    let payloads = agent_event_to_json(&event, 1);
    assert_eq!(payloads.len(), 2);
    assert_eq!(payloads[0]["type"], "tool_execution_end");
    assert_eq!(payloads[1]["type"], "tool_execution_end_details");
    assert_eq!(payloads[1]["details"], json!({"lines": 3}));
}

#[test]
fn turn_end_emits_turn_count_usage_and_stop_reason() {
    let event = AgentEvent::TurnEnd {
        message: assistant_message("done"),
        tool_results: Vec::new(),
    };
    let payloads = agent_event_to_json(&event, 2);
    assert_eq!(
        payloads[0],
        json!({
            "type": "turn_end",
            "turn": 2,
            "usage": payloads[0]["usage"].clone(),
            "stop_reason": "stop",
            "tool_results": 0,
        })
    );
}

#[test]
fn user_message_round_trips_with_full_message_object() {
    let event = AgentEvent::UserMessage(Message {
        role: Role::User,
        content: vec![Content::text("hi")],
        model: None,
    });
    let payloads = agent_event_to_json(&event, 1);
    assert_eq!(payloads[0]["type"], "user_message");
    assert!(payloads[0]["message"].is_object());
}

#[test]
fn error_event_is_a_single_object() {
    let event = AgentEvent::Error("boom".into());
    let payloads = agent_event_to_json(&event, 1);
    assert_eq!(
        payloads[0],
        json!({"type": "error", "message": "boom"})
    );
}