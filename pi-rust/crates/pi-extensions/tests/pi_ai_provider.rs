//! Built-in `pi-ai` provider factories — LUM-1180.
//!
//! Upstream extensions reach the built-in model adapters through
//! `@earendil-works/pi-ai/compat`:
//!
//! ```ts
//! import { anthropicMessagesApi } from "@earendil-works/pi-ai/compat";
//! const stream = anthropicMessagesApi().streamSimple(model, context, { apiKey });
//! for await (const event of stream) { … }
//! const message = await stream.result();
//! ```
//!
//! `pi-extensions` cannot depend on `pi-ai`, so the host streams provider
//! events across `host_pi_ai_stream_start` / `_next` / `_cancel` and an
//! injected [`PiAiStreamRunner`] does the provider work. These tests drive
//! that bridge through a fake runner, so the contract is covered without a
//! network or a `pi-coding-agent` dependency:
//!
//! 1. events arrive **one at a time** and in order — the fake runner's
//!    producer can never run more than one event ahead of the consumer;
//! 2. the JS `AssistantMessageEventStream` collects them, so `for await` and
//!    `await stream.result()` both work and `complete()` returns an
//!    `AssistantMessage`;
//! 3. aborting the extension's `AbortSignal` cancels *and releases* the
//!    host-side stream (no leaked channel / provider connection);
//! 4. a live stream raises the host per-call deadline past the default
//!    timeout, so a slow provider does not cut a model turn short;
//! 5. without a runner the factories still import, but the stream terminates
//!    with a named error event instead of evaluating to `undefined`.
//! 6. every bridged api family — not just the two from LUM-1180 — drives the
//!    same host bridge: `openAICompletionsApi` / `googleGenerativeAIApi` /
//!    `azureOpenAIResponsesApi` (LUM-1204) each stream `text_delta` … `done`.

use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pi_extensions::{
    ExtensionEntry, HostOptions, JsExtensionHost, PiAiEventStream, PiAiStreamRequest,
    PiAiStreamRunner, ToolContext,
};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

static SCRATCH_COUNTER: AtomicU32 = AtomicU32::new(0);

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime")
}

