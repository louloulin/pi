//! stdio listener — no upstream counterpart.
//!
//! Upstream `packages/server` has no stdio transport; the Rust crate adds one
//! so a hosted server can be driven over a parent process's pipes without a
//! socket. The connection is accepted exactly once, and closing the listener
//! stops reading but cannot close the process's inherited standard streams.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::task::JoinHandle;

use crate::connection::{ByteConnection, ByteConnectionAcceptor};
use crate::errors::ServerError;
use crate::listener::ServerListener;
use crate::transports::stream::{pump_reads, StreamConnection};

struct ListenerState {
    task: Option<JoinHandle<()>>,
    started: bool,
    closed: bool,
}

/// A [`ServerListener`] over the process's standard input and output.
pub struct StdioListener {
    state: Mutex<ListenerState>,
}

impl StdioListener {
    /// Creates an unstarted stdio listener.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ListenerState {
                task: None,
                started: false,
                closed: false,
            }),
        })
    }
}

#[async_trait]
impl ServerListener for StdioListener {
    async fn start(&self, accept: ByteConnectionAcceptor) -> Result<(), ServerError> {
        {
            let state = self.state.lock();
            if state.closed {
                return Err(ServerError::internal("stdio listener is closing or closed"));
            }
            if state.started {
                return Err(ServerError::internal("stdio listener is already started"));
            }
        }
        let task = tokio::spawn(async move {
            let connection: Arc<dyn ByteConnection> =
                Arc::new(StreamConnection::new(tokio::io::stdout()));
            let handler = accept(connection);
            pump_reads(tokio::io::stdin(), handler).await;
        });
        let mut state = self.state.lock();
        state.task = Some(task);
        state.started = true;
        Ok(())
    }

    async fn close(&self) -> Result<(), ServerError> {
        let task = {
            let mut state = self.state.lock();
            state.closed = true;
            state.started = false;
            state.task.take()
        };
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        Ok(())
    }
}
