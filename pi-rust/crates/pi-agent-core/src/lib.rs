//! `pi-agent-core` — stateful agent runtime.
//!
//! Stage 6 adds the WASM-bindgen exports (`wasm` module) so a JS host can
//! drive an `Agent` from the browser. Native and WASM builds share the same
//! `Agent` runtime — only the `wasm` module is gated behind the `wasm`
//! cargo feature.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod agent;
mod agent_loop;
mod events;
mod hooks;
mod queue;
mod state;
pub mod tools;

#[cfg(feature = "wasm")]
pub mod wasm;

pub use agent::*;
pub use agent_loop::*;
pub use events::*;
pub use hooks::*;
pub use queue::*;
pub use state::*;
pub use tools::*;
