//! Regression tests for the `should_stop_after_turn` and
//! `prepare_next_turn` hooks (LUM-995).
//!
//! These tests exercise [`AgentHookAdapter`] end-to-end through
//! `AgentLoop::run` to make sure the user-facing callbacks actually fire
//! and that their return values flow back into the loop. They use a
//! scripted stream so the loop drives the hooks deterministically
//! without depending on the real provider adapters.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use async_trait::async_trait;
use futures::stream;
use pi_agent_core::{
    Agent, AgentHookAdapter, AgentLoopTurnUpdate, AgentOptions, PrepareHookFn,
    PrepareNextTurnContext, ShouldStopAfterTurnContext, ShouldStopHookFn,
};
use pi_ai::stream::{AssistantMessageEventStream, StreamFn};
use pi_ai::types::SimpleStreamOptions;
use pi_ai::StreamError;
use pi_protocol::{
    AssistantMessage, AssistantMessageEvent, Content, Context as AgentContext, Message, Model,
    ProviderId, Role, StopReason, TextContent, ToolCall, Usage,
};

fn faux_model(id: &str) -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: id.into(),
        api: pi_protocol::Api::Faux,
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

fn tool_call_message(tool_name: &str, tool_call_id: &str) -> AssistantMessage {
    AssistantMessage {
        model: "faux-model".into(),
        content: vec![Content::ToolCall(ToolCall {
            id: tool_call_id.into(),
            name: tool_name.into(),
            arguments: serde_json::json!({}),
        })],
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

/// Scripted stream — returns the next `AssistantMessage` on every call and
/// records how many times the loop invoked it.
struct ScriptedStream {
    responses: Mutex<Vec<AssistantMessage>>,
    calls: AtomicUsize,
}

impl ScriptedStream {
    fn new(responses: Vec<AssistantMessage>) -> Self {
        Self {
            responses: Mutex::new(responses),
            calls: AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
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
        let message = responses.remove(0);
        self.calls.fetch_add(1, Ordering::SeqCst);
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

fn make_should_stop_returning_counting(counter: Arc<AtomicUsize>, value: bool) -> ShouldStopHookFn {
    Arc::new(move |_ctx: ShouldStopAfterTurnContext| {
        let counter = counter.clone();
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            value
        })
    })
}

fn make_prepare_next_turn_recording(counter: Arc<AtomicUsize>) -> PrepareHookFn {
    Arc::new(move |ctx: PrepareNextTurnContext| {
        let counter = counter.clone();
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            assert!(
                !ctx.new_messages.is_empty(),
                "hook should observe at least one new message from the previous turn"
            );
            assert!(
                ctx.context
                    .messages
                    .iter()
                    .any(|m| matches!(m.role, Role::User)),
                "context passed to hook should include the user prompt"
            );
            None
        })
    })
}

fn make_prepare_next_turn_that_swaps(
    counter: Arc<AtomicUsize>,
    injected_marker: &'static str,
    swap_to_model: &'static str,
) -> PrepareHookFn {
    Arc::new(move |ctx: PrepareNextTurnContext| {
        let counter = counter.clone();
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut next_context = ctx.context.clone();
            next_context.messages.push(Message {
                role: Role::Assistant,
                content: vec![Content::text(injected_marker)],
                model: None,
            });
            Some(AgentLoopTurnUpdate {
                context: Some(next_context),
                model: Some(faux_model(swap_to_model)),
                thinking_level: None,
            })
        })
    })
}

#[tokio::test]
async fn should_stop_after_turn_returns_true_terminates_agent() {
    // The stream produces two replies — the loop must stop after the
    // first one because `should_stop_after_turn` returns true.
    let stream = Arc::new(ScriptedStream::new(vec![
        text_reply("first turn"),
        text_reply("second turn — should never run"),
    ]));

    let hook_calls = Arc::new(AtomicUsize::new(0));
    let should_stop = make_should_stop_returning_counting(hook_calls.clone(), true);

    let mut agent = Agent::new(
        AgentOptions::new(faux_model("faux-model"), stream.clone(), "you are pi")
            .with_should_stop_after_turn(should_stop),
    );

    let outcome = agent
        .loop_mut()
        .run(vec![text_message("hello")], |_turn| {})
        .await
        .expect("loop runs");

    // The loop only streamed the first reply — the second is still
    // queued in the script and `stream_simple` was never invoked for it.
    assert_eq!(stream.call_count(), 1, "loop must stop after first turn");
    assert_eq!(
        hook_calls.load(Ordering::SeqCst),
        1,
        "should_stop_after_turn fires exactly once"
    );
    assert_eq!(outcome.message.stop_reason, StopReason::Stop);
    assert_eq!(outcome.message.content, vec![Content::text("first turn")]);
    assert!(!outcome.tool_executed, "no tool calls in the script");
}

