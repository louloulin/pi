//! Provider-auth registry — the runtime bridge from the static provider
//! registry ([`crate::providers::registry`]) to the auth subsystem.
//!
//! [`crate::auth`] and [`crate::env_api_keys`] were ports of
//! `packages/ai/src/auth/*.ts` and had no production caller: every consumer
//! still read credential env vars itself. This module gives them one, mirroring
//! upstream's `Models.getProviderAuth` / `Models.getApiKeyForProvider`:
//!
//! * [`provider_auth_for`] builds the [`ProviderAuth`] strategies for a
//!   provider id from the registry's `api_key_env` list.
//! * [`resolve_api_key_for_provider`] runs the shared
//!   [`resolve_provider_auth`] policy and returns the resolved key in the
//!   shape the streaming adapters take.
//!
//! # Scope: api keys only, OAuth deliberately unwired
//!
//! Every registered provider gets `api_key = Some(env_api_key_auth(...))` and
//! `oauth = None`. The OAuth-first providers (`github-copilot`,
//! `openai-codex`, `kimi-coding`) are **not** in
//! [`BUILTIN_PROVIDERS`](crate::providers::registry::BUILTIN_PROVIDERS) — their
//! flows are not ported yet — so [`provider_auth_for`] returns `None` for them
//! exactly like it does for an unknown id. The keyless `faux` provider is
//! skipped too: it declares no `api_key_env`, and its adapter needs no
//! credential.
//!
//! The registry is a `OnceLock`-memoized table: the strategies are stateless
//! (a display name plus the registry's `&'static` env var list), so one shared
//! copy is enough and callers get a cheap `ProviderAuth` clone.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::providers::registry::BUILTIN_PROVIDERS;

use super::helpers::env_api_key_auth;
use super::resolve::{resolve_provider_auth, AuthResolutionOverrides};
use super::types::{ApiKeyCredential, AuthContext, CredentialStore, ProviderAuth};
use super::AuthError;

/// Build the `provider_id -> ProviderAuth` table from the provider registry.
fn build_provider_auth_registry() -> BTreeMap<&'static str, ProviderAuth> {
    let mut registry = BTreeMap::new();
    for spec in BUILTIN_PROVIDERS {
        // Keyless providers (`faux`) declare no credential; OAuth-first
        // providers are not in the registry yet and so are absent entirely.
        if spec.api_key_env.is_empty() {
            continue;
        }
        let env_vars: Vec<String> = spec
            .api_key_env
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        registry.insert(
            spec.id,
            ProviderAuth {
                api_key: Some(env_api_key_auth(spec.display_name, env_vars)),
                oauth: None,
            },
        );
    }
    registry
}

/// The memoized registry table.
fn provider_auth_registry() -> &'static BTreeMap<&'static str, ProviderAuth> {
    static REGISTRY: OnceLock<BTreeMap<&'static str, ProviderAuth>> = OnceLock::new();
    REGISTRY.get_or_init(build_provider_auth_registry)
}

/// The auth strategies registered for `provider_id`
/// (`Models.getProviderAuth`).
///
/// Returns `None` for an unknown provider id, for the keyless `faux` provider,
/// and for the OAuth-first providers (`github-copilot`, `openai-codex`,
/// `kimi-coding`), which this build does not register yet — see the module
/// docs.
///
/// A returned [`ProviderAuth`] always carries an api-key strategy built from
/// the provider's registry `api_key_env` list (in registry priority order) and
/// no OAuth strategy.
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
    let Some(resolved) =
        resolve_provider_auth(provider_id, &provider_auth, credentials, auth_context, overrides)
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
