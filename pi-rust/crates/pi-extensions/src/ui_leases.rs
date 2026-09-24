//! UI session leases.
//!
//! The host owns one UI session at a time — extensions cannot
//! simultaneously pop open their own modals over a session the user
//! is already looking at. The lease is the gate: an extension asks
//! for the next UI session via [`UiLeaseGuard::next_ui_session`], and
//! receives a [`UiLease`] token. While the token is alive, no other
//! extension can grab the session.
//!
//! ## RAII semantics
//!
//! The lease is RAII: dropping the [`UiLease`] releases the session.
//! Drop is also wired to:
//!
//! 1. Emit a `ui_session_revoked` event so observers (the runtime
//!    risk layer, the operator UI) learn that the session is gone.
//! 2. Cancel any in-flight UI prompt tied to the lease via the
//!    `cancel_fn` the host supplied at grant time.
//!
//! ## TTL
//!
//! Each lease has a TTL ([`UiLease::expires_at`]). The host calls
//! [`UiLeaseGuard::expire_overdue`] periodically; any lease whose
//! TTL has passed is dropped and revoked. The check is a sweep over
//! the in-memory table, fine for the small number of leases per host.
//!
//! ## Port scope
//!
//! Simplified from
//! `pi_agent_rust/src/extensions/extension_manager_impl.rs`
//! `PendingExtensionUiLease` (~60 lines):
//!
//! - No cooperative cancellation flag on the lease itself — the
//!   host's `cancel_fn` does the work.
//! - No priority queue. `next_ui_session` is FIFO.
//! - No fan-out to N pending leases; only the active lease exists.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use thiserror::Error;

/// Monotonic lease id. Returned to the caller for tracing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UiLeaseId(u64);

impl UiLeaseId {
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for UiLeaseId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ui-lease-{}", self.id())
    }
}

impl UiLeaseId {
    fn id(self) -> u64 {
        self.0
    }
}