/// A unique scratch directory, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let unique = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "pi_pi_ai_provider/{}-{name}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Self(dir)
    }

    fn as_str(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn entry_at(id: &str, path: &str) -> ExtensionEntry {
    ExtensionEntry {
        source: PathBuf::from(path),
        id: id.to_string(),
        label: None,
    }
}

/// Counts a live stream, so a leaked one is observable from the test.
struct StreamGuard {
    live: Arc<AtomicU32>,
    dropped: Arc<AtomicU32>,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

/// What the fake runner does with each `start` call.
#[derive(Clone)]
enum Script {
    /// Deliver one text delta per tick, then a terminal `done`.
    Stream { ticks: u32, delay: Duration },
    /// Every event is the upstream shape the shim expects for a failed turn.
    ErrorEvent,
}

/// A stand-in for the `pi-coding-agent` runner.
///
/// The events are pushed through a **rendezvous-adjacent** channel
/// (`capacity = 1`) whose producer records how far ahead of the consumer it
/// ever got: the bridge therefore cannot batch a turn, and `max_in_flight`
/// pins the one-event-at-a-time contract.
struct FakeRunner {
    script: Script,
    requests: Arc<std::sync::Mutex<Vec<PiAiStreamRequest>>>,
    live: Arc<AtomicU32>,
    dropped: Arc<AtomicU32>,
    max_in_flight: Arc<AtomicU32>,
}

impl FakeRunner {
    fn new(script: Script) -> Arc<Self> {
        Arc::new(Self {
            script,
            requests: Arc::new(std::sync::Mutex::new(Vec::new())),
            live: Arc::new(AtomicU32::new(0)),
            dropped: Arc::new(AtomicU32::new(0)),
            max_in_flight: Arc::new(AtomicU32::new(0)),
        })
    }
}

/// The upstream `done` event: `reason` plus the finished `message`.
fn done_event(model: &Value, text: &str) -> Value {
    json!({
        "type": "done",
        "reason": "stop",
        "message": {
            "role": "assistant",
            "content": [{ "type": "text", "text": text }],
            "api": model.get("api").cloned().unwrap_or(json!("unknown")),
            "provider": model.get("provider").cloned().unwrap_or(json!("unknown")),
            "model": model.get("id").cloned().unwrap_or(json!("unknown")),
            "usage": {"input": 1, "output": 2, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 3},
            "stopReason": "stop",
            "timestamp": 0,
        },
    })
}

impl PiAiStreamRunner for FakeRunner {
    fn start<'a>(
        &'a self,
        request: PiAiStreamRequest,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<PiAiEventStream, String>> + Send + 'a>> {
        let script = self.script.clone();
        let model = request.model.clone();
        self.requests.lock().expect("requests lock").push(request);
        let live = self.live.clone();
        let dropped = self.dropped.clone();
        let max_in_flight = self.max_in_flight.clone();
        Box::pin(async move {
            live.fetch_add(1, Ordering::SeqCst);
            let mut scripted: VecDeque<Value> = VecDeque::new();
            match script {
                Script::Stream { ticks, .. } => {
                    for tick in 0..ticks {
                        scripted.push_back(json!({
                            "type": "text_delta",
                            "contentIndex": 0,
                            "delta": format!("chunk-{tick}"),
                            "partial": {"role": "assistant", "content": [{"type": "text", "text": ""}]},
                        }));
                    }
                    scripted.push_back(done_event(&model, "chunk-0chunk-1chunk-2"));
                }
                Script::ErrorEvent => scripted.push_back(json!({
                    "type": "error",
                    "reason": "error",
                    "error": {
                        "role": "assistant",
                        "content": [],
                        "stopReason": "error",
                        "errorMessage": "boom",
                    },
                })),
            }

            let (tx, rx) = tokio::sync::mpsc::channel::<Value>(1);
            let sent = Arc::new(AtomicU32::new(0));
            let consumed = Arc::new(AtomicU32::new(0));
            let guard = StreamGuard {
                live: live.clone(),
                dropped: dropped.clone(),
            };

            // The producer owns nothing but the sender: the guard travels with
            // the *stream*, so dropping the stream (a cancel) is what releases
            // the stream, not the task finishing.
            let delay = match script {
                Script::Stream { delay, .. } => delay,
                Script::ErrorEvent => Duration::ZERO,
            };
            let producer_cancel = cancel.clone();
            let producer_max = max_in_flight.clone();
            let producer_consumed = consumed.clone();
            tokio::spawn(async move {
                for event in scripted {
                    if !delay.is_zero() {
                        tokio::select! {
                            biased;
                            () = producer_cancel.cancelled() => break,
                            () = tokio::time::sleep(delay) => {}
                        }
                    }
                    if tx.send(event).await.is_err() {
                        break;
                    }
                    // Measured *after* the send: with a capacity of one the
                    // producer cannot get further than one event ahead of the
                    // consumer, which is what "one at a time" means here.
                    let ahead = sent.fetch_add(1, Ordering::SeqCst) + 1
                        - producer_consumed.load(Ordering::SeqCst);
                    let mut best = producer_max.load(Ordering::SeqCst);
                    while best < ahead {
                        match producer_max.compare_exchange(
                            best,
                            ahead,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        ) {
                            Ok(_) => break,
                            Err(current) => best = current,
                        }
                    }
                }
            });

            let stream = futures::stream::unfold((rx, guard), move |(mut rx, guard)| {
                let consumed = consumed.clone();
                async move {
                    let item = rx.recv().await;
                    if item.is_some() {
                        consumed.fetch_add(1, Ordering::SeqCst);
                    }
                    item.map(|event| (event, (rx, guard)))
                }
            });
            let stream: PiAiEventStream = Box::pin(stream);
            Ok(stream)
        })
    }
}

async fn host_with_runner(
    cwd: &str,
    runner: Arc<FakeRunner>,
    timeout: Option<Duration>,
) -> JsExtensionHost {
    let options = HostOptions {
        tool_context: ToolContext {
            mode: "print".to_string(),
            has_ui: false,
            cwd: cwd.to_string(),
        },
        ..HostOptions::default()
    }
    .with_pi_ai_stream_runner(runner);
    let options = match timeout {
        Some(timeout) => options.with_timeout(timeout),
        None => options,
    };
    JsExtensionHost::with_options(options).await.expect("host")
}

async fn host_without_runner(cwd: &str) -> JsExtensionHost {
    JsExtensionHost::with_options(HostOptions {
        tool_context: ToolContext {
            mode: "print".to_string(),
            has_ui: false,
            cwd: cwd.to_string(),
        },
        ..HostOptions::default()
    })
    .await
    .expect("host")
}

/// A **streamed** turn: events arrive one at a time, `for await` sees them in
/// order, and `complete()` folds them into an `AssistantMessage`.
const STREAM_SOURCE: &str = r##"
    import {
        anthropicMessagesApi, complete, createAssistantMessageEventStream, getApiProvider,
    } from "@earendil-works/pi-ai/compat";

    export default function (pi) {
        pi.registerTool({
            name: "provider_probe",
            label: "provider probe",
            description: "drives a builtin provider stream",
            parameters: { type: "object", properties: {} },
            execute: async () => {
                const captured = {};
                const model = { id: "claude-haiku-4-5", provider: "anthropic", api: "anthropic-messages" };
                const context = { messages: [{ role: "user", content: "hi" }] };

                // `streamSimple` is synchronous and returns an
                // AssistantMessageEventStream, like upstream `lazyStream`.
                const stream = anthropicMessagesApi().streamSimple(model, context, { apiKey: "test-key" });
                captured.isEventStream = stream instanceof Object &&
                    typeof stream[Symbol.asyncIterator] === "function" &&
                    typeof stream.result === "function";

                // An independent pure-JS stream proves the class itself is not
                // the source of the events below.
                captured.directStreamText = await (await (async () => {
                    const s = createAssistantMessageEventStream();
                    s.push({ type: "done", reason: "stop", message: { role: "assistant", content: [] } });
                    return s;
                })()).result().then((m) => m.role);

                const events = [];
                for await (const event of stream) events.push(event.type);
                captured.events = events;
                const message = await stream.result();
                captured.messageText = message.content.map((c) => c.text).join("");
                captured.messageRole = message.role;
                captured.messageStopReason = message.stopReason;

                // `complete()` goes through the registry, so it must resolve to
                // a full AssistantMessage for the same model.
                const completed = await complete(model, context, { apiKey: "test-key" });
                captured.completeRole = completed.role;
                captured.completeText = completed.content.map((c) => c.text).join("");
                captured.completeStopReason = completed.stopReason;
                captured.registeredApi = getApiProvider("anthropic-messages").api;

                return { content: [{ type: "text", text: "ok" }], details: captured };
            },
        });
    }
"##;

#[test]
fn builtin_provider_stream_delivers_events_one_at_a_time() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("stream");
        let runner = FakeRunner::new(Script::Stream {
            ticks: 3,
            delay: Duration::ZERO,
        });
        let host = host_with_runner(&scratch.as_str(), runner.clone(), None).await;

        host.load(
            entry_at("provider_probe", "/tmp/pi_ai_provider/stream.mjs"),
            STREAM_SOURCE,
        )
        .await
        .expect("load provider probe extension");

        let outcome = host
            .execute_tool("provider_probe", &json!({}).to_string())
            .await
            .expect("execute provider probe");
        assert!(!outcome.is_error, "{outcome:?}");
        let d = outcome.details.expect("details");

        assert_eq!(
            d["events"],
            json!(["text_delta", "text_delta", "text_delta", "done"]),
            "every event crossed the bridge, in order, one per `next`"
        );
        assert_eq!(d["messageRole"], "assistant");
        assert_eq!(d["messageStopReason"], "stop");
        assert_eq!(d["messageText"], "chunk-0chunk-1chunk-2");
        assert_eq!(
            d["isEventStream"], true,
            "streamSimple returns an AssistantMessageEventStream synchronously"
        );
        assert_eq!(d["directStreamText"], "assistant");
        assert_eq!(d["completeRole"], "assistant");
        assert_eq!(d["completeStopReason"], "stop");
        assert_eq!(
            d["completeText"], "chunk-0chunk-1chunk-2",
            "complete() folds the streamed events into a message"
        );
        assert_eq!(d["registeredApi"], "anthropic-messages");

        // Two streams ran (streamSimple + complete); both were released.
        assert_eq!(
            runner.max_in_flight.load(Ordering::SeqCst),
            1,
            "the bridge must never have more than one event in flight"
        );
        assert_eq!(runner.live.load(Ordering::SeqCst), 0, "no live streams");
        assert_eq!(
            runner.dropped.load(Ordering::SeqCst),
            2,
            "each finished stream is dropped (channel released)"
        );

        let requests = runner.requests.lock().expect("requests lock").clone();
        assert_eq!(requests.len(), 2, "one start per stream");
        assert_eq!(requests[0].api, "anthropic-messages");
        assert_eq!(requests[0].model["id"], "claude-haiku-4-5");
        assert_eq!(requests[0].context["messages"][0]["role"], "user");
        assert_eq!(requests[0].options["apiKey"], "test-key");
        assert!(
            requests[0].options.get("signal").is_none(),
            "the AbortSignal stays on the JS side; it must not be serialised"
        );
    });
}

