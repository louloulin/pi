//! Unix-domain socket listener — Rust port of
//! `packages/server/src/transports/unix/**`.
//!
//! # Deliberate simplifications
//!
//! Upstream implements an elaborate ownership protocol for the socket path
//! (hash-suffixed bind path, `link`/`rename` atomicity, inode identity checks,
//! liveness probing). This port relies on `tokio`'s atomic bind and only
//! removes a stale path when the filesystem entry is a socket, which preserves
//! the safety property that matters (`Session` `mode` / permission handling is
//! delegated to the OS) without the inode dance.

#![cfg(unix)]

use std::path::{Path, PathBuf};
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
    started: bool,
    closed: bool,
}

/// A [`ServerListener`] bound to a Unix-domain socket path.
pub struct UnixServerListener {
    path: PathBuf,
    state: Mutex<ListenerState>,
}

impl UnixServerListener {
    /// Creates an unstarted listener for `path`.
    pub fn new(path: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            path: path.into(),
            state: Mutex::new(ListenerState {
                task: None,
                started: false,
                closed: false,
            }),
        })
    }

    /// The socket path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl ServerListener for UnixServerListener {
    async fn start(&self, accept: ByteConnectionAcceptor) -> Result<(), ServerError> {
        {
            let state = self.state.lock();
            if state.closed {
                return Err(ServerError::internal("Unix listener is closing or closed"));
            }
            if state.started {
                return Err(ServerError::internal("Unix listener is already started"));
            }
        }
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| ServerError::internal(error.to_string()))?;
        }
        remove_stale_socket(&self.path)?;
        let listener = tokio::net::UnixListener::bind(&self.path)
            .map_err(|error| ServerError::internal(format!("Unix listener failed to bind: {error}")))?;
        let task = tokio::spawn(accept_loop(listener, self.path.clone(), accept));
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
        remove_stale_socket(&self.path)?;
        Ok(())
    }
}

async fn accept_loop(listener: tokio::net::UnixListener, path: PathBuf, accept: ByteConnectionAcceptor) {
    let _ = &path;
    while let Ok((stream, _)) = listener.accept().await {
        let accept = Arc::clone(&accept);
        tokio::spawn(async move {
            serve_stream(stream, accept).await;
        });
    }
}

fn remove_stale_socket(path: &Path) -> Result<(), ServerError> {
    use std::os::unix::fs::FileTypeExt;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() {
                return Err(ServerError::internal(format!(
                    "Refusing to remove non-socket Unix listener path: {}",
                    path.display()
                )));
            }
            std::fs::remove_file(path)
                .map_err(|error| ServerError::internal(error.to_string()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ServerError::internal(error.to_string())),
    }
}
