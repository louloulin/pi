//! Multi-provider LLM client.
//!
//! Stage 0 scaffold. Stage 1 fills in the per-provider stream adapters and
//! the unified `stream_simple` API that `pi-agent-core` consumes.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod models;
pub mod providers;
pub mod stream;
pub mod types;

pub use models::Models;
pub use stream::{AssistantMessageEventStream, SharedStreamFn, StreamFn};
pub use types::{SimpleStreamOptions, StreamError};