/// The three families LUM-1204 bridges on top of LUM-1180, all through the
/// same host bridge: `openAICompletionsApi` (Chat Completions wire shape),
/// `googleGenerativeAIApi` and `azureOpenAIResponsesApi` (the
/// deployment-scoped Responses dialect, a distinct `api` id with its own
/// registry entry).
const BRIDGED_FAMILIES_SOURCE: &str = r##"
    import {
        azureOpenAIResponsesApi, completeSimple, getApiProvider, googleGenerativeAIApi,
        openAICompletionsApi,
    } from "@earendil-works/pi-ai/compat";

    const FAMILIES = [
        { api: "openai-completions", provider: "deepseek", id: "deepseek-chat", factory: openAICompletionsApi },
        { api: "google-generative-ai", provider: "google", id: "gemini-2.0-flash", factory: googleGenerativeAIApi },
        { api: "azure-openai-responses", provider: "azure", id: "gpt-4o", factory: azureOpenAIResponsesApi },
    ];

    export default function (pi) {
        pi.registerTool({
            name: "family_probe",
            label: "family probe",
            description: "streams one turn from every bridged api family",
            parameters: { type: "object", properties: {} },
            execute: async () => {
                const context = { messages: [{ role: "user", content: "hi" }] };
                const families = [];
                for (const family of FAMILIES) {
                    const model = { id: family.id, provider: family.provider, api: family.api };
                    const streams = family.factory();
                    const registered = getApiProvider(family.api);
                    const stream = streams.streamSimple(model, context, { apiKey: "test-key" });
                    const events = [];
                    for await (const event of stream) events.push(event.type);
                    const message = await stream.result();
                    const completed = await completeSimple(model, context, { apiKey: "test-key" });
                    families.push({
                        api: family.api,
                        streamKeys: Object.keys(streams).sort(),
                        registeredApi: registered && registered.api,
                        events: events,
                        text: message.content.map((c) => c.text).join(""),
                        stopReason: message.stopReason,
                        completeText: completed.content.map((c) => c.text).join(""),
                    });
                }
                return { content: [{ type: "text", text: "ok" }], details: { families: families } };
            },
        });
    }
