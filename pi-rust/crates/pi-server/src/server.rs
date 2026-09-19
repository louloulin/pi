//! The routed session server — Rust port of `packages/server/src/server.ts`.
//!
//! # Execution model
//!
//! Upstream's server is an async/await state machine: every routing operation
//! (`attachClient`, `invokeService`, ...) returns a promise and the server
//! awaits it inline. The Rust workspace's service and session layers
//! (`pi-chord`) are *synchronous* — they ship their own executor and expose no
//! futures — so this port keeps the service dispatch synchronous and confines
//! the async boundary to the transport:
//!
//! * `Server` owns a Tokio runtime handle captured at [`Server::start`].
//! * Each accepted connection gets a writer task that owns its
//!   [`ByteConnection`] and drains an unbounded outbound queue, so
//!   [`ByteConnection`] never needs an async lock while the connection state
//!   mutex is held.
//! * `handle_request` runs the synchronous service invocation on a
//!   `spawn_blocking` thread; cancellation aborts the request's
//!   [`AbortSignal`](pi_chord::context::AbortSignal) and the response is sent
//!   once the invocation returns, matching upstream's observable ordering.
//!
//! # Deliberate deviations from upstream
//!
//! * `finish_handshake` completes synchronously, so messages never need to be
//!   parked behind a handshake promise; the externally visible `handshaking`
//!   stage still exists and a handshake timeout is still enforced.
//! * `Server::new` returns an `Arc<Server>` because the acceptor closure and
//!   cancellation watcher outlive the constructor.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use pi_chord::context::{background_context, todo_context, with_abort_signal, AbortSignal, Context};
use pi_chord::services::{
    decode_service_control_call, parse_service_call, parse_service_subscription_snapshot,
    ServiceCall, ServiceControlCall, ServiceProviderUpdate, ServiceStateEncoder,
};
use pi_protocol::rpc::{
    encode_server_message, is_supported_protocol_version, CancelEnvelope, ClientMessage,
    ClientMessageDecoder, ProtocolError, RequestEnvelope, RpcTarget, ServerMessage, SessionTarget,
    PROTOCOL_VERSION,
};
use serde_json::Value;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::Notify;

use crate::connection::{
    is_terminal_connection, ByteConnection, ByteConnectionAcceptor, ByteConnectionHandler,
    ConnectionStage,
};
use crate::errors::ServerError;
use crate::listener::ServerListener;
use crate::session_router::{ClientKey, SessionRouter};
use crate::types::{
    target_session, AttachmentSink, ConnectionCountObserver, ErrorObserver,
    RoutedServerPresentation, RoutedServerServiceAttachment, ServerHost, ServerOptions,
    ServicePublish, SessionMetadata, SinkFn,
};

const DEFAULT_HANDSHAKE_TIMEOUT_MS: u64 = 5_000;
const MAX_UINT32: usize = 0xffff_ffff;
const MAX_TIMER_DELAY_MS: u64 = 2_147_483_647;

/// One outbound item queued for a connection's writer task.
enum Outbound {
    /// A complete frame.
    Message(Vec<u8>),
    /// Close the transport, optionally after delivering a final frame.
    Close(Option<Vec<u8>>),
}

/// A request the server has admitted but not answered yet.
struct ActiveRequest {
    signal: AbortSignal,
    target: RpcTarget,
}

struct ConnectionInner {
    stage: ConnectionStage,
    disconnected: bool,
    decoder: ClientMessageDecoder,
    server_services: Option<Arc<dyn RoutedServerServiceAttachment>>,
    active_requests: HashMap<String, ActiveRequest>,
    service_state_encoders: HashMap<String, ServiceStateEncoder>,
}

/// The server's view of one accepted connection.
struct Connection {
    id: ClientKey,
    connection: Arc<dyn ByteConnection>,
    outbound: UnboundedSender<Outbound>,
    inner: Mutex<ConnectionInner>,
}

impl Connection {
    fn stage(&self) -> ConnectionStage {
        self.inner.lock().stage
    }

    fn set_stage(&self, stage: ConnectionStage) {
        self.inner.lock().stage = stage;
    }

    fn is_terminal(&self) -> bool {
        let inner = self.inner.lock();
        is_terminal_connection(inner.stage, inner.disconnected)
    }

