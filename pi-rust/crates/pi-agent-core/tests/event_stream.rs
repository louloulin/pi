//! Stage 40 — the agent event stream is emitted at the real execution points.
//!
//! Before this round the whole `AgentEvent` union was synthesized *after* a
//! turn finished (`fan_turn_to_subscribers` in `agent.rs`): `ToolExecutionStart`
//! / `ToolExecutionEnd` were back-filled with an empty tool name and a
//! duration that covered the whole turn, and every `MessageUpdate` was a
//! replay of the final message rather than a streaming delta. These tests pin
//! the new contract:
//!
//! * `ToolExecutionStart` carries the real [`ToolCall`] and precedes the
//!   matching `ToolExecutionEnd`;
//! * `ToolExecutionEnd::duration_ms` measures the single call, not the turn;
//! * provider deltas are forwarded one by one, before `MessageEnd`;
//! * a concurrent batch emits every start before any of its ends;
//! * a failed provider request truncates the sequence (no `MessageEnd` /
//!   `TurnEnd`), which consumers must tolerate.
//!
//! The event order is observed through the public `Agent::subscribe` channel —
//! the same feed the TUI, the RPC pump and the WASM bridge consume — so these
//! tests also cover the observer wiring in `Agent::prompt`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream;
use pi_agent_core::tools::ToolExecutor;
use pi_agent_core::{Agent, AgentError, AgentEvent, AgentOptions, AssistantMessageUpdate};
use pi_ai::stream::{AssistantMessageEventStream, StreamFn};
use pi_ai::types::SimpleStreamOptions;
use pi_ai::StreamError;
use pi_protocol::{
    Api, AssistantMessage, AssistantMessageEvent, Content, Context as AgentContext, Model,
    ProviderId, StopReason, ToolCall, ToolDefinition, ToolExecutionMode, ToolResult, Usage,
};
use tokio::sync::mpsc::UnboundedReceiver;
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

/// A scripted provider stream: one `(pre-delay, events)` entry per
/// `stream_simple` call. Exhausting the script is an error, which keeps a
/// test from silently dropping an unexpected extra turn.
struct ScriptedEvents {
    scripts: Mutex<Vec<(Duration, Vec<AssistantMessageEvent>)>>,
}

impl ScriptedEvents {
    fn new(scripts: Vec<(Duration, Vec<AssistantMessageEvent>)>) -> Self {
        Self {
            scripts: Mutex::new(scripts),
        }
    }
}

#[async_trait]
impl StreamFn for ScriptedEvents {
    async fn stream_simple(
        &self,
        _model: &Model,
        _ctx: &AgentContext,
        _options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        let script = {
            let mut scripts = self.scripts.lock().expect("scripted events");
            if scripts.is_empty() {
                return Err(StreamError::Malformed("scripted stream exhausted".into()));
            }
            scripts.remove(0)
        };
        let (delay, events) = script;
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let events: Vec<_> = events.into_iter().map(Ok).collect();
        Ok(Box::pin(stream::iter(events)))
    }
}

/// Streams a final text answer with no tool calls — the turn that lets the
/// loop exit.
fn text_reply(text: &str) -> (Duration, Vec<AssistantMessageEvent>) {
    let message = AssistantMessage {
        model: "faux-model".into(),
        content: vec![Content::text(text)],
        stop_reason: StopReason::Stop,
        usage: Usage::default(),
    };
    (
        Duration::ZERO,
        vec![
            AssistantMessageEvent::Start {
                model: message.model.clone(),
            },
            AssistantMessageEvent::TextDelta {
                delta: text.to_string(),
            },
            AssistantMessageEvent::Done {
                content: message.content.clone(),
                stop_reason: message.stop_reason,
                usage: message.usage,
            },
        ],
    )
}

/// Streams an assistant message that calls the given tools, including the
/// streaming `ToolCallDelta`s a real provider emits before `Done`.
fn tool_call_script(
    calls: &[(&str, &str)],
    delay: Duration,
) -> (Duration, Vec<AssistantMessageEvent>) {
    let mut events = vec![AssistantMessageEvent::Start {
        model: "faux-model".into(),
    }];
    let mut content = Vec::with_capacity(calls.len());
    for (index, (name, id)) in calls.iter().enumerate() {
        events.push(AssistantMessageEvent::ToolCallDelta {
            index: index as u32,
            id: Some((*id).into()),
            name: Some((*name).into()),
            arguments_delta: Some("{}".into()),
        });
        content.push(Content::ToolCall(ToolCall {
            id: (*id).into(),
            name: (*name).into(),
            arguments: serde_json::json!({}),
        }));
    }
    events.push(AssistantMessageEvent::Done {
        content: content.clone(),
        stop_reason: StopReason::ToolUse,
        usage: Usage::default(),
    });
    (delay, events)
}

