//! Telemetry instrumentation tests for `pi-agent-core`.
//!
//! The loop's telemetry is opt-in: these tests install a
//! [`MemoryTelemetry`] recorder and assert the span tree, attributes and
//! statuses the run emits, then verify that leaving
//! [`AgentConfig::telemetry`] at `None` changes nothing about the run's
//! observable behaviour.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use pi_agent_core::telemetry::{attribute_name, span_name};
use pi_agent_core::{
    AgentConfig, AgentError, AgentHookAdapter, AgentLoop, AgentState, ToolExecutor,
};
use pi_ai::stream::{AssistantMessageEventStream, SharedStreamFn, StreamFn};
use pi_ai::types::SimpleStreamOptions;
use pi_ai::StreamError;
use pi_protocol::{
    Api, AssistantMessage, AssistantMessageEvent, Content, Context as AgentContext, Message, Model,
    ProviderId, Role, StopReason, TextContent, ToolCall, ToolDefinition, ToolResult, Usage,
};
use pi_telemetry::{
    AttributeValue, MemoryTelemetry, RecordedTelemetrySpan, SpanStatus, TelemetryContext,
};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: None,
        context_window: 1024,
        max_output_tokens: 256,
    }
}

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![Content::Text(TextContent { text: text.into() })],
        model: None,
    }
}

fn tool_call_message(calls: &[(&str, &str)]) -> AssistantMessage {
    AssistantMessage {
        model: "faux-model".into(),
        content: calls
            .iter()
            .map(|(name, id)| {
                Content::ToolCall(ToolCall {
                    id: (*id).into(),
                    name: (*name).into(),
                    arguments: serde_json::json!({}),
                })
            })
            .collect(),
        stop_reason: StopReason::ToolUse,
        usage: Usage {
            input: 12,
            output: 3,
            cache_read: 5,
            cache_write: 1,
            total: 15,
        },
    }
}

fn text_reply(text: &str) -> AssistantMessage {
    AssistantMessage {
        model: "faux-model".into(),
        content: vec![Content::text(text)],
        stop_reason: StopReason::Stop,
        usage: Usage {
            input: 7,
            output: 2,
            cache_read: 0,
            cache_write: 0,
            total: 9,
        },
    }
}

/// Scripted stream — returns the next `AssistantMessage` on every call.
struct ScriptedStream {
    responses: Mutex<Vec<AssistantMessage>>,
}

impl ScriptedStream {
    fn new(responses: Vec<AssistantMessage>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }
}

#[async_trait]
impl StreamFn for ScriptedStream {
    async fn stream_simple(
        &self,
        _model: &Model,
        _ctx: &AgentContext,
        _options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        let mut responses = self.responses.lock().expect("scripted responses");
        if responses.is_empty() {
            return Err(StreamError::Malformed("scripted stream exhausted".into()));
        }
        let message = responses.remove(0);
        let events = vec![
            Ok(AssistantMessageEvent::Start {
                model: message.model.clone(),
            }),
            Ok(AssistantMessageEvent::Done {
                content: message.content,
                stop_reason: message.stop_reason,
                usage: message.usage,
            }),
        ];
        Ok(Box::pin(stream::iter(events)))
    }
}

/// Stream that always fails, for the error-status path.
struct FailingStream;

#[async_trait]
impl StreamFn for FailingStream {
    async fn stream_simple(
        &self,
        _model: &Model,
        _ctx: &AgentContext,
        _options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        Err(StreamError::Malformed("connection refused".into()))
    }
}

/// Executor that succeeds for every tool except `abort`, which fails at the
/// executor level.
struct MockToolExecutor;

#[async_trait]
impl ToolExecutor for MockToolExecutor {
    fn definitions(&self) -> Vec<ToolDefinition> {
        ["alpha", "abort"]
            .iter()
            .map(|name| ToolDefinition {
                name: (*name).to_string(),
                label: (*name).to_string(),
                description: format!("{name} mock tool"),
                parameters: serde_json::json!({"type": "object"}),
                metadata: None,
            })
            .collect()
    }

    async fn execute(
        &self,
        call: &ToolCall,
        _signal: CancellationToken,
    ) -> Result<ToolResult, AgentError> {
        if call.name == "abort" {
            return Err(AgentError::Tool {
                tool: call.name.clone(),
                message: "operation aborted".into(),
            });
        }
        Ok(ToolResult {
            tool_call_id: call.id.clone(),
            content: Box::new(Content::text(format!("ran {}", call.name))),
            is_error: false,
            details: None,
        })
    }
}

fn loop_with(
    stream_fn: SharedStreamFn,
    executor: Option<Arc<dyn ToolExecutor>>,
    telemetry: Option<Arc<dyn TelemetryContext>>,
) -> AgentLoop {
    AgentLoop::new(
        AgentConfig {
            stream_fn,
            model: faux_model(),
            tool_executor: executor,
            tool_execution: pi_protocol::ToolExecutionMode::Parallel,
            // `provider_errors_are_recorded_on_the_request_and_run_spans`
            // streams a permanent `connection refused` failure; retrying it
            // would only make the test slow.
            retry: pi_agent_core::RetryPolicy::disabled(),
            telemetry,
        },
        AgentState {
            system_prompt: "you are pi".into(),
            messages: Vec::new(),
            model_override: None,
        },
        AgentHookAdapter::new(),
    )
}

