//! Smoke tests for `pi-protocol` — verify serde round-trip on the wire types.

use pi_protocol::{
    AssistantMessage, AssistantMessageEvent, Content, Context, ExtensionEvent, Message, Model,
    ProviderId, Role, SessionEntry, StopReason, TextContent, ToolCall, ToolDefinition, ToolResult,
    UiLevel, UiRequest, UiResponse, Usage,
};
use serde_json::json;

#[test]
fn content_round_trip() {
    let original = Content::Text(TextContent {
        text: "hello".into(),
    });
    let s = serde_json::to_string(&original).unwrap();
    let back: Content = serde_json::from_str(&s).unwrap();
    assert_eq!(original, back);
    assert!(!original.is_tool_call());
}

#[test]
fn tool_call_round_trip() {
    let call = ToolCall {
        id: "1".into(),
        name: "bash".into(),
        arguments: json!({"cmd": "ls"}),
    };
    let s = serde_json::to_string(&call).unwrap();
    let back: ToolCall = serde_json::from_str(&s).unwrap();
    assert_eq!(call, back);
}

#[test]
fn context_round_trip() {
    let ctx = Context {
        system_prompt: "you are pi".into(),
        messages: vec![Message {
            role: Role::User,
            content: vec![Content::Text(TextContent { text: "hi".into() })],
            model: None,
        }],
        tools: vec![ToolDefinition {
            name: "bash".into(),
            label: "Bash".into(),
            description: "Run a shell command".into(),
            parameters: json!({"type": "object"}),
            metadata: None,
        }],
    };
    let s = serde_json::to_string(&ctx).unwrap();
    let back: Context = serde_json::from_str(&s).unwrap();
    assert_eq!(ctx, back);
}

#[test]
fn assistant_message_event_round_trip() {
    let event = AssistantMessageEvent::Done {
        content: vec![Content::Text(TextContent { text: "ok".into() })],
        stop_reason: StopReason::Stop,
        usage: Usage {
            input: 10,
            output: 20,
            ..Default::default()
        },
    };
    let s = serde_json::to_string(&event).unwrap();
    let back: AssistantMessageEvent = serde_json::from_str(&s).unwrap();
    assert_eq!(event, back);
}

#[test]
fn extension_event_tagged() {
    let ev = ExtensionEvent::ToolResult {
        result: ToolResult {
            tool_call_id: "1".into(),
content: Box::new(Content::Text(TextContent { text: "ok".into() })),
            is_error: false,
            details: None,
        },
    };
    let v = serde_json::to_value(&ev).unwrap();
    assert_eq!(v["type"], "tool_result");
}

#[test]
fn ui_request_notify_tagged() {
    let req = UiRequest::Notify {
        message: "hi".into(),
        level: UiLevel::Info,
    };
    let v = serde_json::to_value(&req).unwrap();
    assert_eq!(v["kind"], "notify");
    assert_eq!(v["level"], "info");

    let back: UiRequest = serde_json::from_value(v).unwrap();
    assert_eq!(back, req);
}

#[test]
fn ui_response_confirm_round_trip() {
    let resp = UiResponse::Confirm { accepted: true };
    let s = serde_json::to_string(&resp).unwrap();
    let back: UiResponse = serde_json::from_str(&s).unwrap();
    assert_eq!(resp, back);
}

#[test]
fn session_entry_extension_round_trip() {
    let entry = SessionEntry::Extension {
        extension: "summarize".into(),
        kind: "summary".into(),
        payload: json!({"tokens": 1024}),
    };
    let s = serde_json::to_string(&entry).unwrap();
    let back: SessionEntry = serde_json::from_str(&s).unwrap();
    assert_eq!(entry, back);
}

#[test]
fn model_round_trip() {
    let m = Model {
        provider: ProviderId::new("anthropic"),
        id: "claude-sonnet-4-6".into(),
        api: pi_protocol::Api::AnthropicMessages,
        label: Some("Claude Sonnet 4.6".into()),
        context_window: 200_000,
        max_output_tokens: 8_192,
    };
    let s = serde_json::to_string(&m).unwrap();
    let back: Model = serde_json::from_str(&s).unwrap();
    assert_eq!(m, back);
}

#[test]
fn assistant_message_round_trip() {
    let m = AssistantMessage {
        model: "faux-model".into(),
        content: vec![Content::Text(TextContent { text: "hi".into() })],
        stop_reason: StopReason::Stop,
        usage: Usage::default(),
    };
    let s = serde_json::to_string(&m).unwrap();
    let back: AssistantMessage = serde_json::from_str(&s).unwrap();
    assert_eq!(m, back);
}
