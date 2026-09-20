//! Ports of the upstream `packages/ai/src/utils/` helpers that the Rust port
//! needs but that had no home yet.
//!
//! Each submodule carries its own "deliberate differences" notes, because the
//! Rust protocol types are narrower than the TypeScript ones (no per-message
//! usage, no SDK-shaped error objects, no `AbortSignal.reason`, no web
//! `Headers`). The runtime-support helpers — [`abort`], [`abort_signals`],
//! [`headers`], [`pi_user_agent`] and [`provider_env`] — follow that rule too:
//! they exist so provider adapters can stop open-coding header maps, user
//! agents and cancellation plumbing.

pub mod abort;
#[cfg(not(target_arch = "wasm32"))]
pub mod abort_signals;
pub mod deferred_tools;
pub mod error_body;
pub mod estimate;
pub mod headers;
pub mod pi_user_agent;
pub mod provider_env;

pub use abort::{operation_signal, race_with_abort_signal};
#[cfg(not(target_arch = "wasm32"))]
pub use abort_signals::{combine_abort_signals, CombinedAbortSignal};
pub use deferred_tools::{
    added_tool_names_from_messages, identity_tool_name, split_deferred_tools,
    split_deferred_tools_from_context, SplitDeferredTools,
};
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
#[cfg(not(target_arch = "wasm32"))]
pub use headers::header_map_to_record;
pub use headers::{headers_to_record, provider_headers_to_record};
pub use pi_user_agent::{get_pi_user_agent, WASM_USER_AGENT};
pub use provider_env::get_provider_env_value;
