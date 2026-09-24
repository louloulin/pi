//! Event coalescer — merges high-frequency event streams so the JS bridge
//! only sees the latest payload per "in-flight cycle".
//!
//! ## Why
//!
//! Two events fire on a per-token / per-chunk cadence and would otherwise
//! flood the bridge:
//!
//! * `message_update` — every streamed text / thinking delta.
//! * `tool_execution_update` — every tool partial result.
//!
//! Sending each one as a separate JS bridge call costs ~21 µs of fixed
//! overhead (`pi_agent_rust/src/extensions/event_coalescer_impl.rs:31`).
//! The coalescer keeps at most one in-flight dispatch per event name and
//! replaces pending payloads in place, so a burst of N updates becomes a
//! burst of 1 (or at most 2 if a new payload arrives just as the in-flight
//! dispatch finishes).
//!
//! ## Port scope
//!
//! Ported and simplified from
//! `pi_agent_rust/src/extensions/event_coalescer_impl.rs`. Differences
//! from the upstream port:
//!
//! * No `Lazy` payload variant — pi-rust's `ExtensionEvent` serialises
//!   eagerly inside the JS host, so we have nothing to defer.
//! * No batch-buffer drain task — pi-rust's bridge is one
//!   `ExtensionEvent` per call; coalescing alone covers the per-token
//!   storm. Batching non-coalescing events into a single bridge call is
//!   left as a future optimisation.
//! * Tokio runtime, not `asupersync::runtime::RuntimeHandle`.
//!
//! ## Ordering
//!
//! The coalescer dispatches in the background, so it is only safe for a
//! caller that does not need the coalesced events ordered against its own
//! work. A caller that shares one stream with it — the agent event pump is
//! the reason [`EventCoalescer::flush`] exists — must call
//! [`EventCoalescer::flush`] before dispatching anything that has to arrive
//! *after* the pending updates. Without it a `message_update` parked behind
//! an in-flight dispatch lands after the `turn_end` that followed it, and a
//! plugin sees a stale delta after the turn is over.
//!
//! ## Usage
//!
//! ```ignore
//! let coalescer = EventCoalescer::new(
//!     // dispatch: send the event to the JS host
//!     |event| {
//!         let host = host.clone();
//!         Box::pin(async move {
//!             let _ = host.emit_event_with(&event, mode, has_ui, cwd).await;
//!         })
//!     },
//!     // has_subscriber: cheap fast-path filter
//!     |name| dispatcher.has_subscriber_for(name),
//!     tokio::runtime::Handle::current(),
//! );
//!
//! // From the agent loop:
//! if is_coalescable_event_name(event.name()) {
//!     coalescer.submit(event);
//! } else {
//!     coalescer.flush().await;
//!     dispatcher.deliver_event(&event).await;
//! }
//! ```

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use parking_lot::Mutex;
use pi_protocol::ExtensionEvent;
use tokio::runtime::Handle;

/// Event names that the coalescer is allowed to merge.
///
/// Other events are dispatched as-is — they fire rarely enough (agent /
/// turn / message boundaries) that coalescing would change observable
/// behaviour.
pub const COALESCABLE_EVENT_NAMES: &[&str] = &["message_update", "tool_execution_update"];

/// Whether an event name is a candidate for coalescing.
pub fn is_coalescable_event_name(name: &str) -> bool {
    COALESCABLE_EVENT_NAMES.contains(&name)
}

/// Event names the agent loop dispatches **in-line**, awaited in order.
///
/// These are the run boundaries: a plugin that sees `turn_start` then
/// `turn_end` is entitled to see them in that order, and a handler that
/// returns a value is allowed to change what happens next. Routing them
/// through the coalescer would fire-and-forget them out of order and
/// would also double-deliver them on any surface that installs a
/// coalescer *and* keeps the in-loop dispatch — the exact failure
/// `pi_agent_rust/src/extensions/event_coalescer_impl.rs:234` pins.
///
/// The complement — `message_*` and `tool_execution_*` — are observation
/// events: they reach extensions only through the coalescer.
pub const LIFECYCLE_EVENT_NAMES: &[&str] = &["agent_start", "agent_end", "turn_start", "turn_end"];

/// Whether an event name must bypass the coalescer and stay ordered.
pub fn is_lifecycle_event_name(name: &str) -> bool {
    LIFECYCLE_EVENT_NAMES.contains(&name)
}

