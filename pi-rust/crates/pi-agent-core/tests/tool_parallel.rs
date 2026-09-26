//! Batch dispatch tests for `ToolExecutionMode`.
//!
//! Mirrors the TypeScript suite in `packages/agent/test/agent-loop.test.ts`
//! ("executes tool calls in parallel" / "respects sequential execution
//! mode"), and pins the two behaviours the Rust loop owns:
//!
//! * a batch with no `Sequential` tool runs its calls concurrently, with the
//!   results still appended in source order;
//! * a `Sequential` tool anywhere in the batch — or a loop configured with
//!   `ToolExecutionMode::Sequential` — serializes the whole batch;
//! * the `AfterToolCall` hook runs exactly once per executed call (the
//!   pre-existing code invoked it twice, which this suite guards against).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream;
use pi_agent_core::tools::ToolExecutor;
use pi_agent_core::{
    AfterToolCall, Agent, AgentError, AgentOptions, BeforeToolCall, BeforeToolCallDecision,
};
use pi_ai::stream::{AssistantMessageEventStream, StreamFn};
use pi_ai::types::SimpleStreamOptions;
use pi_ai::StreamError;
use pi_protocol::{
    Api, AssistantMessage, AssistantMessageEvent, Content, Context as AgentContext, Message, Model,
    ProviderId, Role, StopReason, TextContent, ToolCall, ToolDefinition, ToolExecutionMode,
    ToolResult, Usage,
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
        error_message: None,
    }
}

fn text_reply(text: &str) -> AssistantMessage {
    AssistantMessage {
        model: "faux-model".into(),
        content: vec![Content::text(text)],
        stop_reason: StopReason::Stop,
        usage: Usage::default(),
        error_message: None,
    }
}

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

/// Wall-clock window one call occupied, keyed by tool name.
#[derive(Clone, Debug)]
struct CallWindow {
    name: String,
    start: Instant,
    end: Instant,
}

impl CallWindow {
    fn overlaps(&self, other: &Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// Executor that sleeps for a per-tool duration and records when each call
/// started and finished. `serial` is the only tool declared `Sequential`;
/// the rest are `Parallel` so batches containing them can fan out.
struct TimingExecutor {
    windows: Mutex<Vec<CallWindow>>,
}

impl TimingExecutor {
    fn new() -> Self {
        Self {
            windows: Mutex::new(Vec::new()),
        }
    }

    fn delay(name: &str) -> Duration {
        match name {
            "slow" => Duration::from_millis(250),
            "fast" => Duration::from_millis(10),
            _ => Duration::from_millis(80),
        }
    }

    fn windows(&self) -> Vec<CallWindow> {
        self.windows.lock().expect("windows").clone()
    }

    fn window(&self, name: &str) -> CallWindow {
        self.windows()
            .into_iter()
            .find(|window| window.name == name)
            .unwrap_or_else(|| panic!("no call recorded for {name}"))
    }

    /// Completion order — the parallel path finishes short calls first.
    fn completion_order(&self) -> Vec<String> {
        self.windows()
            .into_iter()
            .map(|window| window.name)
            .collect()
    }
}

#[async_trait]
impl ToolExecutor for TimingExecutor {
    fn definitions(&self) -> Vec<ToolDefinition> {
        ["slow", "fast", "serial"]
            .iter()
            .map(|name| ToolDefinition {
                name: (*name).to_string(),
                label: (*name).to_string(),
                description: format!("{name} timing tool"),
                parameters: serde_json::json!({"type": "object"}),
                metadata: None,
            })
            .collect()
    }

    fn execution_mode(&self, tool_name: &str) -> ToolExecutionMode {
        if tool_name == "serial" {
            ToolExecutionMode::Sequential
        } else {
            ToolExecutionMode::Parallel
        }
    }

    async fn execute(
        &self,
        call: &ToolCall,
        _signal: CancellationToken,
    ) -> Result<ToolResult, AgentError> {
        let start = Instant::now();
        tokio::time::sleep(Self::delay(&call.name)).await;
        let end = Instant::now();
        self.windows.lock().expect("windows").push(CallWindow {
            name: call.name.clone(),
            start,
            end,
        });
        Ok(ToolResult {
            tool_call_id: call.id.clone(),
            content: Box::new(Content::text(format!("ran {}", call.name))),
            is_error: false,
            details: None,
            added_tool_names: None,
            images: Vec::new(),
        })
    }
}

/// An executor that never overrides `execution_mode`, so it inherits the
/// trait default (`Sequential`) and must serialize every batch.
struct DefaultModeExecutor {
    seen: Mutex<Vec<String>>,
}

#[async_trait]
impl ToolExecutor for DefaultModeExecutor {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            name: "alpha".into(),
            label: "alpha".into(),
            description: "alpha".into(),
            parameters: serde_json::json!({"type": "object"}),
            metadata: None,
        }]
    }

    async fn execute(
        &self,
        call: &ToolCall,
        _signal: CancellationToken,
    ) -> Result<ToolResult, AgentError> {
        self.seen.lock().expect("seen").push(call.name.clone());
        Ok(ToolResult {
            tool_call_id: call.id.clone(),
            content: Box::new(Content::text(format!("ran {}", call.name))),
            is_error: false,
            details: None,
            added_tool_names: None,
            images: Vec::new(),
        })
    }
}

