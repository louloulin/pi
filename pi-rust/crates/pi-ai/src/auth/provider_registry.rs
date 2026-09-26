//! Provider-auth registry — the runtime bridge from the static provider
//! registry ([`crate::providers::registry`]) to the auth subsystem.
//!
//! [`crate::auth`] and [`crate::env_api_keys`] were ports of
//! `packages/ai/src/auth/*.ts` and had no production caller: every consumer
//! still read credential env vars itself. This module gives them one, mirroring
//! upstream's `Models.getProviderAuth` / `Models.getApiKeyForProvider`:
//!
//! * [`provider_auth_for`] builds the [`ProviderAuth`] strategies for a
//!   provider id from the registry's `api_key_env` list and OAuth spec.
//! * [`resolve_api_key_for_provider`] runs the shared
//!   [`resolve_provider_auth`] policy and returns the resolved key in the
//!   shape the streaming adapters take.
//!
//! # OAuth-first providers
//!
//! The OAuth-first providers (`github-copilot`, `openai-codex`,
//! `kimi-coding`) carry an [`OAuthSpec`](crate::providers::registry::OAuthSpec)
//! instead of an `api_key_env` list. They get an OAuth strategy wired to
//! the implementation in [`crate::auth::oauth`]; on `wasm32-unknown-unknown`
//! they fall back to `oauth = None` because the flows depend on
//! `tokio::time::sleep`, `tokio::net::TcpListener`, `reqwest`, and `sha2`.
//!
//! The registry is a `OnceLock`-memoized table: the strategies are stateless
//! (a display name plus the registry's `&'static` env var list), so one shared
//! copy is enough and callers get a cheap `ProviderAuth` clone.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use crate::providers::registry::BUILTIN_PROVIDERS;

use super::helpers::env_api_key_auth;
use super::resolve::{resolve_provider_auth, AuthResolutionOverrides};
use super::types::{
    ApiKeyCredential, AuthContext, AuthInteraction, CredentialStore, ModelAuth, OAuthAuth,
    OAuthCredential, ProviderAuth,
};
use super::AuthError;

/// Build the `provider_id -> ProviderAuth` table from the provider registry.
fn build_provider_auth_registry() -> BTreeMap<&'static str, ProviderAuth> {
    let mut registry = BTreeMap::new();
    for spec in BUILTIN_PROVIDERS {
        let api_key = if spec.api_key_env.is_empty() {
            None
        } else {
            let env_vars: Vec<String> = spec
                .api_key_env
                .iter()
                .map(|name| (*name).to_string())
                .collect();
            Some(env_api_key_auth(spec.display_name, env_vars))
        };
        #[cfg(not(target_arch = "wasm32"))]
        let oauth = spec.oauth.as_ref().map(|oauth_spec| build_oauth(spec.id, oauth_spec));
        #[cfg(target_arch = "wasm32")]
        let oauth: Option<Arc<dyn OAuthAuth>> = None;
        if api_key.is_none() && oauth.is_none() {
            // Keyless providers (`faux`) declare no credential and have no
            // OAuth flow — they never appear in the registry at all.
            continue;
        }
        registry.insert(
            spec.id,
            ProviderAuth {
                api_key,
                oauth,
            },
        );
    }
    registry
}

/// Build the OAuth implementation for a provider id. Centralised here so
/// the wiring reads as one switch and the actual implementations stay in
/// the [`crate::auth::oauth`] module.
#[cfg(not(target_arch = "wasm32"))]
fn build_oauth(
    provider_id: &'static str,
    spec: &crate::providers::registry::OAuthSpec,
) -> Arc<dyn OAuthAuth> {
    match provider_id {
        "github-copilot" => super::oauth::github_copilot_oauth(spec.kind),
        "openai-codex" => super::oauth::openai_codex_oauth(spec.kind),
        "kimi-coding" => super::oauth::kimi_coding_oauth(spec.kind),
        _ => {
            // Unknown OAuth-first provider — return a stub that reports
            // "no interactive flow" so the rest of the auth pipeline keeps
            // working (refresh / to_auth default to identity-preserving
            // behaviour on `OAuthAuth`).
            Arc::new(StubOAuth)
        }
    }
}

