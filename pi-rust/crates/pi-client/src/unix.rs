//! Unix-domain-socket transport and local server discovery — Rust port of
//! `packages/client/src/unix.ts`.
//!
//! Two pieces live here:
//!
//! * [`create_unix_transport_factory`] opens a fresh `AF_UNIX` connection per
//!   client attempt, splits it into halves so one reader task owns reads and one
//!   mutex-guarded `OwnedWriteHalf` serialises writes, and aborts the reader on
//!   close;
//! * [`discover_unix_servers`] scans a directory of `<serverId>.sock` sockets
//!   and probes each with a full handshake, at most 16 at a time.
//!
//! Upstream features that do not survive the port:
//!
//! * `maxPendingBytes` counts bytes queued *behind* an earlier write upstream;
//!   Rust writes through one mutex, so the counter is "bytes inside `send`
//!   right now" — the same back-pressure signal with a smaller ceiling.
//! * `createUnixTransportFactory` throws a `TypeError` for bad options; the
//!   Rust version returns [`ClientError::Configuration`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use pi_protocol::rpc::{is_server_id, DEFAULT_MAX_FRAME_LENGTH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

use crate::client::{Client, ClientOptions};
use crate::errors::ClientError;
use crate::transport::{ByteTransport, ByteTransportFactory, ByteTransportHandlers};

/// Default per-probe discovery timeout, matching upstream's 1,000 ms.
pub const DEFAULT_DISCOVERY_TIMEOUT: Duration = Duration::from_millis(1_000);

/// The largest timeout upstream accepts (`2^31 - 1` ms).
pub const MAX_DISCOVERY_TIMEOUT: Duration = Duration::from_millis(2_147_483_647);

/// How many probes may be in flight at once.
pub const MAX_CONCURRENT_DISCOVERY_PROBES: usize = 16;

/// The socket suffix a server-addressed socket must carry.
pub const UNIX_SOCKET_SUFFIX: &str = ".sock";

const DEFAULT_MAX_PENDING_BYTES: usize = DEFAULT_MAX_FRAME_LENGTH * 4;
const READ_CHUNK_BYTES: usize = 8 * 1024;

/// Options for [`create_unix_transport_factory`].
#[derive(Debug, Clone)]
pub struct UnixTransportOptions {
    /// The socket path to connect to.
    pub path: PathBuf,
    /// The largest number of bytes that may be inside `send` at once.
    pub max_pending_bytes: usize,
}

impl UnixTransportOptions {
    /// Creates options for `path` with the default pending-byte ceiling.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            max_pending_bytes: DEFAULT_MAX_PENDING_BYTES,
        }
    }

    /// Overrides the pending-byte ceiling.
    pub fn with_max_pending_bytes(mut self, max_pending_bytes: usize) -> Self {
        self.max_pending_bytes = max_pending_bytes;
        self
    }
}

/// One discovered server.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnixServerRoute {
    /// The server identity carried by the socket name.
    pub server_id: String,
    /// The socket path.
    pub path: PathBuf,
}

/// Creates a Unix transport factory, validating `options`.
pub fn create_unix_transport_factory(
    options: UnixTransportOptions,
) -> Result<Arc<dyn ByteTransportFactory>, ClientError> {
    if options.path.as_os_str().is_empty() {
        return Err(ClientError::configuration(
            "Unix transport path must not be empty",
        ));
    }
    if options.max_pending_bytes == 0 {
        return Err(ClientError::configuration(
            "Unix transport maxPendingBytes must be positive",
        ));
    }
    Ok(Arc::new(UnixTransportFactory { options }))
}

struct UnixTransportFactory {
    options: UnixTransportOptions,
}

#[async_trait]
impl ByteTransportFactory for UnixTransportFactory {
    async fn connect(
        &self,
        handlers: Arc<dyn ByteTransportHandlers>,
    ) -> Result<Arc<dyn ByteTransport>, ClientError> {
        let stream = UnixStream::connect(&self.options.path)
            .await
            .map_err(transport_error)?;
        let (mut reader, writer) = stream.into_split();
        let transport = Arc::new(UnixTransport {
            writer: AsyncMutex::new(Some(writer)),
            reader: AsyncMutex::new(None),
            closed: AtomicBool::new(false),
            pending_bytes: AtomicUsize::new(0),
            max_pending_bytes: self.options.max_pending_bytes,
        });

        let handle = tokio::spawn(async move {
            let mut buffer = vec![0u8; READ_CHUNK_BYTES];
            loop {
                match reader.read(&mut buffer).await {
                    Ok(0) => {
                        handlers.on_close().await;
                        break;
                    }
                    Ok(read) => handlers.on_data(buffer[..read].to_vec()).await,
                    Err(error) => {
                        handlers.on_error(transport_error(error)).await;
                        break;
                    }
                }
            }
        });
        *transport.reader.lock().await = Some(handle);
        Ok(transport)
    }
}

struct UnixTransport {
    writer: AsyncMutex<Option<OwnedWriteHalf>>,
    reader: AsyncMutex<Option<JoinHandle<()>>>,
    closed: AtomicBool,
    pending_bytes: AtomicUsize,
    max_pending_bytes: usize,
}

impl UnixTransport {
    /// Closes the write half if no write is in flight.
    ///
    /// `close` is synchronous (the transport trait demands it), so a write that
    /// currently holds the mutex wins: the write then observes `closed` on the
    /// next send and the reader task is aborted below either way.
    fn release_writer(&self) {
        if let Ok(mut guard) = self.writer.try_lock() {
            guard.take();
        }
    }
}

