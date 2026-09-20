//! TCP listener — no upstream counterpart.
//!
//! `packages/server` only ships a Unix transport; the Rust crate adds a TCP
//! listener because hosted server profiles may bind a loopback port. It reuses
//! the same [`ServerListener`](crate::listener::ServerListener) contract.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::task::JoinHandle;

use crate::connection::ByteConnectionAcceptor;
use crate::errors::ServerError;
use crate::listener::ServerListener;
use crate::transports::stream::serve_stream;

struct ListenerState {
    task: Option<JoinHandle<()>>,
    local_addr: Option<SocketAddr>,
    started: bool,
    closed: bool,
}

/// A [`ServerListener`] bound to a TCP address.
pub struct TcpServerListener {
    address: SocketAddr,
    state: Mutex<ListenerState>,
}

impl TcpServerListener {
    /// Creates an unstarted listener for `address`.
    pub fn new(address: SocketAddr) -> Arc<Self> {
        Arc::new(Self {
            address,
            state: Mutex::new(ListenerState {
                task: None,
                local_addr: None,
                started: false,
                closed: false,
            }),
        })
    }

    /// The configured address.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The bound address, once started.
    ///
    /// For a port of `0` this is the operating-system-assigned port.
    pub fn local_addr(&self) -> Result<SocketAddr, ServerError> {
        self.state
            .lock()
            .local_addr
            .ok_or_else(|| ServerError::internal("TCP listener is not started"))
    }
}

#[async_trait]
impl ServerListener for TcpServerListener {
    async fn start(&self, accept: ByteConnectionAcceptor) -> Result<(), ServerError> {
        {
            let state = self.state.lock();
            if state.closed {
                return Err(ServerError::internal("TCP listener is closing or closed"));
            }
            if state.started {
                return Err(ServerError::internal("TCP listener is already started"));
            }
        }
        let listener = tokio::net::TcpListener::bind(self.address)
            .await
            .map_err(|error| {
                ServerError::internal(format!("TCP listener failed to bind: {error}"))
            })?;
        let local_addr = listener
            .local_addr()
            .map_err(|error| ServerError::internal(error.to_string()))?;
        let task = tokio::spawn(accept_loop(listener, accept));
        let mut state = self.state.lock();
        state.local_addr = Some(local_addr);
        state.task = Some(task);
        state.started = true;
        Ok(())
    }

    async fn close(&self) -> Result<(), ServerError> {
        let task = {
            let mut state = self.state.lock();
            state.closed = true;
            state.started = false;
            state.local_addr = None;
            state.task.take()
        };
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        Ok(())
    }
}

async fn accept_loop(listener: tokio::net::TcpListener, accept: ByteConnectionAcceptor) {
    while let Ok((stream, _peer)) = listener.accept().await {
        let accept = Arc::clone(&accept);
        tokio::spawn(async move {
            serve_stream(stream, accept).await;
        });
    }
}
