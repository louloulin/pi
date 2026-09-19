//! Shared plumbing for stream-based transports (TCP, Unix, stdio).

use std::sync::atomic::{AtomicBool, Ordering};

use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::connection::{ByteConnection, ByteConnectionAcceptor, ByteConnectionHandler};
use crate::errors::ServerError;

/// A [`ByteConnection`] over an owned write half of a stream.
pub(crate) struct StreamConnection<W> {
    writer: tokio::sync::Mutex<Option<W>>,
    closed: AtomicBool,
}

impl<W> StreamConnection<W> {
    pub(crate) fn new(writer: W) -> Self {
        Self {
            writer: tokio::sync::Mutex::new(Some(writer)),
            closed: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl<W> ByteConnection for StreamConnection<W>
where
    W: AsyncWrite + Unpin + Send,
{
    fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    async fn send(&self, chunk: Vec<u8>) -> Result<(), ServerError> {
        let mut guard = self.writer.lock().await;
        let writer = guard
            .as_mut()
            .ok_or_else(|| ServerError::internal("connection is closed"))?;
        writer.write_all(&chunk).await?;
        writer.flush().await?;
        Ok(())
    }

    async fn close(&self, final_chunk: Option<Vec<u8>>) -> Result<(), ServerError> {
        let mut guard = self.writer.lock().await;
        if let Some(mut writer) = guard.take() {
            if let Some(chunk) = final_chunk {
                let _ = writer.write_all(&chunk).await;
            }
            let _ = writer.shutdown().await;
        }
        self.closed.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// Reads `read` until EOF, feeding `handler` and closing it exactly once.
pub(crate) async fn pump_reads<R>(mut read: R, handler: Arc<dyn ByteConnectionHandler>)
where
    R: AsyncRead + Unpin + Send,
{
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        match read.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read_bytes) => handler.on_data(buffer[..read_bytes].to_vec()),
            Err(error) => {
                handler.on_error(ServerError::from(error));
                break;
            }
        }
    }
    handler.on_close();
}

/// Serves one accepted stream: installs the connection and pumps its reads.
pub(crate) async fn serve_stream<S>(stream: S, accept: ByteConnectionAcceptor)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (read, write) = tokio::io::split(stream);
    let connection: Arc<dyn ByteConnection> = Arc::new(StreamConnection::new(write));
    let handler = accept(connection);
    pump_reads(read, handler).await;
}
