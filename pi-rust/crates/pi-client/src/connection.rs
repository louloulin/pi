//! The connection state machine — Rust port of
//! `packages/client/src/connection.ts`.
//!
//! One `Connection` owns at most one transport and drives the handshake:
//!
//! ```text
//! disconnected ──connect()──▶ connecting ──hello accepted──▶ connected
//!       ▲                          │                            │
//!       └──────── fail / close ────┴──────── fail / close ──────┘
//! ```
//!
//! Stale transport callbacks are ignored through a monotonically increasing
//! connection id, so a transport that finishes opening after the client already
//! moved on is closed instead of being installed.
//!
//! Upstream keeps this class module-private. Rust keeps it `pub(crate)` for the
//! same reason; [`Client`](crate::Client) is the public surface.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use parking_lot::Mutex;
use pi_protocol::rpc::{
    encode_client_message, ClientMessage, ServerHello, ServerMessage, ServerMessageDecoder,
    PROTOCOL_VERSION,
};
use tokio::sync::oneshot;

use crate::errors::{to_disconnected, ClientError};
use crate::transport::{ByteTransport, ByteTransportFactory, ByteTransportHandlers};
use crate::types::{ConnectionState, ConnectionStateChange};

/// The largest frame length the wire layer accepts, mirroring upstream's
/// `MAX_UINT32` guard.
pub(crate) const MAX_FRAME_LENGTH: usize = 0xffff_ffff;

type HandshakeSender = oneshot::Sender<Result<ServerHello, ClientError>>;

enum Lifecycle {
    Disconnected,
    Connecting {
        id: u64,
        decoder: ServerMessageDecoder,
        transport: Option<Arc<dyn ByteTransport>>,
        handshake: Option<HandshakeSender>,
    },
    Connected {
        id: u64,
        decoder: ServerMessageDecoder,
        transport: Arc<dyn ByteTransport>,
        handshake: Option<HandshakeSender>,
    },
}

/// Receives the events the connection produces. Implemented by the client.
pub(crate) trait ConnectionSink: Send + Sync {
    /// The server accepted the handshake.
    fn on_handshake(&self, hello: ServerHello);
    /// An application message arrived on a connected channel.
    fn on_message(&self, message: ServerMessage);
    /// The connection moved to a new state.
    fn on_state_change(&self, change: ConnectionStateChange);
}

/// One connection attempt at a time.
pub(crate) struct Connection {
    factory: Arc<dyn ByteTransportFactory>,
    server_id: String,
    max_frame_length: usize,
    sink: Weak<dyn ConnectionSink>,
    lifecycle: Mutex<Lifecycle>,
    sequence: AtomicU64,
}

impl Connection {
    /// Creates a disconnected connection.
    ///
    /// `sink` is weak because the client owns the connection: a strong
    /// reference here would form a cycle.
    pub(crate) fn new(
        factory: Arc<dyn ByteTransportFactory>,
        server_id: impl Into<String>,
        max_frame_length: usize,
        sink: Weak<dyn ConnectionSink>,
    ) -> Result<Arc<Self>, ClientError> {
        if max_frame_length == 0 || max_frame_length > MAX_FRAME_LENGTH {
            return Err(ClientError::configuration(format!(
                "Client maxFrameLength must be between 1 and {MAX_FRAME_LENGTH}"
            )));
        }
        Ok(Arc::new(Self {
            factory,
            server_id: server_id.into(),
            max_frame_length,
            sink,
            lifecycle: Mutex::new(Lifecycle::Disconnected),
            sequence: AtomicU64::new(0),
        }))
    }

    /// The current state.
    pub(crate) fn state(&self) -> ConnectionState {
        match &*self.lifecycle.lock() {
            Lifecycle::Disconnected => ConnectionState::Disconnected,
            Lifecycle::Connecting { .. } => ConnectionState::Connecting,
            Lifecycle::Connected { .. } => ConnectionState::Connected,
        }
    }

    /// The negotiated frame-length ceiling.
    pub(crate) fn max_frame_length(&self) -> usize {
        self.max_frame_length
    }

