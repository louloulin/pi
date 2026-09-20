//! Provider-auth resolution — port of `packages/ai/src/auth/resolve.ts`.
//!
//! [`resolve_provider_auth`] is the shared policy used by the model and image
//! collections. Its invariant: **a stored credential owns the provider** —
//! ambient/env is consulted only when nothing is stored, and there is no
//! silent env fallback after a failed refresh or for a credential type with no
//! matching handler.
//!
//! # Deliberate differences from the TypeScript implementation
//!
//! * Aborting surfaces as `AuthError::Aborted` instead of an `AbortError`;
//!   coded failures are `ModelsError` inside `AuthError::Models`.
//! * `resolveProviderAuth` takes the provider id and [`ProviderAuth`]
//!   separately rather than a `{ id, auth }` object.
//! * An abort that races a refresh drops the refresh future (the in-memory
//!   store releases its lock); upstream keeps observing the abandoned promise.

use std::fmt::Display;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use thiserror::Error;

use super::types::{
    ApiKeyAuth, ApiKeyCredential, ApiKeyResolveInput, AuthContext, AuthOperationOptions,
    AuthResult, Credential, CredentialModifyFn, CredentialStore, OAuthAuth, OAuthCredential,
    ProviderAuth, ProviderEnv,
};
use super::{check_abort, race_with_abort, AuthError};
use crate::types::AbortSignal;

/// Minimum remaining OAuth validity before a token is refreshed
/// (`DEFAULT_OAUTH_MINIMUM_VALIDITY_MS`, five minutes).
pub const DEFAULT_OAUTH_MINIMUM_VALIDITY_MS: i64 = 5 * 60 * 1000;

/// Refresh network timeout (`DEFAULT_OAUTH_REFRESH_TIMEOUT_MS`).
pub const DEFAULT_OAUTH_REFRESH_TIMEOUT_MS: u64 = 15_000;

/// Error codes upstream's `ModelsError` may carry (`ModelsErrorCode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelsErrorCode {
    /// A model source could not be read.
    ModelSource,
    /// A model failed validation.
    ModelValidation,
    /// A provider-level failure.
    Provider,
    /// A stream-level failure.
    Stream,
    /// Auth resolution failed.
    Auth,
    /// OAuth login / refresh / derivation failed.
    OAuth,
}

impl ModelsErrorCode {
    /// The wire / display value of the code, matching upstream's string union.
    pub fn as_str(self) -> &'static str {
        match self {
            ModelsErrorCode::ModelSource => "model_source",
            ModelsErrorCode::ModelValidation => "model_validation",
            ModelsErrorCode::Provider => "provider",
            ModelsErrorCode::Stream => "stream",
            ModelsErrorCode::Auth => "auth",
            ModelsErrorCode::OAuth => "oauth",
        }
    }
}

impl Display for ModelsErrorCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Coded auth / model error (`ModelsError`).
#[derive(Debug, Error)]
#[error("{message}")]
pub struct ModelsError {
    code: ModelsErrorCode,
    message: String,
}

