//! Credential and provider-auth types — port of
//! `packages/ai/src/auth/types.ts`.
//!
//! The module keeps upstream's naming (`ModelAuth`, `Credential`,
//! `CredentialStore`, `ProviderAuth`, `AuthContext`, `AuthResult`, ...) so the
//! credential subsystem reads the same on both sides.
//!
//! # Deliberate differences from the TypeScript implementation
//!
//! * [`Credential`] is a serde-tagged enum (`type` discriminator, the same
//!   wire shape as today's `auth.json`) instead of a structural union; the
//!   per-variant payloads ([`ApiKeyCredential`], [`OAuthCredential`]) keep
//!   upstream's fields.
//! * The provider-auth members are `async_trait` objects
//!   (`Arc<dyn ApiKeyAuth>`). Upstream's optional `login?` / `check?` become
//!   defaulted trait methods: the default `login` reports "no interactive
//!   flow" with `Ok(None)` and the default `check` reports "unknown" with
//!   `Ok(None)`, which is upstream's "member absent" meaning.
//! * `AbortSignal` is the crate's existing cancellation token
//!   ([`crate::types::AbortSignal`]), not the DOM `AbortSignal`; cancellation
//!   surfaces as [`super::AuthError::Aborted`] because Rust has no
//!   `DOMException`.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::types::AbortSignal;

use super::AuthError;

/// Provider-scoped environment/config values (`ProviderEnv`,
/// `Record<string, string>` upstream).
pub type ProviderEnv = BTreeMap<String, String>;

/// Request headers (`ProviderHeaders`); `None` values are emitted as `null`.
pub type ProviderHeaders = BTreeMap<String, Option<String>>;

/// Request auth for a single model request (`ModelAuth`).
///
/// If a value cannot be expressed as `api_key`, `headers` or `base_url`, it is
/// provider config, not auth.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelAuth {
    /// Bearer / `x-api-key` style credential.
    pub api_key: Option<String>,
    /// Extra request headers.
    pub headers: Option<ProviderHeaders>,
    /// Per-credential base URL (GitHub Copilot).
    pub base_url: Option<String>,
}

/// Stored api-key credential (`ApiKeyCredential`).
///
/// `env` holds provider-scoped environment/config values such as Cloudflare
/// account/gateway ids.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyCredential {
    /// The API key. Absent for providers that only carry `env` values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Provider-scoped config values resolved alongside the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<ProviderEnv>,
}

/// Stored canonical OAuth credential (`OAuthCredential`).
///
/// Unknown fields survive a JSON round-trip through `extra`, matching
/// upstream's `[key: string]: unknown` index signature.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OAuthCredential {
    /// Refresh token used to mint a new access token.
    pub refresh: String,
    /// Current access token.
    pub access: String,
    /// Unix timestamp in milliseconds at which `access` expires.
    pub expires: i64,
    /// Provider-specific extra fields kept verbatim.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// One type-tagged credential per provider — the shape of today's `auth.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Credential {
    /// An api-key credential.
    ApiKey(ApiKeyCredential),
    /// An OAuth credential.
    OAuth(OAuthCredential),
}

impl Credential {
    /// Discriminator value, mirroring upstream's `credential.type`.
    pub fn auth_type(&self) -> AuthType {
        match self {
            Credential::ApiKey(_) => AuthType::ApiKey,
            Credential::OAuth(_) => AuthType::OAuth,
        }
    }

    /// Borrow the api-key payload, if this is an api-key credential.
    pub fn as_api_key(&self) -> Option<&ApiKeyCredential> {
        match self {
            Credential::ApiKey(credential) => Some(credential),
            Credential::OAuth(_) => None,
        }
    }

    /// Borrow the OAuth payload, if this is an OAuth credential.
    pub fn as_oauth(&self) -> Option<&OAuthCredential> {
        match self {
            Credential::OAuth(credential) => Some(credential),
            Credential::ApiKey(_) => None,
        }
    }
}

/// Non-secret credential metadata (`CredentialInfo`) for status enumeration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialInfo {
    /// Provider the credential belongs to.
    pub provider_id: String,
    /// Credential kind.
    #[serde(rename = "type")]
    pub r#type: AuthType,
}

/// The two credential kinds (`AuthType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthType {
    /// An api-key credential.
    ApiKey,
    /// An OAuth credential.
    OAuth,
}

