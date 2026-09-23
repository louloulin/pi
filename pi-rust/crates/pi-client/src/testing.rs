//! In-memory test doubles — the Rust counterpart of
//! `packages/server/src/testing/client.ts`.
//!
//! Upstream drives its client tests through a scripted fake server. Rust offers
//! the same here, without any real socket:
//!
//! ```
//! use pi_client::testing::memory_transport;
//! use pi_client::{Client, ClientOptions};
//!
//! const SERVER_ID: &str = "00000000-0000-4000-8000-000000000001";
//!
//! # async fn example() {
//! let (factory, server) = memory_transport();
//! let connecting = tokio::spawn(async move {
//!     Client::connect_with(ClientOptions::new(factory, SERVER_ID)).await
//! });
//! server.accept_handshake(SERVER_ID).await;
//! let client = connecting.await.expect("join").expect("handshake");
//! # let _ = client;
//! # }
//! ```
//!
//! [`ScriptedServer`] decodes what the client sent and encodes what the test
//! wants to send back, so a test reads as a protocol transcript.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use pi_protocol::rpc::{
    encode_server_message, ClientMessage, ClientMessageDecoder, ProtocolError, ServerMessage,
    SessionTarget, DEFAULT_MAX_FRAME_LENGTH,
};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::errors::ClientError;
use crate::transport::{ByteTransport, ByteTransportFactory, ByteTransportHandlers};

enum ClientToServer {
    Data(Vec<u8>),
    Close,
}

enum ServerToClient {
    Data(Vec<u8>),
    Close,
}

/// Creates a connected in-memory transport pair plus its scripted server.
pub fn memory_transport() -> (Arc<dyn ByteTransportFactory>, ScriptedServer) {
    let (client_to_server, server_receiver) = mpsc::unbounded_channel::<ClientToServer>();
    let (server_to_client, client_receiver) = mpsc::unbounded_channel::<ServerToClient>();
    let factory: Arc<dyn ByteTransportFactory> = Arc::new(MemoryTransportFactory {
        client_to_server,
        server_to_client: Mutex::new(Some(client_receiver)),
    });
    let server = ScriptedServer::new(MemoryServerPeer {
        client_to_server: tokio::sync::Mutex::new(server_receiver),
        server_to_client,
        closed: AtomicBool::new(false),
    });
    (factory, server)
}

struct MemoryTransportFactory {
    client_to_server: mpsc::UnboundedSender<ClientToServer>,
    server_to_client: Mutex<Option<mpsc::UnboundedReceiver<ServerToClient>>>,
}

#[async_trait]
impl ByteTransportFactory for MemoryTransportFactory {
    async fn connect(
        &self,
        handlers: Arc<dyn ByteTransportHandlers>,
    ) -> Result<Arc<dyn ByteTransport>, ClientError> {
        let mut receiver =
            self.server_to_client.lock().take().ok_or_else(|| {
                ClientError::disconnected("Memory transport is already connected")
            })?;
        tokio::spawn(async move {
            loop {
                match receiver.recv().await {
                    Some(ServerToClient::Data(chunk)) => handlers.on_data(chunk).await,
                    Some(ServerToClient::Close) | None => {
                        handlers.on_close().await;
                        break;
                    }
                }
            }
        });
        Ok(Arc::new(MemoryTransport {
            sender: self.client_to_server.clone(),
            closed: AtomicBool::new(false),
        }))
    }
}

struct MemoryTransport {
    sender: mpsc::UnboundedSender<ClientToServer>,
    closed: AtomicBool,
}

#[async_trait]
impl ByteTransport for MemoryTransport {
    async fn send(&self, chunk: Vec<u8>) -> Result<(), ClientError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(ClientError::disconnected("Memory transport is closed"));
        }
        self.sender
            .send(ClientToServer::Data(chunk))
            .map_err(|_| ClientError::disconnected("Memory transport server side is gone"))
    }

    fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self.sender.send(ClientToServer::Close);
        }
    }
}

/// The raw server end of a [`memory_transport`] pair.
struct MemoryServerPeer {
    client_to_server: tokio::sync::Mutex<mpsc::UnboundedReceiver<ClientToServer>>,
    server_to_client: mpsc::UnboundedSender<ServerToClient>,
    closed: AtomicBool,
}

