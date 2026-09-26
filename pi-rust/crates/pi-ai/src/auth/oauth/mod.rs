//! OAuth login flow implementations for the OAuth-first providers
//! (`github-copilot`, `openai-codex`, `kimi-coding`).
//!
//! Port of `packages/ai/src/auth/oauth/*.ts`. Each provider exports an
//! [`OAuthAuth`](crate::auth::OAuthAuth) implementation matching the upstream
//! shape byte for byte (name, login flow, refresh, `to_auth`); the runtime
//! wiring (`provider_registry::provider_auth_for`) reads the implementation
//! from here, so no provider-specific code lives outside this module.
//!
//! # Non-WASM only
//!
//! Device-code polling uses `tokio::time::sleep`, PKCE uses `sha2` + `base64`,
//! the OpenAI Codex callback uses `tokio::net::TcpListener`, and the
//! providers make outbound HTTPS calls through `reqwest`. None of that is
//! available on `wasm32-unknown-unknown`, so the whole module is gated
//! behind `#[cfg(not(target_arch = "wasm32"))]` and the registry's
//! OAuth-first entries remain `oauth: None` on that target until a browser
//! port lands.
//!
//! # Tests
//!
//! Unit tests cover the pure helpers (PKCE, device-code polling state
//! machine, callback state parsing, github-copilot base-URL extraction).
//! Network-touching paths live behind `tokio::test` and are kept offline by
//! running them against a `reqwest::Client::builder().no_proxy()` pointed at
//! the loopback servers the tests start.

pub mod callback;
pub mod device_code;
pub mod github_copilot;
pub mod http;
pub mod kimi_coding;
pub mod oauth_page;
pub mod openai_codex;
pub mod pkce;

pub use callback::{CallbackOutcome, LocalCallbackServer};
pub use device_code::{DeviceCodePollResult, PollOptions, PollStatus, poll_device_code};
pub use github_copilot::github_copilot_oauth;
pub use kimi_coding::kimi_coding_oauth;
pub use oauth_page::{oauth_error_html, oauth_success_html};
pub use openai_codex::openai_codex_oauth;
pub use pkce::{PkcePair, generate_pkce};

use std::sync::Arc;

use crate::auth::OAuthAuth;
use crate::providers::registry::OAuthKindSpec;

/// Build an [`OAuthAuth`] trait object for the OAuth-first provider id,
/// using `spec` to parameterize the kind-specific tunables
/// (device-code poll cadence, callback port range, …).
///
/// Returns `None` for provider ids that are not OAuth-first — callers
/// (notably `pi-coding-agent`'s `/login` dispatch) treat that as a
/// routing error rather than a panic so unknown ids surface as a clear
/// user-visible message instead of a crash.
pub fn oauth_for(provider_id: &str, spec: OAuthKindSpec) -> Option<Arc<dyn OAuthAuth>> {
    match provider_id {
        "github-copilot" => Some(github_copilot_oauth(spec)),
        "openai-codex" => Some(openai_codex_oauth(spec)),
        "kimi-coding" => Some(kimi_coding_oauth(spec)),
        _ => None,
    }
}