/// Optional cancellation for public auth and credential operations
/// (`AuthOperationOptions`).
#[derive(Debug, Clone, Default)]
pub struct AuthOperationOptions {
    /// Cancellation token for the operation.
    pub signal: Option<AbortSignal>,
}

/// Future produced by a [`CredentialStore::modify`] callback.
pub type CredentialModifyFuture =
    Pin<Box<dyn Future<Output = Result<Option<Credential>, AuthError>> + Send + 'static>>;

/// Callback handed to [`CredentialStore::modify`].
///
/// It receives the current credential because correct writes (refresh,
/// login-during-refresh) depend on it, and returns the new credential or
/// `None` to leave the entry unchanged.
pub type CredentialModifyFn =
    Box<dyn FnOnce(Option<Credential>) -> CredentialModifyFuture + Send + 'static>;

/// App-owned credential storage, keyed by provider id, one credential per
/// provider (`CredentialStore`).
///
/// `modify` is the only write path, so every mutation is a serialized
/// read-modify-write — upstream's `Models.getAuth()` runs the OAuth refresh
/// inside `modify` so concurrent requests cannot double-refresh a rotated
/// token.
///
/// Error semantics: `read` resolves `Ok(None)` for missing entries. Methods
/// reject only on storage failure; [`super::resolve::resolve_provider_auth`]
/// wraps such rejections in a `ModelsError` with code `auth`. Best-effort
/// stores that serve an in-memory view and record persistence errors
/// internally are valid implementations.
#[async_trait]
pub trait CredentialStore: Send + Sync {
    /// Read the stored credential, possibly expired. Display/status use;
    /// resolved request auth comes from
    /// [`super::resolve::resolve_provider_auth`].
    async fn read(
        &self,
        provider_id: &str,
        options: Option<AuthOperationOptions>,
    ) -> Result<Option<Credential>, AuthError>;

    /// List stored credential metadata without resolving or exposing secrets.
    ///
    /// Implementations must not execute configured API-key commands while
    /// listing.
    async fn list(
        &self,
        options: Option<AuthOperationOptions>,
    ) -> Result<Vec<CredentialInfo>, AuthError>;

    /// Serialized write — the only write path. Mutual exclusion is per
    /// provider id; the resolved value is the post-write credential
    /// (`next`, or the credential the callback saw when it returned `None`).
    /// Errors from `fn` propagate unchanged.
    async fn modify(
        &self,
        provider_id: &str,
        f: CredentialModifyFn,
        options: Option<AuthOperationOptions>,
    ) -> Result<Option<Credential>, AuthError>;

    /// Remove a credential (logout). Implementations serialize this against
    /// `modify`.
    async fn delete(
        &self,
        provider_id: &str,
        options: Option<AuthOperationOptions>,
    ) -> Result<(), AuthError>;
}

/// Environment access for auth resolution (`AuthContext`).
///
/// Injectable for tests and browsers.
#[async_trait]
pub trait AuthContext: Send + Sync {
    /// Read an environment variable, or `None` when unset/empty.
    async fn env(&self, name: &str) -> Option<String>;
    /// Check whether a file exists. Supports a leading `~`.
    async fn file_exists(&self, path: &str) -> bool;
}

/// Default auth context: env vars from the process environment, file
/// existence through [`std::path::Path`].
///
/// Upstream's browser branch (always `false` for files, no `process.env`) is
/// implicit: on `wasm32` the standard library's env/fs calls return unset /
/// not-found without panicking.
pub struct ProcessAuthContext;

#[async_trait]
impl AuthContext for ProcessAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        match std::env::var(name) {
            Ok(value) if !value.trim().is_empty() => Some(value),
            _ => None,
        }
    }

    async fn file_exists(&self, path: &str) -> bool {
        let resolved = if let Some(rest) = path.strip_prefix('~') {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .ok();
            match home {
                Some(home) => format!("{home}{rest}"),
                None => return false,
            }
        } else {
            path.to_string()
        };
        std::path::Path::new(&resolved).exists()
    }
}

/// Construct the default process-backed [`AuthContext`]
/// (`defaultProviderAuthContext`).
pub fn default_provider_auth_context() -> Arc<dyn AuthContext> {
    Arc::new(ProcessAuthContext)
}

