//! Stage 43 — agent-level retry of the assistant call.
//!
//! Port of `packages/ai/src/utils/retry.ts` (see `src/retry.rs` for why the
//! module lives in `pi-agent-core`). The first half pins the classifier and
//! the backoff schedule, mirroring `packages/ai/test/retry.test.ts`; the
//! second half drives a real [`Agent`] so the wiring inside
//! `stream_assistant_response` is covered end to end:
//!
//! * a transient provider failure is retried and the turn still succeeds;
//! * a quota / billing failure fails on the first attempt;
//! * the retry budget is per assistant call and resets on success;
//! * a disabled policy leaves the loop a passthrough;
//! * a retry that happens after `MessageStart` was already emitted restarts
//!   the assistant sequence (the failed attempt is truncated, not rolled
//!   back).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use pi_agent_core::{
    is_retryable_agent_error, is_retryable_error_message, retry_assistant_call, retry_delay_ms,
    Agent, AgentError, AgentEvent, AgentOptions, RetryCallbacks, RetryPolicy,
    DEFAULT_MAX_AGENT_RETRY_DELAY_MS,
};
use pi_ai::stream::{AssistantMessageEventStream, StreamFn};
use pi_ai::types::SimpleStreamOptions;
use pi_ai::StreamError;
use pi_protocol::{
    Api, AssistantMessage, AssistantMessageEvent, Content, Context as AgentContext, Model,
    ProviderId, StopReason, Usage,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Classifier + schedule (mirrors packages/ai/test/retry.test.ts)
// ---------------------------------------------------------------------------

const OPENAI_EXPLICIT_RETRY_MESSAGE: &str = "An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID req_******** in your message.";
const BEDROCK_EXPLICIT_RETRY_MESSAGE: &str = r#"{"message":"The system encountered an unexpected error during processing. Try your request again."}"#;
const NVIDIA_NIM_RESOURCE_EXHAUSTED_MESSAGE: &str =
    "ResourceExhausted: Worker local total request limit reached (288/48)";
const BUN_FETCH_SOCKET_CLOSED_MESSAGE: &str = "The socket connection was closed unexpectedly. For more information, pass `verbose: true` in the second argument to fetch()";
const OPENAI_RESPONSES_EARLY_EOF_MESSAGE: &str =
    "OpenAI Responses stream ended before a terminal response event";
const WRAPPED_DNS_LOOKUP_ERROR: &str = "The pending stream has been canceled (caused by: getaddrinfo ENOTFOUND bedrock-runtime.us-east-1.amazonaws.com)";

#[test]
fn matches_explicit_provider_retry_guidance() {
    assert!(is_retryable_error_message(OPENAI_EXPLICIT_RETRY_MESSAGE));
    assert!(is_retryable_error_message(BEDROCK_EXPLICIT_RETRY_MESSAGE));
    assert!(is_retryable_error_message(
        NVIDIA_NIM_RESOURCE_EXHAUSTED_MESSAGE
    ));
}

#[test]
fn matches_bun_fetch_socket_drop_wording() {
    assert!(is_retryable_error_message(BUN_FETCH_SOCKET_CLOSED_MESSAGE));
}

#[test]
fn matches_upstream_request_buffer_exhaustion_wording() {
    assert!(is_retryable_error_message(
        "Error: exceeded request buffer limit while retrying upstream"
    ));
}

#[test]
fn matches_dns_transport_failure_wording() {
    for message in [
        WRAPPED_DNS_LOOKUP_ERROR,
        "connect ENOTFOUND api.example.com",
        "EAI_AGAIN api.example.com",
        "getaddrinfo failed for api.example.com",
    ] {
        assert!(is_retryable_error_message(message), "{message}");
    }
}

#[test]
fn matches_openai_responses_streams_that_end_before_terminal_events() {
    assert!(is_retryable_error_message(
        OPENAI_RESPONSES_EARLY_EOF_MESSAGE
    ));
}

#[test]
fn keeps_provider_limit_errors_non_retryable() {
    // The non-retryable pattern wins even though `429` matches the transient
    // pattern.
    assert!(!is_retryable_error_message("429 quota exceeded"));
    assert!(!is_retryable_error_message("insufficient_quota"));
    assert!(!is_retryable_error_message("Monthly usage limit reached"));
    assert!(!is_retryable_error_message("out of budget"));
}

#[test]
fn keeps_context_overflow_errors_non_retryable() {
    // Upstream `_isRetryableError` checks `isContextOverflow` before the
    // transient classifier: the agent compacts instead of replaying the same
    // oversized prompt. The second vector also matches the transient pattern
    // (`503` / `service unavailable`), so it only stays non-retryable because
    // of the overflow check.
    for message in [
        "prompt is too long: 213462 tokens > 200000 maximum",
        "503 service unavailable: requested token count exceeds the model's maximum context length of 131072 tokens.",
        "413 request_too_large: Request exceeds the maximum size",
    ] {
        assert!(!is_retryable_error_message(message), "{message}");
    }
}

#[test]
fn classifies_assistant_error_text() {
    assert!(is_retryable_error_message("overloaded_error"));
    assert!(is_retryable_error_message("524 status code (no body)"));
    assert!(is_retryable_error_message("terminated"));
    // An empty message has nothing to classify.
    assert!(!is_retryable_error_message(""));
    assert!(!is_retryable_error_message(
        "malformed stream: unexpected token"
    ));
}

#[test]
fn classifies_agent_errors_by_kind() {
    assert!(is_retryable_agent_error(&AgentError::Provider(
        "overloaded".into()
    )));
    assert!(is_retryable_agent_error(&AgentError::Stream(
        "provider returned 503: service unavailable".into()
    )));
    assert!(!is_retryable_agent_error(&AgentError::Stream(
        "malformed stream: unexpected token".into()
    )));
    // A tool failure is the model's own doing — never a provider hiccup.
    assert!(!is_retryable_agent_error(&AgentError::Tool {
        tool: "bash".into(),
        message: "overloaded shell".into(),
    }));
}

#[test]
fn caps_agent_retry_delay() {
    // Regression port of #8826.
    let base = RetryPolicy::new(3, 2_000);
    assert_eq!(retry_delay_ms(&base, 6), 60_000);
    assert_eq!(retry_delay_ms(&base, 1), 2_000);
    assert_eq!(retry_delay_ms(&base, 2), 4_000);
    assert_eq!(retry_delay_ms(&base, 5), 32_000);
    assert_eq!(
        retry_delay_ms(
            &RetryPolicy::new(3, 2_000).with_max_agent_delay_ms(5_000),
            5
        ),
        5_000
    );
    assert_eq!(
        retry_delay_ms(&RetryPolicy::new(3, 2_000).with_max_agent_delay_ms(0), 5),
        0
    );
    // A zero base never sleeps.
    assert_eq!(retry_delay_ms(&RetryPolicy::new(3, 0), 6), 0);
    // Attempt 0 is treated like attempt 1 (`Math.max(0, attempt - 1)`).
    assert_eq!(retry_delay_ms(&base, 0), 2_000);
    // No cap above `DEFAULT_MAX_AGENT_RETRY_DELAY_MS` when unset.
    assert_eq!(base.max_agent_delay_ms, None);
    assert_eq!(DEFAULT_MAX_AGENT_RETRY_DELAY_MS, 60_000);
}

#[test]
fn default_policy_matches_upstream_settings() {
    let policy = RetryPolicy::default();
    assert!(policy.enabled);
    assert_eq!(policy.max_retries, 3);
    assert_eq!(policy.base_delay_ms, 2_000);
    assert_eq!(policy.max_agent_delay_ms, None);
    assert!(!RetryPolicy::disabled().enabled);
}

// ---------------------------------------------------------------------------
// `retry_assistant_call` (mirrors packages/ai/test/retry.test.ts)
// ---------------------------------------------------------------------------

fn fast_policy() -> RetryPolicy {
    RetryPolicy::new(3, 0)
}

fn message(text: &str, stop_reason: StopReason) -> AssistantMessage {
    AssistantMessage {
        model: "faux-model".into(),
        content: if text.is_empty() {
            Vec::new()
        } else {
            vec![Content::text(text)]
        },
        stop_reason,
        usage: Usage::default(),
        error_message: None,
    }
}

fn message_text(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            Content::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

/// Records the `(attempt, max_attempts, delay_ms, message)` tuples a policy
/// reported through `on_retry_scheduled`.
type Scheduled = Arc<Mutex<Vec<(u32, u32, u64, String)>>>;

/// Records the `(success, attempt)` tuples reported through `on_retry_finished`.
type Finished = Arc<Mutex<Vec<(bool, u32)>>>;

fn callbacks(scheduled: Scheduled, finished: Finished) -> RetryCallbacks {
    RetryCallbacks {
        on_retry_scheduled: Some(Arc::new(move |attempt, max, delay, error| {
            scheduled
                .lock()
                .expect("scheduled")
                .push((attempt, max, delay, error.to_string()));
        })),
        on_retry_attempt_start: None,
        on_retry_finished: Some(Arc::new(move |success, attempt, _error| {
            finished.lock().expect("finished").push((success, attempt));
        })),
    }
}

#[tokio::test]
async fn returns_a_successful_response_immediately_without_retrying() {
    let calls = AtomicUsize::new(0);
    let result = retry_assistant_call(
        || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Ok(message("ok", StopReason::Stop)) }
        },
        Some(&fast_policy()),
        "faux-model",
        &CancellationToken::new(),
        None,
    )
    .await
    .expect("success");
    assert_eq!(message_text(&result), "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn does_not_retry_an_aborted_message() {
    let calls = AtomicUsize::new(0);
    let scheduled: Scheduled = Arc::default();
    let finished: Finished = Arc::default();
    let result = retry_assistant_call(
        || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Ok(message("", StopReason::Aborted)) }
        },
        Some(&fast_policy()),
        "faux-model",
        &CancellationToken::new(),
        Some(&callbacks(scheduled.clone(), finished.clone())),
    )
    .await
    .expect("aborted message");
    assert_eq!(result.stop_reason, StopReason::Aborted);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(scheduled.lock().expect("scheduled").is_empty());
    assert!(finished.lock().expect("finished").is_empty());
}