"##;

#[test]
fn every_bridged_api_family_streams_through_the_host_bridge() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("families");
        let runner = FakeRunner::new(Script::Stream {
            ticks: 3,
            delay: Duration::ZERO,
        });
        let host = host_with_runner(&scratch.as_str(), runner.clone(), None).await;

        host.load(
            entry_at("family_probe", "/tmp/pi_ai_provider/families.mjs"),
            BRIDGED_FAMILIES_SOURCE,
        )
        .await
        .expect("load family probe extension");

        let outcome = host
            .execute_tool("family_probe", &json!({}).to_string())
            .await
            .expect("execute family probe");
        assert!(!outcome.is_error, "{outcome:?}");
        let d = outcome.details.expect("details");
        let families = d["families"].as_array().expect("families");
        assert_eq!(families.len(), 3, "one entry per bridged family");

        for family in families {
            let api = family["api"].as_str().expect("api");
            assert_eq!(
                family["streamKeys"],
                json!(["stream", "streamSimple"]),
                "{api} hands back a ProviderStreams"
            );
            assert_eq!(
                family["registeredApi"], family["api"],
                "{api} is registered by registerBuiltInApiProviders at module init"
            );
            assert_eq!(
                family["events"],
                json!(["text_delta", "text_delta", "text_delta", "done"]),
                "{api} streams every event in order"
            );
            assert_eq!(family["stopReason"], "stop", "{api}");
            assert_eq!(family["text"], "chunk-0chunk-1chunk-2", "{api}");
            assert_eq!(
                family["completeText"], "chunk-0chunk-1chunk-2",
                "{api}: completeSimple() folds the same stream"
            );
        }

        // `streamSimple` + `completeSimple` per family: two streams each.
        let requests = runner.requests.lock().expect("requests lock").clone();
        let apis: Vec<&str> = requests
            .iter()
            .map(|request| request.api.as_str())
            .collect();
        assert_eq!(
            apis,
            vec![
                "openai-completions",
                "openai-completions",
                "google-generative-ai",
                "google-generative-ai",
                "azure-openai-responses",
                "azure-openai-responses",
            ],
            "every family reaches the runner under its own api id"
        );
        assert_eq!(
            requests[4].model["api"], "azure-openai-responses",
            "the model's api is what the shim validates before streaming"
        );
        assert_eq!(requests[4].options["apiKey"], "test-key");
        assert_eq!(
            runner.live.load(Ordering::SeqCst),
            0,
            "every stream is released"
        );
        assert_eq!(runner.dropped.load(Ordering::SeqCst), 6);
        assert_eq!(runner.max_in_flight.load(Ordering::SeqCst), 1);
    });
}