    fn enqueue(&self, item: Outbound) {
        let _ = self.outbound.send(item);
    }
}

struct ServerInner {
    starting: bool,
    started: bool,
    closed_settled: bool,
    close_result: Option<Result<(), ServerError>>,
    connections: HashMap<ClientKey, Arc<Connection>>,
    next_connection_id: ClientKey,
    runtime: Option<tokio::runtime::Handle>,
}

type RequestOutcome = Result<Option<Value>, RequestFailure>;

/// Why a request could not be answered with a value.
enum RequestFailure {
    /// The message was structurally wrong.
    Protocol(ProtocolError),
    /// The host or router reported a bounded failure.
    Server(ServerError),
}

impl RequestFailure {
    fn to_protocol_error(&self) -> ProtocolError {
        match self {
            RequestFailure::Protocol(error) => error.clone(),
            RequestFailure::Server(error) => error.to_protocol_error(),
        }
    }
}

impl From<ServerError> for RequestFailure {
    fn from(error: ServerError) -> Self {
        RequestFailure::Server(error)
    }
}

fn invalid_request(message: impl Into<String>) -> RequestFailure {
    RequestFailure::Protocol(ProtocolError {
        code: "invalid_request".to_owned(),
        message: message.into(),
    })
}

/// Publishing state for one subscription request.
struct PublishState {
    ready: bool,
    pending: Vec<(String, ServiceProviderUpdate)>,
}

/// A routed session server.
pub struct Server<TMetadata: SessionMetadata> {
    server_id: String,
    host: Arc<dyn ServerHost<TMetadata>>,
    listeners: Vec<Arc<dyn ServerListener>>,
    max_frame_length: usize,
    handshake_timeout: Duration,
    on_connection_count_changed: Option<ConnectionCountObserver>,
    on_error: Option<ErrorObserver>,
    sessions: Arc<SessionRouter<TMetadata>>,
    closing: Arc<Mutex<bool>>,
    inner: Mutex<ServerInner>,
    closed_notify: Notify,
}

impl<TMetadata: SessionMetadata> Server<TMetadata> {
    /// Creates an unstarted server.
    pub fn new(host: Arc<dyn ServerHost<TMetadata>>, options: ServerOptions) -> Result<Arc<Self>, ServerError> {
        let max_frame_length = options.max_frame_length.unwrap_or(
            pi_protocol::rpc::DEFAULT_MAX_FRAME_LENGTH,
        );
        if !pi_protocol::rpc::is_server_id(&options.server_id) {
            return Err(ServerError::internal(
                "serverId must be a canonical lowercase UUIDv4",
            ));
        }
        if max_frame_length == 0 || max_frame_length > MAX_UINT32 {
            return Err(ServerError::internal(format!(
                "Server maxFrameLength must be an integer between 1 and {MAX_UINT32}"
            )));
        }
        let handshake_timeout_ms = options.handshake_timeout_ms.unwrap_or(DEFAULT_HANDSHAKE_TIMEOUT_MS);
        if handshake_timeout_ms == 0 || handshake_timeout_ms > MAX_TIMER_DELAY_MS {
            return Err(ServerError::internal(format!(
                "Server handshakeTimeoutMs must be an integer between 1 and {MAX_TIMER_DELAY_MS}"
            )));
        }
        let server_id = options.server_id;
        let closing = Arc::new(Mutex::new(false));
        let is_closing: Arc<dyn Fn() -> bool + Send + Sync> = {
            let closing = Arc::clone(&closing);
            Arc::new(move || *closing.lock())
        };
        let sessions = Arc::new(SessionRouter::new(
            Arc::clone(&host),
            server_id.clone(),
            is_closing,
            options.on_error.clone(),
        ));
        let server = Arc::new(Self {
            server_id,
            host,
            listeners: options.listeners,
            max_frame_length,
            handshake_timeout: Duration::from_millis(handshake_timeout_ms),
            on_connection_count_changed: options.on_connection_count_changed,
            on_error: options.on_error,
            sessions,
            closing,
            inner: Mutex::new(ServerInner {
                starting: false,
                started: false,
                closed_settled: false,
                close_result: None,
                connections: HashMap::new(),
                next_connection_id: 1,
                runtime: None,
            }),
            closed_notify: Notify::new(),
        });
        Ok(server)
    }