fn span<'a>(spans: &'a [RecordedTelemetrySpan], name: &str) -> &'a RecordedTelemetrySpan {
    spans
        .iter()
        .find(|span| span.name == name)
        .unwrap_or_else(|| panic!("no span named {name}; got {:?}", names(spans)))
}

fn spans_named<'a>(
    spans: &'a [RecordedTelemetrySpan],
    name: &str,
) -> Vec<&'a RecordedTelemetrySpan> {
    spans.iter().filter(|span| span.name == name).collect()
}

fn names(spans: &[RecordedTelemetrySpan]) -> Vec<&str> {
    spans.iter().map(|span| span.name.as_str()).collect()
}

fn string_attr(span: &RecordedTelemetrySpan, key: &str) -> String {
    match span.attributes.get(key) {
        Some(AttributeValue::String(value)) => value.clone(),
        other => panic!("attribute {key} on {} was {other:?}", span.name),
    }
}

fn bool_attr(span: &RecordedTelemetrySpan, key: &str) -> bool {
    match span.attributes.get(key) {
        Some(AttributeValue::Boolean(value)) => *value,
        other => panic!("attribute {key} on {} was {other:?}", span.name),
    }
}

fn number_attr(span: &RecordedTelemetrySpan, key: &str) -> f64 {
    match span.attributes.get(key) {
        Some(AttributeValue::Number(value)) => *value,
        other => panic!("attribute {key} on {} was {other:?}", span.name),
    }
}

fn error_status(span: &RecordedTelemetrySpan) -> &pi_telemetry::SpanError {
    match &span.status {
        SpanStatus::Error(Some(error)) => error,
        other => panic!("span {} status was {other:?}", span.name),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_emits_nested_span_tree() {
    let recorder = Arc::new(MemoryTelemetry::new());
    let stream: SharedStreamFn = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("alpha", "call-1")]),
        text_reply("done"),
    ]));
    let mut agent = loop_with(
        stream,
        Some(Arc::new(MockToolExecutor)),
        Some(recorder.clone() as Arc<dyn TelemetryContext>),
    );

    let outcome = agent
        .run(vec![user_message("hi")], |_| {})
        .await
        .expect("run succeeds");
    // The final turn is the text reply, so the *outcome* carries no tool
    // results — the batch belongs to turn 1 and lives in the message log.
    assert!(!outcome.tool_executed);
    let recorded: Vec<ToolResult> = agent
        .state()
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match content {
            Content::ToolResult(result) => Some(result.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].tool_call_id, "call-1");

    let spans = recorder.spans();
    // Two turns: turn → request → tool, then turn → request.
    assert_eq!(
        names(&spans),
        vec![
            span_name::HARNESS_RUN,
            span_name::HARNESS_TURN,
            span_name::AI_REQUEST,
            span_name::HARNESS_TOOL,
            span_name::HARNESS_TURN,
            span_name::AI_REQUEST,
        ]
    );
    assert!(spans.iter().all(|span| span.settled));

    let run = span(&spans, span_name::HARNESS_RUN);
    assert_eq!(run.parent_id, None);
    assert_eq!(string_attr(run, attribute_name::OPERATION_KIND), "run");
    assert_eq!(
        string_attr(run, attribute_name::OPERATION_OUTCOME),
        "completed"
    );
    assert_eq!(run.status, SpanStatus::Ok);

    let turns = spans_named(&spans, span_name::HARNESS_TURN);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].parent_id, Some(run.id));
    assert_eq!(string_attr(turns[0], attribute_name::TURN_ID), "1");
    assert_eq!(string_attr(turns[1], attribute_name::TURN_ID), "2");

    // Request 1 (tool_use) and request 2 (stop) both hang off their turn.
    let requests = spans_named(&spans, span_name::AI_REQUEST);
    assert_eq!(requests[0].parent_id, Some(turns[0].id));
    assert_eq!(requests[1].parent_id, Some(turns[1].id));
    assert_eq!(
        string_attr(requests[0], attribute_name::AI_OPERATION),
        "stream"
    );
    assert_eq!(
        string_attr(requests[0], attribute_name::AI_PROVIDER),
        "faux"
    );
    assert_eq!(
        string_attr(requests[0], attribute_name::AI_MODEL),
        "faux-model"
    );
    assert_eq!(string_attr(requests[0], attribute_name::AI_API), "faux");
    assert!(bool_attr(requests[0], attribute_name::AI_STREAMING));
    assert_eq!(
        string_attr(requests[0], attribute_name::AI_RESPONSE_MODEL),
        "faux-model"
    );
    assert_eq!(
        string_attr(requests[0], attribute_name::AI_RESPONSE_STOP_REASON),
        "tool_use"
    );
    assert_eq!(
        number_attr(requests[0], attribute_name::AI_USAGE_INPUT_TOKENS),
        12.0
    );
    assert_eq!(
        number_attr(requests[0], attribute_name::AI_USAGE_OUTPUT_TOKENS),
        3.0
    );
    assert_eq!(
        number_attr(requests[0], attribute_name::AI_USAGE_CACHE_READ_TOKENS),
        5.0
    );
    assert_eq!(
        number_attr(requests[0], attribute_name::AI_USAGE_CACHE_WRITE_TOKENS),
        1.0
    );
    assert_eq!(
        number_attr(requests[0], attribute_name::AI_USAGE_TOTAL_TOKENS),
        15.0
    );
    assert_eq!(
        string_attr(requests[1], attribute_name::AI_RESPONSE_STOP_REASON),
        "stop"
    );

    let tool = span(&spans, span_name::HARNESS_TOOL);
    assert_eq!(tool.parent_id, Some(turns[0].id));
    assert_eq!(string_attr(tool, attribute_name::TOOL_NAME), "alpha");
    assert_eq!(string_attr(tool, attribute_name::TOOL_CALL_ID), "call-1");
    assert!(!bool_attr(tool, attribute_name::TOOL_IS_ERROR));
    assert_eq!(tool.status, SpanStatus::Ok);
}

