//! Producer-side replicated state, ported from `packages/chord/src/services/state.ts` and
//! `state-internals.ts`.
//!
//! The core layer already provides the data structure ([`MutableReplicatedState`]). What a remote
//! provider additionally needs is a way to talk about *any* producer through one object-safe
//! interface, because a [`crate::services::provider::ServiceImplementation`] stores members
//! heterogeneously.
//!
//! [`ReplicatedStateMember`] is that interface. It is the Rust counterpart of upstream
//! `ReplicatedStateInternals`: `sequence`, `snapshotValue`, `publish` and `subscribe`, with the
//! blanket implementation over [`MutableReplicatedState`] doing the JSON conversion.
//!
//! The upstream registry that tags a producer object with its internals
//! (`registerReplicatedStateInternals` / `getReplicatedStateInternals`) has no Rust equivalent:
//! there is no way to attach hidden metadata to a value, so the type system carries the
//! distinction instead — a member is a state because it implements this trait.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::context::Context;
use crate::delta::Op;
use crate::json::JsonValue;
use crate::state::MutableReplicatedState;

use super::errors::ServiceError;

/// A source listener registered with [`ReplicatedStateMember::subscribe_source`].
pub type MemberSourceListener = Arc<dyn Fn(&[Op], u64, &Context) + Send + Sync>;

/// A producer that can be exposed as a remote replicated-state member.
///
/// `MutableReplicatedState<T>` implements this for every `T` that is JSON-representable, which
/// mirrors upstream's blanket `getReplicatedStateInternals` lookup.
pub trait ReplicatedStateMember: Send + Sync {
    /// The published sequence number.
    fn sequence(&self) -> u64;

    /// The published value as strict JSON.
    fn snapshot_value(&self) -> JsonValue;

    /// Publishes the diff between the published value and the current mutable value.
    fn publish(&self, context: &Context) -> Result<(), ServiceError>;

    /// Registers `listener` for the delta batches of subsequent publishes.
    ///
    /// The returned closure unsubscribes. Dropping it is not enough on its own — the provider
    /// invokes it when an instance is withdrawn, replaced or disposed, exactly like upstream's
    /// `removeMemberListeners`.
    fn subscribe_source(&self, listener: MemberSourceListener) -> Box<dyn FnOnce() + Send>;
}

impl<T> ReplicatedStateMember for MutableReplicatedState<T>
where
    T: Clone + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn sequence(&self) -> u64 {
        MutableReplicatedState::sequence(self)
    }

    fn snapshot_value(&self) -> JsonValue {
        // `MutableReplicatedState::new` already proved the value is representable, so the only way
        // this can fail is a `Serialize` impl that changes behaviour between calls; `null` keeps
        // the snapshot well-formed instead of panicking a live provider.
        serde_json::to_value(self.value()).unwrap_or(JsonValue::Null)
    }

    fn publish(&self, context: &Context) -> Result<(), ServiceError> {
        MutableReplicatedState::publish(self, context).map_err(ServiceError::from)
    }

    fn subscribe_source(&self, listener: MemberSourceListener) -> Box<dyn FnOnce() + Send> {
        let subscription = MutableReplicatedState::subscribe_source(self, move |ops, sequence, context| {
            listener(ops, sequence, context);
        });
        Box::new(move || subscription.unsubscribe())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::background_context;
    use crate::state::replicated_state;
    use parking_lot::Mutex;

    /// Batches captured by a test source listener.
    type CapturedBatches = Arc<Mutex<Vec<(Vec<Op>, u64)>>>;

    #[test]
    fn a_mutable_state_becomes_a_member() {
        let state = replicated_state(serde_json::json!({ "count": 0 })).unwrap();
        let member: &dyn ReplicatedStateMember = &state;
        assert_eq!(member.sequence(), 0);
        assert_eq!(member.snapshot_value(), serde_json::json!({ "count": 0 }));

        let batches: CapturedBatches = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&batches);
        let _unsubscribe = member.subscribe_source(Arc::new(move |ops, sequence, _| {
            seen.lock().push((ops.to_vec(), sequence));
        }));

        state.state_mut()["count"] = serde_json::json!(1);
        member.publish(background_context()).unwrap();
        assert_eq!(member.sequence(), 1);
        assert_eq!(member.snapshot_value(), serde_json::json!({ "count": 1 }));
        assert_eq!(batches.lock().len(), 1);
        assert_eq!(batches.lock()[0].1, 1);
    }
}