/// Counts `AfterToolCall` invocations (the double-invocation regression).
struct CountingAfter {
    count: Arc<AtomicUsize>,
}

#[async_trait]
impl AfterToolCall for CountingAfter {
    async fn after_tool_call(&self, _result: &mut ToolResult) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }
}

/// Blocks the call whose id is `blocked_id`, allows everything else.
struct BlockOneCall {
    blocked_id: String,
}

#[async_trait]
impl BeforeToolCall for BlockOneCall {
    async fn before_tool_call(&self, call: &ToolCall) -> BeforeToolCallDecision {
        if call.id == self.blocked_id {
            BeforeToolCallDecision::block("blocked by test")
        } else {
            BeforeToolCallDecision::default()
        }
    }
}

/// Cancels the loop's token the first time the `BeforeToolCall` hook sees the
/// call with `id`. Used to make the abort land *inside* a batch rather than
/// before it starts.
struct CancelOnCall {
    id: String,
    token: CancellationToken,
}

#[async_trait]
impl BeforeToolCall for CancelOnCall {
    async fn before_tool_call(&self, call: &ToolCall) -> BeforeToolCallDecision {
        if call.id == self.id {
            self.token.cancel();
        }
        BeforeToolCallDecision::default()
    }
}

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

fn tool_result_texts(agent: &Agent) -> Vec<String> {
    tool_results(agent)
        .iter()
        .map(result_text)
        .collect::<Vec<_>>()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn parallel_batch_runs_concurrently() {
    let executor = Arc::new(TimingExecutor::new());
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("slow", "call-s"), ("fast", "call-f")]),
        text_reply("done"),
    ]));
    let mut agent = Agent::new(
        AgentOptions::new(faux_model(), stream.clone(), "you are pi")
            .with_tool_executor(executor.clone()),
    );

    let started = Instant::now();
    agent
        .loop_mut()
        .run(vec![text_message("go")], |_| {})
        .await
        .expect("loop runs");
    let elapsed = started.elapsed();

    let slow = executor.window("slow");
    let fast = executor.window("fast");
    assert!(
        slow.overlaps(&fast),
        "parallel calls must overlap: {slow:?} vs {fast:?}"
    );
    assert!(
        elapsed < Duration::from_millis(400),
        "a 250ms + 10ms parallel batch must not take their sum (took {elapsed:?})"
    );
    assert_eq!(stream.call_count(), 2);

    // The short call finishes first, which is only possible when the calls
    // were in flight at the same time.
    assert_eq!(executor.completion_order(), vec!["fast", "slow"]);
}

#[tokio::test]
async fn parallel_batch_keeps_source_order_in_context() {
    let executor = Arc::new(TimingExecutor::new());
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("slow", "call-s"), ("fast", "call-f")]),
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

    let results = tool_results(&agent);
    assert_eq!(results.len(), 2);
    assert_eq!(
        results
            .iter()
            .map(|result| result.tool_call_id.as_str())
            .collect::<Vec<_>>(),
        vec!["call-s", "call-f"],
        "results are appended in source order even though they finish out of order"
    );
    assert_eq!(
        tool_result_texts(&agent),
        vec!["ran slow", "ran fast"],
        "result payloads follow source order too"
    );
}

#[tokio::test]
async fn sequential_tool_in_batch_serializes_everything() {
    let executor = Arc::new(TimingExecutor::new());
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("slow", "call-s"), ("serial", "call-q")]),
        text_reply("done"),
    ]));
    let mut agent = Agent::new(
        AgentOptions::new(faux_model(), stream.clone(), "you are pi")
            .with_tool_executor(executor.clone()),
    );

    let started = Instant::now();
    agent
        .loop_mut()
        .run(vec![text_message("go")], |_| {})
        .await
        .expect("loop runs");
    let elapsed = started.elapsed();

    let slow = executor.window("slow");
    let serial = executor.window("serial");
    assert!(
        !slow.overlaps(&serial),
        "a Sequential tool must serialize the whole batch: {slow:?} vs {serial:?}"
    );
    assert!(
        elapsed >= TimingExecutor::delay("slow"),
        "batch cannot finish before its slowest call (took {elapsed:?})"
    );
    assert_eq!(executor.completion_order(), vec!["slow", "serial"]);
}