/// The future a dispatch closure returns.
pub type DispatchFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Cloning wrapper around the caller's dispatch closure.
pub type DispatchFn = Arc<dyn Fn(ExtensionEvent) -> DispatchFuture + Send + Sync>;

/// Cloning wrapper around the caller's subscriber-check closure.
pub type HasSubscriberFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Coalesces high-frequency event streams into single dispatches.
///
/// Cheap to clone — the inner state is shared via [`Arc`].
#[derive(Clone)]
pub struct EventCoalescer {
    inner: Arc<CoalescerInner>,
}

struct CoalescerInner {
    /// Async dispatch callback.
    dispatch: DispatchFn,
    /// Fast-path subscriber check.
    has_subscriber: HasSubscriberFn,
    /// Tokio runtime used to spawn dispatch tasks.
    runtime: Handle,
    /// Shared state behind a non-reentrant mutex.
    state: Mutex<CoalescerState>,
    /// Signalled whenever [`CoalescerState::outstanding`] drops to zero, so
    /// [`EventCoalescer::flush`] can await instead of spinning.
    idle: tokio::sync::Notify,
}

struct CoalescerState {
    /// Event names with a dispatch currently in flight.
    in_flight: HashMap<String, ()>,
    /// Per-event-name payload waiting for the in-flight dispatch to pick up.
    pending: HashMap<String, ExtensionEvent>,
    /// Per-event-name coalesced count, drained via
    /// [`EventCoalescer::drain_coalesced_counts`].
    coalesced: HashMap<String, u64>,
    /// Coalescable events submitted but not yet dispatched.
    ///
    /// Incremented by `submit` for every event that will reach the
    /// dispatch closure — the pump's first payload and each payload that
    /// takes an empty `pending` slot. A payload that *replaces* an
    /// un-dispatched one does not count twice, because the one it replaced
    /// will never be dispatched.
    ///
    /// Decremented by the pump after each dispatch returns. `flush` waits
    /// for this to reach zero.
    outstanding: usize,
}

impl EventCoalescer {
    /// Build a coalescer.
    ///
    /// * `dispatch` is the per-event async work — typically a thin wrapper
    ///   over `JsExtensionHost::emit_event_with`.
    /// * `has_subscriber` is the fast-path filter — typically a thin
    ///   wrapper over `EventDispatcher::has_subscriber_for`. The coalescer
    ///   calls this on every submit; it should be cheap (set lookup).
    /// * `runtime` is the tokio runtime used to spawn dispatch tasks.
    pub fn new<D, H>(dispatch: D, has_subscriber: H, runtime: Handle) -> Self
    where
        D: Fn(ExtensionEvent) -> DispatchFuture + Send + Sync + 'static,
        H: Fn(&str) -> bool + Send + Sync + 'static,
    {
        Self {
            inner: Arc::new(CoalescerInner {
                dispatch: Arc::new(dispatch),
                has_subscriber: Arc::new(has_subscriber),
                runtime,
                state: Mutex::new(CoalescerState {
                    in_flight: HashMap::new(),
                    pending: HashMap::new(),
                    coalesced: HashMap::new(),
                    outstanding: 0,
                }),
                idle: tokio::sync::Notify::new(),
            }),
        }
    }

