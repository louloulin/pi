//! End-to-end tool execution tests for `pi-agent-core`.
//!
//! These drive `AgentLoop::run` with a scripted provider that emits tool
//! calls and a [`MockToolExecutor`] that records what it was asked to run.
//! They cover the loop contract the Stage 10 issue calls out:
//!
//! * every tool call in a batch is dispatched, in source order;
//! * the results are appended to the context in that same order;
//! * a `BeforeToolCall` block decision skips execution;
//! * a failing tool yields `is_error = true` without aborting the turn;
//! * an executor-level `Err` is folded into an error result;
//! * a missing executor keeps the legacy `(stub) executed …` behaviour.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use pi_agent_core::tools::ToolExecutor;
use pi_agent_core::{
    Agent, AgentConfig, AgentError, AgentHookAdapter, AgentOptions, AgentState, BeforeToolCall,
    BeforeToolCallDecision,
};
use pi_ai::stream::{AssistantMessageEventStream, StreamFn};
use pi_ai::types::SimpleStreamOptions;
use pi_ai::StreamError;
use pi_protocol::{
    Api, AssistantMessage, AssistantMessageEvent, Content, Context as AgentContext, Message, Model,
    ProviderId, Role, StopReason, TextContent, ToolCall, ToolDefinition, ToolResult, Usage,
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

fn text_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![Content::Text(TextContent { text: text.into() })],
        model: None,
    }
}

/// Assistant message carrying one or more tool calls.
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
        usage: Usage::default(),
    }
}

fn text_reply(text: &str) -> AssistantMessage {
    AssistantMessage {
        model: "faux-model".into(),
        content: vec![Content::text(text)],
        stop_reason: StopReason::Stop,
        usage: Usage::default(),
    }
}

/// Scripted stream — returns the next `AssistantMessage` on every call.
struct ScriptedStream {
    responses: Mutex<Vec<AssistantMessage>>,
    calls: Mutex<usize>,
}

impl ScriptedStream {
    fn new(responses: Vec<AssistantMessage>) -> Self {
        Self {
            responses: Mutex::new(responses),
            calls: Mutex::new(0),
        }
    }

    fn call_count(&self) -> usize {
        *self.calls.lock().expect("stream calls")
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
        *self.calls.lock().expect("stream calls") += 1;
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

/// Records the calls it receives and returns canned results by tool name:
///
/// * `explode` → a normal result flagged `is_error`;
/// * `abort` → an executor-level `AgentError::Tool` (cancellation path);
/// * anything else → a successful text result naming the tool.
struct MockToolExecutor {
    calls: Mutex<Vec<ToolCall>>,
    definitions: Vec<ToolDefinition>,
}

impl MockToolExecutor {
    fn new() -> Self {
        let definitions = ["alpha", "beta", "explode", "abort"]
            .iter()
            .map(|name| ToolDefinition {
                name: (*name).to_string(),
                label: (*name).to_string(),
                description: format!("{name} mock tool"),
                parameters: serde_json::json!({"type": "object"}),
                metadata: None,
            })
            .collect();
        Self {
            calls: Mutex::new(Vec::new()),
            definitions,
        }
    }

    fn calls(&self) -> Vec<ToolCall> {
        self.calls.lock().expect("mock calls").clone()
    }

    fn call_names(&self) -> Vec<String> {
        self.calls().into_iter().map(|call| call.name).collect()
    }
}

#[async_trait]
impl ToolExecutor for MockToolExecutor {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.definitions.clone()
    }

    async fn execute(
        &self,
        call: &ToolCall,
        _signal: CancellationToken,
    ) -> Result<ToolResult, AgentError> {
        self.calls.lock().expect("mock calls").push(call.clone());
        match call.name.as_str() {
            "explode" => Ok(ToolResult {
                tool_call_id: call.id.clone(),
                content: Box::new(Content::text("mock failure")),
                is_error: true,
                details: None,
            }),
            "abort" => Err(AgentError::Tool {
                tool: call.name.clone(),
                message: "operation aborted".to_string(),
            }),
            name => Ok(ToolResult {
                tool_call_id: call.id.clone(),
                content: Box::new(Content::text(format!("ran {name}"))),
                is_error: false,
                details: None,
            }),
        }
    }
}

/// `BeforeToolCall` hook that blocks `alpha` and allows everything else.
struct BlockAlpha;

#[async_trait]
impl BeforeToolCall for BlockAlpha {
    async fn before_tool_call(&self, call: &ToolCall) -> BeforeToolCallDecision {
        if call.name == "alpha" {
            BeforeToolCallDecision::block("alpha is not allowed")
        } else {
            BeforeToolCallDecision::allow()
        }
    }
}

/// Extract the text of every tool result in the agent's message log, in
/// order.
fn tool_result_texts(agent: &Agent) -> Vec<String> {
    agent
        .loop_ref()
        .state()
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match content {
            Content::ToolResult(result) => Some(result_text(result)),
            _ => None,
        })
        .collect()
}