#[tokio::test]
async fn prepare_next_turn_can_swap_context_and_model() {
    // First reply contains a tool call so the loop continues into a
    // second turn. After the second turn completes, the script returns
    // a `Stop` reply and there are no follow-ups queued.
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message("noop", "tool-1"),
        text_reply("done"),
    ]));

    let hook_calls = Arc::new(AtomicUsize::new(0));
    let prepare_next_turn = make_prepare_next_turn_that_swaps(
        hook_calls.clone(),
        "(injected by prepare_next_turn)",
        "faux-model-v2",
    );

    let mut agent = Agent::new(
        AgentOptions::new(faux_model("faux-model"), stream.clone(), "you are pi")
            .with_prepare_next_turn(prepare_next_turn),
    );

    // Track which turns observed tool calls via the per-turn callback.
    // The final `outcome` returned by `run` is the *last* turn, so we
    // can't read tool execution off of it directly.
    let turns = Arc::new(Mutex::new(Vec::<bool>::new()));
    let turns_for_cb = turns.clone();
    let outcome = agent
        .loop_mut()
        .run(vec![text_message("hello")], move |turn| {
            turns_for_cb
                .lock()
                .expect("turns lock")
                .push(turn.tool_executed);
        })
        .await
        .expect("loop runs");

    // The loop must have called the stream twice — once for the
    // tool-use turn and once for the text reply — proving
    // `prepare_next_turn` ran in between.
    assert_eq!(
        stream.call_count(),
        2,
        "loop must enter turn 2 after the tool call"
    );
    assert_eq!(
        hook_calls.load(Ordering::SeqCst),
        1,
        "prepare_next_turn fires exactly once before turn 2"
    );
    let observed_tool_exec = turns.lock().expect("turns lock").clone();
    assert_eq!(
        observed_tool_exec,
        vec![true, false],
        "turn 1 executed a tool, turn 2 was a clean stop"
    );
    assert!(
        !outcome.tool_executed,
        "the final outcome is turn 2 which had no tool calls"
    );

    // The hook spliced its marker into the live context, so the
    // post-run state must contain it. This is the observable
    // side-effect the issue calls out.
    let state = agent.loop_ref().state();
    let injected = state.messages.iter().any(|m| {
        matches!(&m.content[0], Content::Text(t) if t.text == "(injected by prepare_next_turn)")
    });
    assert!(
        injected,
        "the prepare_next_turn hook must splice its marker into the live state"
    );
}

#[tokio::test]
async fn prepare_next_turn_is_not_called_before_first_turn() {
    let stream = Arc::new(ScriptedStream::new(vec![text_reply("hello")]));

    let hook_calls = Arc::new(AtomicUsize::new(0));
    let prepare_next_turn = make_prepare_next_turn_recording(hook_calls.clone());

    let mut agent = Agent::new(
        AgentOptions::new(faux_model("faux-model"), stream.clone(), "you are pi")
            .with_prepare_next_turn(prepare_next_turn),
    );

    agent
        .loop_mut()
        .run(vec![text_message("hello")], |_turn| {})
        .await
        .expect("loop runs");

    assert_eq!(
        hook_calls.load(Ordering::SeqCst),
        0,
        "prepare_next_turn only runs before turn 2+"
    );
    assert_eq!(stream.call_count(), 1);
}

#[tokio::test]
async fn should_stop_after_turn_default_is_false_when_unregistered() {
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message("noop", "tool-1"),
        text_reply("ok"),
    ]));

    let hook_calls = Arc::new(AtomicUsize::new(0));
    let should_stop = make_should_stop_returning_counting(hook_calls.clone(), false);

    let mut agent = Agent::new(
        AgentOptions::new(faux_model("faux-model"), stream.clone(), "you are pi")
            .with_should_stop_after_turn(should_stop),
    );

    agent
        .loop_mut()
        .run(vec![text_message("hello")], |_turn| {})
        .await
        .expect("loop runs");

    // should_stop fires once per completed turn. The first turn is a
    // tool call (so the loop continues); the second is a clean stop.
    assert_eq!(
        hook_calls.load(Ordering::SeqCst),
        2,
        "should_stop fires after each completed turn"
    );
    assert_eq!(
        stream.call_count(),
        2,
        "loop continues into turn 2 when should_stop returns false"
    );
}

