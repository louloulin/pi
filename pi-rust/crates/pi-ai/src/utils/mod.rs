//! Ports of the upstream `packages/ai/src/utils/` helpers that the Rust port
//! needs but that had no home yet.
//!
//! Each submodule carries its own "deliberate differences" notes, because the
//! Rust protocol types are narrower than the TypeScript ones (no per-message
//! usage, no `addedToolNames`, no SDK-shaped error objects).

pub mod deferred_tools;
pub mod error_body;
pub mod estimate;

pub use deferred_tools::{identity_tool_name, split_deferred_tools, SplitDeferredTools};
pub use error_body::{
    format_provider_error, normalize_provider_error, truncate_error_text,
    truncate_provider_error_body, NormalizedProviderError, MAX_PROVIDER_ERROR_BODY_CHARS,
};
pub use estimate::{
    calculate_context_tokens, estimate_context_usage, estimate_message_tokens,
    estimate_messages_tokens, estimate_text_and_image_content_tokens, estimate_text_tokens,
    estimate_tools_tokens, ContextUsageEstimate, UsageAnchor, CHARS_PER_TOKEN,
    ESTIMATED_IMAGE_CHARS,
};