#[tokio::test]
async fn does_not_retry_a_non_retryable_error() {
    let calls = AtomicUsize::new(0);
    let scheduled: Scheduled = Arc::default();
    let finished: Finished = Arc::default();
    let error = retry_assistant_call(
        || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(AgentError::Provider("insufficient_quota".into())) }
        },
        Some(&fast_policy()),
        "faux-model",
        &CancellationToken::new(),
        Some(&callbacks(scheduled.clone(), finished.clone())),
    )
    .await
    .expect_err("quota error");
    assert_eq!(error.to_string(), "provider error: insufficient_quota");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(scheduled.lock().expect("scheduled").is_empty());
    // Nothing was retried, so the loop never started reporting.
    assert!(finished.lock().expect("finished").is_empty());
}

#[tokio::test]
async fn retries_a_transient_error_up_to_max_retries_then_returns_the_final_error() {
    let calls = AtomicUsize::new(0);
    let scheduled: Scheduled = Arc::default();
    let finished: Finished = Arc::default();
    let error = retry_assistant_call(
        || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(AgentError::Provider("terminated".into())) }
        },
        Some(&fast_policy()),
        "faux-model",
        &CancellationToken::new(),
        Some(&callbacks(scheduled.clone(), finished.clone())),
    )
    .await
    .expect_err("exhausted");
    assert_eq!(error.to_string(), "provider error: terminated");
    assert_eq!(calls.load(Ordering::SeqCst), 4); // 1 initial + 3 retries
    assert_eq!(scheduled.lock().expect("scheduled").len(), 3);
    assert_eq!(
        *finished.lock().expect("finished"),
        vec![(false, 3)],
        "the final failure is reported once, with the last attempt number"
    );
}

