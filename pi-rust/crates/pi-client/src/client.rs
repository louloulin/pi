//! The client — Rust port of `packages/client/src/client.ts`.
//!
//! One [`Client`] owns one [`Connection`] and layers the request/subscription
//! routing on top of it:
//!
//! * `request` correlates a client-chosen id with a pending response, and can
//!   be abandoned with a [`RequestCancel`] token (the Rust form of an
//!   `AbortSignal` — a cancelled request still absorbs the server's late
//!   response instead of failing the connection);
//! * `serviceCatalogue` and `subscribeService` wrap the Chord control calls and
//!   fail the connection when the payload does not decode, mirroring upstream's
//!   transform callback;
//! * attachment and connection-state listeners are registered through
//!   `on_*` and detached by dropping the returned [`Unsubscribe`].
//!
//! Two shape changes from upstream:
//!
//! * `Client.connect(options)` cannot coexist with the instance `connect()`
//!   method in Rust, so the static constructor is [`Client::connect_with`].
//! * listener registration returns `Result`, because upstream throws
//!   `ClientDisposedError` synchronously.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use parking_lot::Mutex;
use pi_chord::services::{
    create_service_catalogue_call, create_service_subscribe_call, parse_service_catalogue,
    parse_wire_service_subscription_snapshot, ServiceCall,
};
use pi_chord::types::{ServiceCatalogueEntry, ServiceMode};
use pi_protocol::rpc::{
    encode_client_message, is_server_id, ClientMessage, ProtocolError, RpcTarget, ServerHello,
    ServerMessage, SessionTarget, DEFAULT_MAX_FRAME_LENGTH,
};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::cancel::RequestCancel;
use crate::connection::{Connection, ConnectionSink};
use crate::errors::ClientError;
use crate::subscription::{ActiveServiceListener, ServiceSubscription};
use crate::transport::ByteTransportFactory;
use crate::types::{
    AttachmentChangeListener, ConnectionState, ConnectionStateChange, ConnectionStateListener,
    ListenerErrorHandler, ServiceListener, Unsubscribe,
};

/// Configures a [`Client`].
///
/// Build one with [`ClientOptions::new`] and the `with_*` helpers:
///
/// ```
/// use std::sync::Arc;
/// use pi_client::{ByteTransportFactory, ClientOptions, FactoryFn, ClientError};
///
/// let factory: Arc<dyn ByteTransportFactory> = Arc::new(FactoryFn::new(|_handlers| {
///     Box::pin(async { Err(ClientError::disconnected("unused")) })
/// }));
/// let options = ClientOptions::new(factory, "00000000-0000-4000-8000-000000000001");
/// # let _ = options;
/// ```
#[derive(Clone)]
pub struct ClientOptions {
    /// Opens the byte transport the client speaks over.
    pub transport_factory: Arc<dyn ByteTransportFactory>,
    /// The logical server identity the handshake must confirm.
    pub server_id: String,
    /// The negotiated frame-length ceiling; `None` uses the protocol default.
    pub max_frame_length: Option<usize>,
    /// Receives subscriber failures so they cannot corrupt client state.
    pub on_listener_error: Option<ListenerErrorHandler>,
}

impl ClientOptions {
    /// Creates options for `server_id` over `transport_factory`.
    pub fn new(
        transport_factory: Arc<dyn ByteTransportFactory>,
        server_id: impl Into<String>,
    ) -> Self {
        Self {
            transport_factory,
            server_id: server_id.into(),
            max_frame_length: None,
            on_listener_error: None,
        }
    }

    /// Overrides the frame-length ceiling (`1..=u32::MAX`).
    pub fn with_max_frame_length(mut self, max_frame_length: usize) -> Self {
        self.max_frame_length = Some(max_frame_length);
        self
    }

    /// Installs the listener-failure reporter.
    pub fn with_listener_error_handler(mut self, handler: ListenerErrorHandler) -> Self {
        self.on_listener_error = Some(handler);
        self
    }
}

impl std::fmt::Debug for ClientOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientOptions")
            .field("server_id", &self.server_id)
            .field("max_frame_length", &self.max_frame_length)
            .field("on_listener_error", &self.on_listener_error.is_some())
            .finish_non_exhaustive()
    }
}

struct PendingRequest {
    sender: oneshot::Sender<Result<Option<Value>, ClientError>>,
}

