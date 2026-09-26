//! Multi-subscriber pubsub for [`ExtensionEvent`].
//!
//! The agent loop's [`ExtensionEventMapper`](crate::extensions::events::ExtensionEventMapper)
//! is a translator — it turns each [`AgentEvent`](pi_agent_core::AgentEvent) into
//! the wire-format [`ExtensionEvent`]s the JS host expects. Today the
//! `interactive` runtime forwards those events to *exactly one*
//! subscriber: the JS host carried by [`ExtensionRuntime`].
//!
//! Anything Rust-side (the TUI's working-message transformer, a
//! `/commands` listener, the prompt observer, a test harness, telemetry)
//! that wants to see the same fan-out had to either poll or read the
//! mapper directly — neither of which is a real pubsub.
//!
//! [`EventBus`] fills that gap. It is a tiny, lock-based fan-out:
//!
//! * `subscribe(name, callback)` returns a [`SubscriptionId`].
//! * `dispatch(event)` invokes every callback registered for the
//!   event's [`ExtensionEvent::name`].
//! * `unsubscribe(id)` removes one subscription.
//!
//! The bus is intentionally lock-only and in-process: it is meant to be
//! constructed per-interaction (cheap, copy-free) and torn down with the
//! session. Threading it through [`ExtensionRuntime`] would force every
//! extension host to keep an extra `Arc<EventBus>` it doesn't need.
//!
//! The pump in [`crate::interactive::run_extension_event_pump`] owns the
//! bus for the lifetime of one interactive session and exposes it via
//! the agent loop, which in turn hands the bus to anything that wants
//! to register a subscription.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pi_protocol::ExtensionEvent;

type Callback = Arc<dyn Fn(&ExtensionEvent) + Send + Sync + 'static>;

/// Identifier returned by [`EventBus::subscribe`]. Pass to
/// [`EventBus::unsubscribe`] to remove the matching subscription.
///
/// IDs are unique within one [`EventBus`] and are not reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(u64);

impl SubscriptionId {
    /// Raw numeric value, for debugging only.
    pub fn get(self) -> u64 {
        self.0
    }
}

struct Entry {
    callback: Callback,
}

/// Multi-subscriber pubsub keyed by [`ExtensionEvent::name`].
///
/// `subscribe` is `O(1)` amortised (one Mutex acquisition); `dispatch`
/// is `O(n)` over the matching callback list. The list is drained from
/// the Mutex before invocation, so a callback that takes its time (or
/// re-enters the bus) cannot deadlock the dispatcher.
#[derive(Default)]
pub struct EventBus {
    next_id: AtomicU64,
    by_name: Mutex<HashMap<String, Vec<(SubscriptionId, Callback)>>>,
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let by_name = self.by_name.lock().expect("event bus poisoned");
        f.debug_struct("EventBus")
            .field("subscriptions", &by_name.values().map(|v| v.len()).sum::<usize>())
            .finish()
    }
}

impl EventBus {
    /// Build an empty bus.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `callback` under `name`. The callback fires for every
    /// [`ExtensionEvent`] whose [`ExtensionEvent::name`] matches the
    /// string passed here.
    ///
    /// The match is exact — no wildcard / glob / prefix support — because
    /// the JS host already canonicalises upstream's aliases (see
    /// `canonical_event_name`) and Rust callers can pre-translate. Exact
    /// matches keep the dispatch table small and the intent obvious.
    pub fn subscribe<F>(&self, name: impl Into<String>, callback: F) -> SubscriptionId
    where
        F: Fn(&ExtensionEvent) + Send + Sync + 'static,
    {
        let id = SubscriptionId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let callback: Callback = Arc::new(callback);
        let mut by_name = self.by_name.lock().expect("event bus poisoned");
        by_name
            .entry(name.into())
            .or_default()
            .push((id, Arc::clone(&callback)));
        id
    }

