//! Client-facing type vocabulary — Rust port of `packages/client/src/types.ts`.
//!
//! Upstream also declares `ServiceSubscription` here; the Rust version lives in
//! [`crate::subscription`] because it owns a delivery task and is therefore
//! constructed rather than declared inline.

use std::sync::Arc;

use pi_protocol::rpc::SessionTarget;

use crate::subscription::ServiceUpdateListener;

/// The three connection states upstream reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionState {
    /// No transport is open.
    Disconnected,
    /// A transport is opening and the handshake is in flight.
    Connecting,
    /// The handshake succeeded and frames may be sent.
    Connected,
}

impl ConnectionState {
    /// The upstream spelling (`disconnected` / `connecting` / `connected`).
    pub fn as_str(self) -> &'static str {
        match self {
            ConnectionState::Disconnected => "disconnected",
            ConnectionState::Connecting => "connecting",
            ConnectionState::Connected => "connected",
        }
    }
}

impl std::fmt::Display for ConnectionState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One connection-state transition.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectionStateChange {
    /// The state the connection moved to.
    pub state: ConnectionState,
    /// The failure that caused a move to [`ConnectionState::Disconnected`].
    pub error: Option<crate::ClientError>,
}

/// A callback receiving every connection-state transition.
pub type ConnectionStateListener = Arc<dyn Fn(&ConnectionStateChange) + Send + Sync>;

/// A callback receiving the current attachment whenever it changes.
pub type AttachmentChangeListener = Arc<dyn Fn(Option<&SessionTarget>) + Send + Sync>;

/// Reports subscriber failures without letting them corrupt client state.
pub type ListenerErrorHandler = Arc<dyn Fn(&crate::ClientError) + Send + Sync>;

/// The listener type expected by
/// [`Client::subscribe_service`](crate::Client::subscribe_service).
pub type ServiceListener = ServiceUpdateListener;

/// Detaches a listener registered with the client.
///
/// Dropping the value detaches the listener; [`Unsubscribe::unsubscribe`] does
/// it eagerly. Upstream returns a closure, which Rust cannot express as a
/// droppable cancellation token, so it is a value with `Drop`.
pub struct Unsubscribe {
    pub(crate) detach: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl Unsubscribe {
    /// Builds an `Unsubscribe` from a detach action.
    pub fn new(detach: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self {
            detach: Some(Box::new(detach)),
        }
    }

    /// Runs the detach action now. Later calls are no-ops.
    pub fn unsubscribe(mut self) {
        self.run();
    }

    /// Runs the detach action if it has not run yet.
    pub(crate) fn run(&mut self) {
        if let Some(detach) = self.detach.take() {
            detach();
        }
    }
}

impl std::fmt::Debug for Unsubscribe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Unsubscribe")
            .field("attached", &self.detach.is_some())
            .finish()
    }
}

impl Drop for Unsubscribe {
    fn drop(&mut self) {
        self.run();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_state_spellings_match_upstream() {
        assert_eq!(ConnectionState::Disconnected.as_str(), "disconnected");
        assert_eq!(ConnectionState::Connecting.as_str(), "connecting");
        assert_eq!(ConnectionState::Connected.as_str(), "connected");
    }

    #[test]
    fn unsubscribe_runs_once_and_on_drop() {
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        let first = Unsubscribe::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        first.unsubscribe();
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);

        let counter = Arc::clone(&hits);
        {
            let _guard = Unsubscribe::new(move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            });
        }
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