/// The shared client state. Released once the last [`Client`] clone drops.
pub(crate) struct ClientInner {
    options: ClientOptions,
    connection: OnceLock<Arc<Connection>>,
    pending: Mutex<HashMap<String, PendingRequest>>,
    state_listeners: Mutex<Vec<(u64, ConnectionStateListener)>>,
    attachment_listeners: Mutex<Vec<(u64, AttachmentChangeListener)>>,
    services: Mutex<HashMap<String, Arc<ActiveServiceListener>>>,
    request_sequence: AtomicU64,
    subscription_sequence: AtomicU64,
    listener_sequence: AtomicU64,
    hello: Mutex<Option<ServerHello>>,
    attachment: Mutex<Option<SessionTarget>>,
    disposed: AtomicBool,
}

impl ClientInner {
    fn connection(&self) -> &Arc<Connection> {
        self.connection
            .get()
            .expect("Client connection is initialised before any other call")
    }

    fn connection_state(&self) -> ConnectionState {
        self.connection().state()
    }

    fn next_listener_id(&self) -> u64 {
        self.listener_sequence.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub(crate) async fn request(
        self: &Arc<Self>,
        target: RpcTarget,
        call: ServiceCall,
        cancel: Option<RequestCancel>,
    ) -> Result<Option<Value>, ClientError> {
        if self.disposed.load(Ordering::SeqCst) {
            return Err(ClientError::Disposed);
        }
        let connection = Arc::clone(self.connection());
        if connection.state() != ConnectionState::Connected {
            return Err(ClientError::disconnected("Client is disconnected"));
        }
        if let Some(cancel) = &cancel {
            if cancel.is_cancelled() {
                return Err(ClientError::Cancelled);
            }
        }

        let id = format!(
            "request-{}",
            self.request_sequence.fetch_add(1, Ordering::SeqCst) + 1
        );
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .insert(id.clone(), PendingRequest { sender });

        let frame = match encode_client_message(
            &ClientMessage::request(id.clone(), target.clone(), call.to_json()),
            Some(connection.max_frame_length()),
        ) {
            Ok(frame) => frame,
            Err(error) => {
                self.take_pending(&id);
                return Err(ClientError::from(error));
            }
        };
        if let Err(error) = connection.send(frame).await {
            self.take_pending(&id);
            return Err(error);
        }

        let Some(cancel) = cancel else {
            return receiver
                .await
                .map_err(|_| ClientError::disconnected("Request was dropped"))?;
        };
        let mut receiver = receiver;
        tokio::select! {
            result = &mut receiver => result
                .map_err(|_| ClientError::disconnected("Request was dropped"))?,
            _ = cancel.cancelled() => {
                if let Ok(frame) = encode_client_message(
                    &ClientMessage::cancel(id, target),
                    Some(connection.max_frame_length()),
                ) {
                    let _ = connection.send(frame).await;
                }
                // The pending entry stays so the server's late response is
                // absorbed by `handle_message` instead of failing the client.
                Err(ClientError::Cancelled)
            }
        }
    }

    fn take_pending(&self, id: &str) -> Option<PendingRequest> {
        self.pending.lock().remove(id)
    }

    fn reject_pending(&self, error: ClientError) {
        let requests: Vec<PendingRequest> = self.pending.lock().drain().map(|(_, it)| it).collect();
        for request in requests {
            let _ = request.sender.send(Err(error.clone()));
        }
    }

    fn fail_connection(&self, error: ClientError) {
        self.connection().fail(error);
    }

    fn handle_message(&self, message: ServerMessage) {
        match message {
            ServerMessage::Attachment { attachment } => {
                if let Some(target) = &attachment {
                    if target.server_id != self.options.server_id {
                        self.fail_connection(ClientError::protocol(
                            "Attachment update belongs to another server",
                        ));
                        return;
                    }
                }
                self.set_attachment(attachment);
            }
            ServerMessage::ServiceUpdate {
                subscription_id,
                update,
            } => {
                let active = self.services.lock().get(&subscription_id).cloned();
                if let Some(active) = active {
                    if let Err(error) = active.push_wire_update(&update) {
                        self.fail_connection(error);
                    }
                }
            }
            ServerMessage::Response {
                id,
                ok,
                result,
                error,
                ..
            } => {
                let Some(pending) = self.take_pending(&id) else {
                    self.fail_connection(ClientError::protocol("Response has no matching request"));
                    return;
                };
                if !ok {
                    let error = error.unwrap_or(ProtocolError {
                        code: "internal".to_owned(),
                        message: "Response failed without an error".to_owned(),
                    });
                    let _ = pending.sender.send(Err(ClientError::Server(error)));
                    return;
                }
                let _ = pending.sender.send(Ok(result));
            }
            ServerMessage::Hello { .. } | ServerMessage::HelloError { .. } => {
                self.fail_connection(ClientError::protocol("Unexpected handshake message"));
            }
        }
    }

    fn handle_connection_state_change(&self, change: ConnectionStateChange) {
        if change.state == ConnectionState::Disconnected {
            *self.hello.lock() = None;
            self.set_attachment(None);
            let error = change
                .error
                .clone()
                .unwrap_or_else(|| ClientError::disconnected("Client is disconnected"));
            self.reject_pending(error);
            self.services.lock().clear();
        }
        let listeners: Vec<ConnectionStateListener> = self
            .state_listeners
            .lock()
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect();
        for listener in listeners {
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| listener(&change)));
            if outcome.is_err() {
                self.report_listener_error(&ClientError::listener(
                    "Connection state listener panicked",
                ));
            }
        }
    }

    fn set_attachment(&self, attachment: Option<SessionTarget>) {
        {
            let current = self.attachment.lock();
            if same_target(current.as_ref(), attachment.as_ref()) {
                return;
            }
        }
        *self.attachment.lock() = attachment.clone();
        let listeners: Vec<AttachmentChangeListener> = self
            .attachment_listeners
            .lock()
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect();
        for listener in listeners {
            let outcome =
                std::panic::catch_unwind(AssertUnwindSafe(|| listener(attachment.as_ref())));
            if outcome.is_err() {
                self.report_listener_error(&ClientError::listener("Attachment listener panicked"));
            }
        }
    }

    fn report_listener_error(&self, error: &ClientError) {
        if let Some(handler) = &self.options.on_listener_error {
            let _ = std::panic::catch_unwind(AssertUnwindSafe(|| handler(error)));
        }
    }

    /// Whether `target` can still be addressed on this connection.
    ///
    /// A server target needs the confirmed handshake; a session target must
    /// still match the live attachment. Both need a connected transport.
    pub(crate) fn target_is_current(&self, target: &RpcTarget) -> bool {
        if self.connection_state() != ConnectionState::Connected {
            return false;
        }
        match target {
            RpcTarget::Server(server) => self
                .hello
                .lock()
                .as_ref()
                .is_some_and(|hello| hello.server_id == server.server_id),
            RpcTarget::Session(session) => self.attachment.lock().as_ref().is_some_and(|current| {
                current.server_id == session.server_id
                    && current.session_id == session.session_id
                    && current.attachment_id == session.attachment_id
            }),
        }
    }

    pub(crate) fn remove_service(&self, id: &str, active: &Arc<ActiveServiceListener>) {
        let mut guard = self.services.lock();
        if guard
            .get(id)
            .is_some_and(|current| Arc::ptr_eq(current, active))
        {
            guard.remove(id);
        }
    }

    fn is_service_active(&self, id: &str, active: &Arc<ActiveServiceListener>) -> bool {
        self.services
            .lock()
            .get(id)
            .is_some_and(|current| Arc::ptr_eq(current, active))
    }

    fn add_state_listener(self: &Arc<Self>, listener: ConnectionStateListener) -> Unsubscribe {
        let id = self.next_listener_id();
        self.state_listeners.lock().push((id, listener));
        detach_on(self, move |inner| {
            inner
                .state_listeners
                .lock()
                .retain(|(current, _)| *current != id);
        })
    }

    fn add_attachment_listener(
        self: &Arc<Self>,
        listener: AttachmentChangeListener,
    ) -> Unsubscribe {
        let id = self.next_listener_id();
        self.attachment_listeners.lock().push((id, listener));
        detach_on(self, move |inner| {
            inner
                .attachment_listeners
                .lock()
                .retain(|(current, _)| *current != id);
        })
    }
}