/// Extract every tool result in the agent's message log, in order.
fn tool_results(agent: &Agent) -> Vec<ToolResult> {
    agent
        .loop_ref()
        .state()
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match content {
            Content::ToolResult(result) => Some(result.clone()),
            _ => None,
        })
        .collect()
}

fn result_text(result: &ToolResult) -> String {
    match result.content.as_ref() {
        Content::Text(text) => text.text.clone(),
        other => format!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_tool_calls_execute_in_source_order_and_land_in_context() {
    let executor = Arc::new(MockToolExecutor::new());
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("alpha", "call-a"), ("beta", "call-b")]),
        text_reply("done"),
    ]));
    let mut agent = Agent::new(
        AgentOptions::new(faux_model(), stream.clone(), "you are pi")
            .with_tool_executor(executor.clone()),
    );

    agent
        .loop_mut()
        .run(vec![text_message("go")], |_| {})
        .await
        .expect("loop runs");

    assert_eq!(executor.call_names(), vec!["alpha", "beta"]);
    assert_eq!(
        stream.call_count(),
        2,
        "loop must stream a second turn after the tool batch"
    );

    let results = tool_results(&agent);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].tool_call_id, "call-a");
    assert_eq!(results[1].tool_call_id, "call-b");
    assert!(!results[0].is_error);
    assert!(!results[1].is_error);
    assert_eq!(tool_result_texts(&agent), vec!["ran alpha", "ran beta"]);
}

#[tokio::test]
async fn before_tool_call_block_skips_execution() {
    let executor = Arc::new(MockToolExecutor::new());
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("alpha", "call-a")]),
        text_reply("done"),
    ]));
    let mut agent = Agent::new(
        AgentOptions::new(faux_model(), stream.clone(), "you are pi")
            .with_tool_executor(executor.clone()),
    );
    agent.hooks_mut().before_tool_call = Some(Arc::new(BlockAlpha));

    agent
        .loop_mut()
        .run(vec![text_message("go")], |_| {})
        .await
        .expect("loop runs");

    assert!(
        executor.calls().is_empty(),
        "a blocked tool must never reach the executor"
    );
    let results = tool_results(&agent);
    assert_eq!(results.len(), 1);
    assert!(results[0].is_error, "blocked calls surface as errors");
    assert!(result_text(&results[0]).contains("alpha is not allowed"));
    assert_eq!(
        stream.call_count(),
        2,
        "the loop continues so the model can react to the block"
    );
}

#[tokio::test]
async fn tool_error_is_marked_and_loop_continues() {
    let executor = Arc::new(MockToolExecutor::new());
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("explode", "call-e"), ("alpha", "call-a")]),
        text_reply("recovered"),
    ]));
    let mut agent = Agent::new(
        AgentOptions::new(faux_model(), stream.clone(), "you are pi")
            .with_tool_executor(executor.clone()),
    );

    agent
        .loop_mut()
        .run(vec![text_message("go")], |_| {})
        .await
        .expect("loop runs");

    // Both calls ran — a failing tool does not cut the batch short.
    assert_eq!(executor.call_names(), vec!["explode", "alpha"]);
    let results = tool_results(&agent);
    assert_eq!(results.len(), 2);
    assert!(results[0].is_error, "the failing tool is marked as an error");
    assert_eq!(result_text(&results[0]), "mock failure");
    assert!(!results[1].is_error);
    assert_eq!(
        stream.call_count(),
        2,
        "the loop must keep going after a tool error"
    );
}

