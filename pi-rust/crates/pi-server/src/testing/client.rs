//! A protocol-level test client — Rust port of
//! `packages/server/src/testing/client.ts`.
//!
//! The client decodes server frames in a background task and lets a test await
//! the next message matching a predicate.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use pi_chord::services::ServiceCall;
use pi_protocol::rpc::{
    encode_client_message, ClientMessage, ResponseEnvelope, RpcTarget, ServerMessage,
    ServerMessageDecoder, SessionTarget,
};
use tokio::sync::Notify;

use crate::errors::ServerError;
use crate::transports::memory::MemoryClient;

const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

struct ClientInner {
    messages: Mutex<Vec<ServerMessage>>,
    decoder: Mutex<ServerMessageDecoder>,
    attachment: Mutex<Option<SessionTarget>>,
    closed: AtomicBool,
    notify: Notify,
}

/// Drives one in-memory connection with protocol messages.
pub struct ProtocolTestClient {
    channel: Arc<MemoryClient>,
    inner: Arc<ClientInner>,
    request_sequence: AtomicUsize,
    _reader: tokio::task::JoinHandle<()>,
}

impl ProtocolTestClient {
    /// Wraps an in-memory client and starts decoding its frames.
    pub fn new(channel: Arc<MemoryClient>) -> Self {
        let inner = Arc::new(ClientInner {
            messages: Mutex::new(Vec::new()),
            decoder: Mutex::new(ServerMessageDecoder::new(None)),
            attachment: Mutex::new(None),
            closed: AtomicBool::new(false),
            notify: Notify::new(),
        });
        let reader_inner = Arc::clone(&inner);
        let reader_channel = Arc::clone(&channel);
        let reader = tokio::spawn(async move {
            while let Some(chunk) = reader_channel.recv().await {
                let decoded = { reader_inner.decoder.lock().push(&chunk) };
                match decoded {
                    Ok(messages) => {
                        for message in messages {
                            if let ServerMessage::Attachment { attachment } = &message {
                                *reader_inner.attachment.lock() = attachment.clone();
                            }
                            reader_inner.messages.lock().push(message);
                        }
                    }
                    Err(error) => {
                        reader_inner.messages.lock().clear();
                        reader_inner.closed.store(true, Ordering::SeqCst);
                        let _ = error;
                        reader_inner.notify.notify_waiters();
                        reader_inner.notify.notify_one();
                        return;
                    }
                }
                // Wake every waiter registered right now and leave a permit for
                // a waiter that has not polled yet, so no observer loses the
                // notification when several clients await concurrently.
                reader_inner.notify.notify_waiters();
                reader_inner.notify.notify_one();
            }
            reader_inner.closed.store(true, Ordering::SeqCst);
            reader_inner.notify.notify_waiters();
            reader_inner.notify.notify_one();
        });
        Self {
            channel,
            inner,
            request_sequence: AtomicUsize::new(0),
            _reader: reader,
        }
    }

    /// The full ordered message history observed so far.
    pub fn messages(&self) -> Vec<ServerMessage> {
        self.inner.messages.lock().clone()
    }

    /// The current attachment, if any.
    pub fn attachment(&self) -> Option<SessionTarget> {
        self.inner.attachment.lock().clone()
    }

    /// Whether the transport closed.
    pub fn closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    /// Sends `hello` and awaits the server's answer.
    pub async fn hello(&self, version: u32) -> Result<ServerMessage, ServerError> {
        let index = self.inner.messages.lock().len();
        self.send_message(&ClientMessage::hello(version))?;
        self.next_from(index, |message| {
            matches!(
                message,
                ServerMessage::Hello { .. } | ServerMessage::HelloError { .. }
            )
        })
        .await
    }

    /// Sends a request and awaits its response.
    pub async fn request_service(
        &self,
        target: RpcTarget,
        call: ServiceCall,
        id: Option<String>,
    ) -> Result<ResponseEnvelope, ServerError> {
        let id = id.unwrap_or_else(|| {
            format!(
                "request-{}",
                self.request_sequence.fetch_add(1, Ordering::SeqCst) + 1
            )
        });
        let index = self.inner.messages.lock().len();
        self.send_message(&ClientMessage::request(id.clone(), target, call.to_json()))?;
        let message = self
            .next_from(index, |message| {
                matches!(message, ServerMessage::Response { id: rid, .. } if *rid == id)
            })
            .await?;
        match message {
            ServerMessage::Response {
                id: rid,
                ok,
                result,
                error,
            } => Ok(ResponseEnvelope {
                id: rid,
                ok,
                result,
                error,
            }),
            _ => Err(ServerError::internal("unexpected server message")),
        }
    }

