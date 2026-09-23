//! Replicated state: a mutable producer value, live read-only views and cold replicas.
//!
//! Upstream `services/state.ts` is three things at once: `MutableReplicatedStateImpl` (the
//! producer), the `ReplicatedState` view shared with consumers, and `ReplicatedStateReplica` (the
//! cold consumer side that hydrates from a delta snapshot and applies updates). All three are pure
//! data structures — no transport, no timers — so they belong to the core layer.
//!
//! Producer semantics that must stay exact:
//!
//! * the published value equals the initial value from construction on (upstream applies
//!   `track(initial).flush()`, a single `r` base op, to `undefined`), at sequence `0`;
//! * `publish` flushes a diff and *only* bumps the sequence and notifies when the diff is
//!   non-empty, so a no-op publish is silent;
//! * `subscribe` first publishes (usually a no-op), then registers, then immediately delivers the
//!   current value with [`DeliveryKind::Hydrate`].

use std::sync::Arc;

use parking_lot::{Mutex, MutexGuard};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::context::{background_context, Context};
use crate::delta::{apply_immutable, diff, is_base, Op};
use crate::json::JsonValue;
use crate::types::{
    DeliveryKind, FacetError, ReplicatedStateDelivery, ReplicatedStateListener,
    ReplicatedStateSourceListener,
};

struct ListenerEntry<T> {
    id: u64,
    listener: ReplicatedStateListener<T>,
}

struct SourceListenerEntry {
    id: u64,
    listener: ReplicatedStateSourceListener,
}

#[derive(Default)]
struct SourceListeners {
    entries: Mutex<Vec<SourceListenerEntry>>,
    next_id: Mutex<u64>,
}

impl SourceListeners {
    fn register(&self, listener: ReplicatedStateSourceListener) -> u64 {
        let mut next = self.next_id.lock();
        *next += 1;
        let id = *next;
        self.entries
            .lock()
            .push(SourceListenerEntry { id, listener });
        id
    }

    fn remove(&self, id: u64) {
        self.entries.lock().retain(|entry| entry.id != id);
    }

    fn snapshot(&self) -> Vec<ReplicatedStateSourceListener> {
        self.entries
            .lock()
            .iter()
            .map(|entry| Arc::clone(&entry.listener))
            .collect()
    }
}

struct SharedState<T> {
    published: Mutex<Option<T>>,
    sequence: Mutex<u64>,
    listeners: Mutex<Vec<ListenerEntry<T>>>,
    next_listener_id: Mutex<u64>,
    sources: Arc<SourceListeners>,
}

impl<T> SharedState<T> {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            published: Mutex::new(None),
            sequence: Mutex::new(0),
            listeners: Mutex::new(Vec::new()),
            next_listener_id: Mutex::new(0),
            sources: Arc::new(SourceListeners::default()),
        })
    }

    fn next_id(&self) -> u64 {
        let mut next = self.next_listener_id.lock();
        *next += 1;
        *next
    }

    /// Registers `listener` and hydrates it when a value is already published.
    fn register(shared: &Arc<Self>, listener: ReplicatedStateListener<T>) -> StateSubscription<T>
    where
        T: Clone,
    {
        let id = shared.next_id();
        shared.listeners.lock().push(ListenerEntry { id, listener });
        if let Some(value) = shared.published.lock().clone() {
            let delivery = ReplicatedStateDelivery::new(DeliveryKind::Hydrate, shared.sequence());
            shared.deliver(&value, background_context(), delivery);
        }
        StateSubscription {
            shared: Arc::clone(shared),
            id,
        }
    }

    fn sequence(&self) -> u64 {
        *self.sequence.lock()
    }

    fn deliver(&self, value: &T, context: &Context, delivery: ReplicatedStateDelivery) {
        let listeners: Vec<ReplicatedStateListener<T>> = self
            .listeners
            .lock()
            .iter()
            .map(|entry| Arc::clone(&entry.listener))
            .collect();
        for listener in listeners {
            listener(value, context, delivery);
        }
    }
}

/// A read-only, live view of a producer's published value.
///
/// Obtained from [`MutableReplicatedState::replica`]. The view shares the producer's published
/// slot, so it always observes the latest value and can subscribe independently.
pub struct ReplicatedState<T> {
    shared: Arc<SharedState<T>>,
}

impl<T> Clone for ReplicatedState<T> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> ReplicatedState<T>
where
    T: Clone,
{
    /// The currently published value, if a value has been published.
    pub fn value(&self) -> Option<T> {
        self.shared.published.lock().clone()
    }
}

impl<T> ReplicatedState<T> {
    /// The sequence of the currently published value.
    pub fn sequence(&self) -> u64 {
        self.shared.sequence()
    }

    /// Registers `listener` and immediately delivers the current value with a hydrate delivery.
    ///
    /// The returned subscription removes the listener when dropped.
    pub fn subscribe<F>(&self, listener: F) -> StateSubscription<T>
    where
        T: Clone,
        F: Fn(&T, &Context, ReplicatedStateDelivery) + Send + Sync + 'static,
    {
        SharedState::register(
            &self.shared,
            Arc::new(move |value, context, delivery| listener(value, context, delivery)),
        )
    }
}