    /// Submit an event. Skips silently if no subscriber registered; coalesces
    /// `message_update` / `tool_execution_update`; dispatches everything else
    /// immediately.
    ///
    /// Never blocks: a coalescable event either starts a dispatch task or
    /// replaces the payload waiting for the in-flight one.
    pub fn submit(&self, event: ExtensionEvent) {
        let name = event.name().to_string();

        // Fast path: drop events nobody cares about before doing any
        // allocation (`pi_agent_rust/src/extensions/event_coalescer_impl.rs:43`).
        if !(self.inner.has_subscriber)(&name) {
            return;
        }

        if !is_coalescable_event_name(&name) {
            // Non-coalescable: dispatch immediately on the runtime.
            let dispatch = self.inner.dispatch.clone();
            self.inner.runtime.spawn(async move {
                dispatch(event).await;
            });
            return;
        }

        // Coalescable path: if a dispatch is already in flight, replace
        // the pending payload so the in-flight task sees the *latest*
        // event on completion. Either way the call returns without waiting.
        let mut first_event: Option<ExtensionEvent> = None;
        let spawn_pump = {
            let mut state = self.inner.state.lock();
            if state.in_flight.contains_key(&name) {
                // An event only adds to `outstanding` when it takes an
                // *empty* pending slot: replacing an undispatched payload
                // drops the one it replaced, so the count is unchanged.
                if state.pending.insert(name.clone(), event).is_none() {
                    state.outstanding += 1;
                }
                *state.coalesced.entry(name.clone()).or_insert(0) += 1;
                false
            } else {
                state.in_flight.insert(name.clone(), ());
                state.outstanding += 1;
                first_event = Some(event);
                true
            }
        };

        if !spawn_pump {
            return;
        }

        // We are the first submit for this event. Capture the event
        // directly into the spawned task — do NOT put it into `pending`,
        // otherwise a second submit that races with the spawn would
        // overwrite the original and the in-flight task would dispatch
        // the replacement first.
        let inner = self.inner.clone();
        let name_for_task = name;
        let first_event = first_event.expect("first_event is set whenever spawn_pump is true");
        self.inner.runtime.spawn(async move {
            let mut next_event: Option<ExtensionEvent> = Some(first_event);
            loop {
                let Some(payload) = next_event.take() else {
                    break;
                };
                (inner.dispatch)(payload).await;

                // This payload is delivered. Look for a replacement that
                // arrived while it ran, and settle `outstanding` in the
                // same critical section so `flush` can never observe a
                // half-updated count.
                let (replacement, went_idle) = {
                    let mut state = inner.state.lock();
                    state.outstanding = state.outstanding.saturating_sub(1);
                    let idle = state.outstanding == 0;
                    let taken = state.pending.remove(&name_for_task);
                    if taken.is_none() {
                        state.in_flight.remove(&name_for_task);
                    }
                    (taken, idle)
                };
                if went_idle {
                    inner.idle.notify_waiters();
                }
                next_event = replacement;
            }
        });
    }

    /// Wait until every coalescable event submitted so far has reached the
    /// dispatch closure.
    ///
    /// A caller that shares a single ordered stream with the coalescer (the
    /// agent event pump is the reason this exists) must call this before
    /// dispatching anything that has to come *after* the pending updates.
    /// Without it a `message_update` parked behind an in-flight dispatch can
    /// land after the `turn_end` / `agent_end` that followed it, and a
    /// plugin sees a stale delta after the turn is over.
    ///
    /// Returns immediately when nothing is outstanding, which is the common
    /// case: a `flush` before every non-coalescable event costs one atomic
    /// read.
    pub async fn flush(&self) {
        loop {
            // Register interest *before* reading the count, so a completion
            // between the read and the await cannot be missed. `enable()`
            // is what makes this waiter visible to `notify_waiters()`, which
            // (unlike `notify_one`) stores no permit for late arrivals.
            let notified = self.inner.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            if self.outstanding() == 0 {
                return;
            }
            notified.await;
        }
    }

    /// Coalescable events submitted but not yet dispatched.
    pub fn outstanding(&self) -> usize {
        self.inner.state.lock().outstanding
    }

    /// Drain the per-event coalesced counters.
    ///
    /// The map counts "events that would have been dispatched but were
    /// replaced by a later submit". Useful for telemetry and tests.
    pub fn drain_coalesced_counts(&self) -> HashMap<String, u64> {
        let mut state = self.inner.state.lock();
        std::mem::take(&mut state.coalesced)
    }