#[tokio::test]
async fn reports_capped_retry_delays() {
    // Regression port of #8826: 10, 20→15, 40→15, 80→15.
    let policy = RetryPolicy::new(4, 10).with_max_agent_delay_ms(15);
    let calls = AtomicUsize::new(0);
    let scheduled: Scheduled = Arc::default();
    let finished: Finished = Arc::default();
    let result = retry_assistant_call(
        || {
            let call = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if call < 4 {
                    Err(AgentError::Provider("terminated".into()))
                } else {
                    Ok(message("recovered", StopReason::Stop))
                }
            }
        },
        Some(&policy),
        "faux-model",
        &CancellationToken::new(),
        Some(&callbacks(scheduled.clone(), finished.clone())),
    )
    .await
    .expect("recovered");
    assert_eq!(message_text(&result), "recovered");
    let delays: Vec<u64> = scheduled
        .lock()
        .expect("scheduled")
        .iter()
        .map(|(_, _, delay, _)| *delay)
        .collect();
    assert_eq!(delays, vec![10, 15, 15, 15]);
    assert_eq!(*finished.lock().expect("finished"), vec![(true, 4)]);
}

#[tokio::test]
async fn stops_retrying_once_a_call_succeeds() {
    let calls = AtomicUsize::new(0);
    let finished: Finished = Arc::default();
    let result = retry_assistant_call(
        || {
            let call = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if call < 2 {
                    Err(AgentError::Provider("terminated".into()))
                } else {
                    Ok(message("recovered", StopReason::Stop))
                }
            }
        },
        Some(&fast_policy()),
        "faux-model",
        &CancellationToken::new(),
        Some(&callbacks(Arc::default(), finished.clone())),
    )
    .await
    .expect("recovered");
    assert_eq!(message_text(&result), "recovered");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(*finished.lock().expect("finished"), vec![(true, 2)]);
}