/// Executor that sleeps for a fixed duration on every call. Both tools are
/// declared `Parallel` so a two-call batch fans out.
struct SleepyExecutor {
    delay: Duration,
}

#[async_trait]
impl ToolExecutor for SleepyExecutor {
    fn definitions(&self) -> Vec<ToolDefinition> {
        ["alpha", "beta"]
            .iter()
            .map(|name| ToolDefinition {
                name: (*name).to_string(),
                label: (*name).to_string(),
                description: format!("{name} test tool"),
                parameters: serde_json::json!({"type": "object"}),
                metadata: None,
            })
            .collect()
    }

    fn execution_mode(&self, _tool_name: &str) -> ToolExecutionMode {
        ToolExecutionMode::Parallel
    }

    async fn execute(
        &self,
        call: &ToolCall,
        _signal: CancellationToken,
    ) -> Result<ToolResult, AgentError> {
        tokio::time::sleep(self.delay).await;
        Ok(ToolResult {
            tool_call_id: call.id.clone(),
            content: Box::new(Content::text(format!("ran {}", call.name))),
            is_error: false,
            details: None,
        })
    }
}

/// Draining helper — the subscriber channel is unbounded, so everything the
/// loop emitted is available once `prompt` returned.
fn drain(rx: &mut UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

fn position(events: &[AgentEvent], predicate: impl Fn(&AgentEvent) -> bool) -> usize {
    events
        .iter()
        .position(predicate)
        .unwrap_or_else(|| panic!("event not found in sequence: {events:#?}"))
}

fn tool_start_names(events: &[AgentEvent]) -> Vec<(usize, String, String)> {
    events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            AgentEvent::ToolExecutionStart { call } => {
                Some((index, call.id.clone(), call.name.clone()))
            }
            _ => None,
        })
        .collect()
}

fn tool_end_indices(events: &[AgentEvent]) -> Vec<(usize, String)> {
    events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            AgentEvent::ToolExecutionEnd { result, .. } => {
                Some((index, result.tool_call_id.clone()))
            }
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tool_execution_start_precedes_end_and_carries_the_real_call() {
    let stream = Arc::new(ScriptedEvents::new(vec![
        tool_call_script(&[("alpha", "call-1")], Duration::ZERO),
        text_reply("done"),
    ]));
    let mut agent = Agent::new(
        AgentOptions::new(faux_model(), stream, "you are pi").with_tool_executor(Arc::new(
            SleepyExecutor {
                delay: Duration::ZERO,
            },
        )),
    );
    let mut rx = agent.subscribe();
    agent.prompt("go").await.expect("prompt succeeds");
    let events = drain(&mut rx);

    let starts = tool_start_names(&events);
    assert_eq!(
        starts,
        vec![(
            position(&events, |event| matches!(
                event,
                AgentEvent::ToolExecutionStart { call } if call.id == "call-1"
            )),
            "call-1".to_string(),
            "alpha".to_string(),
        )],
        "exactly one ToolExecutionStart with the real id and name"
    );

    let start = starts[0].0;
    let end = tool_end_indices(&events)
        .into_iter()
        .find(|(_, id)| id == "call-1")
        .expect("matching ToolExecutionEnd")
        .0;
    assert!(
        start < end,
        "ToolExecutionStart must precede its ToolExecutionEnd: {events:#?}"
    );

    // The turn brackets the message, which brackets the tool.
    let turn_start = position(&events, |event| matches!(event, AgentEvent::TurnStart));
    let message_start = position(&events, |event| {
        matches!(event, AgentEvent::MessageStart { .. })
    });
    let message_end = position(&events, |event| {
        matches!(event, AgentEvent::MessageEnd { .. })
    });
    let turn_end = position(&events, |event| matches!(event, AgentEvent::TurnEnd { .. }));
    assert!(turn_start < message_start);
    assert!(message_end < start);
    assert!(end < turn_end);
}

#[tokio::test]
async fn tool_execution_end_duration_is_per_call_not_per_turn() {
    // The provider stalls 300ms before answering; the tool itself sleeps
    // 100ms. The old back-fill reported "time since the turn started" for
    // every tool (~400ms here); the real per-call duration is ~100ms.
    let stream = Arc::new(ScriptedEvents::new(vec![
        tool_call_script(&[("alpha", "call-1")], Duration::from_millis(300)),
        text_reply("done"),
    ]));
    let mut agent = Agent::new(
        AgentOptions::new(faux_model(), stream, "you are pi").with_tool_executor(Arc::new(
            SleepyExecutor {
                delay: Duration::from_millis(100),
            },
        )),
    );
    let mut rx = agent.subscribe();
    let turn_started = Instant::now();
    agent.prompt("go").await.expect("prompt succeeds");
    let turn_elapsed_ms = turn_started.elapsed().as_millis() as u64;
    let events = drain(&mut rx);

    let duration_ms = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolExecutionEnd {
                result,
                duration_ms,
            } if result.tool_call_id == "call-1" => Some(*duration_ms),
            _ => None,
        })
        .expect("ToolExecutionEnd for call-1");

    assert!(
        duration_ms >= 100,
        "a 100ms tool must report at least 100ms (got {duration_ms})"
    );
    assert!(
        duration_ms < 250,
        "duration_ms must measure the call, not the ~400ms turn (got {duration_ms})"
    );
    assert!(
        turn_elapsed_ms >= 380,
        "the turn (300ms provider + 100ms tool) must take at least 380ms (took {turn_elapsed_ms})"
    );
    assert!(
        duration_ms * 2 < turn_elapsed_ms,
        "the per-call duration ({duration_ms}ms) must be far below the turn ({turn_elapsed_ms}ms)"
    );
}