    /// Snapshot the in-flight event names (for tests / diagnostics).
    pub fn in_flight(&self) -> Vec<String> {
        let mut names: Vec<String> = self.inner.state.lock().in_flight.keys().cloned().collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;
    use tokio::sync::Semaphore;

    /// A coalescer plus the handles the test drives it with.
    ///
    /// `gate` starts with zero permits, so the first thing every dispatch
    /// does is block. `open()` releases exactly one parked dispatch, which
    /// makes "what is in flight right now" deterministic: an assertion
    /// right after `submit` sees the state `submit` itself installed,
    /// because `in_flight.insert` happens synchronously before the spawn.
    struct Fixture {
        coalescer: EventCoalescer,
        counter: Arc<AtomicU64>,
        gate: Arc<Semaphore>,
    }

    impl Fixture {
        fn new() -> Self {
            let gate = Arc::new(Semaphore::new(0));
            let counter = Arc::new(AtomicU64::new(0));

            let dispatch = {
                let gate = gate.clone();
                let counter = counter.clone();
                move |_event: ExtensionEvent| {
                    let gate = gate.clone();
                    let counter = counter.clone();
                    Box::pin(async move {
                        let permit = gate.acquire().await.expect("gate open");
                        permit.forget();
                        counter.fetch_add(1, Ordering::SeqCst);
                    }) as DispatchFuture
                }
            };
            let has_subscriber = |_: &str| true;

            Self {
                coalescer: EventCoalescer::new(
                    dispatch,
                    has_subscriber,
                    tokio::runtime::Handle::current(),
                ),
                counter,
                gate,
            }
        }

        /// Release one parked dispatch.
        fn open(&self) {
            self.gate.add_permits(1);
        }

        fn dispatched(&self) -> u64 {
            self.counter.load(Ordering::SeqCst)
        }

        /// Poll until `pred` holds, or panic after ~500 ms.
        async fn wait_until(&self, what: &str, mut pred: impl FnMut(&Self) -> bool) {
            for _ in 0..500 {
                if pred(self) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            panic!(
                "timed out waiting for {what} (dispatched={}, in_flight={:?})",
                self.dispatched(),
                self.coalescer.in_flight()
            );
        }
    }

    fn message_update(delta: &str) -> ExtensionEvent {
        ExtensionEvent::MessageUpdate {
            assistant_message_event: serde_json::json!({
                "type": "text_delta",
                "delta": delta,
            }),
        }
    }

    fn tool_update(tool_call_id: &str) -> ExtensionEvent {
        ExtensionEvent::ToolExecutionUpdate {
            tool_call_id: tool_call_id.into(),
            tool_name: "bash".into(),
            args: serde_json::json!({}),
            partial_result: "partial".into(),
        }
    }

    /// `message_update` and `tool_execution_update` are coalescable;
    /// everything else dispatches immediately.
    #[test]
    fn coalescable_event_names_are_recognised() {
        assert!(is_coalescable_event_name("message_update"));
        assert!(is_coalescable_event_name("tool_execution_update"));
        assert!(!is_coalescable_event_name("message_start"));
        assert!(!is_coalescable_event_name("agent_start"));
        assert!(!is_coalescable_event_name("tool_execution_start"));
        assert!(!is_coalescable_event_name("tool_execution_end"));
    }

    /// The two-route split this module depends on, pinned.
    ///
    /// Lifecycle events are dispatched to extensions from inside the agent
    /// loop, so a pump that also routed them through the coalescer would
    /// deliver them twice — and out of order, because the coalescer is
    /// fire-and-forget. Observation events are the coalescer's job. If this
    /// classification ever changes, both halves change with it and this test
    /// is where that shows up.
    /// (`pi_agent_rust/src/extensions/event_coalescer_impl.rs:234`)
    #[test]
    fn lifecycle_events_bypass_and_observation_events_do_not() {
        for name in LIFECYCLE_EVENT_NAMES {
            assert!(
                is_lifecycle_event_name(name),
                "{name} must stay a lifecycle event"
            );
            assert!(
                !is_coalescable_event_name(name),
                "{name} must not be coalescable: it is dispatched in-loop and routing it through \
                 the coalescer would double-deliver it and lose its ordering"
            );
        }
        for name in [
            "message_start",
            "message_update",
            "message_end",
            "tool_execution_start",
            "tool_execution_update",
            "tool_execution_end",
        ] {
            assert!(
                !is_lifecycle_event_name(name),
                "{name} must stay an observation event: it reaches extensions only through the \
                 coalescer"
            );
        }
    }

    /// `submit` skips entirely when no subscriber is registered, so a
    /// busy stream of `message_update` events costs nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn submit_skips_when_no_subscriber() {
        let counter = Arc::new(AtomicU64::new(0));
        let counter_for_closure = counter.clone();
        let coalescer = EventCoalescer::new(
            move |_event: ExtensionEvent| {
                let counter = counter_for_closure.clone();
                Box::pin(async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                }) as DispatchFuture
            },
            |_: &str| false,
            tokio::runtime::Handle::current(),
        );

        for _ in 0..10 {
            coalescer.submit(message_update("x"));
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(20)).await;

        assert_eq!(counter.load(Ordering::SeqCst), 0);
        assert!(coalescer.in_flight().is_empty());
        assert!(coalescer.drain_coalesced_counts().is_empty());
    }

    /// First submit dispatches. A second submit while that dispatch is
    /// in flight replaces the pending payload instead of starting a
    /// second dispatch. When the first finishes, the pump dispatches the
    /// replacement, then clears `in_flight` and exits.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn second_submit_coalesces_into_pending() {
        let f = Fixture::new();