#[tokio::test]
async fn hook_adapter_default_decisions_are_noops() {
    // Calling the adapter without registering a hook must return the
    // loop's defaults — `false` for `should_stop`, `None` for
    // `prepare_next` — without panicking or hanging.
    let adapter = AgentHookAdapter::new();

    let ctx = ShouldStopAfterTurnContext {
        message: text_reply("hello"),
        tool_results: Vec::new(),
        context: AgentContext::new("you are pi"),
        new_messages: vec![text_message("hello")],
    };
    assert!(!adapter.invoke_should_stop(ctx).await);

    let ctx = PrepareNextTurnContext {
        message: text_reply("hello"),
        tool_results: Vec::new(),
        context: AgentContext::new("you are pi"),
        new_messages: vec![text_message("hello")],
    };
    assert!(adapter.invoke_prepare_next_turn(ctx).await.is_none());
}

#[tokio::test]
async fn should_stop_after_turn_fires_with_owned_context() {
    // Regression test for the HRTB lifetime issue: the hook receives an
    // owned `ShouldStopAfterTurnContext` that survives the loop's
    // borrow scope. If the adapter accidentally returned a future that
    // borrowed from the loop, this test would fail to compile or
    // trigger lifetime errors when the loop drops its borrow before
    // awaiting.
    //
    // The hook captures every `ShouldStopAfterTurnContext` it sees
    // (the loop calls it once per completed turn) so we can verify the
    // owned-data contract against every fired context, not just the
    // last one.
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message("noop", "tool-1"),
        text_reply("after tool"),
    ]));

    let captured: Arc<Mutex<Vec<ShouldStopAfterTurnContext>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_for_hook = captured.clone();
    let should_stop: ShouldStopHookFn = Arc::new(move |ctx: ShouldStopAfterTurnContext| {
        let captured = captured_for_hook.clone();
        Box::pin(async move {
            captured.lock().expect("captured lock").push(ctx);
            false
        })
    });

    let mut agent = Agent::new(
        AgentOptions::new(faux_model("faux-model"), stream.clone(), "you are pi")
            .with_should_stop_after_turn(should_stop),
    );

    agent
        .loop_mut()
        .run(vec![text_message("hello")], |_turn| {})
        .await
        .expect("loop runs");

    let contexts = captured.lock().expect("captured lock").clone();
    assert_eq!(
        contexts.len(),
        2,
        "should_stop fires once per completed turn"
    );
    // First turn was a tool-use, second was a clean stop.
    assert!(matches!(
        contexts[0].message.stop_reason,
        StopReason::ToolUse
    ));
    assert!(matches!(contexts[1].message.stop_reason, StopReason::Stop));
    for ctx in &contexts {
        assert!(
            ctx.context
                .messages
                .iter()
                .any(|m| matches!(m.role, Role::User)),
            "every should_stop context still contains the user prompt"
        );
        assert!(
            ctx.new_messages
                .iter()
                .any(|m| matches!(m.role, Role::User)),
            "new_messages starts with the user prompt"
        );
        // The just-completed turn's assistant message is in
        // new_messages (turn 2 sees both turn 1 and turn 2 assistant
        // messages, since new_messages accumulates across the run).
        let turn_assistant_count = ctx
            .new_messages
            .iter()
            .filter(|m| matches!(m.role, Role::Assistant))
            .count();
        assert!(
            turn_assistant_count >= 1,
            "new_messages includes at least one assistant message"
        );
    }
}

// ---------------------------------------------------------------------------
// LifecycleHooks (LUM-1432) — the async seam for `context` /
// `before_agent_start` / `agent_settled` / the provider boundary.
// ---------------------------------------------------------------------------

/// Records every lifecycle call and rewrites the two request/response ones.
#[derive(Default)]
struct RecordingLifecycle {
    before_agent_start: Mutex<Vec<(String, String)>>,
    context: Mutex<Vec<usize>>,
    provider_requests: Mutex<Vec<serde_json::Value>>,
    provider_headers: AtomicUsize,
    provider_responses: Mutex<Vec<bool>>,
    settled: AtomicUsize,
}