#[tokio::test]
async fn tool_errors_mark_the_tool_span_but_not_the_run() {
    let recorder = Arc::new(MemoryTelemetry::new());
    let stream: SharedStreamFn = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("abort", "call-9")]),
        text_reply("recovered"),
    ]));
    let mut agent = loop_with(
        stream,
        Some(Arc::new(MockToolExecutor)),
        Some(recorder.clone() as Arc<dyn TelemetryContext>),
    );

    agent
        .run(vec![user_message("go")], |_| {})
        .await
        .expect("tool failure never aborts the run");

    let spans = recorder.spans();
    let tool = span(&spans, span_name::HARNESS_TOOL);
    assert!(bool_attr(tool, attribute_name::TOOL_IS_ERROR));
    assert_eq!(error_status(tool).name, "ToolError");

    // Executor-level failures are folded into error results, so the batch
    // continues and the run still completes.
    let run = span(&spans, span_name::HARNESS_RUN);
    assert_eq!(run.status, SpanStatus::Ok);
    assert_eq!(
        string_attr(run, attribute_name::OPERATION_OUTCOME),
        "completed"
    );
    assert_eq!(spans_named(&spans, span_name::AI_REQUEST).len(), 2);
}

#[tokio::test]
async fn provider_errors_are_recorded_on_the_request_and_run_spans() {
    let recorder = Arc::new(MemoryTelemetry::new());
    let stream: SharedStreamFn = Arc::new(FailingStream);
    let mut agent = loop_with(
        stream,
        None,
        Some(recorder.clone() as Arc<dyn TelemetryContext>),
    );

    let error = agent
        .run(vec![user_message("hi")], |_| {})
        .await
        .expect_err("failing stream aborts the run");
    assert!(matches!(error, AgentError::Stream(_)));

    let spans = recorder.spans();
    assert_eq!(
        names(&spans),
        vec![
            span_name::HARNESS_RUN,
            span_name::HARNESS_TURN,
            span_name::AI_REQUEST,
        ]
    );

    let request = span(&spans, span_name::AI_REQUEST);
    assert_eq!(
        string_attr(request, attribute_name::AI_ERROR_TYPE),
        "stream"
    );
    assert_eq!(error_status(request).name, "stream");

    let run = span(&spans, span_name::HARNESS_RUN);
    assert_eq!(
        string_attr(run, attribute_name::OPERATION_OUTCOME),
        "failed"
    );
    assert_eq!(error_status(run).name, "stream");
}

#[tokio::test]
async fn telemetry_is_opt_in_and_does_not_change_the_run() {
    let stream = || -> SharedStreamFn {
        Arc::new(ScriptedStream::new(vec![
            tool_call_message(&[("alpha", "call-1")]),
            text_reply("done"),
        ]))
    };

    // A recorder that is never installed must stay empty.
    let unused = MemoryTelemetry::new();

    let mut plain = loop_with(stream(), Some(Arc::new(MockToolExecutor)), None);
    let plain_outcome = plain
        .run(vec![user_message("hi")], |_| {})
        .await
        .expect("plain run succeeds");

    let recorder = Arc::new(MemoryTelemetry::new());
    let mut instrumented = loop_with(
        stream(),
        Some(Arc::new(MockToolExecutor)),
        Some(recorder.clone() as Arc<dyn TelemetryContext>),
    );
    let instrumented_outcome = instrumented
        .run(vec![user_message("hi")], |_| {})
        .await
        .expect("instrumented run succeeds");

    assert!(unused.is_empty());
    assert!(!recorder.is_empty());
    assert_eq!(plain_outcome.message, instrumented_outcome.message);
    assert_eq!(
        plain_outcome.tool_results.len(),
        instrumented_outcome.tool_results.len()
    );
    assert_eq!(
        plain_outcome.tool_executed,
        instrumented_outcome.tool_executed
    );
}