impl ConnectionSink for ClientInner {
    fn on_handshake(&self, hello: ServerHello) {
        *self.hello.lock() = Some(hello);
    }

    fn on_message(&self, message: ServerMessage) {
        self.handle_message(message);
    }

    fn on_state_change(&self, change: ConnectionStateChange) {
        self.handle_connection_state_change(change);
    }
}

fn same_target(current: Option<&SessionTarget>, next: Option<&SessionTarget>) -> bool {
    match (current, next) {
        (None, None) => true,
        (Some(current), Some(next)) => {
            current.server_id == next.server_id
                && current.session_id == next.session_id
                && current.attachment_id == next.attachment_id
        }
        _ => false,
    }
}

fn detach_on(
    inner: &Arc<ClientInner>,
    detach: impl FnOnce(&ClientInner) + Send + Sync + 'static,
) -> Unsubscribe {
    let weak = Arc::downgrade(inner);
    Unsubscribe::new(move || {
        if let Some(inner) = weak.upgrade() {
            detach(&inner);
        }
    })
}

/// A connected (or connecting) client.
///
/// `Client` is cheap to clone; every clone shares one connection.
#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientInner>,
}

impl Client {
    /// Creates a disconnected client.
    ///
    /// Fails with [`ClientError::Configuration`] when `serverId` is not a
    /// canonical lowercase UUIDv4, or when the frame-length ceiling is out of
    /// range.
    pub fn new(options: ClientOptions) -> Result<Self, ClientError> {
        if !is_server_id(&options.server_id) {
            return Err(ClientError::configuration(
                "serverId must be a canonical lowercase UUIDv4",
            ));
        }
        let inner = Arc::new(ClientInner {
            options,
            connection: OnceLock::new(),
            pending: Mutex::new(HashMap::new()),
            state_listeners: Mutex::new(Vec::new()),
            attachment_listeners: Mutex::new(Vec::new()),
            services: Mutex::new(HashMap::new()),
            request_sequence: AtomicU64::new(0),
            subscription_sequence: AtomicU64::new(0),
            listener_sequence: AtomicU64::new(0),
            hello: Mutex::new(None),
            attachment: Mutex::new(None),
            disposed: AtomicBool::new(false),
        });
        let sink: Weak<dyn ConnectionSink> = {
            let erased: Arc<ClientInner> = Arc::clone(&inner);
            let weak: Weak<ClientInner> = Arc::downgrade(&erased);
            weak
        };
        let connection = Connection::new(
            inner.options.transport_factory.clone(),
            inner.options.server_id.clone(),
            inner
                .options
                .max_frame_length
                .unwrap_or(DEFAULT_MAX_FRAME_LENGTH),
            sink,
        )?;
        let _ = inner.connection.set(connection);
        Ok(Client { inner })
    }

