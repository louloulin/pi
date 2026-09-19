//! Locking helpers.
//!
//! The reference adapters only ever hold a lock while touching their own
//! bookkeeping — never across a callback — so a poisoned lock cannot hide a
//! half-updated span. Recovering the guard keeps telemetry passive: recording
//! must never turn into a panic for the instrumented operation.

use std::sync::{Mutex, MutexGuard};

/// Lock `mutex`, recovering the guard when a previous holder panicked.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