/// An aborted stream is cancelled *and* dropped: the extension's
/// `AbortController` reaches `host_pi_ai_stream_cancel`, which removes the
/// entry, so the runner's channel is released mid-turn.
const ABORT_SOURCE: &str = r##"
    import { anthropicMessagesApi } from "@earendil-works/pi-ai/compat";

    export default function (pi) {
        pi.registerTool({
            name: "abort_probe",
            label: "abort probe",
            description: "aborts a builtin provider stream",
            parameters: { type: "object", properties: {} },
            execute: async () => {
                const captured = {};
                const model = { id: "claude-haiku-4-5", provider: "anthropic", api: "anthropic-messages" };
                const controller = new AbortController();
                const stream = anthropicMessagesApi().streamSimple(
                    model,
                    { messages: [] },
                    { apiKey: "test-key", signal: controller.signal },
                );
                const seen = [];
                let abortedError = null;
                for await (const event of stream) {
                    seen.push(event.type);
                    if (event.type === "text_delta") controller.abort();
                    if (event.type === "error") abortedError = event.error;
                }
                captured.seen = seen;
                captured.abortReason = abortedError ? abortedError.stopReason : null;
                captured.signalAborted = controller.signal.aborted;
                return { content: [{ type: "text", text: "ok" }], details: captured };
            },
        });
    }