    /// Creates a client and immediately connects it, disposing it on failure.
    pub async fn connect_with(options: ClientOptions) -> Result<Self, ClientError> {
        let client = Self::new(options)?;
        match client.connect().await {
            Ok(_) => Ok(client),
            Err(error) => {
                client.dispose().await;
                Err(error)
            }
        }
    }

    /// Whether [`dispose`](Self::dispose) has run.
    pub fn disposed(&self) -> bool {
        self.inner.disposed.load(Ordering::SeqCst)
    }

    /// The current connection state.
    pub fn connection_state(&self) -> ConnectionState {
        self.inner.connection_state()
    }

    /// Shorthand for `connection_state() == Connected`.
    pub fn connected(&self) -> bool {
        self.connection_state() == ConnectionState::Connected
    }

    /// The expected logical server identity.
    pub fn server_id(&self) -> &str {
        &self.inner.options.server_id
    }

    /// The confirmed handshake, or `None` while disconnected.
    pub fn hello(&self) -> Option<ServerHello> {
        self.inner.hello.lock().clone()
    }

    /// The currently selected session attachment.
    pub fn attachment(&self) -> Option<SessionTarget> {
        self.inner.attachment.lock().clone()
    }

    /// Opens the transport and completes the handshake.
    pub async fn connect(&self) -> Result<ServerHello, ClientError> {
        if self.disposed() {
            return Err(ClientError::Disposed);
        }
        *self.inner.hello.lock() = None;
        self.inner.connection().connect().await
    }

    /// Alias of [`connect`](Self::connect), matching upstream's `reconnect`.
    pub async fn reconnect(&self) -> Result<ServerHello, ClientError> {
        self.connect().await
    }

    /// Fails the connection with a human-readable reason.
    pub fn disconnect(&self, reason: impl Into<String>) {
        self.inner
            .connection()
            .disconnect(ClientError::disconnected(reason));
    }

    /// Registers a connection-state listener.
    pub fn on_connection_state_change(
        &self,
        listener: ConnectionStateListener,
    ) -> Result<Unsubscribe, ClientError> {
        if self.disposed() {
            return Err(ClientError::Disposed);
        }
        Ok(self.inner.add_state_listener(listener))
    }

    /// Registers an attachment-change listener.
    pub fn on_attachment_change(
        &self,
        listener: AttachmentChangeListener,
    ) -> Result<Unsubscribe, ClientError> {
        if self.disposed() {
            return Err(ClientError::Disposed);
        }
        Ok(self.inner.add_attachment_listener(listener))
    }

