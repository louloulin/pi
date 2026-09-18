//! Multi-provider LLM client.
//!
//! Stage 6 adds the WASM-bindgen exports (`wasm` module) so a JS host can
//! register a faux provider and look up models from the browser. Native and
//! WASM builds share the same `StreamFn` trait — only the `wasm` module is
//! gated behind the `wasm` cargo feature.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod models;
pub mod providers;
pub mod stream;
pub mod types;

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
pub mod wasm;

pub use models::Models;
pub use stream::{AssistantMessageEventStream, StreamFn};
pub use types::{AbortSignal, SimpleStreamOptions, StreamError};