/// Removes a replicated-state listener on drop.
pub struct StateSubscription<T> {
    shared: Arc<SharedState<T>>,
    id: u64,
}

impl<T> StateSubscription<T> {
    /// Removes the listener now; dropping the subscription afterwards is a no-op.
    pub fn unsubscribe(mut self) {
        self.remove();
    }

    fn remove(&mut self) {
        if self.id == 0 {
            return;
        }
        self.shared
            .listeners
            .lock()
            .retain(|entry| entry.id != self.id);
        self.id = 0;
    }
}

impl<T> Drop for StateSubscription<T> {
    fn drop(&mut self) {
        self.remove();
    }
}

impl<T> std::fmt::Debug for StateSubscription<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StateSubscription")
            .field("listener_id", &self.id)
            .finish()
    }
}

/// Removes a source listener on drop.
pub struct StateSourceSubscription {
    sources: Arc<SourceListeners>,
    id: u64,
}

impl StateSourceSubscription {
    /// Removes the listener now.
    pub fn unsubscribe(mut self) {
        self.remove();
    }

    fn remove(&mut self) {
        if self.id == 0 {
            return;
        }
        self.sources.remove(self.id);
        self.id = 0;
    }
}

impl Drop for StateSourceSubscription {
    fn drop(&mut self) {
        self.remove();
    }
}

impl std::fmt::Debug for StateSourceSubscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StateSourceSubscription")
            .field("listener_id", &self.id)
            .finish()
    }
}

/// The producer side of a replicated state: mutate, then [`MutableReplicatedState::publish`].
///
/// Upstream `MutableReplicatedStateImpl<T extends object>`; the `object` bound exists because the
/// delta tracker proxies property access. Rust has no proxy, so this port exposes the mutable
/// value through [`MutableReplicatedState::state_mut`] and diffs the whole value on publish. The
/// observable contract (ops, sequence, deliveries) is unchanged; see `delta::tracker` for the
/// documented cost model.
pub struct MutableReplicatedState<T> {
    view: ReplicatedState<T>,
    state: Mutex<T>,
    published_json: Mutex<JsonValue>,
    last_batch: Mutex<Vec<Op>>,
}

impl<T> MutableReplicatedState<T>
where
    T: Clone + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    /// Creates a producer whose published value starts at `initial`.
    ///
    /// Fails only when `initial` cannot be represented as JSON.
    pub fn new(initial: T) -> Result<Self, FacetError> {
        let json = to_json(&initial)?;
        let state = Self {
            view: ReplicatedState {
                shared: SharedState::new(),
            },
            state: Mutex::new(initial.clone()),
            published_json: Mutex::new(json.clone()),
            last_batch: Mutex::new(vec![Op::Replace(json)]),
        };
        *state.view.shared.published.lock() = Some(initial);
        Ok(state)
    }

    /// A read-only live view of this producer.
    pub fn replica(&self) -> ReplicatedState<T> {
        self.view.clone()
    }

    /// The currently published value.
    pub fn value(&self) -> T {
        self.state.lock().clone()
    }

    /// The currently published sequence number.
    pub fn sequence(&self) -> u64 {
        self.view.sequence()
    }

    /// The delta batch produced by the most recent non-empty publish (a base `r` op initially).
    pub fn last_batch(&self) -> Vec<Op> {
        self.last_batch.lock().clone()
    }

    /// Locks the mutable value for in-place edits.
    ///
    /// Every edit is picked up by the next [`MutableReplicatedState::publish`].
    pub fn state_mut(&self) -> MutexGuard<'_, T> {
        self.state.lock()
    }

    /// Publishes the diff between the last published value and the current mutable value.
    ///
    /// A publish that produces no ops is a no-op: sequence and listeners are untouched.
    pub fn publish(&self, context: &Context) -> Result<(), FacetError> {
        let next_json = to_json(&*self.state.lock())?;
        let ops = {
            let mut published = self.published_json.lock();
            let ops = diff(&published, &next_json);
            if ops.is_empty() {
                return Ok(());
            }
            *published = next_json;
            ops
        };
        *self.last_batch.lock() = ops.clone();
        let sequence = {
            let mut current = self.view.shared.sequence.lock();
            *current += 1;
            *current
        };
        let value = self.state.lock().clone();
        *self.view.shared.published.lock() = Some(value.clone());
        for listener in self.view.shared.sources.snapshot() {
            listener(&ops, sequence, context);
        }
        self.view.shared.deliver(
            &value,
            context,
            ReplicatedStateDelivery::new(DeliveryKind::Update, sequence),
        );
        Ok(())
    }

    /// Registers `listener`, publishes first, then immediately delivers a hydrate delivery.
    pub fn subscribe<F>(&self, listener: F) -> Result<StateSubscription<T>, FacetError>
    where
        T: Clone,
        F: Fn(&T, &Context, ReplicatedStateDelivery) + Send + Sync + 'static,
    {
        let context = background_context();
        self.publish(context)?;
        Ok(SharedState::register(
            &self.view.shared,
            Arc::new(move |value, context, delivery| listener(value, context, delivery)),
        ))
    }

    /// Registers a source listener notified with the ops and sequence of each publish.
    pub fn subscribe_source<F>(&self, listener: F) -> StateSourceSubscription
    where
        F: Fn(&[Op], u64, &Context) + Send + Sync + 'static,
    {
        let sources = Arc::clone(&self.view.shared.sources);
        let id = sources.register(Arc::new(move |ops, sequence, context| {
            listener(ops, sequence, context)
        }));
        StateSourceSubscription { sources, id }
    }
}

