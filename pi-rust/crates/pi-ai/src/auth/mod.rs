//! Credential storage and provider-auth resolution — port of
//! `packages/ai/src/auth/` (plus the query half of
//! `packages/ai/src/env-api-keys.ts`, which lives in [`crate::env_api_keys`]).
//!
//! The subsystem has two halves, matching upstream:
//!
//! * [`types`] — the data model (`Credential`, `CredentialStore`,
//!   `ProviderAuth`, `AuthContext`, `AuthResult`, ...) and the auth strategy
//!   traits.
//! * [`resolve`] — `resolve_provider_auth`, the shared resolution policy:
//!   **a stored credential owns the provider**, ambient/env is consulted only
//!   when nothing is stored, and a failed OAuth refresh never falls back to
//!   the environment.
//!
//! [`credential_store`] provides the default in-memory store and [`helpers`]
//! the standard api-key / lazy-OAuth builders.
//!
//! # Deliberate differences from the TypeScript implementation
//!
//! * Rust has no `DOMException`, so an aborted operation surfaces as
//!   [`AuthError::Aborted`] instead of a thrown abort reason; a coded failure
//!   is [`AuthError::Models`] wrapping [`ModelsError`].
//! * `CredentialStore` callbacks are `'static` boxed closures
//!   ([`types::CredentialModifyFn`]) rather than ordinary async functions, so
//!   the trait stays object-safe for app-injected stores.
//! * The in-memory store serializes per provider with a `tokio` mutex instead
//!   of a promise chain. Cancelling a `modify` while its callback is running
//!   drops the callback future (upstream keeps observing the abandoned
//!   promise); the per-provider lock is still released, so later writes do not
//!   deadlock.

pub mod credential_store;
pub mod helpers;
pub mod resolve;
pub mod types;

use thiserror::Error;

use crate::types::AbortSignal;

pub use credential_store::InMemoryCredentialStore;
pub use helpers::{env_api_key_auth, lazy_oauth, LazyOAuthConfig};
pub use resolve::{
    resolve_provider_auth, AuthResolutionOverrides, ModelsError, ModelsErrorCode,
    DEFAULT_OAUTH_MINIMUM_VALIDITY_MS, DEFAULT_OAUTH_REFRESH_TIMEOUT_MS,
};
pub use types::{
    default_provider_auth_context, ApiKeyAuth, ApiKeyCheckInput, ApiKeyCredential,
    ApiKeyResolveInput, AuthCheck, AuthContext, AuthEvent, AuthInfoLink, AuthInteraction,
    AuthOperationOptions, AuthPrompt, AuthPromptOption, AuthResult, AuthType, Credential,
    CredentialInfo, CredentialModifyFn, CredentialModifyFuture, CredentialStore, ModelAuth,
    OAuthAuth, OAuthCredential, ProcessAuthContext, ProviderAuth, ProviderAuthInteraction,
    ProviderEnv, ProviderHeaders,
};

/// Error returned by the auth subsystem.
///
/// Upstream rejects with either an `AbortError` (from the operation signal) or
/// a [`ModelsError`]; Rust models those as two variants of one error type so a
/// caller can handle both without a downcast.
#[derive(Debug, Error)]
pub enum AuthError {
    /// The operation's `AbortSignal` was triggered (`AbortError` upstream).
    #[error("the operation was aborted")]
    Aborted,
    /// A coded failure — the [`ModelsError`] upstream raises.
    #[error(transparent)]
    Models(#[from] ModelsError),
    /// A credential-store failure that carries no [`ModelsError`] code.
    #[error("credential store failure: {0}")]
    Store(String),
}

/// Fail with [`AuthError::Aborted`] when `signal` has already fired
/// (`signal.throwIfAborted()` upstream).
pub(crate) fn check_abort(signal: &AbortSignal) -> Result<(), AuthError> {
    if signal.is_cancelled() {
        Err(AuthError::Aborted)
    } else {
        Ok(())
    }
}

/// Resolve `operation`, giving up when `signal` fires
/// (`raceWithAbortSignal` upstream).
///
/// The native implementation races the operation against cancellation; the
/// WASM build (which has no native select) checks cancellation before and
/// after awaiting, matching the crate's existing WASM cancellation model.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn race_with_abort<F>(
    operation: F,
    signal: &AbortSignal,
) -> Result<F::Output, AuthError>
where
    F: std::future::Future,
{
    check_abort(signal)?;
    tokio::select! {
        output = operation => Ok(output),
        _ = signal.cancelled() => Err(AuthError::Aborted),
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn race_with_abort<F>(
    operation: F,
    signal: &AbortSignal,
) -> Result<F::Output, AuthError>
where
    F: std::future::Future,
{
    check_abort(signal)?;
    let output = operation.await;
    check_abort(signal)?;
    Ok(output)
}
