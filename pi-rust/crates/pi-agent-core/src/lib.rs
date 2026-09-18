//! `pi-agent-core` — stateful agent runtime.
//!
//! Stage 0 declares the public surface. Stage 2 fills in the event loop,
//! tool execution, and queue draining logic from `packages/agent/src/agent-loop.ts`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod agent;
mod agent_loop;
mod events;
mod hooks;
mod queue;
mod state;

pub use agent::*;
pub use agent_loop::*;
pub use events::*;
pub use hooks::*;
pub use queue::*;
pub use state::*;