    /// Attaches to `session_id` through the test server services.
    pub async fn attach(&self, server_id: &str, session_id: &str) -> Result<ResponseEnvelope, ServerError> {
        self.request_service(
            RpcTarget::Server(pi_protocol::rpc::ServerTarget {
                server_id: server_id.to_owned(),
            }),
            ServiceCall::new(
                "pi.session-management",
                "attach",
                vec![serde_json::Value::String(session_id.to_owned())],
            ),
            None,
        )
        .await
    }

    /// Detaches through the test server services.
    pub async fn detach(&self, server_id: &str) -> Result<ResponseEnvelope, ServerError> {
        self.request_service(
            RpcTarget::Server(pi_protocol::rpc::ServerTarget {
                server_id: server_id.to_owned(),
            }),
            ServiceCall::new("pi.session-management", "detach", Vec::new()),
            None,
        )
        .await
    }

    /// Sends a request addressed to a session, using the current attachment.
    pub async fn request_session_service(
        &self,
        server_id: &str,
        session_id: &str,
        call: ServiceCall,
        id: Option<String>,
    ) -> Result<ResponseEnvelope, ServerError> {
        let target = match self.attachment() {
            Some(attachment) if attachment.session_id == session_id => RpcTarget::Session(attachment),
            _ => RpcTarget::Session(SessionTarget {
                server_id: server_id.to_owned(),
                session_id: session_id.to_owned(),
                attachment_id: "missing-attachment".to_owned(),
            }),
        };
        self.request_service(target, call, id).await
    }

    /// Sends an arbitrary client message.
    pub fn send_message(&self, message: &ClientMessage) -> Result<(), ServerError> {
        let frame = encode_client_message(message, None)
            .map_err(|error| ServerError::internal(error.to_string()))?;
        self.channel.send(frame)
    }

    /// Sends raw bytes.
    pub fn send_bytes(&self, chunk: Vec<u8>) -> Result<(), ServerError> {
        self.channel.send(chunk)
    }

    /// Sends one encoded message split across two chunks.
    pub fn send_fragmented_message(
        &self,
        message: &ClientMessage,
        split_at: usize,
    ) -> Result<(), ServerError> {
        let frame = encode_client_message(message, None)
            .map_err(|error| ServerError::internal(error.to_string()))?;
        let split_at = split_at.min(frame.len());
        self.channel.send(frame[..split_at].to_vec())?;
        self.channel.send(frame[split_at..].to_vec())
    }

    /// Waits for the connection to close.
    pub async fn wait_for_close(&self) {
        let deadline = tokio::time::Instant::now() + CLIENT_TIMEOUT;
        loop {
            if self.closed() {
                return;
            }
            let notified = self.inner.notify.notified();
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return;
            }
        }
    }

    /// Closes the transport.
    pub fn close(&self) {
        self.channel.close();
    }

    /// Awaits the next message matching `predicate`, scanning from `index`.
    pub async fn next_from<F>(&self, index: usize, predicate: F) -> Result<ServerMessage, ServerError>
    where
        F: Fn(&ServerMessage) -> bool,
    {
        let deadline = tokio::time::Instant::now() + CLIENT_TIMEOUT;
        loop {
            let notified = self.inner.notify.notified();
            {
                let messages = self.inner.messages.lock();
                if let Some(found) = messages.iter().skip(index).find(|message| predicate(message)) {
                    return Ok(found.clone());
                }
            }
            if self.closed() {
                return Err(ServerError::internal("Wire connection closed"));
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Err(ServerError::internal("Timed out waiting for a server message"));
            }
        }
    }

    /// The advertised protocol version.
    pub const PROTOCOL_VERSION: u32 = pi_protocol::rpc::PROTOCOL_VERSION;
}