/// Placeholder OAuth implementation for unknown providers — keeps the
/// type system happy without locking the registry into a hard error.
struct StubOAuth;

#[async_trait::async_trait]
impl super::types::OAuthAuth for StubOAuth {
    fn name(&self) -> &'static str {
        "unknown OAuth provider"
    }

    async fn login(
        &self,
        _interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, AuthError> {
        Err(AuthError::Store(
            "unknown OAuth provider: no login flow registered".to_string(),
        ))
    }

    async fn refresh(
        &self,
        _credential: &OAuthCredential,
        _signal: &crate::types::AbortSignal,
    ) -> Result<OAuthCredential, AuthError> {
        Err(AuthError::Store(
            "unknown OAuth provider: no refresh flow registered".to_string(),
        ))
    }

    async fn to_auth(
        &self,
        _credential: &OAuthCredential,
    ) -> Result<ModelAuth, AuthError> {
        Err(AuthError::Store(
            "unknown OAuth provider: no to_auth mapping".to_string(),
        ))
    }
}

/// The memoized registry table.
fn provider_auth_registry() -> &'static BTreeMap<&'static str, ProviderAuth> {
    static REGISTRY: OnceLock<BTreeMap<&'static str, ProviderAuth>> = OnceLock::new();
    REGISTRY.get_or_init(build_provider_auth_registry)
}

/// The auth strategies registered for `provider_id`
/// (`Models.getProviderAuth`).
///
/// Returns `None` for an unknown provider id and for the keyless `faux`
/// provider, which declares no credential. OAuth-first providers
/// (`github-copilot`, `openai-codex`, `kimi-coding`) carry an OAuth
/// strategy under `oauth`, with `api_key = None` because they have no
/// env-var fallback.
pub fn provider_auth_for(provider_id: &str) -> Option<ProviderAuth> {
    provider_auth_registry().get(provider_id).cloned()
}

/// Resolve the api key a request to `provider_id` should use
/// (`Models.getApiKeyForProvider`).
///
/// Runs the shared [`resolve_provider_auth`] policy unchanged — a **stored
/// credential owns the provider**, the environment is consulted only when
/// nothing is stored, and a provider with no matching handler never falls back
/// — then narrows the result to the key the streaming adapters take.
///
/// `overrides.env` is where a caller injects a scoped environment (the
/// `pi-coding-agent` router passes its injectable `get_env` through it);
/// `overrides.api_key` deliberately still outranks the store, matching
/// upstream.
///
/// `Ok(None)` means "not configured": an unknown provider id, a provider with
/// no registered api-key strategy, or a provider whose store and environment
/// are both empty. An OAuth-only resolution is reported as `Ok(None)` too —
/// the adapters only accept an api key today — but the OAuth refresh (and its
/// credential rotation) still ran, and any provider-scoped `env` the
/// resolution produced is preserved on the returned [`ApiKeyCredential`].
pub async fn resolve_api_key_for_provider(
    provider_id: &str,
    credentials: &dyn CredentialStore,
    auth_context: &dyn AuthContext,
    overrides: Option<AuthResolutionOverrides>,
) -> Result<Option<ApiKeyCredential>, AuthError> {
    let Some(provider_auth) = provider_auth_for(provider_id) else {
        return Ok(None);
    };
    let Some(resolved) = resolve_provider_auth(
        provider_id,
        &provider_auth,
        credentials,
        auth_context,
        overrides,
    )
    .await?
    else {
        return Ok(None);
    };
    match resolved.auth.api_key {
        Some(api_key) if !api_key.is_empty() => Ok(Some(ApiKeyCredential {
            key: Some(api_key),
            env: resolved.env,
        })),
        // OAuth (headers / per-credential base URL, no api key) or a
        // credential that only carries provider-scoped env values: the
        // adapters have nothing to send, so this is "not configured" for them.
        _ => Ok(None),
    }
}