    /// The logical server identity.
    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    /// The maximum framed byte length.
    pub fn max_frame_length(&self) -> usize {
        self.max_frame_length
    }

    /// The live connection count.
    pub fn connection_count(&self) -> usize {
        self.inner.lock().connections.len()
    }

    /// Starts every listener and begins accepting connections.
    pub async fn start(self: &Arc<Self>) -> Result<(), ServerError> {
        {
            let inner = self.inner.lock();
            if inner.started {
                return Err(ServerError::internal("Server is already started"));
            }
            if inner.starting {
                return Err(ServerError::internal("Server is already starting"));
            }
            drop(inner);
            if self.is_closing() {
                return Err(ServerError::internal("Server is closing or closed"));
            }
            let mut inner = self.inner.lock();
            inner.starting = true;
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                inner.runtime = Some(handle);
            }
        }

        let accept: ByteConnectionAcceptor = {
            let server = Arc::clone(self);
            Arc::new(move |connection| server.accept(connection))
        };

        for (started, listener) in self.listeners.iter().enumerate() {
            if let Err(error) = listener.start(Arc::clone(&accept)).await {
                *self.closing.lock() = true;
                let mut cleanup_errors = Vec::new();
                for listener in self.listeners.iter().take(started) {
                    if let Err(cleanup) = listener.close().await {
                        cleanup_errors.push(cleanup);
                    }
                }
                if let Err(cleanup) = self.close_server_state().await {
                    cleanup_errors.push(cleanup);
                }
                let result = if cleanup_errors.is_empty() {
                    Err(error)
                } else {
                    Err(ServerError::internal(format!(
                        "Server startup and cleanup failed: {error}"
                    )))
                };
                self.settle_closed(result.clone());
                return result;
            }
        }
        {
            let mut inner = self.inner.lock();
            inner.started = true;
            inner.starting = false;
        }
        Ok(())
    }

    /// Stops every listener and closes every hosted session.
    pub async fn close(self: &Arc<Self>) -> Result<(), ServerError> {
        {
            let inner = self.inner.lock();
            if inner.closed_settled {
                return inner.close_result.clone().unwrap_or(Ok(()));
            }
        }
        *self.closing.lock() = true;
        let mut errors = Vec::new();
        for listener in &self.listeners {
            if let Err(error) = listener.close().await {
                errors.push(error);
            }
        }
        if let Err(error) = self.close_server_state().await {
            errors.push(error);
        }
        self.inner.lock().started = false;
        let result = match errors.len() {
            0 => Ok(()),
            1 => Err(errors.remove(0)),
            _ => Err(ServerError::internal("Server shutdown failed")),
        };
        self.settle_closed(result.clone());
        result
    }

    /// Resolves after shutdown, or with the failure that ended it.
    pub async fn closed(&self) -> Result<(), ServerError> {
        loop {
            let notified = self.closed_notify.notified();
            if let Some(result) = self.inner.lock().close_result.clone() {
                return result;
            }
            notified.await;
        }
    }

    /// Whether the server is shutting down or shut down.
    pub fn is_closing(&self) -> bool {
        *self.closing.lock()
    }

    /// Accepts one authorized byte connection.
    pub fn accept(self: &Arc<Self>, connection: Arc<dyn ByteConnection>) -> Arc<dyn ByteConnectionHandler> {
        if self.is_closing() {
            self.spawn(async move {
                let _ = connection.close(None).await;
            });
            return Arc::new(RejectHandler {
                on_error: self.on_error.clone(),
            });
        }
        let id = {
            let mut inner = self.inner.lock();
            let id = inner.next_connection_id;
            inner.next_connection_id = id.wrapping_add(1);
            id
        };
        let (outbound, receiver) = unbounded_channel();
        let conn = Arc::new(Connection {
            id,
            connection: Arc::clone(&connection),
            outbound,
            inner: Mutex::new(ConnectionInner {
                stage: ConnectionStage::AwaitingHello,
                disconnected: false,
                decoder: ClientMessageDecoder::new(None),
                server_services: None,
                active_requests: HashMap::new(),
                service_state_encoders: HashMap::new(),
            }),
        });
        {
            let mut inner = self.inner.lock();
            inner.connections.insert(id, Arc::clone(&conn));
        }
        self.notify_connection_count();
        self.spawn(write_loop(connection, receiver));
        let timeout_server = Arc::clone(self);
        let timeout_conn = Arc::clone(&conn);
        let handshake_timeout = self.handshake_timeout;
        self.spawn(async move {
            tokio::time::sleep(handshake_timeout).await;
            let still_handshaking = {
                let inner = timeout_conn.inner.lock();
                matches!(
                    inner.stage,
                    ConnectionStage::AwaitingHello | ConnectionStage::Handshaking
                )
            };
            if still_handshaking {
                timeout_server
                    .fail_protocol(
                        &timeout_conn,
                        ProtocolError {
                            code: "invalid_request".to_owned(),
                            message: "Handshake timeout".to_owned(),
                        },
                    )
                    .await;
            }
        });
        Arc::new(ServerConnectionHandler {
            server: Arc::clone(self),
            conn,
        })
    }

    /// Sends one protocol message, returning whether it was queued.
    fn send_message(
        self: &Arc<Self>,
        conn: &Arc<Connection>,
        message: &ServerMessage,
    ) -> bool {
        if conn.is_terminal() || conn.connection.closed() {
            return false;
        }
        let frame = match encode_server_message(message, Some(self.max_frame_length)) {
            Ok(frame) => frame,
            Err(error) => {
                self.report(ServerError::internal(error.to_string()));
                conn.enqueue(Outbound::Close(None));
                self.disconnect(conn);
                return false;
            }
        };
        if conn.connection.closed() {
            self.disconnect(conn);
            return false;
        }
        conn.enqueue(Outbound::Message(frame));
        true
    }

    fn send_service_update(
        self: &Arc<Self>,
        conn: &Arc<Connection>,
        subscription_id: &str,
        update: &ServiceProviderUpdate,
    ) -> Result<(), ServerError> {
        let message = {
            let mut inner = conn.inner.lock();
            match inner.service_state_encoders.get_mut(subscription_id) {
                Some(encoder) => ServerMessage::service_update(
                    subscription_id,
                    encoder.encode_update(update.clone())?.to_json(),
                ),
                None => return Ok(()),
            }
        };
        self.send_message(conn, &message);
        Ok(())
    }

    fn receive(self: &Arc<Self>, conn: &Arc<Connection>, chunk: Vec<u8>) {
        if conn.is_terminal() {
            return;
        }
        let messages = {
            let mut inner = conn.inner.lock();
            inner.decoder.push(&chunk)
        };
        match messages {
            Err(error) => {
                self.spawn_fail(
                    conn,
                    ProtocolError {
                        code: "invalid_request".to_owned(),
                        message: error.to_string(),
                    },
                );
            }
            Ok(messages) => {
                for message in messages {
                    if conn.is_terminal() {
                        return;
                    }
                    self.dispatch_message(conn, message);
                }
            }
        }
    }

    fn dispatch_message(self: &Arc<Self>, conn: &Arc<Connection>, message: ClientMessage) {
        let stage = conn.stage();
        if stage == ConnectionStage::AwaitingHello {
            match message {
                ClientMessage::Hello { version } => {
                    conn.set_stage(ConnectionStage::Handshaking);
                    self.finish_handshake(conn, version);
                }
                _ => self.spawn_fail(
                    conn,
                    ProtocolError {
                        code: "invalid_request".to_owned(),
                        message: "The first client message must be hello".to_owned(),
                    },
                ),
            }
            return;
        }
        if matches!(message, ClientMessage::Hello { .. }) {
            self.spawn_fail(
                conn,
                ProtocolError {
                    code: "invalid_request".to_owned(),
                    message: "hello may only be sent as the first message".to_owned(),
                },
            );
            return;
        }
        if stage != ConnectionStage::Ready {
            return;
        }
        match message {
            ClientMessage::Cancel { id, target } => {
                self.handle_cancel(conn, &CancelEnvelope { id, target })
            }
            ClientMessage::Request { id, target, call } => {
                let server = Arc::clone(self);
                let conn = Arc::clone(conn);
                let envelope = RequestEnvelope { id, target, call };
                self.spawn(async move {
                    server.handle_request(conn, envelope).await;
                });
            }
            ClientMessage::Hello { .. } => {}
        }
    }

    fn finish_handshake(self: &Arc<Self>, conn: &Arc<Connection>, version: u32) {
        if !is_supported_protocol_version(version) {
            self.spawn_fail(
                conn,
                ProtocolError {
                    code: "version".to_owned(),
                    message: format!(
                        "Unsupported protocol version {version}; expected {PROTOCOL_VERSION}"
                    ),
                },
            );
            return;
        }
        if self.is_closing() || conn.is_terminal() || conn.connection.closed() {
            return;
        }
        let sink: Arc<dyn AttachmentSink> = {
            let server = Arc::clone(self);
            let conn = Arc::clone(conn);
            Arc::new(SinkFn(Arc::new(move |attachment: Option<SessionTarget>| {
                server.send_message(&conn, &ServerMessage::attachment(attachment));
            })))
        };
        let presentation: Arc<dyn RoutedServerPresentation> = Arc::new(ServerPresentation {
            server: Arc::clone(self),
            conn: Arc::clone(conn),
            sink,
        });
        let services = match self
            .host
            .server_services()
            .attach_client(presentation, todo_context())
        {
            Ok(services) => services,
            Err(error) => {
                self.spawn_fail(conn, error.to_protocol_error());
                return;
            }
        };
        if self.is_closing() || conn.is_terminal() {
            let _ = services.release(todo_context());
            return;
        }
        conn.inner.lock().server_services = Some(services);
        let sent = self.send_message(conn, &ServerMessage::hello(self.server_id.clone()));
        if sent && !conn.is_terminal() {
            conn.set_stage(ConnectionStage::Ready);
        }
    }

    fn handle_cancel(&self, conn: &Arc<Connection>, envelope: &CancelEnvelope) {
        if envelope.target.server_id() != self.server_id {
            return;
        }
        let inner = conn.inner.lock();
        if let Some(active) = inner.active_requests.get(&envelope.id) {
            if same_target(&active.target, &envelope.target) {
                active.signal.abort("RPC request cancelled");
            }
        }
    }

    async fn handle_request(self: Arc<Self>, conn: Arc<Connection>, envelope: RequestEnvelope) {
        {
            let inner = conn.inner.lock();
            if inner.active_requests.contains_key(&envelope.id) {
                drop(inner);
                self.send_message(
                    &conn,
                    &ServerMessage::response_error(
                        envelope.id.clone(),
                        ProtocolError {
                            code: "invalid_request".to_owned(),
                            message: "Request ID is already active".to_owned(),
                        },
                    ),
                );
                return;
            }
        }
        let call = match parse_service_call(&envelope.call) {
            Ok(call) => call,
            Err(_) => {
                self.send_message(
                    &conn,
                    &ServerMessage::response_error(
                        envelope.id.clone(),
                        ProtocolError {
                            code: "invalid_request".to_owned(),
                            message: "Invalid service call".to_owned(),
                        },
                    ),
                );
                return;
            }
        };
        let control = decode_service_control_call(&call);
        let subscribing = match &control {
            Some(ServiceControlCall::Subscribe {
                subscription_id, ..
            }) => Some(subscription_id.clone()),
            _ => None,
        };
        if let Some(subscription_id) = &subscribing {
            let duplicate = conn
                .inner
                .lock()
                .service_state_encoders
                .contains_key(subscription_id);
            if duplicate {
                self.send_message(
                    &conn,
                    &ServerMessage::response_error(
                        envelope.id.clone(),
                        ProtocolError {
                            code: "invalid_request".to_owned(),
                            message: format!("Duplicate service subscription {subscription_id}"),
                        },
                    ),
                );
                return;
            }
        }
        if target_session(&envelope.target).is_none() && conn.inner.lock().server_services.is_none() {
            self.send_message(
                &conn,
                &ServerMessage::response_error(
                    envelope.id.clone(),
                    ProtocolError {
                        code: "invalid_request".to_owned(),
                        message: format!("Unknown service member {}.{}", call.service_id, call.member),
                    },
                ),
            );
            return;
        }

        let controller = pi_chord::context::AbortController::new();
        let signal = controller.signal();
        conn.inner.lock().active_requests.insert(
            envelope.id.clone(),
            ActiveRequest {
                signal: signal.clone(),
                target: envelope.target.clone(),
            },
        );
        let context = with_abort_signal(signal.clone(), todo_context());
        let state = Arc::new(Mutex::new(PublishState {
            ready: subscribing.is_none(),
            pending: Vec::new(),
        }));
        let publish: ServicePublish = {
            let server = Arc::clone(&self);
            let conn = Arc::clone(&conn);
            let state = Arc::clone(&state);
            let subscribing = subscribing.clone();
            Arc::new(
                move |subscription_id: &str,
                      update: &ServiceProviderUpdate,
                      _context: &Context|
                      -> Result<(), ServerError> {
                    let mut guard = state.lock();
                    if let Some(target) = &subscribing {
                        if subscription_id == target && !guard.ready {
                            guard.pending.push((subscription_id.to_owned(), update.clone()));
                            return Ok(());
                        }
                    }
                    drop(guard);
                    server.send_service_update(&conn, subscription_id, update)
                },
            )
        };

        let invoke_server = Arc::clone(&self);
        let invoke_conn = Arc::clone(&conn);
        let invoke_call = call.clone();
        let invoke_target = envelope.target.clone();
        let invoke_publish = Arc::clone(&publish);
        let invoke_context = context.clone();
        let joined = tokio::task::spawn_blocking(move || {
            invoke_server.invoke_target(
                &invoke_conn,
                &invoke_call,
                &invoke_target,
                &invoke_publish,
                &invoke_context,
            )
        })
        .await;

        let outcome: RequestOutcome = match joined {
            Err(error) => Err(RequestFailure::Server(ServerError::internal(error.to_string()))),
            Ok(result) => result,
        };
        if signal.is_aborted() {
            // Cancellation arrived while the synchronous invocation was running;
            // the abort cannot interrupt a blocking call, so answer with the
            // cancelled failure instead of the value it eventually produced.
            self.respond_failure(
                &conn,
                &envelope.id,
                &signal,
                RequestFailure::Server(ServerError::internal("RPC request cancelled")),
            );
            conn.inner.lock().active_requests.remove(&envelope.id);
            return;
        }
        match outcome {
            Ok(Some(result)) => {
                let response = if let Some(subscription_id) = &subscribing {
                    match self.install_subscription(&conn, subscription_id, result) {
                        Ok(value) => value,
                        Err(error) => {
                            self.respond_failure(&conn, &envelope.id, &signal, error);
                            conn.inner.lock().active_requests.remove(&envelope.id);
                            return;
                        }
                    }
                } else {
                    if let Some(ServiceControlCall::Unsubscribe { subscription_id }) = &control {
                        conn.inner.lock().service_state_encoders.remove(subscription_id);
                    }
                    result
                };
                self.send_message(&conn, &ServerMessage::response_ok(envelope.id.clone(), Some(response)));
                if subscribing.is_some() {
                    let pending = {
                        let mut guard = state.lock();
                        let pending = std::mem::take(&mut guard.pending);
                        guard.ready = true;
                        pending
                    };
                    for (subscription_id, update) in pending {
                        let _ = self.send_service_update(&conn, &subscription_id, &update);
                    }
                }
            }
            Ok(None) => {
                if let Some(ServiceControlCall::Unsubscribe { subscription_id }) = &control {
                    conn.inner.lock().service_state_encoders.remove(subscription_id);
                }
                self.send_message(&conn, &ServerMessage::response_ok(envelope.id.clone(), None));
            }
            Err(error) => {
                if let Some(subscription_id) = &subscribing {
                    conn.inner.lock().service_state_encoders.remove(subscription_id);
                }
                self.respond_failure(&conn, &envelope.id, &signal, error);
            }
        }
        conn.inner.lock().active_requests.remove(&envelope.id);
    }

    fn invoke_target(
        &self,
        conn: &Arc<Connection>,
        call: &ServiceCall,
        target: &RpcTarget,
        publish: &ServicePublish,
        context: &Context,
    ) -> RequestOutcome {
        if target.server_id() != self.server_id {
            return Err(RequestFailure::Server(ServerError::wrong_server()));
        }
        if target_session(target).is_some() {
            return self
                .sessions
                .execute_service_call(call, target, conn.id, publish, context)
                .map_err(RequestFailure::Server);
        }
        let services = match conn.inner.lock().server_services.as_ref() {
            Some(services) => Arc::clone(services),
            None => {
                return Err(invalid_request(format!(
                    "Unknown service member {}.{}",
                    call.service_id, call.member
                )))
            }
        };
        services
            .invoke_service(call, publish, context)
            .map_err(RequestFailure::Server)
    }

    fn install_subscription(
        &self,
        conn: &Arc<Connection>,
        subscription_id: &str,
        result: Value,
    ) -> Result<Value, RequestFailure> {
        let snapshot = parse_service_subscription_snapshot(&result)
            .map_err(|error| RequestFailure::Server(ServerError::from(error)))?;
        let mut encoder = ServiceStateEncoder::new();
        let wire = encoder
            .encode_snapshot(snapshot)
            .map_err(|error| RequestFailure::Server(ServerError::from(error)))?;
        conn.inner
            .lock()
            .service_state_encoders
            .insert(subscription_id.to_owned(), encoder);
        Ok(wire.to_json())
    }

    fn respond_failure(
        self: &Arc<Self>,
        conn: &Arc<Connection>,
        request_id: &str,
        signal: &AbortSignal,
        error: RequestFailure,
    ) {
        let protocol_error = if signal.is_aborted() {
            ProtocolError {
                code: "cancelled".to_owned(),
                message: "RPC request cancelled".to_owned(),
            }
        } else {
            error.to_protocol_error()
        };
        self.send_message(
            conn,
            &ServerMessage::response_error(request_id.to_owned(), protocol_error),
        );
    }

    fn transport_closed(self: &Arc<Self>, conn: &Arc<Connection>) {
        if !conn.is_terminal() {
            let result = {
                let mut inner = conn.inner.lock();
                inner.decoder.end()
            };
            if let Err(error) = result {
                self.report(ServerError::internal(error.to_string()));
            }
        }
        self.disconnect(conn);
    }

    fn disconnect(self: &Arc<Self>, conn: &Arc<Connection>) {
        let server_services = {
            let mut inner = conn.inner.lock();
            if inner.disconnected {
                return;
            }
            inner.disconnected = true;
            inner.stage = ConnectionStage::Closed;
            let signals: Vec<AbortSignal> = inner
                .active_requests
                .values()
                .map(|active| active.signal.clone())
                .collect();
            for signal in signals {
                signal.abort("Client disconnected");
            }
            inner.active_requests.clear();
            inner.service_state_encoders.clear();
            inner.server_services.take()
        };
        let removed = {
            let mut inner = self.inner.lock();
            inner.connections.remove(&conn.id).is_some()
        };
        if removed {
            self.notify_connection_count();
        }
        conn.enqueue(Outbound::Close(None));
        if let Err(error) = self.sessions.disconnect(conn.id, todo_context()) {
            self.report(error);
        }
        if let Some(services) = server_services {
            if let Err(error) = services.release(todo_context()) {
                self.report(error);
            }
        }
    }

    async fn fail_protocol(self: &Arc<Self>, conn: &Arc<Connection>, error: ProtocolError) {
        let proceed = {
            let mut inner = conn.inner.lock();
            if inner.disconnected
                || matches!(inner.stage, ConnectionStage::Closing | ConnectionStage::Closed)
            {
                false
            } else {
                inner.stage = ConnectionStage::Closing;
                true
            }
        };
        if !proceed {
            return;
        }
        let final_frame = encode_server_message(
            &ServerMessage::hello_error(error),
            Some(self.max_frame_length),
        )
        .ok();
        conn.enqueue(Outbound::Close(final_frame));
        self.disconnect(conn);
    }

    async fn close_server_state(self: &Arc<Self>) -> Result<(), ServerError> {
        let connections: Vec<Arc<Connection>> = {
            let inner = self.inner.lock();
            inner.connections.values().cloned().collect()
        };
        for connection in &connections {
            connection.set_stage(ConnectionStage::Closing);
        }
        let mut errors = Vec::new();
        for connection in &connections {
            if let Err(error) = connection.connection.close(None).await {
                errors.push(error);
            }
        }
        for connection in &connections {
            self.disconnect(connection);
        }
        if let Err(error) = self.sessions.close(background_context()) {
            errors.push(error);
        }
        self.inner.lock().connections.clear();
        match errors.len() {
            0 => Ok(()),
            1 => Err(errors.remove(0)),
            _ => Err(ServerError::internal("Failed to close server Sessions")),
        }
    }

    fn notify_connection_count(&self) {
        if let Some(observer) = &self.on_connection_count_changed {
            observer(self.connection_count());
        }
    }

    fn report(&self, error: ServerError) {
        if let Some(observer) = &self.on_error {
            observer(error);
        }
    }

    fn settle_closed(&self, result: Result<(), ServerError>) {
        let mut inner = self.inner.lock();
        if inner.closed_settled {
            return;
        }
        inner.closed_settled = true;
        inner.close_result = Some(result);
        drop(inner);
        self.closed_notify.notify_waiters();
    }

    fn runtime(&self) -> Option<tokio::runtime::Handle> {
        self.inner.lock().runtime.clone()
    }

    fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if let Some(runtime) = self.runtime() {
            runtime.spawn(future);
        }
    }

    fn spawn_fail(self: &Arc<Self>, conn: &Arc<Connection>, error: ProtocolError) {
        let server = Arc::clone(self);
        let conn = Arc::clone(conn);
        self.spawn(async move {
            server.fail_protocol(&conn, error).await;
        });
    }

    /// The router used by this server (exposed for tests and hosts).
    pub fn sessions(&self) -> &Arc<SessionRouter<TMetadata>> {
        &self.sessions
    }

    /// The host this server routes to.
    pub fn host(&self) -> &Arc<dyn ServerHost<TMetadata>> {
        &self.host
    }
}