impl ModelsError {
    /// Build an error with no cause detail.
    pub fn new(code: ModelsErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Build an error folding `cause` into the message
    /// (`withCauseDetail` upstream).
    ///
    /// Callers surface only the message, so the underlying reason is appended
    /// as `"<message>: <cause>"` unless it is empty or already present.
    pub fn with_cause(
        code: ModelsErrorCode,
        message: impl Into<String>,
        cause: impl Display,
    ) -> Self {
        let message = message.into();
        let detail = cause.to_string();
        let detail = detail.trim();
        let message = if detail.is_empty() || message.contains(detail) {
            message
        } else {
            format!("{message}: {detail}")
        };
        Self { code, message }
    }

    /// The error code.
    pub fn code(&self) -> ModelsErrorCode {
        self.code
    }

    /// The human-readable message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Caller-supplied overrides for [`resolve_provider_auth`]
/// (`AuthResolutionOverrides`).
#[derive(Debug, Clone, Default)]
pub struct AuthResolutionOverrides {
    /// Use this API key instead of reading the store (still requires the
    /// provider to declare api-key auth).
    pub api_key: Option<String>,
    /// Provider env values that take priority over the ambient environment.
    pub env: Option<ProviderEnv>,
    /// Require this much remaining OAuth validity; defaults to five minutes.
    pub min_oauth_validity_ms: Option<i64>,
    /// Cancellation token for the whole resolution.
    pub signal: Option<AbortSignal>,
}

/// Resolve request auth for `provider_id` given its auth strategies
/// (`resolveProviderAuth`).
///
/// Resolution order:
///
/// 1. an `overrides.api_key` (when the provider declares api-key auth);
/// 2. the stored credential — OAuth (with refresh) or api key;
/// 3. ambient env / files, only when nothing is stored.
///
/// `Ok(None)` means "not configured".
pub async fn resolve_provider_auth(
    provider_id: &str,
    auth: &ProviderAuth,
    credentials: &dyn CredentialStore,
    auth_context: &dyn AuthContext,
    overrides: Option<AuthResolutionOverrides>,
) -> Result<Option<AuthResult>, AuthError> {
    let overrides = overrides.unwrap_or_default();
    let signal = overrides.signal.clone().unwrap_or_default();
    race_with_abort(
        resolve_with_signal(
            provider_id,
            auth,
            credentials,
            auth_context,
            &overrides,
            &signal,
        ),
        &signal,
    )
    .await?
}

async fn resolve_with_signal(
    provider_id: &str,
    auth: &ProviderAuth,
    credentials: &dyn CredentialStore,
    auth_context: &dyn AuthContext,
    overrides: &AuthResolutionOverrides,
    signal: &AbortSignal,
) -> Result<Option<AuthResult>, AuthError> {
    check_abort(signal)?;
    let overlay = overrides
        .env
        .as_ref()
        .map(|env| EnvOverlayAuthContext::new(auth_context, env.clone()));
    let request_context: &dyn AuthContext = match &overlay {
        Some(context) => context,
        None => auth_context,
    };

    // An explicit override key only applies when the provider speaks api keys.
    if let (Some(api_key), Some(api)) = (overrides.api_key.as_ref(), auth.api_key.as_ref()) {
        let credential = ApiKeyCredential {
            key: Some(api_key.clone()),
            env: overrides.env.clone(),
        };
        return resolve_api_key(
            request_context,
            api.as_ref(),
            provider_id,
            Some(&credential),
            signal,
        )
        .await;
    }

    let stored = read_credential(credentials, provider_id, signal).await?;
    if let Some(stored) = stored {
        match (stored, auth.oauth.as_ref(), auth.api_key.as_ref()) {
            (Credential::OAuth(credential), Some(oauth), _) => {
                return resolve_stored_oauth(
                    credentials,
                    provider_id,
                    oauth.clone(),
                    credential,
                    signal,
                    overrides.min_oauth_validity_ms,
                )
                .await;
            }
            (Credential::ApiKey(mut credential), _, Some(api)) => {
                // `{ ...stored, env: { ...stored.env, ...overrides.env } }`
                if let Some(env) = overrides.env.as_ref() {
                    let mut merged = credential.env.take().unwrap_or_default();
                    merged.extend(env.clone());
                    credential.env = Some(merged);
                }
                return resolve_api_key(
                    request_context,
                    api.as_ref(),
                    provider_id,
                    Some(&credential),
                    signal,
                )
                .await;
            }
            // A stored credential with no matching handler short-circuits:
            // there is deliberately no env fallback.
            _ => return Ok(None),
        }
    }

    // Ambient (env vars, AWS profiles, ADC files).
    match auth.api_key.as_ref() {
        Some(api) => {
            resolve_api_key(request_context, api.as_ref(), provider_id, None, signal).await
        }
        None => Ok(None),
    }
}

/// `overlayEnvAuthContext`: override env values win, then the base context.
struct EnvOverlayAuthContext<'a> {
    base: &'a dyn AuthContext,
    env: ProviderEnv,
}

impl<'a> EnvOverlayAuthContext<'a> {
    fn new(base: &'a dyn AuthContext, env: ProviderEnv) -> Self {
        Self { base, env }
    }
}

#[async_trait]
impl AuthContext for EnvOverlayAuthContext<'_> {
    async fn env(&self, name: &str) -> Option<String> {
        match self.env.get(name) {
            Some(value) if !value.is_empty() => Some(value.clone()),
            _ => self.base.env(name).await,
        }
    }