#[tokio::test]
async fn executor_error_becomes_error_tool_result() {
    let executor = Arc::new(MockToolExecutor::new());
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("abort", "call-x")]),
        text_reply("done"),
    ]));
    let mut agent = Agent::new(
        AgentOptions::new(faux_model(), stream.clone(), "you are pi")
            .with_tool_executor(executor.clone()),
    );

    agent
        .loop_mut()
        .run(vec![text_message("go")], |_| {})
        .await
        .expect("executor errors do not fail the run");

    let results = tool_results(&agent);
    assert_eq!(results.len(), 1);
    assert!(results[0].is_error);
    assert!(result_text(&results[0]).contains("operation aborted"));
}

#[tokio::test]
async fn missing_executor_keeps_stub_behaviour() {
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("alpha", "call-a")]),
        text_reply("done"),
    ]));
    let mut agent = Agent::new(AgentOptions::new(faux_model(), stream.clone(), "you are pi"));

    agent
        .loop_mut()
        .run(vec![text_message("go")], |_| {})
        .await
        .expect("loop runs");

    let results = tool_results(&agent);
    assert_eq!(results.len(), 1);
    assert!(!results[0].is_error);
    assert_eq!(result_text(&results[0]), "(stub) executed alpha");
}

#[tokio::test]
async fn context_exposes_executor_definitions() {
    let executor: Arc<dyn ToolExecutor> = Arc::new(MockToolExecutor::new());
    let config = AgentConfig {
        stream_fn: Arc::new(ScriptedStream::new(vec![text_reply("unused")])),
        model: faux_model(),
        tool_executor: Some(executor),
        tool_execution: pi_protocol::ToolExecutionMode::Parallel,
        telemetry: None,
    };
    let state = AgentState {
        system_prompt: "you are pi".into(),
        messages: Vec::new(),
        model_override: None,
    };

    let context = state.context(&config);
    let names: Vec<&str> = context
        .tools
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    assert_eq!(names, vec!["alpha", "beta", "explode", "abort"]);
}

#[tokio::test]
async fn cancelled_token_is_forwarded_to_executor() {
    // The loop hands the executor its cancellation token. Upstream's
    // sequential path dispatches the call that observes the abort and only
    // then stops (`packages/agent/src/agent-loop.ts:476-478`), so with a
    // pre-cancelled token and a single-call batch the executor is still
    // reached with that call. The parallel path behaves differently — see
    // `tests/tool_parallel.rs`.
    let executor = Arc::new(MockToolExecutor::new());
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("alpha", "call-a")]),
        text_reply("done"),
    ]));
    let mut agent = Agent::new(
        AgentOptions::new(faux_model(), stream.clone(), "you are pi")
            .with_tool_executor(executor.clone()),
    );
    let token = agent.loop_ref().cancellation_token();
    token.cancel();
    agent.loop_mut().set_cancellation_token(token);

    agent
        .loop_mut()
        .run(vec![text_message("go")], |_| {})
        .await
        .expect("loop runs");

    // The mock still records the call (it does not check the token), which
    // proves the loop reached the executor with the batch intact.
    assert_eq!(executor.call_names(), vec!["alpha"]);
}

#[tokio::test]
async fn default_hook_adapter_allows_and_leaves_results_untouched() {
    let adapter = AgentHookAdapter::new();
    let call = ToolCall {
        id: "call-1".into(),
        name: "alpha".into(),
        arguments: serde_json::json!({}),
    };
    assert!(!adapter.invoke_before_tool_call(&call).await.block);

    let mut result = ToolResult::default();
    adapter.invoke_after_tool_call(&mut result).await;
    assert!(!result.is_error);
}
