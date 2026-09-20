//! Provider → API-key environment lookup — port of
//! `packages/ai/src/env-api-keys.ts`.
//!
//! The provider → env-var mapping is **not** duplicated here:
//! [`crate::providers::registry`]'s `ProviderSpec::api_key_env` table is the
//! single data source (upstream's `getApiKeyEnvVars` was folded into the
//! registry in Stage 7), and this module only answers "which of those env vars
//! are set" and "what key do they yield".
//!
//! # Deliberate differences from the TypeScript implementation
//!
//! * Only env-var sources are inspected. Upstream's `google-vertex` branch
//!   probes Google Application Default Credentials on disk; `google-vertex`
//!   is not in the Rust registry yet, and this module must not carry a second
//!   provider table, so ADC detection is deferred to the provider port.
//!   The Bedrock ambient branch is pure env probing, so it is kept.
//! * Anthropic's `ANTHROPIC_AUTH_TOKEN` is skipped when *selecting* a key
//!   (upstream does the same: requests must pass it as `Authorization:
//!   Bearer`). Because the registry lists `ANTHROPIC_API_KEY` first, the
//!   selection order is `ANTHROPIC_API_KEY` then `ANTHROPIC_OAUTH_TOKEN` —
//!   upstream's own table lists the OAuth token first. Availability reporting
//!   in [`find_env_keys`] keeps the registry order for both.

use crate::auth::types::ProviderEnv;
use crate::providers::registry::api_key_env_vars;

/// Anthropic auth-token env var (`ANTHROPIC_AUTH_TOKEN_ENV`).
pub const ANTHROPIC_AUTH_TOKEN_ENV: &str = "ANTHROPIC_AUTH_TOKEN";
/// Anthropic OAuth-token env var (`ANTHROPIC_OAUTH_TOKEN_ENV`).
pub const ANTHROPIC_OAUTH_TOKEN_ENV: &str = "ANTHROPIC_OAUTH_TOKEN";
/// Anthropic API-key env var (`ANTHROPIC_API_KEY_ENV`).
pub const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// Sentinel returned when ambient credentials (IAM / ADC) are configured
/// rather than a literal API key.
const AUTHENTICATED: &str = "<authenticated>";

/// Resolve one env value from the scoped overrides first, then `process.env`
/// (`getProviderEnvValue`).
fn env_value(name: &str, env: Option<&ProviderEnv>) -> Option<String> {
    if let Some(env) = env {
        if let Some(value) = env.get(name) {
            if !value.is_empty() {
                return Some(value.clone());
            }
        }
    }
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

/// Find configured environment variables that can provide an API key for a
/// provider (`findEnvKeys`).
///
/// Returns the registry's `api_key_env` entries that are set, in registry
/// priority order, or `None` when none are set (or the provider is unknown).
/// This only reports actual API key variables; ambient credential sources
/// such as AWS profiles, IAM credentials and Google ADC are intentionally
/// excluded.
pub fn find_env_keys(provider: &str, env: Option<&ProviderEnv>) -> Option<Vec<String>> {
    let env_vars = api_key_env_vars(provider);
    if env_vars.is_empty() {
        return None;
    }
    let found: Vec<String> = env_vars
        .iter()
        .filter(|env_var| env_value(env_var, env).is_some())
        .map(|env_var| (*env_var).to_string())
        .collect();
    if found.is_empty() {
        None
    } else {
        Some(found)
    }
}

/// Get an API key for a provider from its known environment variables
/// (`getEnvApiKey`).
///
/// Returns the first configured credential variable in registry order, except
/// that Anthropic skips its bearer-only `ANTHROPIC_AUTH_TOKEN`. Ambient-only
/// providers (`amazon-bedrock`) yield the [`AUTHENTICATED`] sentinel when
/// their credential sources are configured. Returns `None` when the provider
/// is unknown or unconfigured.
pub fn get_env_api_key(provider: &str, env: Option<&ProviderEnv>) -> Option<String> {
    if let Some(keys) = find_env_keys(provider, env) {
        let selected = if provider == "anthropic" {
            keys.iter()
                .find(|key| key.as_str() != ANTHROPIC_AUTH_TOKEN_ENV)
        } else {
            keys.first()
        };
        if let Some(env_var) = selected {
            return env_value(env_var, env);
        }
    }

    ambient_env_api_key(provider, env)
}

/// Ambient (non-API-key) credential sources (`getEnvApiKey` tail).
fn ambient_env_api_key(provider: &str, env: Option<&ProviderEnv>) -> Option<String> {
    if provider == "amazon-bedrock" {
        // 1. AWS_PROFILE - named profile from ~/.aws/credentials
        // 2. AWS_ACCESS_KEY_ID + AWS_SECRET_ACCESS_KEY - standard IAM keys
        // 3. AWS_BEARER_TOKEN_BEDROCK - Bedrock bearer token
        // 4. AWS_CONTAINER_CREDENTIALS_RELATIVE_URI - ECS task roles
        // 5. AWS_CONTAINER_CREDENTIALS_FULL_URI - ECS task roles (full URI)
        // 6. AWS_WEB_IDENTITY_TOKEN_FILE - IRSA (IAM Roles for Service Accounts)
        let has_iam_keys = env_value("AWS_ACCESS_KEY_ID", env).is_some()
            && env_value("AWS_SECRET_ACCESS_KEY", env).is_some();
        if env_value("AWS_PROFILE", env).is_some()
            || has_iam_keys
            || env_value("AWS_BEARER_TOKEN_BEDROCK", env).is_some()
            || env_value("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI", env).is_some()
            || env_value("AWS_CONTAINER_CREDENTIALS_FULL_URI", env).is_some()
            || env_value("AWS_WEB_IDENTITY_TOKEN_FILE", env).is_some()
        {
            return Some(AUTHENTICATED.to_string());
        }
    }

    None
}