    async fn file_exists(&self, path: &str) -> bool {
        self.base.file_exists(path).await
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn oauth_expires_soon(credential: &OAuthCredential, minimum_validity_ms: i64) -> bool {
    now_ms() + minimum_validity_ms >= credential.expires
}

/// OAuth resolution with double-checked locking: tokens with less than the
/// minimum remaining validity lock, re-check expiry under the lock, refresh
/// once globally, and persist the rotated credential before release.
async fn resolve_stored_oauth(
    credentials: &dyn CredentialStore,
    provider_id: &str,
    oauth: Arc<dyn OAuthAuth>,
    stored: OAuthCredential,
    signal: &AbortSignal,
    min_oauth_validity_ms: Option<i64>,
) -> Result<Option<AuthResult>, AuthError> {
    let minimum_validity_ms =
        DEFAULT_OAUTH_MINIMUM_VALIDITY_MS.max(min_oauth_validity_ms.unwrap_or(0));
    let mut credential = stored;

    if oauth_expires_soon(&credential, minimum_validity_ms) {
        let provider_id_owned = provider_id.to_string();
        let refresh_signal = signal.clone();
        let refresh_oauth = oauth.clone();
        let modify: CredentialModifyFn = Box::new(move |current| {
            Box::pin(async move {
                let current = match current {
                    Some(Credential::OAuth(credential)) => credential,
                    // Logged out meanwhile, or a different credential type landed.
                    _ => return Ok(None),
                };
                if !oauth_expires_soon(&current, minimum_validity_ms) {
                    // Another process/request refreshed already.
                    return Ok(None);
                }
                match refresh_with_timeout(
                    refresh_oauth.as_ref(),
                    &current,
                    &refresh_signal,
                    DEFAULT_OAUTH_REFRESH_TIMEOUT_MS,
                )
                .await
                {
                    Ok(refreshed) => Ok(Some(Credential::OAuth(refreshed))),
                    Err(cause) => Err(AuthError::Models(ModelsError::with_cause(
                        ModelsErrorCode::OAuth,
                        format!("OAuth refresh failed for {provider_id_owned}"),
                        cause,
                    ))),
                }
            })
        });

        let post = credentials
            .modify(
                provider_id,
                modify,
                Some(AuthOperationOptions {
                    signal: Some(signal.clone()),
                }),
            )
            .await;
        let post = match post {
            Ok(post) => post,
            // A ModelsError raised inside the callback propagates unchanged.
            Err(AuthError::Models(error)) => return Err(AuthError::Models(error)),
            Err(AuthError::Aborted) => return Err(AuthError::Aborted),
            Err(error) => {
                return Err(AuthError::Models(ModelsError::with_cause(
                    ModelsErrorCode::Auth,
                    format!("Credential store modify failed for {provider_id}"),
                    error,
                )));
            }
        };
        let post = match post {
            Some(Credential::OAuth(credential)) => credential,
            // Logged out meanwhile.
            _ => return Ok(None),
        };
        credential = post;
        // The default five-minute window triggers a refresh but does not impose
        // a provider contract. Explicit callers (such as bearer-token export)
        // do require the requested minimum after the refresh.
        if min_oauth_validity_ms.is_some() && oauth_expires_soon(&credential, minimum_validity_ms) {
            return Err(AuthError::Models(ModelsError::new(
                ModelsErrorCode::OAuth,
                format!("OAuth refresh returned a token that expires too soon for {provider_id}"),
            )));
        }
    }

    match oauth.to_auth(&credential).await {
        Ok(auth) => Ok(Some(AuthResult {
            auth,
            env: None,
            source: Some("OAuth".to_string()),
        })),
        Err(error) => Err(AuthError::Models(ModelsError::with_cause(
            ModelsErrorCode::OAuth,
            format!("OAuth auth derivation failed for {provider_id}"),
            error,
        ))),
    }
}

/// `AbortSignal.any([signal, AbortSignal.timeout(ms)])` around a refresh.
async fn refresh_with_timeout(
    oauth: &dyn OAuthAuth,
    credential: &OAuthCredential,
    signal: &AbortSignal,
    timeout_ms: u64,
) -> Result<OAuthCredential, AuthError> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        tokio::select! {
            result = oauth.refresh(credential, signal) => result,
            _ = tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)) => {
                Err(AuthError::Store(format!(
                    "OAuth refresh timed out after {timeout_ms} ms"
                )))
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = timeout_ms;
        oauth.refresh(credential, signal).await
    }
}

async fn resolve_api_key(
    auth_context: &dyn AuthContext,
    api_key: &dyn ApiKeyAuth,
    provider_id: &str,
    credential: Option<&ApiKeyCredential>,
    signal: &AbortSignal,
) -> Result<Option<AuthResult>, AuthError> {
    let input = ApiKeyResolveInput {
        ctx: auth_context,
        credential,
        signal,
    };
    match api_key.resolve(input).await {
        Ok(result) => Ok(result),
        Err(AuthError::Aborted) => Err(AuthError::Aborted),
        Err(error) => Err(AuthError::Models(ModelsError::with_cause(
            ModelsErrorCode::Auth,
            format!("API key auth failed for provider {provider_id}"),
            error,
        ))),
    }
}

async fn read_credential(
    credentials: &dyn CredentialStore,
    provider_id: &str,
    signal: &AbortSignal,
) -> Result<Option<Credential>, AuthError> {
    let options = AuthOperationOptions {
        signal: Some(signal.clone()),
    };
    match credentials.read(provider_id, Some(options)).await {
        Ok(credential) => Ok(credential),
        Err(AuthError::Aborted) => Err(AuthError::Aborted),
        Err(error) => Err(AuthError::Models(ModelsError::with_cause(
            ModelsErrorCode::Auth,
            format!("Credential store read failed for {provider_id}"),
            error,
        ))),
    }
}