    /// Invokes one low-level protocol call against an explicit routed target.
    pub async fn request(
        &self,
        target: RpcTarget,
        call: ServiceCall,
        cancel: Option<RequestCancel>,
    ) -> Result<Option<Value>, ClientError> {
        self.inner.request(target, call, cancel).await
    }

    /// Lists the target's published services.
    pub async fn service_catalogue(
        &self,
        target: RpcTarget,
        cancel: Option<RequestCancel>,
    ) -> Result<Vec<ServiceCatalogueEntry>, ClientError> {
        let result = self
            .inner
            .request(target, create_service_catalogue_call(), cancel)
            .await?;
        match parse_service_catalogue(result.as_ref().unwrap_or(&Value::Null)) {
            Ok(catalogue) => Ok(catalogue),
            Err(error) => {
                let error = ClientError::from(error);
                self.inner.fail_connection(error.clone());
                Err(error)
            }
        }
    }

    /// Opens a service subscription, returning its snapshot plus a live stream.
    ///
    /// Updates are buffered until [`ServiceSubscription::start`] runs.
    pub async fn subscribe_service(
        &self,
        target: RpcTarget,
        service_id: &str,
        mode: ServiceMode,
        listener: ServiceListener,
        cancel: Option<RequestCancel>,
    ) -> Result<ServiceSubscription, ClientError> {
        if self.disposed() {
            return Err(ClientError::Disposed);
        }
        let subscription_id = format!(
            "service-{}",
            self.inner
                .subscription_sequence
                .fetch_add(1, Ordering::SeqCst)
                + 1
        );
        let active =
            ActiveServiceListener::new(listener, self.inner.options.on_listener_error.clone());
        self.inner
            .services
            .lock()
            .insert(subscription_id.clone(), Arc::clone(&active));

        let result = match self
            .inner
            .request(
                target.clone(),
                create_service_subscribe_call(&subscription_id, service_id, mode),
                cancel,
            )
            .await
        {
            Ok(result) => result,
            Err(error) => {
                self.inner.remove_service(&subscription_id, &active);
                active.close().await;
                return Err(error);
            }
        };
        let wire =
            match parse_wire_service_subscription_snapshot(result.as_ref().unwrap_or(&Value::Null))
            {
                Ok(wire) => wire,
                Err(error) => {
                    self.inner.remove_service(&subscription_id, &active);
                    let error = ClientError::from(error);
                    self.inner.fail_connection(error.clone());
                    active.close().await;
                    return Err(error);
                }
            };
        let snapshot = match active.hydrate(&wire) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.inner.remove_service(&subscription_id, &active);
                self.inner.fail_connection(error.clone());
                active.close().await;
                return Err(error);
            }
        };
        if !self.inner.is_service_active(&subscription_id, &active) {
            active.close().await;
            return Err(ClientError::disconnected("Client is disconnected"));
        }
        Ok(ServiceSubscription::new(
            subscription_id,
            target,
            snapshot,
            active,
            Arc::downgrade(&self.inner),
        ))
    }

    /// Disposes the client, rejecting every pending request.
    ///
    /// Idempotent.
    pub async fn dispose(&self) {
        if self.inner.disposed.swap(true, Ordering::SeqCst) {
            return;
        }
        let error = ClientError::Disposed;
        self.inner.reject_pending(error.clone());
        self.inner.connection().disconnect(error);
        *self.inner.hello.lock() = None;
        self.inner.set_attachment(None);
        self.inner.state_listeners.lock().clear();
        self.inner.attachment_listeners.lock().clear();
        self.inner.services.lock().clear();
    }
}

impl std::fmt::Debug for Client {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Client")
            .field("server_id", &self.server_id())
            .field("connection_state", &self.connection_state())
            .field("disposed", &self.disposed())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{ByteTransport, FactoryFn};

    #[test]
    fn server_ids_are_validated_before_connecting() {
        let error = Client::new(ClientOptions::new(unused_factory(), "not-a-uuid"))
            .expect_err("invalid server id");
        assert!(matches!(error, ClientError::Configuration(_)));
    }

    #[test]
    fn frame_length_bounds_are_validated() {
        let options = ClientOptions::new(unused_factory(), "00000000-0000-4000-8000-000000000001")
            .with_max_frame_length(0);
        let error = Client::new(options).expect_err("zero frame length");
        assert!(matches!(error, ClientError::Configuration(_)));
    }

    fn unused_factory() -> Arc<dyn ByteTransportFactory> {
        Arc::new(FactoryFn::new(|_handlers| {
            Box::pin(async {
                Err::<Arc<dyn ByteTransport>, ClientError>(ClientError::disconnected("unused"))
            })
        }))
    }
}