impl MemoryServerPeer {
    /// Receives the next client byte chunk, or `None` after the client closed.
    async fn recv(&self) -> Option<Vec<u8>> {
        if self.closed.load(Ordering::SeqCst) {
            return None;
        }
        let chunk = {
            let mut receiver = self.client_to_server.lock().await;
            receiver.recv().await
        };
        match chunk {
            Some(ClientToServer::Data(bytes)) => Some(bytes),
            Some(ClientToServer::Close) | None => {
                self.closed.store(true, Ordering::SeqCst);
                None
            }
        }
    }

    fn send(&self, chunk: Vec<u8>) -> Result<(), ClientError> {
        self.server_to_client
            .send(ServerToClient::Data(chunk))
            .map_err(|_| ClientError::disconnected("Memory transport client side is gone"))
    }

    fn close(&self) {
        let _ = self.server_to_client.send(ServerToClient::Close);
    }
}

/// A scripted server speaking the client RPC protocol over an in-memory pair.
pub struct ScriptedServer {
    peer: MemoryServerPeer,
    max_frame_length: usize,
    decoder: Mutex<ClientMessageDecoder>,
}

impl ScriptedServer {
    fn new(peer: MemoryServerPeer) -> Self {
        Self {
            peer,
            max_frame_length: DEFAULT_MAX_FRAME_LENGTH,
            decoder: Mutex::new(ClientMessageDecoder::new(None)),
        }
    }

    /// Receives and decodes the next client message.
    pub async fn recv(&self) -> Option<ClientMessage> {
        loop {
            let chunk = self.peer.recv().await?;
            let messages = {
                let mut decoder = self.decoder.lock();
                decoder.push(&chunk).ok()?
            };
            if let Some(message) = messages.into_iter().next() {
                return Some(message);
            }
        }
    }

    /// Receives the client hello and answers it with `server_id`.
    ///
    /// Returns the received hello so a test can assert its version.
    pub async fn accept_handshake(&self, server_id: &str) -> ClientMessage {
        let hello = self.recv().await.expect("client hello");
        assert!(
            matches!(hello, ClientMessage::Hello { .. }),
            "expected hello"
        );
        self.send(&ServerMessage::hello(server_id))
            .expect("handshake reply");
        hello
    }

    /// Rejects the handshake with a bounded protocol error.
    pub async fn reject_handshake(&self, code: &str, message: &str) {
        let _ = self.recv().await;
        self.send(&ServerMessage::hello_error(ProtocolError {
            code: code.to_owned(),
            message: message.to_owned(),
        }))
        .expect("handshake rejection");
    }

    /// Sends a successful response for `request_id`.
    pub fn respond_ok(&self, request_id: &str, result: Option<Value>) {
        self.send(&ServerMessage::response_ok(request_id, result))
            .expect("response");
    }

    /// Sends a failed response for `request_id`.
    pub fn respond_error(&self, request_id: &str, code: &str, message: &str) {
        self.send(&ServerMessage::response_error(
            request_id,
            ProtocolError {
                code: code.to_owned(),
                message: message.to_owned(),
            },
        ))
        .expect("response");
    }

    /// Sends an out-of-band service update.
    pub fn service_update(&self, subscription_id: &str, update: Value) {
        self.send(&ServerMessage::service_update(subscription_id, update))
            .expect("service update");
    }

    /// Sends an attachment change (`None` detaches).
    pub fn attachment(&self, attachment: Option<SessionTarget>) {
        self.send(&ServerMessage::attachment(attachment))
            .expect("attachment update");
    }

    /// Closes the server side of the transport.
    pub fn close(&self) {
        self.peer.close();
    }

    /// Encodes and sends one server message.
    pub fn send(&self, message: &ServerMessage) -> Result<(), ClientError> {
        let frame = encode_server_message(message, Some(self.max_frame_length))?;
        self.peer.send(frame)
    }
}

impl std::fmt::Debug for ScriptedServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScriptedServer")
            .field("closed", &self.peer.closed.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}