#[tokio::test]
async fn reports_an_aborted_retried_call_as_unsuccessful() {
    let calls = AtomicUsize::new(0);
    let finished: Finished = Arc::default();
    let result = retry_assistant_call(
        || {
            let call = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if call == 0 {
                    Err(AgentError::Provider("terminated".into()))
                } else {
                    Ok(message("", StopReason::Aborted))
                }
            }
        },
        Some(&fast_policy()),
        "faux-model",
        &CancellationToken::new(),
        Some(&callbacks(Arc::default(), finished.clone())),
    )
    .await
    .expect("aborted message");
    assert_eq!(result.stop_reason, StopReason::Aborted);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(*finished.lock().expect("finished"), vec![(false, 1)]);
}

#[tokio::test]
async fn does_not_retry_when_the_policy_is_disabled() {
    let calls = AtomicUsize::new(0);
    let scheduled: Scheduled = Arc::default();
    let finished: Finished = Arc::default();
    let error = retry_assistant_call(
        || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(AgentError::Provider("terminated".into())) }
        },
        Some(&RetryPolicy::disabled()),
        "faux-model",
        &CancellationToken::new(),
        Some(&callbacks(scheduled.clone(), finished.clone())),
    )
    .await
    .expect_err("disabled policy is a passthrough");
    assert_eq!(error.to_string(), "provider error: terminated");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(scheduled.lock().expect("scheduled").is_empty());
    assert!(finished.lock().expect("finished").is_empty());
}

#[tokio::test]
async fn no_policy_is_a_passthrough() {
    let calls = AtomicUsize::new(0);
    let error = retry_assistant_call(
        || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(AgentError::Provider("overloaded".into())) }
        },
        None,
        "faux-model",
        &CancellationToken::new(),
        None,
    )
    .await
    .expect_err("passthrough");
    assert_eq!(error.to_string(), "provider error: overloaded");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_abort_during_the_backoff_yields_an_aborted_message() {
    let signal = CancellationToken::new();
    signal.cancel();
    let calls = AtomicUsize::new(0);
    let finished: Finished = Arc::default();
    let mut callbacks = callbacks(Arc::default(), finished.clone());
    let started = Arc::new(AtomicUsize::new(0));
    let started_inner = started.clone();
    callbacks.on_retry_attempt_start = Some(Arc::new(move || {
        started_inner.fetch_add(1, Ordering::SeqCst);
    }));

    let result = retry_assistant_call(
        || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(AgentError::Provider("terminated".into())) }
        },
        Some(&RetryPolicy::new(3, 60_000)),
        "faux-model",
        &signal,
        Some(&callbacks),
    )
    .await
    .expect("aborted message");
    assert_eq!(result.stop_reason, StopReason::Aborted);
    assert_eq!(result.model, "faux-model");
    assert!(result.content.is_empty());
    // The backoff was skipped, so the retried call never started.
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(started.load(Ordering::SeqCst), 0);
    assert_eq!(*finished.lock().expect("finished"), vec![(false, 1)]);
}

#[tokio::test]
async fn an_error_stop_reason_without_text_is_returned_as_is() {
    // `pi-protocol` carries no `errorMessage`, so a `StopReason::Error`
    // response can never be classified as retryable.
    let calls = AtomicUsize::new(0);
    let result = retry_assistant_call(
        || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Ok(message("", StopReason::Error)) }
        },
        Some(&fast_policy()),
        "faux-model",
        &CancellationToken::new(),
        None,
    )
    .await
    .expect("error message");
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// Agent-loop wiring
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

