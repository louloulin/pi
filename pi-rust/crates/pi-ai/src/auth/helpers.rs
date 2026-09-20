//! Auth strategy helpers — port of `packages/ai/src/auth/helpers.ts`.
//!
//! [`env_api_key_auth`] is the standard api-key strategy (stored key wins,
//! then the first set env var) and [`lazy_oauth`] defers loading an OAuth
//! implementation until the flow first needs it.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;

use super::types::{
    ApiKeyAuth, ApiKeyCredential, ApiKeyResolveInput, AuthInteraction, AuthPrompt, AuthResult,
    ModelAuth, OAuthAuth, OAuthCredential,
};
use super::{check_abort, AuthError};
use crate::types::AbortSignal;

/// Standard api-key auth: a stored credential key wins, otherwise the first
/// set env var resolves. Includes a `login` that prompts for the key.
/// (`envApiKeyAuth`)
///
/// Providers with non-standard resolution (provider env, ambient files, IAM)
/// write their own [`ApiKeyAuth`].
pub fn env_api_key_auth(name: impl Into<String>, env_vars: Vec<String>) -> Arc<dyn ApiKeyAuth> {
    Arc::new(EnvApiKeyAuth {
        name: name.into(),
        env_vars,
    })
}

struct EnvApiKeyAuth {
    name: String,
    env_vars: Vec<String>,
}

#[async_trait]
impl ApiKeyAuth for EnvApiKeyAuth {
    fn name(&self) -> &str {
        &self.name
    }

    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<Option<ApiKeyCredential>, AuthError> {
        if let Some(signal) = interaction.signal() {
            check_abort(signal)?;
        }
        let key = interaction
            .prompt(AuthPrompt::Secret {
                signal: interaction.signal().cloned(),
                message: format!("Enter {}", self.name),
                placeholder: None,
            })
            .await?;
        if let Some(signal) = interaction.signal() {
            check_abort(signal)?;
        }
        Ok(Some(ApiKeyCredential {
            key: Some(key),
            env: None,
        }))
    }

    async fn resolve(
        &self,
        input: ApiKeyResolveInput<'_>,
    ) -> Result<Option<AuthResult>, AuthError> {
        check_abort(input.signal)?;
        if let Some(credential) = input.credential {
            if let Some(key) = credential.key.as_ref().filter(|key| !key.is_empty()) {
                return Ok(Some(AuthResult {
                    auth: ModelAuth {
                        api_key: Some(key.clone()),
                        ..ModelAuth::default()
                    },
                    env: credential.env.clone(),
                    source: Some("stored credential".to_string()),
                }));
            }
        }
        for env_var in &self.env_vars {
            let value = input.ctx.env(env_var).await;
            check_abort(input.signal)?;
            if let Some(value) = value.filter(|value| !value.is_empty()) {
                return Ok(Some(AuthResult {
                    auth: ModelAuth {
                        api_key: Some(value),
                        ..ModelAuth::default()
                    },
                    env: None,
                    source: Some(env_var.clone()),
                }));
            }
        }
        Ok(None)
    }
}

/// Future returned by [`LazyOAuthConfig::load`].
pub type LazyOAuthLoadFuture =
    Pin<Box<dyn Future<Output = Result<Arc<dyn OAuthAuth>, AuthError>> + Send + 'static>>;

/// Configuration for [`lazy_oauth`] (`lazyOAuth` input).
pub struct LazyOAuthConfig {
    /// Display name, e.g. `"Anthropic (Claude Pro/Max)"`.
    pub name: String,
    /// Whether the auth method is subscription-backed.
    pub is_subscription: bool,
    /// Selector label for the login option.
    pub login_label: Option<String>,
    /// Loader invoked once, on first `login` / `refresh` / `to_auth` call.
    pub load: Box<dyn Fn() -> LazyOAuthLoadFuture + Send + Sync>,
}

/// Wrap a deferred [`OAuthAuth`] implementation (`lazyOAuth`).
///
/// Provider definitions can advertise OAuth without importing the
/// implementation; the flow loads on first `login` / `refresh` / `to_auth`.
/// Concurrent first calls share one load (upstream memoizes the load promise).
pub fn lazy_oauth(config: LazyOAuthConfig) -> Arc<dyn OAuthAuth> {
    Arc::new(LazyOAuth {
        name: config.name,
        is_subscription: config.is_subscription,
        login_label: config.login_label,
        load: config.load,
        loaded: tokio::sync::Mutex::new(None),
    })
}

struct LazyOAuth {
    name: String,
    is_subscription: bool,
    login_label: Option<String>,
    load: Box<dyn Fn() -> LazyOAuthLoadFuture + Send + Sync>,
    loaded: tokio::sync::Mutex<Option<Arc<dyn OAuthAuth>>>,
}

impl LazyOAuth {
    async fn get(&self) -> Result<Arc<dyn OAuthAuth>, AuthError> {
        let mut guard = self.loaded.lock().await;
        if let Some(provider) = guard.as_ref() {
            return Ok(provider.clone());
        }
        let provider = (self.load)().await?;
        *guard = Some(provider.clone());
        Ok(provider)
    }
}

#[async_trait]
impl OAuthAuth for LazyOAuth {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_subscription(&self) -> bool {
        self.is_subscription
    }

    fn login_label(&self) -> Option<&str> {
        self.login_label.as_deref()
    }

    async fn login(&self, interaction: &dyn AuthInteraction) -> Result<OAuthCredential, AuthError> {
        let provider = self.get().await?;
        provider.login(interaction).await
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &AbortSignal,
    ) -> Result<OAuthCredential, AuthError> {
        let provider = self.get().await?;
        provider.refresh(credential, signal).await
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        let provider = self.get().await?;
        provider.to_auth(credential).await
    }
}
