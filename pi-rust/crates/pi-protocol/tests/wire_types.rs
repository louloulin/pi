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
        tool_call_id: "1".into(),
        tool_name: "read".into(),
        input: serde_json::json!({ "path": "src/main.rs" }),
        content: vec![Content::Text(TextContent { text: "ok".into() })],
        is_error: false,
        details: None,
    };
    let v = serde_json::to_value(&ev).unwrap();
    assert_eq!(v["type"], "tool_result");
    // Upstream field names, not Rust ones (LUM-1330).
    assert_eq!(v["toolCallId"], "1");
    assert_eq!(v["toolName"], "read");
    assert_eq!(v["isError"], false);
    assert_eq!(v["input"]["path"], "src/main.rs");
    assert!(
        v.get("tool_call_id").is_none(),
        "no Rust field name on the wire"
    );
    let back: ExtensionEvent = serde_json::from_value(v).unwrap();
    assert_eq!(back, ev);
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
        error_message: None,
    };
    let s = serde_json::to_string(&m).unwrap();
    let back: AssistantMessage = serde_json::from_str(&s).unwrap();
    assert_eq!(m, back);
}

#[test]
fn tool_result_added_tool_names_round_trip() {
    let result = ToolResult {
        tool_call_id: "call_1".into(),
        content: Box::new(Content::Text(TextContent { text: "ok".into() })),
        is_error: false,
        details: None,
        added_tool_names: Some(vec!["late_tool".into(), "other_tool".into()]),
        images: Vec::new(),
    };
    let v = serde_json::to_value(&result).unwrap();
    assert_eq!(v["added_tool_names"], json!(["late_tool", "other_tool"]));
    let back: ToolResult = serde_json::from_value(v).unwrap();
    assert_eq!(back, result);
    assert_eq!(
        back.added_tool_names.as_deref(),
        Some(["late_tool".to_string(), "other_tool".to_string()].as_slice())
    );
}

#[test]
fn tool_result_omits_added_tool_names_when_none() {
    let result = ToolResult {
        tool_call_id: "call_1".into(),
        content: Box::new(Content::Text(TextContent { text: "ok".into() })),
        is_error: false,
        details: None,
        added_tool_names: None,
            images: Vec::new(),
    };
    let v = serde_json::to_value(&result).unwrap();
    assert!(
        v.get("added_tool_names").is_none(),
        "an absent field must not be serialized: {v}"
    );
    // Backward compatibility: a payload written before the field existed
    // still decodes.
    let back: ToolResult = serde_json::from_value(v).unwrap();
    assert_eq!(back, result);
    assert_eq!(back.added_tool_names, None);
}

#[test]
fn assistant_message_error_message_round_trip() {
    let m = AssistantMessage {
        model: "faux-model".into(),
        content: Vec::new(),
        stop_reason: StopReason::Error,
        usage: Usage::default(),
        error_message: Some("prompt is too long: 213462 tokens > 200000 maximum".into()),
    };
    let v = serde_json::to_value(&m).unwrap();
    assert_eq!(
        v["error_message"],
        json!("prompt is too long: 213462 tokens > 200000 maximum")
    );
    let back: AssistantMessage = serde_json::from_value(v).unwrap();
    assert_eq!(back, m);
    assert_eq!(
        back.error_message.as_deref(),
        Some("prompt is too long: 213462 tokens > 200000 maximum")
    );
}

#[test]
fn assistant_message_omits_error_message_when_none() {
    let m = AssistantMessage {
        model: "faux-model".into(),
        content: vec![Content::Text(TextContent { text: "hi".into() })],
        stop_reason: StopReason::Stop,
        usage: Usage::default(),
        error_message: None,
    };
    let v = serde_json::to_value(&m).unwrap();
    assert!(
        v.get("error_message").is_none(),
        "an absent field must not be serialized: {v}"
    );
    // Backward compatibility: a payload written before the field existed
    // still decodes.
    let back: AssistantMessage = serde_json::from_value(v).unwrap();
    assert_eq!(back, m);
    assert_eq!(back.error_message, None);
}
