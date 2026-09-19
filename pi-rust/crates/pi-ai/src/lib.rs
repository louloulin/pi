//! Multi-provider LLM client.
//!
//! Stage 6 adds the WASM-bindgen exports (`wasm` module) so a JS host can
//! register a faux provider and look up models from the browser. Native and
//! WASM builds share the same `StreamFn` trait — only the `wasm` module is
//! gated behind the `wasm` cargo feature.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod json_parse;
pub mod models;
pub mod overflow;
pub mod providers;
#[cfg(not(target_arch = "wasm32"))]
pub mod retry;
pub mod stream;
pub mod types;

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
pub mod wasm;

pub use json_parse::{
    close_partial_json, parse_json_with_repair, parse_streaming_json, parse_value_with_repair,
    repair_json,
};
pub use models::Models;
pub use overflow::{
    get_non_overflow_patterns, get_overflow_patterns, is_context_overflow,
    is_context_overflow_error_text, is_recoverable_length,
};
#[cfg(not(target_arch = "wasm32"))]
pub use retry::{ProviderRetryPolicy, RetryStreamFn};
pub use stream::{AssistantMessageEventStream, SharedStreamFn, StreamFn};
pub use types::{AbortSignal, ProviderRetryHint, SimpleStreamOptions, StreamError};