/// One scripted `stream_simple` call.
enum Step {
    /// The provider fails before the stream is handed over — the loop sees
    /// `Err(StreamError)`, i.e. `AgentError::Stream`.
    FailAtOpen(StreamError),
    /// The provider opens the stream, emits `Start`, then fails mid-stream —
    /// the loop sees `AgentError::Provider` after `MessageStart` was already
    /// emitted.
    FailMidStream(String),
    /// A final text answer with no tool calls.
    Reply(String),
}

/// Streams one [`Step`] per `stream_simple` call and counts the calls.
struct ScriptedStream {
    steps: Mutex<Vec<Step>>,
    calls: AtomicUsize,
}

impl ScriptedStream {
    fn new(steps: Vec<Step>) -> Self {
        Self {
            steps: Mutex::new(steps),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
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
        self.calls.fetch_add(1, Ordering::SeqCst);
        let step = {
            let mut steps = self.steps.lock().expect("scripted steps");
            if steps.is_empty() {
                return Err(StreamError::Malformed("scripted stream exhausted".into()));
            }
            steps.remove(0)
        };
        let events: Vec<Result<AssistantMessageEvent, StreamError>> = match step {
            Step::FailAtOpen(error) => return Err(error),
            Step::FailMidStream(message) => vec![
                Ok(AssistantMessageEvent::Start {
                    model: "faux-model".into(),
                }),
                Ok(AssistantMessageEvent::Error { message }),
            ],
            Step::Reply(text) => vec![
                Ok(AssistantMessageEvent::Start {
                    model: "faux-model".into(),
                }),
                Ok(AssistantMessageEvent::TextDelta {
                    delta: text.clone(),
                }),
                Ok(AssistantMessageEvent::Done {
                    content: vec![Content::text(text)],
                    stop_reason: StopReason::Stop,
                    usage: Usage::default(),
                }),
            ],
        };
        Ok(Box::pin(stream::iter(events)))
    }
}

fn agent_with(stream: Arc<ScriptedStream>, policy: RetryPolicy) -> Agent {
    Agent::new(
        AgentOptions::new(
            faux_model(),
            Arc::new(stream) as Arc<dyn StreamFn>,
            "you are pi",
        )
        .with_retry_policy(policy),
    )
}

/// Drain every event the agent fanned out, as labels.
async fn event_labels(receiver: &mut UnboundedReceiver<AgentEvent>) -> Vec<&'static str> {
    let mut labels = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        labels.push(match event {
            AgentEvent::AgentStart => "AgentStart",
            AgentEvent::AgentEnd { .. } => "AgentEnd",
            AgentEvent::TurnStart => "TurnStart",
            AgentEvent::MessageStart { .. } => "MessageStart",
            AgentEvent::MessageUpdate(_) => "MessageUpdate",
            AgentEvent::MessageEnd { .. } => "MessageEnd",
            AgentEvent::ToolExecutionStart { .. } => "ToolExecutionStart",
            AgentEvent::ToolExecutionUpdate { .. } => "ToolExecutionUpdate",
            AgentEvent::ToolExecutionEnd { .. } => "ToolExecutionEnd",
            AgentEvent::TurnEnd { .. } => "TurnEnd",
            AgentEvent::UserMessage(_) => "UserMessage",
            AgentEvent::Error(_) => "Error",
        });
    }
    labels
}

