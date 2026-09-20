//! Built-in `pi-ai` provider bridge (LUM-1180).
//!
//! Upstream extensions reach the built-in model adapters through
//! `@earendil-works/pi-ai/compat`:
//!
//! ```ts
//! import { anthropicMessagesApi, openAIResponsesApi } from "@earendil-works/pi-ai/compat";
//! const stream = anthropicMessagesApi().streamSimple(model, context, options);
//! for await (const event of stream) { /* … */ }
//! ```
//!
//! Those factories normally load a TypeScript provider module that talks to
//! the network. The shim cannot do that, and `pi-extensions` must not depend
//! on `pi-ai` (the crate that owns the providers), so the host exposes a
//! *streaming* bridge and lets the embedding crate inject the runner:
//!
//! * [`PiAiStreamRunner`] is implemented by `pi-coding-agent` and installed
//!   through [`HostOptions::pi_ai_stream_runner`](crate::HostOptions::pi_ai_stream_runner);
//! * three host imports carry the traffic —
//!   `host_pi_ai_stream_start(requestJson)` resolves to `{ok, id}`,
//!   `host_pi_ai_stream_next(id)` resolves to one upstream-shaped
//!   `AssistantMessageEvent` at a time, and `host_pi_ai_stream_cancel(id)`
//!   aborts and releases a stream;
//! * the shim's `AssistantMessageEventStream` (pure JS) collects the events
//!   and resolves `result()` with the terminal `AssistantMessage`.
//!
//! Cancellation mirrors `pi.exec` / `fetch`: the shim calls
//! `host_pi_ai_stream_cancel` when the extension's `AbortSignal` fires, which
//! trips the runner's [`CancellationToken`] *and* drops the provider stream
//! once the in-flight `next` returns. A live stream also raises the host
//! per-call deadline to [`PI_AI_STREAM_TIMEOUT`], because a single
//! `execute`-tool call can drive a whole model turn — the default host
//! timeout would otherwise cut the stream off after five seconds.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::Stream;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

/// Longest a single built-in provider stream may keep a host call alive.
///
/// The default host timeout (5s) is far below a realistic model turn, so a
/// live stream raises the deadline to this bound. Cancellation through the
/// extension's `AbortSignal` is the normal way to stop earlier.
pub const PI_AI_STREAM_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// One built-in provider request, exactly as the shim serialises it.
///
/// `model`, `context` and `options` keep the upstream JS shapes; the injected
/// runner converts them (see `pi_ai::ext_bridge`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiAiStreamRequest {
    /// Upstream api id — `anthropic-messages` or `openai-responses`.
    pub api: String,
    /// Upstream `Model` object.
    #[serde(default)]
    pub model: serde_json::Value,
    /// Upstream `Context` object.
    #[serde(default)]
    pub context: serde_json::Value,
    /// Upstream `SimpleStreamOptions` / `StreamOptions` object.
    #[serde(default)]
    pub options: serde_json::Value,
}

/// Boxed stream of upstream-shaped `AssistantMessageEvent` JSON objects.
///
/// The final item is the terminal `done` / `error` event; the runner must
/// never end the stream without one.
pub type PiAiEventStream = Pin<Box<dyn Stream<Item = serde_json::Value> + Send>>;

/// Runner behind the built-in `pi-ai` provider factories.
///
/// `pi-extensions` cannot depend on the crate that owns the concrete
/// providers, so the embedding crate injects one through
/// [`HostOptions::pi_ai_stream_runner`](crate::HostOptions::pi_ai_stream_runner).
/// Without a runner the factories still import and return a stream, but the
/// stream terminates with a named error event.
pub trait PiAiStreamRunner: Send + Sync + 'static {
    /// Start one provider stream.
    ///
    /// `cancel` fires when the extension's `AbortSignal` aborts (or the host
    /// call deadline expires). The runner is expected to honour it and end
    /// the returned stream with an aborted terminal event; the host also
    /// drops the stream, so a runner that ignores the token still stops.
    fn start<'a>(
        &'a self,
        request: PiAiStreamRequest,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<PiAiEventStream, String>> + Send + 'a>>;
}

/// Live built-in provider streams, keyed by the id the shim is handed.
///
/// The bridge owns the cancel / deadline interaction with the rest of the
/// host; call sites are the three `host_pi_ai_stream_*` imports.
#[derive(Clone)]
pub(crate) struct PiAiStreamBridge {
    state: Arc<Mutex<PiAiStreamState>>,
    /// Wall-clock nanos of the furthest live-stream deadline, in the same
    /// encoding as the host's own deadline atomic.
    deadline_nanos: Arc<AtomicU64>,
}

#[derive(Default)]
struct PiAiStreamState {
    /// Monotonic id source. Ids are handed to JS only after registration, so
    /// a cancel can never target an id before its stream exists.
    next_id: u64,
    live: HashMap<u64, Arc<PiAiStreamEntry>>,
    /// Furthest deadline armed by a live stream.
    deadline: Option<Instant>,
}