/// A cold, transport-fed replica of a replicated state.
///
/// Upstream `ReplicatedStateReplica`: it starts empty and is filled by a base `r` snapshot
/// ([`StateReplica::hydrate`]) followed by strictly consecutive updates ([`StateReplica::update`]).
/// An update before hydration is refused, a non-base hydrate is refused, and a sequence gap clears
/// the replica.
pub struct StateReplica<T> {
    shared: Arc<SharedState<T>>,
}

impl<T> Default for StateReplica<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> StateReplica<T> {
    /// Creates an empty replica.
    pub fn new() -> Self {
        Self {
            shared: SharedState::new(),
        }
    }

    /// The replicated value, if the replica has been hydrated.
    pub fn value(&self) -> Option<T>
    where
        T: Clone,
    {
        self.shared.published.lock().clone()
    }

    /// The replicated sequence, if the replica has been hydrated.
    pub fn sequence(&self) -> Option<u64> {
        if self.shared.published.lock().is_none() {
            return None;
        }
        Some(self.shared.sequence())
    }

    /// Registers `listener`; an already hydrated replica delivers immediately.
    pub fn subscribe<F>(&self, listener: F) -> StateSubscription<T>
    where
        T: Clone,
        F: Fn(&T, &Context, ReplicatedStateDelivery) + Send + Sync + 'static,
    {
        SharedState::register(
            &self.shared,
            Arc::new(move |value, context, delivery| listener(value, context, delivery)),
        )
    }

    /// Drops the replicated value and sequence, forcing the next message to be a hydrate.
    pub fn clear(&self) {
        *self.shared.published.lock() = None;
        *self.shared.sequence.lock() = 0;
    }
}

impl<T> StateReplica<T>
where
    T: Clone + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    /// Applies a base snapshot batch. The batch must be a base batch (`r` first).
    pub fn hydrate(&self, sequence: u64, ops: &[Op], context: &Context) -> Result<(), FacetError> {
        if !is_base(ops) {
            return Err(FacetError::new(
                "Replicated state snapshot is not a base operation batch",
            ));
        }
        let json = apply_immutable(None, ops)?;
        let value: T = from_json(json, "snapshot")?;
        *self.shared.published.lock() = Some(value.clone());
        *self.shared.sequence.lock() = sequence;
        self.shared.deliver(
            &value,
            context,
            ReplicatedStateDelivery::new(DeliveryKind::Hydrate, sequence),
        );
        Ok(())
    }

    /// Applies an incremental update. Sequences must be consecutive.
    pub fn update(&self, sequence: u64, ops: &[Op], context: &Context) -> Result<(), FacetError> {
        let current = self.shared.published.lock().clone();
        let current_sequence = self.shared.sequence();
        let Some(current) = current else {
            return Err(FacetError::new(
                "Replicated state received an update before hydration",
            ));
        };
        if sequence != current_sequence + 1 {
            self.clear();
            return Err(FacetError::new(
                "Replicated state update sequence has a gap",
            ));
        }
        let json = to_json(&current)?;
        let json = apply_immutable(Some(json), ops)?;
        let value: T = from_json(json, "update")?;
        *self.shared.published.lock() = Some(value.clone());
        *self.shared.sequence.lock() = sequence;
        self.shared.deliver(
            &value,
            context,
            ReplicatedStateDelivery::new(DeliveryKind::Update, sequence),
        );
        Ok(())
    }
}

fn to_json<T: Serialize>(value: &T) -> Result<JsonValue, FacetError> {
    serde_json::to_value(value)
        .map_err(|error| FacetError::new(format!("Replicated state value is not JSON: {error}")))
}

fn from_json<T: DeserializeOwned>(json: JsonValue, kind: &str) -> Result<T, FacetError> {
    serde_json::from_value(json).map_err(|error| {
        FacetError::new(format!(
            "Replicated state {kind} is not a valid value: {error}"
        ))
    })
}

/// Creates a producer from an initial value (upstream `env.replicatedState`).
pub fn replicated_state<T>(initial: T) -> Result<MutableReplicatedState<T>, FacetError>
where
    T: Clone + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    MutableReplicatedState::new(initial)
}