#[async_trait]
impl pi_agent_core::LifecycleHooks for RecordingLifecycle {
    async fn before_agent_start(&self, prompt: &str, system_prompt: &str) -> Option<String> {
        self.before_agent_start
            .lock()
            .expect("lock")
            .push((prompt.to_string(), system_prompt.to_string()));
        Some(format!("overridden::{system_prompt}"))
    }

    async fn context(&self, messages: Vec<Message>) -> Vec<Message> {
        self.context.lock().expect("lock").push(messages.len());
        let mut messages = messages;
        messages.push(text_message("CONTEXT-INJECTED"));
        messages
    }

    async fn agent_settled(&self) {
        self.settled.fetch_add(1, Ordering::SeqCst);
    }

    async fn before_provider_request(&self, payload: serde_json::Value) -> serde_json::Value {
        self.provider_requests.lock().expect("lock").push(payload);
        serde_json::Value::Null
    }

    async fn before_provider_headers(
        &self,
        headers: serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        self.provider_headers.fetch_add(1, Ordering::SeqCst);
        headers
    }

    async fn after_provider_response(&self, ok: bool) {
        self.provider_responses.lock().expect("lock").push(ok);
    }
}

/// Stream that records the context it was handed, so the test can see the
/// `context` rewrite the loop applied.
struct ContextCapturingStream {
    inner: FauxishStream,
    seen: Arc<Mutex<Vec<AgentContext>>>,
}

#[async_trait]
impl StreamFn for ContextCapturingStream {
    async fn stream_simple(
        &self,
        model: &Model,
        ctx: &AgentContext,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        self.seen.lock().expect("lock").push(ctx.clone());
        self.inner.stream_simple(model, ctx, options).await
    }
}

/// Minimal inner stream: one text reply per call.
struct FauxishStream;

#[async_trait]
impl StreamFn for FauxishStream {
    async fn stream_simple(
        &self,
        _model: &Model,
        _ctx: &AgentContext,
        _options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        let message = text_reply("done");
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

#[tokio::test(flavor = "current_thread")]
async fn lifecycle_hooks_fire_at_the_run_boundaries() {
    let lifecycle = Arc::new(RecordingLifecycle::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let stream = Arc::new(ContextCapturingStream {
        inner: FauxishStream,
        seen: seen.clone(),
    });

    let mut agent = Agent::new(
        AgentOptions::new(faux_model("faux-model"), stream, "you are pi")
            .with_lifecycle(lifecycle.clone()),
    );
    agent.prompt("hello").await.expect("prompt");

    // `before_agent_start` saw the expanded prompt and the base system prompt,
    // and its override became the run's system prompt.
    let starts = lifecycle.before_agent_start.lock().expect("lock").clone();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].0, "hello");
    assert_eq!(starts[0].1, "you are pi");

    // `context` ran before the provider call and its rewrite is what the
    // provider saw.
    assert_eq!(lifecycle.context.lock().expect("lock").len(), 1);
    let contexts = seen.lock().expect("lock").clone();
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].system_prompt, "overridden::you are pi");
    assert!(
        contexts[0]
            .messages
            .iter()
            .any(|message| message.content.iter().any(
                |block| matches!(block, Content::Text(text) if text.text == "CONTEXT-INJECTED")
            )),
        "the context rewrite must reach the provider: {:?}",
        contexts[0].messages
    );

    // The provider-boundary hooks saw the request the loop was about to make
    // and the response edge.
    let requests = lifecycle.provider_requests.lock().expect("lock").clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["model"]["id"], "faux-model");
    assert_eq!(lifecycle.provider_headers.load(Ordering::SeqCst), 1);
    assert_eq!(
        lifecycle.provider_responses.lock().expect("lock").clone(),
        vec![true]
    );

    // `agent_settled` closed the run.
    assert_eq!(lifecycle.settled.load(Ordering::SeqCst), 1);
}

/// A host with no lifecycle hook behaves exactly as before: every seam is a
/// no-op and the run still completes.
#[tokio::test(flavor = "current_thread")]
async fn runs_without_lifecycle_hooks_are_unchanged() {
    let stream = Arc::new(FauxishStream);
    let mut agent = Agent::new(AgentOptions::new(
        faux_model("faux-model"),
        stream,
        "you are pi",
    ));
    agent.prompt("hello").await.expect("prompt");
    assert_eq!(agent.state().system_prompt, "you are pi");
}