#[async_trait]
impl ByteTransport for UnixTransport {
    async fn send(&self, chunk: Vec<u8>) -> Result<(), ClientError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(ClientError::disconnected("Unix transport is closed"));
        }
        let length = chunk.len();
        if self.pending_bytes.fetch_add(length, Ordering::SeqCst) + length > self.max_pending_bytes
        {
            self.pending_bytes.fetch_sub(length, Ordering::SeqCst);
            return Err(ClientError::disconnected(
                "Unix transport exceeded its pending byte limit",
            ));
        }
        let mut guard = self.writer.lock().await;
        let Some(writer) = guard.as_mut() else {
            self.pending_bytes.fetch_sub(length, Ordering::SeqCst);
            return Err(ClientError::disconnected("Unix transport is closed"));
        };
        let result = writer.write_all(&chunk).await;
        self.pending_bytes.fetch_sub(length, Ordering::SeqCst);
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                self.closed.store(true, Ordering::SeqCst);
                Err(transport_error(error))
            }
        }
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.release_writer();
        if let Ok(mut reader) = self.reader.try_lock() {
            if let Some(handle) = reader.take() {
                handle.abort();
            }
        }
    }
}

/// Discovers reachable servers in `directory` with the default timeout.
pub async fn discover_unix_servers(
    directory: impl AsRef<Path>,
) -> Result<Vec<UnixServerRoute>, ClientError> {
    discover_unix_servers_with_timeout(directory, DEFAULT_DISCOVERY_TIMEOUT).await
}

/// Discovers reachable servers in `directory`, probing each for `timeout`.
///
/// Missing directories resolve to an empty list. Processes are probed with at
/// most [`MAX_CONCURRENT_DISCOVERY_PROBES`] handshakes at a time and the result
/// is sorted by server id so callers see a stable order.
pub async fn discover_unix_servers_with_timeout(
    directory: impl AsRef<Path>,
    timeout: Duration,
) -> Result<Vec<UnixServerRoute>, ClientError> {
    if timeout.is_zero() || timeout > MAX_DISCOVERY_TIMEOUT {
        return Err(ClientError::configuration(
            "Unix discovery timeout must be between 1 ms and 2147483647 ms",
        ));
    }
    let directory = directory.as_ref();
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(transport_error(error)),
    };

    let mut candidates = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(transport_error)? {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(server_id) = name.strip_suffix(UNIX_SOCKET_SUFFIX) else {
            continue;
        };
        if !is_server_id(server_id) {
            continue;
        }
        candidates.push(UnixServerRoute {
            server_id: server_id.to_owned(),
            path: directory.join(&name),
        });
    }

    let mut probes = stream::iter(candidates.into_iter().map(|route| async move {
        // A socket can disappear between `readdir` and `lstat` during a normal
        // server shutdown; that is not a discovery failure.
        match tokio::fs::symlink_metadata(&route.path).await {
            Ok(metadata) if is_socket(&metadata) => probe_unix_server(route, timeout).await,
            Ok(_) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(transport_error(error)),
        }
    }))
    .buffered(MAX_CONCURRENT_DISCOVERY_PROBES);

    let mut servers = Vec::new();
    while let Some(result) = probes.next().await {
        if let Some(route) = result? {
            servers.push(route);
        }
    }
    servers.sort();
    Ok(servers)
}

async fn probe_unix_server(
    route: UnixServerRoute,
    timeout: Duration,
) -> Result<Option<UnixServerRoute>, ClientError> {
    let factory = create_unix_transport_factory(UnixTransportOptions::new(&route.path))?;
    let client = Client::new(ClientOptions::new(factory, route.server_id.clone()))?;
    let outcome = tokio::time::timeout(timeout, client.connect()).await;
    let result = match outcome {
        // A timeout means the endpoint never completed a handshake.
        Err(_) => Ok(None),
        Ok(Ok(_)) => Ok(Some(route)),
        Ok(Err(error)) if is_skippable_probe_error(&error) => Ok(None),
        Ok(Err(error)) => Err(error),
    };
    client.dispose().await;
    result
}

/// Whether a probe failure means "this socket is not the advertised server".
fn is_skippable_probe_error(error: &ClientError) -> bool {
    match error {
        ClientError::Protocol(_) => true,
        // Our `Disconnected` has no cause chain, so every disconnect during a
        // probe is treated as a stale or shutting-down socket.
        ClientError::Disconnected { .. } => true,
        ClientError::Server(error) => error.code == "version",
        ClientError::Disposed | ClientError::Cancelled | ClientError::Configuration(_) => false,
        ClientError::Listener(_) => false,
    }
}

fn is_socket(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;
    metadata.file_type().is_socket()
}

fn transport_error(error: std::io::Error) -> ClientError {
    ClientError::disconnected(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_are_validated() {
        assert!(create_unix_transport_factory(UnixTransportOptions::new("/tmp/pi.sock")).is_ok());
        let empty = UnixTransportOptions::new("");
        assert!(matches!(
            create_unix_transport_factory(empty),
            Err(ClientError::Configuration(_))
        ));
        let zero = UnixTransportOptions::new("/tmp/pi.sock").with_max_pending_bytes(0);
        assert!(matches!(
            create_unix_transport_factory(zero),
            Err(ClientError::Configuration(_))
        ));
    }

    #[tokio::test]
    async fn discovery_of_a_missing_directory_is_empty() {
        let servers = discover_unix_servers("/nonexistent-pi-client-directory")
            .await
            .expect("missing directory is empty");
        assert!(servers.is_empty());
    }

    #[tokio::test]
    async fn discovery_timeout_is_validated() {
        let error = discover_unix_servers_with_timeout(".", Duration::ZERO)
            .await
            .expect_err("zero timeout is rejected");
        assert!(matches!(error, ClientError::Configuration(_)));
    }
}