/// Result of resolving auth for a model (`AuthResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthResult {
    /// Request auth for the model call.
    pub auth: ModelAuth,
    /// Provider-scoped environment/config values resolved from credentials
    /// and ambient context.
    pub env: Option<ProviderEnv>,
    /// Human-readable label for status UI (`"ANTHROPIC_API_KEY"`, `"OAuth"`,
    /// `"~/.aws/credentials"`).
    pub source: Option<String>,
}

/// Side-effect-free availability result (`AuthCheck`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthCheck {
    /// Human-readable source label, when known.
    pub source: Option<String>,
    /// Credential kind the check resolved to.
    pub r#type: AuthType,
}

/// Input for [`ApiKeyAuth::resolve`].
pub struct ApiKeyResolveInput<'a> {
    /// Environment / file access for ambient sources.
    pub ctx: &'a dyn AuthContext,
    /// Stored credential, when one exists.
    pub credential: Option<&'a ApiKeyCredential>,
    /// Cancellation token.
    pub signal: &'a AbortSignal,
}

/// Input for [`ApiKeyAuth::check`].
pub struct ApiKeyCheckInput<'a> {
    /// Environment / file access for ambient sources.
    pub ctx: &'a dyn AuthContext,
    /// Stored credential, when one exists.
    pub credential: Option<&'a ApiKeyCredential>,
    /// Cancellation token.
    pub signal: &'a AbortSignal,
}

/// Api-key auth (`ApiKeyAuth`): stored key/provider env plus ambient sources
/// (env vars, AWS profiles, ADC files). Ambient-only providers omit `login`.
#[async_trait]
pub trait ApiKeyAuth: Send + Sync {
    /// Display name, e.g. `"Anthropic API key"`.
    fn name(&self) -> &str;

    /// Interactive setup (prompt for key/provider env).
    ///
    /// The default implementation reports "no interactive flow" with
    /// `Ok(None)`, which is upstream's absent `login?` member
    /// (ambient-only providers).
    async fn login(
        &self,
        _interaction: &dyn AuthInteraction,
    ) -> Result<Option<ApiKeyCredential>, AuthError> {
        Ok(None)
    }

    /// Optional side-effect-free availability check.
    ///
    /// Use this when [`ApiKeyAuth::resolve`] may execute commands or perform
    /// other request-time work. The default `Ok(None)` is upstream's absent
    /// `check?` member: the caller falls back to resolving auth.
    async fn check(&self, _input: ApiKeyCheckInput<'_>) -> Result<Option<AuthCheck>, AuthError> {
        Ok(None)
    }

    /// Resolve auth from the stored credential and/or ambient sources,
    /// merging per field (`credential.key`, then `env("...")`). `Ok(None)`
    /// means not configured. Resolution is provider-scoped; model-specific
    /// endpoint preparation happens after auth has been resolved.
    async fn resolve(&self, input: ApiKeyResolveInput<'_>)
        -> Result<Option<AuthResult>, AuthError>;
}

/// OAuth auth (`OAuthAuth`).
///
/// The `refresh` / `to_auth` split lets the resolver own the locked refresh
/// pattern: `refresh` produces a credential, `to_auth` derives request auth
/// from whatever credential ends up stored.
#[async_trait]
pub trait OAuthAuth: Send + Sync {
    /// Display name, e.g. `"Anthropic (Claude Pro/Max)"`.
    fn name(&self) -> &str;

    /// Whether access through this auth method is backed by a provider
    /// subscription. Upstream's optional `isSubscription`; defaults to false.
    fn is_subscription(&self) -> bool {
        false
    }

    /// Selector label for the OAuth login option (`loginLabel`).
    fn login_label(&self) -> Option<&str> {
        None
    }

    /// Run the interactive login flow.
    async fn login(&self, interaction: &dyn AuthInteraction) -> Result<OAuthCredential, AuthError>;

    /// Exchange the refresh token. Network call; fails on `invalid_grant`
    /// etc. The resolver runs this under the credential-store lock.
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &AbortSignal,
    ) -> Result<OAuthCredential, AuthError>;

    /// Side-effect-free derivation of request auth from a valid credential.
    /// Covers per-credential base URLs.
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError>;
}

/// One selectable option in an [`AuthPrompt::Select`] prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthPromptOption {
    /// Option id returned by [`AuthInteraction::prompt`].
    pub id: String,
    /// Display label.
    pub label: String,
    /// Optional helper text.
    pub description: Option<String>,
}