        f.coalescer.submit(message_update("first"));
        // Synchronous: `submit` installed the in-flight marker before it
        // returned, so this assertion needs no await.
        assert_eq!(f.coalescer.in_flight(), vec!["message_update".to_string()]);

        f.coalescer.submit(message_update("second"));
        assert_eq!(
            f.coalescer
                .drain_coalesced_counts()
                .get("message_update")
                .copied(),
            Some(1)
        );
        // Still exactly one dispatch in flight — no second pump spawned.
        assert_eq!(f.coalescer.in_flight(), vec!["message_update".to_string()]);

        // Release the first dispatch; the pump then picks up "second".
        f.open();
        f.wait_until("first dispatch to complete", |f| f.dispatched() == 1).await;
        // The pump has picked up the replacement, so a dispatch is still
        // in flight and blocked on the gate.
        assert_eq!(f.coalescer.in_flight(), vec!["message_update".to_string()]);

        f.open();
        f.wait_until("pump to drain and exit", |f| {
            f.dispatched() == 2 && f.coalescer.in_flight().is_empty()
        })
        .await;
        // Exactly two dispatches: the original and one replacement.
        assert_eq!(f.dispatched(), 2);
    }

    /// A burst of submits during one in-flight dispatch collapses into a
    /// single replacement payload. Every submit that lost is counted.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn burst_of_submits_collapse_to_one_replacement() {
        let f = Fixture::new();

        f.coalescer.submit(message_update("a"));
        for n in 0..10 {
            f.coalescer.submit(message_update(&format!("burst-{n}")));
        }

        // Ten submits lost to coalescing, so the count is ten even though
        // only one payload is waiting.
        assert_eq!(
            f.coalescer
                .drain_coalesced_counts()
                .get("message_update")
                .copied(),
            Some(10)
        );
        assert_eq!(f.coalescer.in_flight(), vec!["message_update".to_string()]);

        f.open();
        f.wait_until("first dispatch to complete", |f| f.dispatched() == 1).await;
        f.open();
        f.wait_until("pump to drain and exit", |f| {
            f.dispatched() == 2 && f.coalescer.in_flight().is_empty()
        })
        .await;

        // Ten updates in, two dispatches out.
        assert_eq!(f.dispatched(), 2);
        assert!(f.coalescer.drain_coalesced_counts().is_empty());
    }

    /// `tool_execution_update` coalesces independently of
    /// `message_update` — the in-flight marker is per event name.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn different_event_names_track_independent_in_flight() {
        let f = Fixture::new();

        f.coalescer.submit(message_update("a"));
        f.coalescer.submit(tool_update("call-1"));
        assert_eq!(
            f.coalescer.in_flight(),
            vec![
                "message_update".to_string(),
                "tool_execution_update".to_string()
            ]
        );

        // Each name accumulates its own coalesced count.
        f.coalescer.submit(message_update("b"));
        f.coalescer.submit(tool_update("call-2"));
        let counts = f.coalescer.drain_coalesced_counts();
        assert_eq!(counts.get("message_update").copied(), Some(1));
        assert_eq!(counts.get("tool_execution_update").copied(), Some(1));

        // Two pumps, two dispatches each.
        for _ in 0..2 {
            f.open();
            f.open();
        }
        f.wait_until("both pumps to drain and exit", |f| {
            f.dispatched() == 4 && f.coalescer.in_flight().is_empty()
        })
        .await;
        assert_eq!(f.dispatched(), 4);
    }

    /// Non-coalescable events dispatch immediately, never enter
    /// `in_flight`, and never accumulate a coalesced count.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn non_coalescable_event_dispatches_immediately() {
        let f = Fixture::new();

        f.coalescer.submit(ExtensionEvent::AgentStart);
        // AgentStart goes straight to the dispatch closure — it never
        // touches the coalescer's bookkeeping.
        assert!(f.coalescer.in_flight().is_empty());
        assert!(f.coalescer.drain_coalesced_counts().is_empty());

        f.open();
        f.wait_until("immediate dispatch to complete", |f| f.dispatched() == 1).await;
        assert_eq!(f.dispatched(), 1);
    }

    /// `drain_coalesced_counts` returns the map and clears it, so a
    /// telemetry caller cannot double-count.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_coalesced_counts_clears_state() {
        let f = Fixture::new();

        f.coalescer.submit(message_update("a"));
        f.coalescer.submit(message_update("b"));
        f.coalescer.submit(message_update("c"));

        let counts = f.coalescer.drain_coalesced_counts();
        assert_eq!(counts.get("message_update").copied(), Some(2));
        assert!(f.coalescer.drain_coalesced_counts().is_empty());

        // Drain the parked dispatch so the runtime shuts down cleanly.
        f.open();
        f.wait_until("dispatch to complete", |f| f.dispatched() == 1).await;
    }

    /// `in_flight` is sorted, so callers (and the tests above) can
    /// compare it directly regardless of hash iteration order.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn in_flight_is_sorted() {
        let f = Fixture::new();
        f.coalescer.submit(tool_update("call-1"));
        f.coalescer.submit(message_update("a"));
        assert_eq!(
            f.coalescer.in_flight(),
            vec![
                "message_update".to_string(),
                "tool_execution_update".to_string()
            ]
        );
        f.open();
        f.open();
        f.wait_until("both dispatches to complete", |f| f.dispatched() == 2).await;
    }

    /// A flush with nothing outstanding returns at once — the common case
    /// on the pump's non-coalescable path.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn flush_returns_immediately_when_idle() {
        let f = Fixture::new();
        assert_eq!(f.coalescer.outstanding(), 0);
        f.coalescer.flush().await;
        assert_eq!(f.dispatched(), 0);
    }

    /// `flush` does not return while a coalescable event is still waiting
    /// to be dispatched. This is what keeps a `message_update` from
    /// landing after the `turn_end` that followed it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn flush_waits_for_pending_dispatch() {
        let f = Fixture::new();
        f.coalescer.submit(message_update("a"));
        // The first payload is parked on the gate; the second is pending.
        f.coalescer.submit(message_update("b"));
        assert_eq!(f.coalescer.outstanding(), 2);

        // Release both dispatches from a helper task, so `flush` has to
        // actually wait rather than spin.
        let gate = f.gate.clone();
        let opener = tokio::spawn(async move {
            for _ in 0..2 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                gate.add_permits(1);
            }
        });

        f.coalescer.flush().await;
        opener.await.expect("opener");

        // Everything submitted before the flush is delivered by the time
        // it returns — the assertion the pump relies on.
        assert_eq!(f.dispatched(), 2);
        assert_eq!(f.coalescer.outstanding(), 0);
    }

    /// `outstanding` counts events that will actually be dispatched: a
    /// submit that replaces an undispatched payload is not counted twice.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn outstanding_counts_only_events_that_will_be_dispatched() {
        let f = Fixture::new();

        f.coalescer.submit(message_update("a"));
        assert_eq!(f.coalescer.outstanding(), 1);
        // Takes the empty pending slot: both `a` and `b` will dispatch.
        f.coalescer.submit(message_update("b"));
        assert_eq!(f.coalescer.outstanding(), 2);
        // Replaces `b` — still two dispatches, not three.
        f.coalescer.submit(message_update("c"));
        assert_eq!(f.coalescer.outstanding(), 2);
        f.coalescer.submit(message_update("d"));
        assert_eq!(f.coalescer.outstanding(), 2);

        let gate = f.gate.clone();
        let opener = tokio::spawn(async move {
            for _ in 0..2 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                gate.add_permits(1);
            }
        });
        f.coalescer.flush().await;
        opener.await.expect("opener");

        // Exactly two dispatches — the original and the last replacement.
        assert_eq!(f.dispatched(), 2);
        assert_eq!(f.coalescer.outstanding(), 0);
        assert!(f.coalescer.in_flight().is_empty());
    }

    /// A flush only waits for what was submitted *before* it: work
    /// submitted afterwards is not its problem, and does not deadlock it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn flush_does_not_wait_for_later_submissions() {
        let f = Fixture::new();
        f.coalescer.submit(message_update("first"));
        assert_eq!(f.coalescer.outstanding(), 1);

        let gate = f.gate.clone();
        let opener = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            gate.add_permits(1);
        });

        f.coalescer.flush().await;
        opener.await.expect("opener");
        assert_eq!(f.dispatched(), 1);

        // A submit *after* the flush starts a fresh cycle.
        f.coalescer.submit(message_update("second"));
        f.open();
        f.wait_until("second dispatch to complete", |f| f.dispatched() == 2).await;
    }
}
