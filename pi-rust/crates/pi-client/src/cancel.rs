//! Cancellation for one in-flight request — the Rust counterpart of passing an
//! `AbortSignal` to `Client#request` / `Client#subscribeService`.
//!
//! Rust has no `AbortSignal`, so [`RequestCancel`] is an explicit, cheap,
//! cloneable token: whoever holds a clone can cancel the request, and the
//! request side awaits [`RequestCancel::cancelled`]. Cancelling is idempotent
//! and panic-free, and a cancel that fires before the request is awaited is
//! still observed because [`tokio::sync::Notify`] stores a permit.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::Notify;

/// A one-shot cancellation token for a single request.
#[derive(Clone, Debug, Default)]
pub struct RequestCancel {
    inner: Arc<CancelState>,
}

#[derive(Debug, Default)]
struct CancelState {
    cancelled: AtomicBool,
    notify: Notify,
}

impl RequestCancel {
    /// Creates an unsignalled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels the request. Idempotent.
    pub fn cancel(&self) {
        if !self.inner.cancelled.swap(true, Ordering::SeqCst) {
            self.inner.notify.notify_one();
        }
    }

    /// Whether [`cancel`](Self::cancel) has run.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// Resolves once the token is cancelled.
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        self.inner.notify.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancel_wakes_a_waiter() {
        let cancel = RequestCancel::new();
        let waiter = cancel.clone();
        let handle = tokio::spawn(async move { waiter.cancelled().await });
        cancel.cancel();
        handle.await.expect("waiter completes");
        assert!(cancel.is_cancelled());
    }

    #[tokio::test]
    async fn cancel_before_waiting_is_still_observed() {
        let cancel = RequestCancel::new();
        cancel.cancel();
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_millis(100), cancel.cancelled())
            .await
            .expect("cancelled resolves immediately");
    }
}