/// Text of the last assistant message in the agent's log.
fn last_assistant_text(agent: &Agent) -> String {
    agent
        .state()
        .messages
        .iter()
        .rev()
        .find(|message| message.role == pi_protocol::Role::Assistant)
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(|content| match content {
                    Content::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn agent_retries_a_transient_provider_failure_and_recovers() {
    let stream = Arc::new(ScriptedStream::new(vec![
        Step::FailAtOpen(StreamError::provider(503, "service unavailable")),
        Step::Reply("recovered".into()),
    ]));
    let mut agent = agent_with(stream.clone(), RetryPolicy::new(3, 0));
    let mut events = agent.subscribe();

    agent.prompt("hi").await.expect("the retried turn succeeds");

    assert_eq!(stream.calls(), 2);
    assert_eq!(last_assistant_text(&agent), "recovered");
    // The failed attempt never opened a stream, so the recovered attempt's
    // sequence is the only one the observer sees.
    assert_eq!(
        event_labels(&mut events).await,
        vec![
            "UserMessage",
            "TurnStart",
            "MessageStart",
            "MessageUpdate",
            "MessageEnd",
            "TurnEnd",
        ]
    );
}

#[tokio::test]
async fn agent_restarts_the_sequence_when_a_mid_stream_failure_is_retried() {
    let stream = Arc::new(ScriptedStream::new(vec![
        Step::FailMidStream("503 service unavailable".into()),
        Step::Reply("recovered".into()),
    ]));
    let mut agent = agent_with(stream.clone(), RetryPolicy::new(3, 0));
    let mut events = agent.subscribe();

    agent.prompt("hi").await.expect("the retried turn succeeds");

    assert_eq!(stream.calls(), 2);
    assert_eq!(last_assistant_text(&agent), "recovered");
    // The failed attempt is *not* rolled back: it emitted `MessageStart` and
    // then stopped, and the retry opens a second assistant sequence.
    assert_eq!(
        event_labels(&mut events).await,
        vec![
            "UserMessage",
            "TurnStart",
            "MessageStart",
            "MessageStart",
            "MessageUpdate",
            "MessageEnd",
            "TurnEnd",
        ]
    );
}

#[tokio::test]
async fn agent_does_not_retry_a_quota_failure() {
    let stream = Arc::new(ScriptedStream::new(vec![Step::FailAtOpen(
        StreamError::provider(429, "insufficient_quota"),
    )]));
    let mut agent = agent_with(stream.clone(), RetryPolicy::new(3, 0));

    let error = agent.prompt("hi").await.expect_err("quota fails fast");

    assert_eq!(
        error.to_string(),
        "stream error: provider returned 429: insufficient_quota"
    );
    assert_eq!(stream.calls(), 1);
}

#[tokio::test]
async fn agent_exhausts_the_retry_budget_then_fails() {
    let stream = Arc::new(ScriptedStream::new(vec![
        Step::FailAtOpen(StreamError::Malformed("terminated".into())),
        Step::FailAtOpen(StreamError::Malformed("terminated".into())),
        Step::FailAtOpen(StreamError::Malformed("terminated".into())),
    ]));
    // `max_retries: 2` → three calls in total.
    let mut agent = agent_with(stream.clone(), RetryPolicy::new(2, 0));

    let error = agent.prompt("hi").await.expect_err("budget exhausted");

    assert_eq!(
        error.to_string(),
        "stream error: malformed stream: terminated"
    );
    assert_eq!(stream.calls(), 3);
}

#[tokio::test]
async fn agent_passes_a_disabled_policy_through() {
    let stream = Arc::new(ScriptedStream::new(vec![Step::FailAtOpen(
        StreamError::provider(503, "service unavailable"),
    )]));
    let mut agent = agent_with(stream.clone(), RetryPolicy::disabled());

    agent.prompt("hi").await.expect_err("no retry");

    assert_eq!(stream.calls(), 1);
}

#[tokio::test]
async fn each_assistant_call_gets_its_own_retry_budget() {
    let stream = Arc::new(ScriptedStream::new(vec![
        Step::FailAtOpen(StreamError::provider(503, "service unavailable")),
        Step::Reply("one".into()),
        Step::FailAtOpen(StreamError::provider(502, "bad gateway")),
        Step::Reply("two".into()),
    ]));
    let mut agent = agent_with(stream.clone(), RetryPolicy::new(3, 0));

    agent.prompt("first").await.expect("first turn recovers");
    assert_eq!(last_assistant_text(&agent), "one");

    agent.prompt("second").await.expect("second turn recovers");
    assert_eq!(last_assistant_text(&agent), "two");

    // Two turns, one retry each: the counter resets between calls instead of
    // accumulating across the run.
    assert_eq!(stream.calls(), 4);
}

/// The aborted-backoff path must still end the turn normally: the loop gets
/// an aborted assistant message instead of the provider error.
#[tokio::test]
async fn agent_reports_an_aborted_message_when_the_signal_fires_during_backoff() {
    let stream = Arc::new(ScriptedStream::new(vec![Step::FailAtOpen(
        StreamError::provider(503, "service unavailable"),
    )]));
    let mut agent = agent_with(stream.clone(), RetryPolicy::new(3, 60_000));
    let mut events = agent.subscribe();
    let signal = CancellationToken::new();
    signal.cancel();
    agent.loop_mut().set_cancellation_token(signal);

    // The backoff is skipped, so the failed attempt is the only call and the
    // turn ends with an aborted message rather than an error.
    agent.prompt("hi").await.expect("aborted turn");

    assert_eq!(stream.calls(), 1);
    assert_eq!(
        event_labels(&mut events).await,
        vec!["UserMessage", "TurnStart", "TurnEnd"]
    );
}