struct ServerPresentation<TMetadata: SessionMetadata> {
    server: Arc<Server<TMetadata>>,
    conn: Arc<Connection>,
    sink: Arc<dyn AttachmentSink>,
}

impl<TMetadata: SessionMetadata> RoutedServerPresentation for ServerPresentation<TMetadata> {
    fn attach_session(&self, session_id: &str, context: &Context) -> Result<(), ServerError> {
        self.server
            .sessions
            .attach_client(self.conn.id, session_id, context, Arc::clone(&self.sink))
    }

    fn detach_session(&self, context: &Context) -> Result<(), ServerError> {
        self.server.sessions.detach_client(self.conn.id, context)
    }

    fn prepare_session_removal(&self, session_id: &str, context: &Context) -> Result<(), ServerError> {
        self.server.sessions.remove_session(session_id, context)
    }
}

struct ServerConnectionHandler<TMetadata: SessionMetadata> {
    server: Arc<Server<TMetadata>>,
    conn: Arc<Connection>,
}

impl<TMetadata: SessionMetadata> ByteConnectionHandler for ServerConnectionHandler<TMetadata> {
    fn on_data(&self, chunk: Vec<u8>) {
        self.server.receive(&self.conn, chunk);
    }

    fn on_close(&self) {
        self.server.transport_closed(&self.conn);
    }

    fn on_error(&self, error: ServerError) {
        self.server.report(error);
        self.server.disconnect(&self.conn);
    }
}

struct RejectHandler {
    on_error: Option<ErrorObserver>,
}

impl ByteConnectionHandler for RejectHandler {
    fn on_data(&self, _chunk: Vec<u8>) {}

    fn on_close(&self) {}

    fn on_error(&self, error: ServerError) {
        if let Some(observer) = &self.on_error {
            observer(error);
        }
    }
}

async fn write_loop(connection: Arc<dyn ByteConnection>, mut receiver: UnboundedReceiver<Outbound>) {
    while let Some(item) = receiver.recv().await {
        match item {
            Outbound::Message(frame) => {
                if connection.send(frame).await.is_err() {
                    break;
                }
            }
            Outbound::Close(final_chunk) => {
                let _ = connection.close(final_chunk).await;
                return;
            }
        }
    }
    let _ = connection.close(None).await;
}

fn same_target(left: &RpcTarget, right: &RpcTarget) -> bool {
    if left.server_id() != right.server_id() {
        return false;
    }
    match (target_session(left), target_session(right)) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.session_id == right.session_id && left.attachment_id == right.attachment_id
        }
        _ => false,
    }
}