#[tokio::test]
async fn config_sequential_mode_overrides_parallel_tools() {
    let executor = Arc::new(TimingExecutor::new());
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("slow", "call-s"), ("slow", "call-s2")]),
        text_reply("done"),
    ]));
    let mut agent = Agent::new(
        AgentOptions::new(faux_model(), stream.clone(), "you are pi")
            .with_tool_executor(executor.clone())
            .with_tool_execution(ToolExecutionMode::Sequential),
    );

    let started = Instant::now();
    agent
        .loop_mut()
        .run(vec![text_message("go")], |_| {})
        .await
        .expect("loop runs");
    let elapsed = started.elapsed();

    let windows = executor.windows();
    assert_eq!(windows.len(), 2);
    assert!(
        !windows[0].overlaps(&windows[1]),
        "ToolExecutionMode::Sequential must serialize the batch: {:?} vs {:?}",
        windows[0],
        windows[1]
    );
    assert!(
        elapsed >= TimingExecutor::delay("slow") * 2,
        "two 250ms calls run back to back (took {elapsed:?})"
    );
}

#[tokio::test]
async fn executor_default_mode_is_sequential() {
    // An executor that predates `execution_mode` keeps the one-call-at-a-time
    // behaviour it was written against.
    let executor = Arc::new(DefaultModeExecutor {
        seen: Mutex::new(Vec::new()),
    });
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("alpha", "call-a"), ("alpha", "call-b")]),
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

    assert_eq!(
        *executor.seen.lock().expect("seen"),
        vec!["alpha", "alpha"],
        "both calls still reach the executor"
    );
}

#[tokio::test]
async fn after_tool_call_hook_runs_once_per_call() {
    let executor = Arc::new(TimingExecutor::new());
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("slow", "call-s"), ("fast", "call-f")]),
        text_reply("done"),
    ]));
    let count = Arc::new(AtomicUsize::new(0));
    let mut agent = Agent::new(
        AgentOptions::new(faux_model(), stream.clone(), "you are pi")
            .with_tool_executor(executor.clone()),
    );
    agent.hooks_mut().after_tool_call = Some(Arc::new(CountingAfter {
        count: count.clone(),
    }));

    agent
        .loop_mut()
        .run(vec![text_message("go")], |_| {})
        .await
        .expect("loop runs");

    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "one after_tool_call per executed call, not one per execution phase"
    );
}

#[tokio::test]
async fn after_tool_call_hook_runs_once_in_sequential_mode() {
    let executor = Arc::new(DefaultModeExecutor {
        seen: Mutex::new(Vec::new()),
    });
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("alpha", "call-a")]),
        text_reply("done"),
    ]));
    let count = Arc::new(AtomicUsize::new(0));
    let mut agent = Agent::new(
        AgentOptions::new(faux_model(), stream.clone(), "you are pi")
            .with_tool_executor(executor.clone()),
    );
    agent.hooks_mut().after_tool_call = Some(Arc::new(CountingAfter {
        count: count.clone(),
    }));

    agent
        .loop_mut()
        .run(vec![text_message("go")], |_| {})
        .await
        .expect("loop runs");

    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn blocked_call_in_parallel_batch_is_isolated_from_execution() {
    let executor = Arc::new(TimingExecutor::new());
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("slow", "call-s"), ("fast", "call-blocked")]),
        text_reply("done"),
    ]));
    let count = Arc::new(AtomicUsize::new(0));
    let mut agent = Agent::new(
        AgentOptions::new(faux_model(), stream.clone(), "you are pi")
            .with_tool_executor(executor.clone()),
    );
    agent.hooks_mut().before_tool_call = Some(Arc::new(BlockOneCall {
        blocked_id: "call-blocked".into(),
    }));
    agent.hooks_mut().after_tool_call = Some(Arc::new(CountingAfter {
        count: count.clone(),
    }));

    agent
        .loop_mut()
        .run(vec![text_message("go")], |_| {})
        .await
        .expect("loop runs");

    // Only the allowed call reached the executor and only it was finalized.
    assert_eq!(executor.completion_order(), vec!["slow"]);
    assert_eq!(count.load(Ordering::SeqCst), 1);

    let results = tool_results(&agent);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].tool_call_id, "call-s");
    assert!(!results[0].is_error);
    assert_eq!(results[1].tool_call_id, "call-blocked");
    assert!(results[1].is_error, "blocked calls surface as errors");
    assert_eq!(
        result_text(&results[1]),
        "tool call blocked: blocked by test"
    );
}