    /// Opens a transport and completes the handshake.
    pub(crate) async fn connect(self: &Arc<Self>) -> Result<ServerHello, ClientError> {
        let (id, receiver) = {
            let mut guard = self.lifecycle.lock();
            let state = match &*guard {
                Lifecycle::Disconnected => ConnectionState::Disconnected,
                Lifecycle::Connecting { .. } => ConnectionState::Connecting,
                Lifecycle::Connected { .. } => ConnectionState::Connected,
            };
            if state != ConnectionState::Disconnected {
                return Err(ClientError::disconnected(format!(
                    "Client is already {state}"
                )));
            }
            let id = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
            let (sender, receiver) = oneshot::channel();
            *guard = Lifecycle::Connecting {
                id,
                decoder: ServerMessageDecoder::new(Some(self.max_frame_length)),
                transport: None,
                handshake: Some(sender),
            };
            (id, receiver)
        };
        self.notify_state(ConnectionState::Connecting, None);

        let handlers: Arc<dyn ByteTransportHandlers> = Arc::new(ConnectionHandlers {
            connection: Arc::downgrade(self),
            id,
        });
        let transport = match self.factory.connect(handlers).await {
            Ok(transport) => transport,
            Err(error) => {
                let error = to_disconnected(error);
                if self.is_current(id) {
                    self.fail(error.clone());
                }
                return Err(error);
            }
        };
        {
            let mut guard = self.lifecycle.lock();
            match &mut *guard {
                Lifecycle::Connecting {
                    id: current,
                    transport: slot,
                    ..
                } if *current == id => *slot = Some(Arc::clone(&transport)),
                _ => {
                    drop(guard);
                    transport.close();
                    return Err(ClientError::disconnected(
                        "Connection attempt was superseded",
                    ));
                }
            }
        }

        let hello = match encode_client_message(
            &ClientMessage::hello(PROTOCOL_VERSION),
            Some(self.max_frame_length),
        ) {
            Ok(frame) => frame,
            Err(error) => {
                let error = ClientError::from(error);
                self.fail_and_close_transport(&transport, error.clone());
                return Err(error);
            }
        };
        if let Err(error) = transport.send(hello).await {
            let error = to_disconnected(error);
            self.fail_and_close_transport(&transport, error.clone());
            return Err(error);
        }

        match receiver.await {
            Ok(result) => result,
            Err(_) => Err(ClientError::disconnected("Handshake was dropped")),
        }
    }

    /// Fails the connection with `error` and closes the active transport.
    pub(crate) fn disconnect(&self, error: ClientError) {
        let transport = {
            let guard = self.lifecycle.lock();
            match &*guard {
                Lifecycle::Connected { transport, .. } => Some(Arc::clone(transport)),
                Lifecycle::Connecting { transport, .. } => transport.clone(),
                Lifecycle::Disconnected => None,
            }
        };
        self.fail(error);
        if let Some(transport) = transport {
            transport.close();
        }
    }

    /// Fails the connection without touching the transport.
    pub(crate) fn fail(&self, error: ClientError) {
        let handshake = {
            let mut guard = self.lifecycle.lock();
            match std::mem::replace(&mut *guard, Lifecycle::Disconnected) {
                Lifecycle::Disconnected => return,
                Lifecycle::Connecting { handshake, .. } => handshake,
                Lifecycle::Connected { handshake, .. } => handshake,
            }
        };
        if let Some(sender) = handshake {
            let _ = sender.send(Err(error.clone()));
        }
        if let Some(sink) = self.sink() {
            sink.on_state_change(ConnectionStateChange {
                state: ConnectionState::Disconnected,
                error: Some(error),
            });
        }
    }

    /// Sends one frame on the connected transport.
    pub(crate) async fn send(&self, frame: Vec<u8>) -> Result<(), ClientError> {
        let transport = {
            let guard = self.lifecycle.lock();
            match &*guard {
                Lifecycle::Connected { transport, .. } => Arc::clone(transport),
                _ => return Err(ClientError::disconnected("Client is disconnected")),
            }
        };
        match transport.send(frame).await {
            Ok(()) => Ok(()),
            Err(error) => {
                let error = to_disconnected(error);
                self.fail_and_close_transport(&transport, error.clone());
                Err(error)
            }
        }
    }

    fn fail_and_close_transport(&self, transport: &Arc<dyn ByteTransport>, error: ClientError) {
        let current = {
            let guard = self.lifecycle.lock();
            matches!(&*guard, Lifecycle::Connected { transport: active, .. } if Arc::ptr_eq(active, transport))
        };
        if current {
            self.fail(error);
            transport.close();
        }
    }

    fn handle_data(&self, id: u64, chunk: Vec<u8>) {
        let outcome = {
            let mut guard = self.lifecycle.lock();
            match &mut *guard {
                Lifecycle::Connecting {
                    id: current,
                    transport,
                    decoder,
                    ..
                } if *current == id => {
                    if transport.is_none() {
                        None
                    } else {
                        Some(decoder.push(&chunk))
                    }
                }
                Lifecycle::Connected {
                    id: current,
                    decoder,
                    ..
                } if *current == id => Some(decoder.push(&chunk)),
                _ => return,
            }
        };
        let messages = match outcome {
            None => {
                self.fail(ClientError::protocol(
                    "Received server data before the client hello was sent",
                ));
                return;
            }
            Some(Ok(messages)) => messages,
            Some(Err(error)) => {
                self.fail(ClientError::from(error));
                return;
            }
        };
        for message in messages {
            if self.state() == ConnectionState::Disconnected {
                return;
            }
            self.handle_message(message);
        }
    }

