//! Service subscriptions — Rust port of the `#serviceListeners` machinery in
//! `packages/client/src/client.ts` plus the `ServiceSubscription` interface
//! from `packages/client/src/types.ts`.
//!
//! A subscription is a small state machine fed by three sources:
//!
//! * the subscribe **response** hydrates the decoder ([`ActiveServiceListener::hydrate`]);
//! * **wire updates** that arrive before hydration are queued verbatim and
//!   decoded in arrival order while the registry is still warm
//!   ([`ActiveServiceListener::push_wire_update`]);
//! * [`ServiceSubscription::start`] releases the updates buffered until the
//!   caller is ready to consume them.
//!
//! Upstream gets ordering for free from the JavaScript event loop. Rust has no
//! such oracle, so one [`parking_lot::Mutex`] guards the decoder *and* both
//! queues: hydration, the queued flush and every later decode are serialised
//! against each other and updates reach the delivery channel in wire order.
//!
//! Delivery itself is a single `tokio` task draining an unbounded FIFO, which
//! is the Rust form of upstream's `deliveryTail` promise chain: listener calls
//! run one at a time, in order, and a listener that panics is reported through
//! `onListenerError` instead of killing client state.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use futures::future::BoxFuture;
use futures::FutureExt;
use parking_lot::Mutex;
use pi_chord::services::{
    parse_wire_service_provider_update, ServiceProviderUpdate, ServiceStateDecoder,
    ServiceSubscriptionSnapshot, WireServiceSubscriptionSnapshot,
};
use pi_protocol::rpc::RpcTarget;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::client::ClientInner;
use crate::errors::ClientError;
use crate::types::ListenerErrorHandler;

/// A listener invoked with every provider update of a subscription.
///
/// Returning a boxed future lets callers pass sync or async work:
///
/// ```
/// use pi_client::service_listener;
///
/// let listener = service_listener(|_update| async move {});
/// # let _ = listener;
/// ```
pub type ServiceUpdateListener =
    Arc<dyn Fn(ServiceProviderUpdate) -> BoxFuture<'static, ()> + Send + Sync>;

/// Adapts an async closure to [`ServiceUpdateListener`].
pub fn service_listener<F, Fut>(listener: F) -> ServiceUpdateListener
where
    F: Fn(ServiceProviderUpdate) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Arc::new(move |update| Box::pin(listener(update)))
}

struct ActiveState {
    decoder: ServiceStateDecoder,
    queued_wire: Vec<serde_json::Value>,
    queued: Vec<ServiceProviderUpdate>,
    hydrated: bool,
    ready: bool,
}

impl Default for ActiveState {
    fn default() -> Self {
        Self {
            decoder: ServiceStateDecoder::new(),
            queued_wire: Vec::new(),
            queued: Vec::new(),
            hydrated: false,
            ready: false,
        }
    }
}

/// The client-side bookkeeping for one `subscribeService` call.
pub(crate) struct ActiveServiceListener {
    listener: ServiceUpdateListener,
    state: Mutex<ActiveState>,
    sender: Mutex<Option<mpsc::UnboundedSender<ServiceProviderUpdate>>>,
    delivery: Mutex<Option<JoinHandle<()>>>,
}

impl ActiveServiceListener {
    /// Registers the delivery channel and its draining task.
    pub(crate) fn new(
        listener: ServiceUpdateListener,
        error_handler: Option<ListenerErrorHandler>,
    ) -> Arc<Self> {
        let (sender, mut receiver) = mpsc::unbounded_channel::<ServiceProviderUpdate>();
        let active = Arc::new(Self {
            listener,
            state: Mutex::new(ActiveState::default()),
            sender: Mutex::new(Some(sender)),
            delivery: Mutex::new(None),
        });
        let weak = Arc::downgrade(&active);
        let handle = tokio::spawn(async move {
            while let Some(update) = receiver.recv().await {
                let Some(active) = weak.upgrade() else {
                    break;
                };
                deliver_once(&active.listener, update, error_handler.as_ref()).await;
            }
        });
        *active.delivery.lock() = Some(handle);
        active
    }

    /// Decodes the snapshot and flushes every update queued before hydration.
    pub(crate) fn hydrate(
        &self,
        wire: &WireServiceSubscriptionSnapshot,
    ) -> Result<ServiceSubscriptionSnapshot, ClientError> {
        let mut state = self.state.lock();
        let snapshot = state.decoder.decode_snapshot(wire)?;
        state.hydrated = true;
        let queued_wire = std::mem::take(&mut state.queued_wire);
        for value in queued_wire {
            let decoded = decode_wire_update(&mut state, &value)?;
            self.enqueue(&mut state, decoded);
        }
        Ok(snapshot)
    }