/// Event emitted when a UI lease is revoked. The host forwards it
/// to its event channel; the runtime risk layer subscribes to count
/// drops as a signal of extension misbehaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiLeaseEvent {
    /// Lease was dropped (RAII) or expired (TTL).
    Revoked {
        lease_id: UiLeaseId,
        extension_id: String,
        /// Why the lease was revoked. `RaiiDrop` is the normal
        /// case; `TtlExpired` means the lease timed out without
        /// the extension returning its token.
        reason: UiLeaseRevokeReason,
    },
    /// Lease was granted. Useful for tracing / audit logs.
    Granted {
        lease_id: UiLeaseId,
        extension_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiLeaseRevokeReason {
    RaiiDrop,
    TtlExpired,
    Explicit,
}

#[derive(Debug, Error)]
pub enum UiLeaseError {
    #[error("UI session is already held by lease `{holder}`")]
    AlreadyHeld { holder: UiLeaseId },
    #[error("no UI lease found with id `{lease_id:?}`")]
    Unknown { lease_id: UiLeaseId },
}

/// One granted UI lease. Clone-cheap (Arc inside).
#[derive(Clone)]
pub struct UiLease {
    inner: Arc<UiLeaseInner>,
}

struct UiLeaseInner {
    lease_id: UiLeaseId,
    extension_id: String,
    expires_at: Instant,
    /// The host-supplied cancel fn. Called when the lease is
    /// revoked so any in-flight prompt can be torn down. Not
    /// `Debug` because `dyn FnOnce` doesn't implement Debug; the
    /// owning struct therefore opts out of `derive(Debug)`.
    cancel_fn: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Guard bookkeeping. Set to `false` on drop so the manager
    /// doesn't double-revoke.
    revoked: Mutex<bool>,
    /// Shared state with the manager (so drop can notify).
    manager: UiLeaseManager,
}

impl UiLease {
    pub fn lease_id(&self) -> UiLeaseId {
        self.inner.lease_id
    }

    pub fn extension_id(&self) -> &str {
        &self.inner.extension_id
    }

    pub fn expires_at(&self) -> Instant {
        self.inner.expires_at
    }

    pub fn is_revoked(&self) -> bool {
        *self.inner.revoked.lock()
    }

    /// Time remaining on the TTL. `None` when the lease has expired.
    pub fn ttl_remaining(&self, now: Instant) -> Option<Duration> {
        self.inner.expires_at.checked_duration_since(now)
    }

    /// Manually revoke the lease (extension returns the token
    /// early). Equivalent to dropping the lease, but lets the
    /// extension record an explicit `UiLeaseRevokeReason::Explicit`.
    pub fn revoke_explicit(self) {
        // Same lock-discipline fix as `Drop`: release `revoked`
        // and `cancel_fn` locks before calling `revoke_internal`,
        // so that the inner drop triggered by clearing the active
        // slot cannot re-enter them.
        let already_revoked = {
            let mut revoked = self.inner.revoked.lock();
            if *revoked {
                true
            } else {
                *revoked = true;
                false
            }
        };
        if already_revoked {
            return;
        }
        let cancel = self.inner.cancel_fn.lock().take();
        if let Some(cancel) = cancel {
            cancel();
        }
        self.inner.manager.revoke_internal(
            self.inner.lease_id,
            &self.inner.extension_id,
            UiLeaseRevokeReason::Explicit,
        );
    }
}

impl Drop for UiLease {
    fn drop(&mut self) {
        // Check + flip the revoked flag in a tight scope so the
        // lock is released before we call `revoke_internal`.
        // `revoke_internal` mutates `manager.active` — if the
        // active slot still held a clone of this lease, dropping
        // it triggers another `Drop` impl which would try to
        // re-acquire the same `revoked` lock and deadlock.
        let already_revoked = {
            let mut revoked = self.inner.revoked.lock();
            if *revoked {
                true
            } else {
                *revoked = true;
                false
            }
        };
        if already_revoked {
            return;
        }
        // Take the cancel fn out (also in a tight scope).
        let cancel = self.inner.cancel_fn.lock().take();
        if let Some(cancel) = cancel {
            cancel();
        }
        let reason = if Instant::now() >= self.inner.expires_at {
            UiLeaseRevokeReason::TtlExpired
        } else {
            UiLeaseRevokeReason::RaiiDrop
        };
        self.inner.manager.revoke_internal(
            self.inner.lease_id,
            &self.inner.extension_id,
            reason,
        );
    }
}

/// Bookkeeping for granted leases + event buffer. Cheap to clone.
#[derive(Clone, Default)]
pub struct UiLeaseManager {
    inner: Arc<UiLeaseManagerInner>,
}

struct UiLeaseManagerInner {
    /// Currently-held lease, if any.
    active: Mutex<Option<UiLease>>,
    /// Monotonic lease id source.
    next_id: Mutex<u64>,
    /// Default TTL applied when the request does not specify one.
    /// Initialised to 5 minutes — long enough that the host can
    /// observe the lease, short enough that a forgotten lease
    /// doesn't hold the session forever.
    default_ttl: Mutex<Duration>,
    /// Append-only event log. `next_ui_session` and
    /// `expire_overdue` push here; the host drains into its own
    /// channel.
    events: Mutex<Vec<UiLeaseEvent>>,
}

impl Default for UiLeaseManagerInner {
    fn default() -> Self {
        Self {
            active: Mutex::new(None),
            next_id: Mutex::new(0),
            default_ttl: Mutex::new(Duration::from_secs(5 * 60)),
            events: Mutex::new(Vec::new()),
        }
    }
}

impl UiLeaseManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_default_ttl(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(UiLeaseManagerInner {
                default_ttl: Mutex::new(ttl),
                ..Default::default()
            }),
        }
    }

    pub fn set_default_ttl(&self, ttl: Duration) {
        *self.inner.default_ttl.lock() = ttl;
    }

    /// Grant the next UI session to `extension_id`. The lease has a
    /// `ttl` (defaulting to the manager's default TTL) and an
    /// optional `cancel_fn` the manager calls when the lease is
    /// revoked.
    pub fn next_ui_session(
        &self,
        extension_id: impl Into<String>,
        cancel_fn: Option<Box<dyn FnOnce() + Send>>,
        ttl: Option<Duration>,
    ) -> Result<UiLease, UiLeaseError> {
        let mut active = self.inner.active.lock();
        if let Some(existing) = active.as_ref() {
            return Err(UiLeaseError::AlreadyHeld {
                holder: existing.inner.lease_id,
            });
        }
        let lease_id = {
            let mut id = self.inner.next_id.lock();
            *id += 1;
            UiLeaseId(*id)
        };
        let extension_id = extension_id.into();
        let ttl = ttl.unwrap_or_else(|| *self.inner.default_ttl.lock());
        let expires_at = Instant::now() + ttl;
        let lease = UiLease {
            inner: Arc::new(UiLeaseInner {
                lease_id,
                extension_id: extension_id.clone(),
                expires_at,
                cancel_fn: Mutex::new(cancel_fn),
                revoked: Mutex::new(false),
                manager: self.clone(),
            }),
        };
        *active = Some(lease.clone());
        self.inner.events.lock().push(UiLeaseEvent::Granted {
            lease_id,
            extension_id,
        });
        Ok(lease)
    }

    /// Sweep leases whose TTL has passed. Returns the IDs of the
    /// leases that were revoked. Normally called from a tokio task
    /// every few seconds; safe to call as often as you like.
    pub fn expire_overdue(&self, now: Instant) -> Vec<UiLeaseId> {
        // Collect candidate IDs inside the active lock, then
        // release it before calling `revoke_internal` (which
        // re-acquires it). Reentrancy is not supported by
        // `parking_lot::Mutex`.
        let candidates: Vec<UiLeaseId> = {
            let active = self.inner.active.lock();
            match active.as_ref() {
                Some(lease) if now >= lease.inner.expires_at && !*lease.inner.revoked.lock() => {
                    vec![lease.inner.lease_id]
                }
                _ => Vec::new(),
            }
        };
        for id in &candidates {
            self.revoke_internal(*id, "", UiLeaseRevokeReason::TtlExpired);
        }
        candidates
    }

    pub fn has_active_lease(&self) -> bool {
        self.inner.active.lock().is_some()
    }

    pub fn active_lease_id(&self) -> Option<UiLeaseId> {
        self.inner.active.lock().as_ref().map(|l| l.inner.lease_id)
    }

    pub fn take_events(&self) -> Vec<UiLeaseEvent> {
        std::mem::take(&mut *self.inner.events.lock())
    }

    /// Internal revoke used by `UiLease::Drop` and `revoke_explicit`.
    fn revoke_internal(
        &self,
        lease_id: UiLeaseId,
        extension_id: &str,
        reason: UiLeaseRevokeReason,
    ) {
        // Pop the active slot only if it points at the lease we
        // are revoking. Capture the extension_id before clearing
        // so the event log has it.
        let (captured_extension_id, was_active) = {
            let mut active = self.inner.active.lock();
            let matched = matches!(
                active.as_ref(),
                Some(l) if l.inner.lease_id == lease_id
            );
            let captured = if matched {
                active.as_ref().map(|l| l.extension_id().to_string())
            } else {
                None
            };
            if matched {
                // Pre-mark the cloned lease as revoked so that
                // its Drop impl (triggered by `*active = None`)
                // does NOT call back into `revoke_internal` —
                // that would re-acquire `active.lock()` and
                // deadlock because parking_lot::Mutex is not
                // reentrant.
                if let Some(l) = active.as_ref() {
                    *l.inner.revoked.lock() = true;
                }
                *active = None;
            }
            (captured, matched)
        };
        let resolved_extension_id = if extension_id.is_empty() {
            captured_extension_id.unwrap_or_default()
        } else {
            extension_id.to_string()
        };
        // Only push the Revoked event when this call actually
        // cleared the active slot. Otherwise the lease was
        // already revoked (or never existed) and pushing would
        // produce a spurious event.
        if was_active {
            self.inner.events.lock().push(UiLeaseEvent::Revoked {
                lease_id,
                extension_id: resolved_extension_id,
                reason,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn grant_emits_granted_event() {
        let m = UiLeaseManager::with_default_ttl(Duration::from_secs(60));
        let lease = m.next_ui_session("ext-1", None, None).unwrap();
        let events = m.take_events();
        assert!(matches!(events[0], UiLeaseEvent::Granted { lease_id, .. } if lease_id == lease.lease_id()));
        m.take_events();
    }

    #[test]
    fn second_grant_is_rejected() {
        let m = UiLeaseManager::new();
        let _a = m.next_ui_session("ext-a", None, None).unwrap();
        let b = m.next_ui_session("ext-b", None, None);
        assert!(matches!(b, Err(UiLeaseError::AlreadyHeld { .. })));
    }

    #[test]
    fn drop_releases_session() {
        let m = UiLeaseManager::new();
        let lease = m.next_ui_session("ext", None, None).unwrap();
        assert!(m.has_active_lease());
        drop(lease);
        assert!(!m.has_active_lease());
        let events = m.take_events();
        assert!(matches!(
            events.last().unwrap(),
            UiLeaseEvent::Revoked {
                reason: UiLeaseRevokeReason::RaiiDrop,
                ..
            }
        ));
    }

    #[test]
    fn revoke_explicit_releases_session() {
        let m = UiLeaseManager::new();
        let lease = m.next_ui_session("ext", None, None).unwrap();
        lease.revoke_explicit();
        assert!(!m.has_active_lease());
        let events = m.take_events();
        assert!(matches!(
            events.last().unwrap(),
            UiLeaseEvent::Revoked {
                reason: UiLeaseRevokeReason::Explicit,
                ..
            }
        ));
    }

    #[test]
    fn cancel_fn_runs_on_drop() {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_inner = counter.clone();
        let m = UiLeaseManager::new();
        let lease = m
            .next_ui_session(
                "ext",
                Some(Box::new(move || {
                    counter_inner.fetch_add(1, Ordering::SeqCst);
                })),
                None,
            )
            .unwrap();
        drop(lease);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cancel_fn_runs_exactly_once_on_explicit_then_drop() {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_inner = counter.clone();
        let m = UiLeaseManager::new();
        let lease = m
            .next_ui_session(
                "ext",
                Some(Box::new(move || {
                    counter_inner.fetch_add(1, Ordering::SeqCst);
                })),
                None,
            )
            .unwrap();
        // Revoke explicitly, then drop. Cancel must run only once.
        lease.revoke_explicit();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn ttl_expiry_releases_session_via_sweep() {
        let m = UiLeaseManager::with_default_ttl(Duration::from_millis(10));
        let _lease = m.next_ui_session("ext", None, None).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let revoked = m.expire_overdue(Instant::now());
        assert_eq!(revoked.len(), 1);
        assert!(!m.has_active_lease());
        let events = m.take_events();
        assert!(matches!(
            events.last().unwrap(),
            UiLeaseEvent::Revoked {
                reason: UiLeaseRevokeReason::TtlExpired,
                ..
            }
        ));
    }

    #[test]
    fn ttl_remaining_is_some_within_window() {
        let m = UiLeaseManager::with_default_ttl(Duration::from_secs(60));
        let lease = m.next_ui_session("ext", None, None).unwrap();
        let remaining = lease.ttl_remaining(Instant::now()).unwrap();
        assert!(remaining > Duration::from_secs(50));
    }

    #[test]
    fn drop_after_ttl_emits_ttl_reason() {
        let m = UiLeaseManager::with_default_ttl(Duration::from_millis(10));
        let lease = m.next_ui_session("ext", None, None).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        drop(lease);
        let events = m.take_events();
        assert!(matches!(
            events.last().unwrap(),
            UiLeaseEvent::Revoked {
                reason: UiLeaseRevokeReason::TtlExpired,
                ..
            }
        ));
    }

    #[test]
    fn lease_ids_are_unique() {
        let m = UiLeaseManager::new();
        let a = m.next_ui_session("a", None, None).unwrap();
        let a_id = a.lease_id();
        a.revoke_explicit();
        let b = m.next_ui_session("b", None, None).unwrap();
        assert_ne!(a_id, b.lease_id());
    }

    #[test]
    fn expire_overdue_with_no_active_lease_is_noop() {
        let m = UiLeaseManager::new();
        let revoked = m.expire_overdue(Instant::now());
        assert!(revoked.is_empty());
    }
}