    fn handle_close(&self, id: u64) {
        let error = {
            let mut guard = self.lifecycle.lock();
            match &mut *guard {
                Lifecycle::Connecting {
                    id: current,
                    decoder,
                    ..
                } if *current == id => decoder.end().err().map(ClientError::from),
                Lifecycle::Connected {
                    id: current,
                    decoder,
                    ..
                } if *current == id => decoder.end().err().map(ClientError::from),
                _ => return,
            }
        };
        self.fail(error.unwrap_or_else(|| ClientError::disconnected("Byte transport closed")));
    }

    fn handle_error(&self, id: u64, error: ClientError) {
        if self.is_current(id) {
            self.fail(to_disconnected(error));
        }
    }

    fn handle_message(&self, message: ServerMessage) {
        let Some(sink) = self.sink() else {
            return;
        };
        let connecting = matches!(&*self.lifecycle.lock(), Lifecycle::Connecting { .. });
        if connecting {
            match message {
                ServerMessage::HelloError { error } => {
                    self.fail(ClientError::Server(error));
                    return;
                }
                ServerMessage::Hello { server_id, .. } => {
                    if server_id != self.server_id {
                        self.fail(ClientError::protocol(format!(
                            "Connected server {server_id:?} does not match {:?}",
                            self.server_id
                        )));
                        return;
                    }
                    let id = {
                        let mut guard = self.lifecycle.lock();
                        match std::mem::replace(&mut *guard, Lifecycle::Disconnected) {
                            Lifecycle::Connecting {
                                id,
                                decoder,
                                transport: Some(transport),
                                handshake,
                            } => {
                                *guard = Lifecycle::Connected {
                                    id,
                                    decoder,
                                    transport,
                                    handshake,
                                };
                                id
                            }
                            other => {
                                *guard = other;
                                return;
                            }
                        }
                    };
                    let hello = ServerHello {
                        version: PROTOCOL_VERSION,
                        server_id,
                    };
                    sink.on_handshake(hello.clone());
                    if self.is_current(id) {
                        sink.on_state_change(ConnectionStateChange {
                            state: ConnectionState::Connected,
                            error: None,
                        });
                    }
                    if self.is_current(id) {
                        let sender = {
                            let mut guard = self.lifecycle.lock();
                            match &mut *guard {
                                Lifecycle::Connected { handshake, .. } => handshake.take(),
                                _ => None,
                            }
                        };
                        if let Some(sender) = sender {
                            let _ = sender.send(Ok(hello));
                        }
                    }
                    return;
                }
                _ => {
                    self.fail(ClientError::protocol(
                        "Expected server hello as first message",
                    ));
                    return;
                }
            }
        }
        match message {
            ServerMessage::Hello { .. } | ServerMessage::HelloError { .. } => {
                self.fail(ClientError::protocol("Unexpected handshake message"));
            }
            other => sink.on_message(other),
        }
    }

    fn is_current(&self, id: u64) -> bool {
        match &*self.lifecycle.lock() {
            Lifecycle::Disconnected => false,
            Lifecycle::Connecting { id: current, .. } => *current == id,
            Lifecycle::Connected { id: current, .. } => *current == id,
        }
    }

    fn notify_state(&self, state: ConnectionState, error: Option<ClientError>) {
        if let Some(sink) = self.sink() {
            sink.on_state_change(ConnectionStateChange { state, error });
        }
    }

    fn sink(&self) -> Option<Arc<dyn ConnectionSink>> {
        self.sink.upgrade()
    }
}

struct ConnectionHandlers {
    connection: Weak<Connection>,
    id: u64,
}

#[async_trait]
impl ByteTransportHandlers for ConnectionHandlers {
    async fn on_data(&self, chunk: Vec<u8>) {
        if let Some(connection) = self.connection.upgrade() {
            connection.handle_data(self.id, chunk);
        }
    }

    async fn on_close(&self) {
        if let Some(connection) = self.connection.upgrade() {
            connection.handle_close(self.id);
        }
    }

    async fn on_error(&self, error: ClientError) {
        if let Some(connection) = self.connection.upgrade() {
            connection.handle_error(self.id, error);
        }
    }
}
