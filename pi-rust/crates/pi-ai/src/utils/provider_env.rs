//! Provider → environment lookup — port of
//! `packages/ai/src/utils/provider-env.ts`.
//!
//! `getProviderEnvValue` resolves one value from the provider-scoped `env`
//! map first and the process environment second. OAuth callback hosts,
//! `GOOGLE_APPLICATION_CREDENTIALS`, `PI_CACHE_RETENTION` and friends all read
//! through it, and the providers in this crate consult the same helper.
//!
//! # Deliberate differences from the TypeScript implementation
//!
//! * **The Bun `/proc/self/environ` fallback is not ported.** Upstream adds
//!   `getBunSandboxEnvValue` for oven-sh/bun#27802, where a compiled Bun binary
//!   sees an empty `process.env` inside a Linux sandbox although
//!   `/proc/self/environ` is populated. This crate never runs under Bun (the
//!   WASM host passes its environment explicitly, and native processes have a
//!   normal environment), so the duplicate of
//!   `restore-sandbox-env.ts` would be dead code. If a sandboxed-Bun consumer
//!   ever appears, that lookup belongs next to the caller, not here.
//! * **Lookup order is scoped → process, and empty strings fall through.**
//!   That is upstream's `env?.[name] || process.env[name] || ...` chain: the
//!   `||` treats `""` as absent, so a deliberately-empty scoped value does not
//!   shadow a real process value. Whitespace-only values are *not* trimmed,
//!   because `" "` is truthy in JavaScript too. (This differs from
//!   `env_api_keys`'s internal lookup, which trims process values to reject
//!   whitespace-only keys — that helper answers a different question.)
//! * **Non-Unicode process values are treated as absent.** `std::env::var`
//!   returns `NotUnicode` for them, and a `ProviderEnv` value is a `String`.
//!   Upstream's `process.env` is byte-oriented, so this can only differ for a
//!   non-UTF-8 value, which no provider credential is.

use crate::auth::types::ProviderEnv;

/// `getProviderEnvValue(name, env)` — resolve `name` from the scoped `env`
/// map first, then `std::env::var`, treating an empty string as absent.
///
/// Returns `None` when neither source holds a non-empty value.
pub fn get_provider_env_value(name: &str, env: Option<&ProviderEnv>) -> Option<String> {
    if let Some(value) = env.and_then(|env| env.get(name)) {
        if !value.is_empty() {
            return Some(value.clone());
        }
    }

    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}