struct PiAiStreamEntry {
    /// The provider stream, behind an async mutex so `next` can hold it
    /// across the `await` without blocking `cancel` (which only needs the
    /// token and the map).
    events: AsyncMutex<PiAiEventStream>,
    cancel: CancellationToken,
}

impl PiAiStreamBridge {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(PiAiStreamState::default())),
            deadline_nanos: Arc::new(AtomicU64::new(u64::MAX)),
        }
    }

    /// Share the deadline atomic with the JS interrupt handler.
    pub(crate) fn deadline_nanos(&self) -> Arc<AtomicU64> {
        self.deadline_nanos.clone()
    }

    /// Furthest live-stream deadline, if any.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.state.lock().deadline
    }

    /// Clear the live-stream deadline at the end of a host call.
    pub(crate) fn reset_deadline(&self) {
        self.state.lock().deadline = None;
        self.deadline_nanos.store(u64::MAX, Ordering::Relaxed);
    }

    /// Cancel every live stream (host deadline fired, or the host is
    /// shutting down).
    pub(crate) fn cancel_all(&self) {
        let entries: Vec<Arc<PiAiStreamEntry>> = self
            .state
            .lock()
            .live
            .drain()
            .map(|(_, entry)| entry)
            .collect();
        for entry in entries {
            entry.cancel.cancel();
        }
    }

    /// `host_pi_ai_stream_start(requestJson)`.
    pub(crate) async fn start(
        &self,
        runner: Option<Arc<dyn PiAiStreamRunner>>,
        request_json: &str,
    ) -> String {
        let Some(runner) = runner else {
            return serde_json::json!({
                "ok": false,
                "error": "the extension host has no pi-ai stream runner; the built-in provider factories cannot reach a model here",
            })
            .to_string();
        };
        let request: PiAiStreamRequest = match serde_json::from_str(request_json) {
            Ok(request) => request,
            Err(error) => {
                return serde_json::json!({
                    "ok": false,
                    "error": format!("invalid pi-ai stream request: {error}"),
                })
                .to_string()
            }
        };
        let id = {
            let mut state = self.state.lock();
            state.next_id = state.next_id.saturating_add(1);
            state.next_id
        };
        let cancel = CancellationToken::new();
        match runner.start(request, cancel.clone()).await {
            Ok(events) => {
                let entry = Arc::new(PiAiStreamEntry {
                    events: AsyncMutex::new(events),
                    cancel,
                });
                {
                    let mut state = self.state.lock();
                    state.live.insert(id, entry);
                    let deadline = Instant::now() + PI_AI_STREAM_TIMEOUT;
                    let needs_update = match state.deadline {
                        Some(current) => deadline > current,
                        None => true,
                    };
                    if needs_update {
                        state.deadline = Some(deadline);
                        self.deadline_nanos
                            .store(wall_clock_nanos(deadline), Ordering::Relaxed);
                    }
                }
                serde_json::json!({"ok": true, "id": id}).to_string()
            }
            Err(error) => serde_json::json!({"ok": false, "error": error}).to_string(),
        }
    }

    /// `host_pi_ai_stream_next(id)`.
    ///
    /// Resolves with `{ok:true,done:false,event}` for one event,
    /// `{ok:true,done:true}` when the stream ended (cancellation included) and
    /// `{ok:false,error}` for an unknown id. Never rejects, like `host_exec`.
    pub(crate) async fn next(&self, id: u64) -> String {
        let entry = { self.state.lock().live.get(&id).cloned() };
        let Some(entry) = entry else {
            return serde_json::json!({
                "ok": false,
                "error": format!("no live pi-ai stream with id {id}"),
            })
            .to_string();
        };
        let mut guard = entry.events.lock().await;
        let item = tokio::select! {
            biased;
            () = entry.cancel.cancelled() => {
                drop(guard);
                self.unregister(id);
                return serde_json::json!({"ok": true, "done": true, "cancelled": true}).to_string();
            }
            item = futures::StreamExt::next(&mut *guard) => item,
        };
        drop(guard);
        match item {
            Some(event) => {
                serde_json::json!({"ok": true, "done": false, "event": event}).to_string()
            }
            None => {
                self.unregister(id);
                serde_json::json!({"ok": true, "done": true}).to_string()
            }
        }
    }

    /// `host_pi_ai_stream_cancel(id)` — drop the stream and trip its token.
    pub(crate) fn cancel(&self, id: u64) {
        let entry = self.state.lock().live.remove(&id);
        if let Some(entry) = entry {
            entry.cancel.cancel();
        }
    }

    fn unregister(&self, id: u64) {
        // The deadline is a high-water mark and is cleared by
        // [`PiAiStreamBridge::reset_deadline`] at the end of the host call,
        // so a finished stream does not lower it here.
        self.state.lock().live.remove(&id);
    }
}

/// Convert an [`Instant`] to wall-clock nanos-since-epoch matching what
/// [`SystemTime`] produces, so the interrupt handler compares like values.
fn wall_clock_nanos(target: Instant) -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let delta = target.saturating_duration_since(Instant::now()).as_nanos() as u64;
    now.saturating_add(delta)
}