/// Prompt shown to the user during login (`AuthPrompt`).
///
/// `signal` lets the flow cancel a pending prompt when an out-of-band event
/// resolves the step, e.g. a `manual_code` prompt raced against a callback
/// server, aborted when the callback wins.
#[derive(Debug, Clone)]
pub enum AuthPrompt {
    /// Free-text prompt.
    Text {
        /// Per-prompt cancellation token.
        signal: Option<AbortSignal>,
        /// Prompt message.
        message: String,
        /// Optional placeholder.
        placeholder: Option<String>,
    },
    /// Secret (masked) prompt.
    Secret {
        /// Per-prompt cancellation token.
        signal: Option<AbortSignal>,
        /// Prompt message.
        message: String,
        /// Optional placeholder.
        placeholder: Option<String>,
    },
    /// Single-select prompt; the option id is returned.
    Select {
        /// Per-prompt cancellation token.
        signal: Option<AbortSignal>,
        /// Prompt message.
        message: String,
        /// Selectable options.
        options: Vec<AuthPromptOption>,
    },
    /// Manual-code prompt (device flow fallback).
    ManualCode {
        /// Per-prompt cancellation token.
        signal: Option<AbortSignal>,
        /// Prompt message.
        message: String,
        /// Optional placeholder.
        placeholder: Option<String>,
    },
}

/// A link surfaced by an [`AuthEvent::Info`] event (`AuthInfoLink`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthInfoLink {
    /// Destination URL.
    pub url: String,
    /// Optional display label.
    pub label: Option<String>,
}

/// Progress / instruction event emitted during login (`AuthEvent`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthEvent {
    /// Informational message, optionally with links.
    Info {
        /// Message body.
        message: String,
        /// Related links.
        links: Vec<AuthInfoLink>,
    },
    /// Authorization URL the user must visit.
    AuthUrl {
        /// Authorization URL.
        url: String,
        /// Optional instructions.
        instructions: Option<String>,
    },
    /// Device-code flow instructions.
    DeviceCode {
        /// Code the user enters at the verification URI.
        user_code: String,
        /// URI the user visits.
        verification_uri: String,
        /// Polling interval in seconds.
        interval_seconds: Option<u64>,
        /// Device-code lifetime in seconds.
        expires_in_seconds: Option<u64>,
    },
    /// Progress message.
    Progress {
        /// Message body.
        message: String,
    },
}

/// Login interaction callbacks serving both api-key and OAuth flows
/// (`AuthInteraction`).
///
/// `prompt()` returns the entered/selected string (`Select` returns the option
/// id). It fails on cancel/abort. `signal()` aborts the whole login flow;
/// per-prompt cancellation uses the prompt's own `signal`.
#[async_trait]
pub trait AuthInteraction: Send + Sync {
    /// Signal that aborts the whole login flow, when the caller supplied one.
    fn signal(&self) -> Option<&AbortSignal>;
    /// Ask the user for input and return the entered / selected value.
    async fn prompt(&self, prompt: AuthPrompt) -> Result<String, AuthError>;
    /// Emit a progress / instruction event.
    fn notify(&self, event: AuthEvent);
}

/// Interaction whose `signal` is guaranteed present
/// (`ProviderAuthInteraction` = `AuthInteraction & { signal: AbortSignal }`).
pub trait ProviderAuthInteraction: AuthInteraction {
    /// The always-present flow cancellation token.
    fn required_signal(&self) -> &AbortSignal;
}

/// Provider auth (`ProviderAuth`).
///
/// At least one of `api_key` / `oauth` must be present: even ambient-credential
/// providers and keyless local servers provide `api_key` auth whose `resolve`
/// reports whether the provider is configured.
#[derive(Clone, Default)]
pub struct ProviderAuth {
    /// Api-key auth strategy.
    pub api_key: Option<Arc<dyn ApiKeyAuth>>,
    /// OAuth auth strategy.
    pub oauth: Option<Arc<dyn OAuthAuth>>,
}

impl std::fmt::Debug for ProviderAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderAuth")
            .field("api_key", &self.api_key.as_ref().map(|auth| auth.name()))
            .field("oauth", &self.oauth.as_ref().map(|auth| auth.name()))
            .finish()
    }
}
