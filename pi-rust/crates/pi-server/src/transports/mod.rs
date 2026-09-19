//! Byte transports that feed the server's [`ServerListener`](crate::listener::ServerListener) contract.
//!
//! | Transport | Upstream | Notes |
//! | --- | --- | --- |
//! | [`memory`] | `testing/**` | in-memory loopback used by the offline tests |
//! | [`tcp`] | — | Rust-only loopback/port listener |
//! | [`unix`] | `transports/unix/**` | Unix-domain sockets (Unix only) |
//! | [`stdio`] | — | parent-process pipes |

pub mod memory;
pub(crate) mod stream;
pub mod stdio;
pub mod tcp;
#[cfg(unix)]
pub mod unix;

pub use memory::{MemoryClient, MemoryListener};
pub use stdio::StdioListener;
pub use tcp::TcpServerListener;
#[cfg(unix)]
pub use unix::UnixServerListener;