    /// Routes one inbound `service_update` payload.
    pub(crate) fn push_wire_update(&self, value: &serde_json::Value) -> Result<(), ClientError> {
        let mut state = self.state.lock();
        if !state.hydrated {
            state.queued_wire.push(value.clone());
            return Ok(());
        }
        let decoded = decode_wire_update(&mut state, value)?;
        self.enqueue(&mut state, decoded);
        Ok(())
    }

    /// Releases the buffered updates; idempotent.
    pub(crate) fn start(&self) {
        let mut state = self.state.lock();
        if state.ready {
            return;
        }
        state.ready = true;
        let queued = std::mem::take(&mut state.queued);
        let sender = self.sender.lock();
        if let Some(sender) = sender.as_ref() {
            for update in queued {
                let _ = sender.send(update);
            }
        }
    }

    /// Clears every buffer. Called once a subscription is disposed.
    pub(crate) fn clear(&self) {
        let mut state = self.state.lock();
        state.queued_wire.clear();
        state.queued.clear();
    }

    /// Drops the delivery channel and waits for the in-flight listener call.
    ///
    /// This is the Rust form of upstream's `await active.deliveryTail`.
    pub(crate) async fn close(&self) {
        let handle = self.delivery.lock().take();
        self.sender.lock().take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
    }

    /// Enqueues one decoded update, honouring the pre-`start` buffer.
    fn enqueue(&self, state: &mut ActiveState, update: ServiceProviderUpdate) {
        if !state.ready {
            state.queued.push(update);
            return;
        }
        let sender = self.sender.lock();
        if let Some(sender) = sender.as_ref() {
            let _ = sender.send(update);
        }
    }
}

fn decode_wire_update(
    state: &mut ActiveState,
    value: &serde_json::Value,
) -> Result<ServiceProviderUpdate, ClientError> {
    let wire = parse_wire_service_provider_update(value)?;
    state
        .decoder
        .decode_update(&wire)
        .map_err(ClientError::from)
}

async fn deliver_once(
    listener: &ServiceUpdateListener,
    update: ServiceProviderUpdate,
    error_handler: Option<&ListenerErrorHandler>,
) {
    let listener = Arc::clone(listener);
    let outcome = AssertUnwindSafe(async move { listener(update).await })
        .catch_unwind()
        .await;
    if let Err(panic) = outcome {
        if let Some(handler) = error_handler {
            let error = ClientError::listener(panic_message(&panic));
            let _ = std::panic::catch_unwind(AssertUnwindSafe(|| handler(&error)));
        }
    }
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "Service update listener panicked".to_owned()
    }
}

/// An open service subscription.
///
/// `start` releases the snapshot-ordered backlog; `dispose` unsubscribes on the
/// server, drains the in-flight listener call and releases the buffers.
pub struct ServiceSubscription {
    id: String,
    target: RpcTarget,
    snapshot: ServiceSubscriptionSnapshot,
    active: Arc<ActiveServiceListener>,
    client: Weak<ClientInner>,
    disposed: AtomicBool,
}

impl ServiceSubscription {
    pub(crate) fn new(
        id: String,
        target: RpcTarget,
        snapshot: ServiceSubscriptionSnapshot,
        active: Arc<ActiveServiceListener>,
        client: Weak<ClientInner>,
    ) -> Self {
        Self {
            id,
            target,
            snapshot,
            active,
            client,
            disposed: AtomicBool::new(false),
        }
    }

    /// The client-chosen subscription id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The routed target this subscription was opened against.
    pub fn target(&self) -> &RpcTarget {
        &self.target
    }

    /// The subscription snapshot returned by the subscribe call.
    pub fn snapshot(&self) -> &ServiceSubscriptionSnapshot {
        &self.snapshot
    }

    /// Whether [`dispose`](Self::dispose) has run.
    pub fn disposed(&self) -> bool {
        self.disposed.load(Ordering::SeqCst)
    }

    /// Releases the updates buffered until the caller was ready.
    pub fn start(&self) {
        self.active.start();
    }

    /// Unsubscribes and waits for delivery to drain.
    pub async fn dispose(&self) {
        if self.disposed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(client) = self.client.upgrade() {
            client.remove_service(&self.id, &self.active);
        }
        if let Some(client) = self.client.upgrade() {
            if client.target_is_current(&self.target) {
                let _ = client
                    .request(
                        self.target.clone(),
                        pi_chord::services::create_service_unsubscribe_call(&self.id),
                        None,
                    )
                    .await;
            }
        }
        self.active.close().await;
        self.active.clear();
    }
}

impl std::fmt::Debug for ServiceSubscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceSubscription")
            .field("id", &self.id)
            .field("target", &self.target)
            .field("disposed", &self.disposed())
            .finish()
    }
}