#[tokio::test]
async fn text_deltas_are_forwarded_before_message_end() {
    // The provider streams two fragments; the final message collapses them.
    // The old implementation replayed that single final block as one
    // synthesized delta — the two `MessageUpdate`s below only exist when the
    // deltas are forwarded at the moment they arrive.
    let done = AssistantMessage {
        model: "faux-model".into(),
        content: vec![Content::text("hello world")],
        stop_reason: StopReason::Stop,
        usage: Usage::default(),
    };
    let stream = Arc::new(ScriptedEvents::new(vec![(
        Duration::ZERO,
        vec![
            AssistantMessageEvent::Start {
                model: done.model.clone(),
            },
            AssistantMessageEvent::TextDelta {
                delta: "hello ".into(),
            },
            AssistantMessageEvent::TextDelta {
                delta: "world".into(),
            },
            AssistantMessageEvent::Done {
                content: done.content.clone(),
                stop_reason: done.stop_reason,
                usage: done.usage,
            },
        ],
    )]));
    let mut agent = Agent::new(AgentOptions::new(faux_model(), stream, "you are pi"));
    let mut rx = agent.subscribe();
    agent.prompt("go").await.expect("prompt succeeds");
    let events = drain(&mut rx);

    let deltas: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::MessageUpdate(AssistantMessageUpdate::TextDelta { delta }) => {
                Some(delta.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        deltas,
        vec!["hello ".to_string(), "world".to_string()],
        "each provider delta is forwarded verbatim"
    );

    let message_end = position(&events, |event| {
        matches!(event, AgentEvent::MessageEnd { .. })
    });
    let last_delta = events
        .iter()
        .enumerate()
        .rev()
        .find(|(_, event)| {
            matches!(
                event,
                AgentEvent::MessageUpdate(AssistantMessageUpdate::TextDelta { .. })
            )
        })
        .map(|(index, _)| index)
        .expect("a text delta");
    assert!(
        last_delta < message_end,
        "every delta must be emitted before MessageEnd (first at {last_delta}, end at {message_end})"
    );
}

#[tokio::test]
async fn parallel_batch_emits_every_start_before_any_end() {
    let stream = Arc::new(ScriptedEvents::new(vec![
        tool_call_script(&[("alpha", "call-a"), ("beta", "call-b")], Duration::ZERO),
        text_reply("done"),
    ]));
    let mut agent = Agent::new(
        AgentOptions::new(faux_model(), stream, "you are pi").with_tool_executor(Arc::new(
            SleepyExecutor {
                delay: Duration::from_millis(80),
            },
        )),
    );
    let mut rx = agent.subscribe();
    agent.prompt("go").await.expect("prompt succeeds");
    let events = drain(&mut rx);

    let starts = tool_start_names(&events);
    assert_eq!(
        starts
            .iter()
            .map(|(_, id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["call-a", "call-b"],
        "starts are emitted in source order: {events:#?}"
    );
    let last_start = starts.iter().map(|(index, _, _)| *index).max().unwrap();
    let first_end = tool_end_indices(&events)
        .iter()
        .map(|(index, _)| *index)
        .min()
        .expect("at least one ToolExecutionEnd");

    assert!(
        last_start < first_end,
        "both starts ({}) must precede the first end ({first_end}) — the batch must not \
         back-fill its events after the join: {events:#?}",
        starts
            .iter()
            .map(|(index, _, _)| index.to_string())
            .collect::<Vec<_>>()
            .join(", "),
    );
    assert_eq!(tool_end_indices(&events).len(), 2);
}

#[tokio::test]
async fn aborted_queued_call_still_pairs_its_start_with_an_end() {
    // `execute_batch_parallel` emits `ToolExecutionStart` for every call it
    // prepares and only then stops on the abort (LUM-1143), so the call that
    // is finalized as `Operation aborted` still owes consumers a matching
    // `ToolExecutionEnd` — otherwise a consumer would show it as running
    // forever.
    let stream = Arc::new(ScriptedEvents::new(vec![
        tool_call_script(&[("alpha", "call-a"), ("beta", "call-b")], Duration::ZERO),
        text_reply("done"),
    ]));
    let mut agent = Agent::new(
        AgentOptions::new(faux_model(), stream, "you are pi").with_tool_executor(Arc::new(
            SleepyExecutor {
                delay: Duration::ZERO,
            },
        )),
    );
    let token = agent.loop_ref().cancellation_token();
    token.cancel();
    agent.loop_mut().set_cancellation_token(token);
    let mut rx = agent.subscribe();
    agent.prompt("go").await.expect("prompt succeeds");
    let events = drain(&mut rx);

    assert_eq!(
        tool_start_names(&events)
            .iter()
            .map(|(_, id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["call-a"],
        "calls after the abort are never dispatched: {events:#?}"
    );
    let ends = tool_end_indices(&events);
    assert_eq!(
        ends.iter().map(|(_, id)| id.as_str()).collect::<Vec<_>>(),
        vec!["call-a"],
        "a start without a matching end would leave the call running forever: {events:#?}"
    );
    let (end_index, _) = ends[0];
    let AgentEvent::ToolExecutionEnd { result, .. } = &events[end_index] else {
        unreachable!("position only returns ToolExecutionEnd indices")
    };
    assert!(result.is_error, "the aborted call is an error result");
    assert!(
        matches!(result.content.as_ref(), Content::Text(text) if text.text == "Operation aborted"),
        "unexpected aborted payload: {result:?}"
    );
}

#[tokio::test]
async fn provider_error_truncates_the_event_sequence() {
    let stream = Arc::new(ScriptedEvents::new(vec![(
        Duration::ZERO,
        vec![
            AssistantMessageEvent::Start {
                model: "faux-model".into(),
            },
            AssistantMessageEvent::TextDelta {
                delta: "partial".into(),
            },
            AssistantMessageEvent::Error {
                message: "boom".into(),
            },
        ],
    )]));
    let mut agent = Agent::new(AgentOptions::new(faux_model(), stream, "you are pi"));
    let mut rx = agent.subscribe();
    let error = agent
        .prompt("go")
        .await
        .expect_err("provider error surfaces");
    assert!(
        matches!(error, AgentError::Provider(ref message) if message == "boom"),
        "unexpected error: {error}"
    );

    let events = drain(&mut rx);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnStart)),
        "TurnStart is emitted before the provider request: {events:#?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::MessageStart { .. })),
        "MessageStart is emitted as soon as the provider starts: {events:#?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::MessageEnd { .. })),
        "a truncated stream has no MessageEnd: {events:#?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnEnd { .. })),
        "a failed turn has no TurnEnd: {events:#?}"
    );
}
