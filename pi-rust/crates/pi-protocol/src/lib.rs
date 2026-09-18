//! Shared wire types between the Pi Rust crates and the WASM extension ABI.
//!
//! These types are the source of truth for messages, tool calls, session
//! entries, and the event surface the extension host exposes. They are
//! serde-friendly so the same definitions travel across the
//! `wasmtime`-hosted JS shim, the native CLI, and (eventually) the
//! `wasm32-unknown-unknown` build of the agent core.
//!
//! See `docs/ARCHITECTURE.md` for the role of this crate in the workspace.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod content;
mod context;
mod events;
mod model;
mod session;
mod tool;

pub use content::*;
pub use context::*;
pub use events::*;
pub use model::*;
pub use session::*;
pub use tool::*;
