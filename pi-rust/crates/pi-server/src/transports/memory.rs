//! In-memory (loopback) transport used by the offline conformance tests.
//!
//! This has no upstream counterpart: `packages/server` only ships a Unix
//! transport, but the crate's tests must exercise handshake, routing and
//! cancellation without sockets or external processes, so the test double is
//! part of the public surface like upstream's `testing/**`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::connection::{ByteConnection, ByteConnectionAcceptor};
use crate::errors::ServerError;
use crate::listener::ServerListener;

enum ClientToServer {
    Data(Vec<u8>),
    Close,
}

/// The server side of one in-memory connection.
struct MemoryConnection {
    to_client: Mutex<Option<UnboundedSender<Vec<u8>>>>,
    closed: AtomicBool,
}

#[async_trait]
impl ByteConnection for MemoryConnection {
    fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    async fn send(&self, chunk: Vec<u8>) -> Result<(), ServerError> {
        if self.closed() {
            return Err(ServerError::internal("memory connection is closed"));
        }
        let guard = self.to_client.lock();
        match guard.as_ref() {
            Some(sender) => sender
                .send(chunk)
                .map_err(|_| ServerError::internal("memory connection is closed")),
            None => Err(ServerError::internal("memory connection is closed")),
        }
    }

    async fn close(&self, final_chunk: Option<Vec<u8>>) -> Result<(), ServerError> {
        let mut guard = self.to_client.lock();
        if let Some(sender) = guard.as_ref() {
            if let Some(chunk) = final_chunk {
                let _ = sender.send(chunk);
            }
        }
        // Dropping the last sender lets the client observe EOF after any
        // buffered frame, which is what a socket close looks like.
        guard.take();
        self.closed.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// The client side of one in-memory connection, driven directly by a test.
pub struct MemoryClient {
    to_server: Option<UnboundedSender<ClientToServer>>,
    from_server: tokio::sync::Mutex<UnboundedReceiver<Vec<u8>>>,
    closed: AtomicBool,
    closed_notify: tokio::sync::Notify,
}

impl MemoryClient {
    /// Sends one chunk to the server.
    pub fn send(&self, chunk: Vec<u8>) -> Result<(), ServerError> {
        match &self.to_server {
            Some(sender) => sender
                .send(ClientToServer::Data(chunk))
                .map_err(|_| ServerError::internal("memory connection is closed")),
            None => Err(ServerError::internal("memory connection is closed")),
        }
    }

    /// Receives the next frame the server wrote.
    pub async fn recv(&self) -> Option<Vec<u8>> {
        self.from_server.lock().await.recv().await
    }

    /// Whether the client side observed a close.
    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Waits until the server closes this connection.
    pub async fn wait_for_close(&self) {
        loop {
            if self.closed() {
                return;
            }
            let notified = self.closed_notify.notified();
            if self.closed() {
                return;
            }
            notified.await;
        }
    }

    /// Closes the client side, notifying the server handler.
    pub fn close(&self) {
        if let Some(sender) = &self.to_server {
            let _ = sender.send(ClientToServer::Close);
        }
        self.closed.store(true, Ordering::SeqCst);
        self.closed_notify.notify_waiters();
    }
}

/// A listener whose connections are created in-process by [`Self::connect`].
pub struct MemoryListener {
    accept: Mutex<Option<ByteConnectionAcceptor>>,
    started: AtomicBool,
    closed: AtomicBool,
}

impl MemoryListener {
    /// Creates an unstarted in-memory listener.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            accept: Mutex::new(None),
            started: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        })
    }

    /// Creates one authorized connection and returns its client side.
    pub fn connect(self: &Arc<Self>) -> Result<Arc<MemoryClient>, ServerError> {
        let accept = self
            .accept
            .lock()
            .clone()
            .ok_or_else(|| ServerError::internal("memory listener is not started"))?;
        if self.closed.load(Ordering::SeqCst) {
            return Err(ServerError::internal("memory listener is closed"));
        }
        let (to_server, mut server_rx) = unbounded_channel();
        let (to_client, client_rx) = unbounded_channel();
        let connection: Arc<dyn ByteConnection> = Arc::new(MemoryConnection {
            to_client: Mutex::new(Some(to_client)),
            closed: AtomicBool::new(false),
        });
        let handler = accept(connection);
        let client = Arc::new(MemoryClient {
            to_server: Some(to_server),
            from_server: tokio::sync::Mutex::new(client_rx),
            closed: AtomicBool::new(false),
            closed_notify: tokio::sync::Notify::new(),
        });
        let client_notify = Arc::clone(&client);
        let task = async move {
            while let Some(item) = server_rx.recv().await {
                match item {
                    ClientToServer::Data(chunk) => handler.on_data(chunk),
                    ClientToServer::Close => {
                        handler.on_close();
                        break;
                    }
                }
            }
            client_notify.closed.store(true, Ordering::SeqCst);
            client_notify.closed_notify.notify_waiters();
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(task);
        }
        Ok(client)
    }
}

impl Default for MemoryListener {
    fn default() -> Self {
        Self {
            accept: Mutex::new(None),
            started: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl ServerListener for MemoryListener {
    async fn start(&self, accept: ByteConnectionAcceptor) -> Result<(), ServerError> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Err(ServerError::internal("memory listener is already started"));
        }
        *self.accept.lock() = Some(accept);
        Ok(())
    }

    async fn close(&self) -> Result<(), ServerError> {
        self.closed.store(true, Ordering::SeqCst);
        *self.accept.lock() = None;
        Ok(())
    }
}