"##;

#[test]
fn aborting_a_stream_releases_the_host_side_channel() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("abort");
        let runner = FakeRunner::new(Script::Stream {
            ticks: 5,
            delay: Duration::from_millis(1),
        });
        let host = host_with_runner(&scratch.as_str(), runner.clone(), None).await;

        host.load(
            entry_at("abort_probe", "/tmp/pi_ai_provider/abort.mjs"),
            ABORT_SOURCE,
        )
        .await
        .expect("load abort probe extension");

        let outcome = host
            .execute_tool("abort_probe", &json!({}).to_string())
            .await
            .expect("execute abort probe");
        assert!(!outcome.is_error, "{outcome:?}");
        let d = outcome.details.expect("details");

        assert_eq!(
            d["seen"],
            json!(["text_delta", "error"]),
            "the abort ends the stream with an aborted error event"
        );
        assert_eq!(d["abortReason"], "aborted");
        assert_eq!(d["signalAborted"], true);
        assert_eq!(runner.live.load(Ordering::SeqCst), 0, "no live streams");
        assert_eq!(
            runner.dropped.load(Ordering::SeqCst),
            1,
            "the cancelled stream (and its channel) was dropped"
        );
        assert_eq!(
            runner.max_in_flight.load(Ordering::SeqCst),
            1,
            "even an abort never batches events"
        );
    });
}

/// A live stream raises the host per-call deadline: with the *default* 5s
/// timeout an extension that streams for longer would be cut off, so the
/// bridge arms the pi-ai deadline and keeps the call alive.
#[test]
fn a_live_stream_extends_the_host_call_deadline() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("deadline");
        let runner = FakeRunner::new(Script::Stream {
            ticks: 2,
            delay: Duration::from_millis(120),
        });
        // Far below the stream's total runtime: only the pi-ai deadline can
        // keep this call alive.
        let host = host_with_runner(
            &scratch.as_str(),
            runner.clone(),
            Some(Duration::from_millis(50)),
        )
        .await;

        host.load(
            entry_at("provider_probe", "/tmp/pi_ai_provider/deadline.mjs"),
            STREAM_SOURCE,
        )
        .await
        .expect("load provider probe extension");

        let outcome = host
            .execute_tool("provider_probe", &json!({}).to_string())
            .await
            .expect("execute provider probe");
        assert!(!outcome.is_error, "{outcome:?}");
        let d = outcome.details.expect("details");
        assert_eq!(
            d["completeText"], "chunk-0chunk-1chunk-2",
            "the stream outlived the base host timeout"
        );
        assert_eq!(runner.dropped.load(Ordering::SeqCst), 2);
    });
}

/// Transport / setup failures surface as a terminal `error` event whose
/// `error` field is a whole `AssistantMessage` — never a thrown exception and
/// never `undefined`.
const FAILURE_SOURCE: &str = r##"
    import { anthropicMessagesApi } from "@earendil-works/pi-ai/compat";

    export default function (pi) {
        pi.registerTool({
            name: "failure_probe",
            label: "failure probe",
            description: "observes a failed builtin provider stream",
            parameters: { type: "object", properties: {} },
            execute: async () => {
                const model = { id: "claude-haiku-4-5", provider: "anthropic", api: "anthropic-messages" };
                const captured = {};
                let threw = null;
                try {
                    const events = [];
                    for await (const event of anthropicMessagesApi().streamSimple(model, { messages: [] }, { apiKey: "x" })) {
                        events.push(event.type);
                        if (event.type === "error") {
                            captured.errorReason = event.reason;
                            captured.errorIsMessage = event.error && event.error.role === "assistant";
                            captured.errorMessage = event.error.errorMessage;
                            captured.errorStopReason = event.error.stopReason;
                        }
                    }
                    captured.events = events;
                } catch (err) {
                    threw = String(err);
                }
                captured.threw = threw;
                return { content: [{ type: "text", text: "ok" }], details: captured };
            },
        });
    }