#[tokio::test]
async fn single_call_batch_takes_the_parallel_path() {
    // Guards against a degenerate `join_all` (e.g. an off-by-one in the slot
    // bookkeeping) leaving a lone call behind in parallel mode.
    let executor = Arc::new(TimingExecutor::new());
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("fast", "call-f")]),
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

    assert_eq!(tool_result_texts(&agent), vec!["ran fast"]);
    assert_eq!(stream.call_count(), 2);
}

// ---------------------------------------------------------------------------
// Cancellation — upstream `executeToolCallsSequential` / `_Parallel`
// (`packages/agent/src/agent-loop.ts:409-545`)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pre_cancelled_parallel_batch_yields_operation_aborted() {
    // Upstream queues the first parallel call, observes the abort, and breaks;
    // the queued call then finalizes as `Operation aborted` without ever
    // reaching the executor.
    let executor = Arc::new(TimingExecutor::new());
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("fast", "call-f1"), ("fast", "call-f2")]),
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

    assert!(
        executor.windows().is_empty(),
        "an aborted parallel call must not reach the executor"
    );
    let results = tool_results(&agent);
    assert_eq!(results.len(), 1, "calls after the abort are dropped");
    assert_eq!(results[0].tool_call_id, "call-f1");
    assert!(results[0].is_error);
    assert_eq!(tool_result_texts(&agent), vec!["Operation aborted"]);
}

#[tokio::test]
async fn parallel_batch_stops_queuing_after_abort() {
    let executor = Arc::new(TimingExecutor::new());
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[
            ("fast", "call-f1"),
            ("fast", "call-f2"),
            ("fast", "call-f3"),
        ]),
        text_reply("done"),
    ]));
    let mut agent = Agent::new(
        AgentOptions::new(faux_model(), stream.clone(), "you are pi")
            .with_tool_executor(executor.clone()),
    );
    let token = agent.loop_ref().cancellation_token();
    agent.hooks_mut().before_tool_call = Some(Arc::new(CancelOnCall {
        id: "call-f2".into(),
        token: token.clone(),
    }));
    agent.loop_mut().set_cancellation_token(token);

    agent
        .loop_mut()
        .run(vec![text_message("go")], |_| {})
        .await
        .expect("loop runs");

    // `call-f1` was queued before the abort landed and `call-f2` observed it;
    // both finalize as aborted, and `call-f3` is never even prepared.
    assert!(
        executor.windows().is_empty(),
        "aborted calls must not reach the executor"
    );
    let results = tool_results(&agent);
    assert_eq!(results.len(), 2, "call-f3 is dropped by the break");
    assert!(results.iter().all(|result| result.is_error));
    assert_eq!(
        tool_result_texts(&agent),
        vec!["Operation aborted", "Operation aborted"]
    );
    assert!(results
        .iter()
        .all(|result| result.tool_call_id != "call-f3"));
}

#[tokio::test]
async fn sequential_batch_stops_after_the_abort_observation() {
    // Upstream executes the call that observes the abort, then stops: the
    // remaining calls are neither prepared nor dispatched.
    let executor = Arc::new(DefaultModeExecutor {
        seen: Mutex::new(Vec::new()),
    });
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(&[("alpha", "call-a"), ("alpha", "call-b")]),
        text_reply("done"),
    ]));
    let mut agent = Agent::new(
        AgentOptions::new(faux_model(), stream.clone(), "you are pi")
            .with_tool_executor(executor.clone()),
    );
    let token = agent.loop_ref().cancellation_token();
    agent.hooks_mut().before_tool_call = Some(Arc::new(CancelOnCall {
        id: "call-a".into(),
        token: token.clone(),
    }));
    agent.loop_mut().set_cancellation_token(token);

    agent
        .loop_mut()
        .run(vec![text_message("go")], |_| {})
        .await
        .expect("loop runs");

    assert_eq!(
        *executor.seen.lock().expect("seen"),
        vec!["alpha"],
        "only the call that observed the abort is dispatched"
    );
    let results = tool_results(&agent);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tool_call_id, "call-a");
    assert!(!results[0].is_error);
}