    /// Remove the subscription with this id. Returns `true` when one was
    /// removed, `false` when the id was unknown (already unsubscribed,
    /// or never existed).
    pub fn unsubscribe(&self, id: SubscriptionId) -> bool {
        let mut by_name = self.by_name.lock().expect("event bus poisoned");
        let mut removed = false;
        let empty_keys: Vec<String> = by_name
            .iter_mut()
            .filter_map(|(name, list)| {
                let before = list.len();
                list.retain(|(sid, _)| *sid != id);
                if list.len() != before {
                    removed = true;
                }
                if list.is_empty() {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect();
        for key in empty_keys {
            by_name.remove(&key);
        }
        removed
    }

    /// Number of currently registered subscriptions.
    pub fn len(&self) -> usize {
        let by_name = self.by_name.lock().expect("event bus poisoned");
        by_name.values().map(|v| v.len()).sum()
    }

    /// True iff no subscriptions are registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// True iff at least one subscription is registered for `name`.
    pub fn has_subscriber_for(&self, name: &str) -> bool {
        let by_name = self.by_name.lock().expect("event bus poisoned");
        by_name.contains_key(name)
    }

    /// Snapshot of the registered event names (useful for tests and
    /// for the runtime to decide whether to wire up the agent fan-out at
    /// all).
    pub fn subscribed_events(&self) -> Vec<String> {
        let by_name = self.by_name.lock().expect("event bus poisoned");
        let mut names: Vec<String> = by_name.keys().cloned().collect();
        names.sort();
        names
    }

    /// Dispatch `event` to every subscription whose name matches
    /// [`ExtensionEvent::name`]. Subscribers that panic are isolated:
    /// the rest of the fan-out continues, and the panicking subscription
    /// is dropped (so a buggy observer does not crash the pump on the
    /// next event).
    pub fn dispatch(&self, event: &ExtensionEvent) {
        let name = event.name().to_string();
        let snapshot: Vec<(SubscriptionId, Callback)> = {
            let by_name = self.by_name.lock().expect("event bus poisoned");
            by_name
                .get(&name)
                .map(|list| list.iter().map(|(id, c)| (*id, Arc::clone(c))).collect())
                .unwrap_or_default()
        };
        for (id, callback) in snapshot {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                callback(event);
            }));
            if result.is_err() {
                self.unsubscribe(id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_protocol::{AssistantMessage, Content, Role, StopReason, Usage};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn assistant(text: &str) -> AssistantMessage {
        AssistantMessage {
            model: "faux".into(),
            content: vec![Content::text(text)],
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
            error_message: None,
        }
    }

    fn turn_start_payload() -> ExtensionEvent {
        ExtensionEvent::TurnStart {
            turn_index: 0,
            timestamp: 1,
        }
    }

    fn agent_end_payload() -> ExtensionEvent {
        ExtensionEvent::AgentEnd { messages: vec![] }
    }

    #[test]
    fn subscribe_then_dispatch_invokes_callback() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_for_cb = Arc::clone(&counter);
        bus.subscribe("turn_start", move |_event| {
            counter_for_cb.fetch_add(1, Ordering::Relaxed); ();
        });
        assert!(bus.has_subscriber_for("turn_start"));
        assert!(bus.has_subscriber_for("turn_start"));
        bus.dispatch(&turn_start_payload());
        bus.dispatch(&turn_start_payload());
        assert_eq!(counter.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn dispatch_to_unregistered_name_is_a_noop() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_for_cb = Arc::clone(&counter);
        bus.subscribe("turn_start", move |_event| {
            counter_for_cb.fetch_add(1, Ordering::Relaxed); ();
        });
        assert!(bus.has_subscriber_for("turn_start"));
        bus.dispatch(&agent_end_payload());
        assert_eq!(counter.load(Ordering::Relaxed), 0);
        assert!(!bus.has_subscriber_for("agent_end"));
    }

    #[test]
    fn multiple_subscribers_all_fire() {
        let bus = EventBus::new();
        let a = Arc::new(AtomicUsize::new(0));
        let b = Arc::new(AtomicUsize::new(0));
        let ac = Arc::clone(&a);
        let bc = Arc::clone(&b);
        bus.subscribe("turn_start", move |_| { let _ = ac.fetch_add(1, Ordering::Relaxed); });
        bus.subscribe("turn_start", move |_| { let _ = bc.fetch_add(1, Ordering::Relaxed); });
        bus.dispatch(&turn_start_payload());
        assert_eq!(a.load(Ordering::Relaxed), 1);
        assert_eq!(b.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn unsubscribe_removes_one_subscription() {
        let bus = EventBus::new();
        let a = Arc::new(AtomicUsize::new(0));
        let b = Arc::new(AtomicUsize::new(0));
        let ac = Arc::clone(&a);
        let bc = Arc::clone(&b);
        let id_a = bus.subscribe("turn_start", move |_| { let _ = ac.fetch_add(1, Ordering::Relaxed); });
        bus.subscribe("turn_start", move |_| { let _ = bc.fetch_add(1, Ordering::Relaxed); });
        bus.dispatch(&turn_start_payload());
        assert_eq!(a.load(Ordering::Relaxed), 1);
        assert_eq!(b.load(Ordering::Relaxed), 1);
        assert!(bus.unsubscribe(id_a));
        bus.dispatch(&turn_start_payload());
        assert_eq!(a.load(Ordering::Relaxed), 1, "unsubscribed A does not fire");
        assert_eq!(b.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn unsubscribe_unknown_returns_false() {
        let bus = EventBus::new();
        assert!(!bus.unsubscribe(SubscriptionId(42)));
    }

    #[test]
    fn subscribed_events_lists_keys_sorted() {
        let bus = EventBus::new();
        bus.subscribe("agent_end", |_| {});
        bus.subscribe("turn_start", |_| {});
        bus.subscribe("message_update", |_| {});
        assert_eq!(
            bus.subscribed_events(),
            vec![
                "agent_end".to_string(),
                "message_update".to_string(),
                "turn_start".to_string(),
            ]
        );
        assert_eq!(bus.len(), 3);
        assert!(!bus.is_empty());
        let id = bus.subscribe("turn_start", |_| {});
        bus.unsubscribe(id);
        assert_eq!(bus.len(), 3, "adding + removing leaves three subscribers");
    }

    #[test]
    fn panicking_subscriber_is_isolated_and_dropped() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_for_cb = Arc::clone(&counter);
        // First subscription panics.
        bus.subscribe("turn_start", |_| panic!("boom"));
        // Second subscription tracks whether it ran.
        bus.subscribe("turn_start", move |_| {
            counter_for_cb.fetch_add(1, Ordering::Relaxed);
        });
        bus.dispatch(&turn_start_payload());
        assert_eq!(
            counter.load(Ordering::Relaxed),
            1,
            "the second subscriber runs even though the first panicked"
        );
        // The panicking subscription has been removed; a second dispatch
        // invokes the survivor only.
        bus.dispatch(&turn_start_payload());
        assert_eq!(counter.load(Ordering::Relaxed), 2);
        assert_eq!(bus.len(), 1, "the panicking subscriber is dropped");
    }

    #[test]
    fn callbacks_observe_event_payload() {
        let bus = EventBus::new();
        let observed: Arc<Mutex<Vec<ExtensionEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&observed);
        bus.subscribe("agent_end", move |event| {
            observed.lock().unwrap().push(event.clone());
        });
        let payload = agent_end_payload();
        bus.dispatch(&payload);
        let observed = sink.lock().unwrap();
        assert_eq!(observed.len(), 1);
        assert!(matches!(observed[0], ExtensionEvent::AgentEnd { .. }));
    }

    #[test]
    fn subscription_ids_are_unique() {
        let bus = EventBus::new();
        let a = bus.subscribe("turn_start", |_| {});
        let b = bus.subscribe("turn_start", |_| {});
        assert_ne!(a, b);
    }

    // Used by the panic-isolation test; silences the unused-import lint
    // when the test module compiles with `--all-features`.
    #[allow(dead_code)]
    fn _ensure_assistant_helper_compiles() -> AssistantMessage {
        assistant("unused")
    }
}