"##;

#[test]
fn a_failed_stream_terminates_with_an_error_event() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("failure");
        let runner = FakeRunner::new(Script::ErrorEvent);
        let host = host_with_runner(&scratch.as_str(), runner.clone(), None).await;

        host.load(
            entry_at("failure_probe", "/tmp/pi_ai_provider/failure.mjs"),
            FAILURE_SOURCE,
        )
        .await
        .expect("load failure probe extension");

        let outcome = host
            .execute_tool("failure_probe", &json!({}).to_string())
            .await
            .expect("execute failure probe");
        assert!(!outcome.is_error, "{outcome:?}");
        let d = outcome.details.expect("details");

        assert_eq!(d["threw"], Value::Null, "a stream failure never throws");
        assert_eq!(d["events"], json!(["error"]));
        assert_eq!(d["errorReason"], "error");
        assert_eq!(d["errorIsMessage"], true);
        assert_eq!(d["errorMessage"], "boom");
        assert_eq!(d["errorStopReason"], "error");
        assert_eq!(runner.live.load(Ordering::SeqCst), 0);
        assert_eq!(runner.dropped.load(Ordering::SeqCst), 1);
    });
}

/// Without an injected runner the factories still import and are callable —
/// the `custom-provider-*` examples use them for their *types* too — but the
/// stream resolves to a named error instead of hanging forever.
const NO_RUNNER_SOURCE: &str = r##"
    import { anthropicMessagesApi, openAIResponsesApi } from "@earendil-works/pi-ai/compat";

    export default function (pi) {
        pi.registerTool({
            name: "no_runner_probe",
            label: "no runner probe",
            description: "builtins without a host runner",
            parameters: { type: "object", properties: {} },
            execute: async () => {
                const captured = {
                    factoryTypes: [typeof anthropicMessagesApi, typeof openAIResponsesApi],
                };
                const model = { id: "m", provider: "anthropic", api: "anthropic-messages" };
                const stream = anthropicMessagesApi().streamSimple(model, { messages: [] }, {});
                let reason = null;
                let message = null;
                for await (const event of stream) {
                    if (event.type === "error") {
                        reason = event.reason;
                        message = event.error && event.error.errorMessage;
                    }
                }
                captured.reason = reason;
                captured.message = message;
                const result = await stream.result();
                captured.resultHasError = typeof result.errorMessage === "string";
                return { content: [{ type: "text", text: "ok" }], details: captured };
            },
        });
    }
"##;

#[test]
fn builtins_without_a_runner_report_a_named_error() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("no-runner");
        let host = host_without_runner(&scratch.as_str()).await;

        host.load(
            entry_at("no_runner_probe", "/tmp/pi_ai_provider/no_runner.mjs"),
            NO_RUNNER_SOURCE,
        )
        .await
        .expect("load no-runner probe extension");

        let outcome = host
            .execute_tool("no_runner_probe", &json!({}).to_string())
            .await
            .expect("execute no-runner probe");
        assert!(!outcome.is_error, "{outcome:?}");
        let d = outcome.details.expect("details");

        assert_eq!(d["factoryTypes"], json!(["function", "function"]));
        assert_eq!(d["reason"], "error");
        assert!(
            d["message"]
                .as_str()
                .is_some_and(|message| message.contains("no pi-ai stream runner")),
            "the error names the missing runner: {d:?}"
        );
        assert_eq!(d["resultHasError"], true);
    });
}
