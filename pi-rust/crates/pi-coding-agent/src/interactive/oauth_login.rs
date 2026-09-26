//! `/login` OAuth dispatch — port of `handleLoginCommand` + `loginProvider` in
//! `packages/coding-agent/src/modes/interactive/runSlashCommand.ts`.
//!
//! The driver-side [`AuthInteraction`] impl writes OAuth progress events to
//! the TUI's info pane via [`App::info_block`]. Prompts are answered with a
//! conservative default (empty text, first option id) and a one-line
//! "OAuth asks: …" notice is written so the user understands the flow is
//! running unattended; rendering an interactive prompt dialog inside the
//! TUI event loop is a follow-up. The default is enough to drive every
//! flow currently shipped:
//!
//! * `github-copilot` asks for a GitHub Enterprise URL; an empty answer
//!   resolves to `github.com`, which is what the device-code path
//!   expects.
//! * `openai-codex` asks which method to use (browser vs. device code);
//!   the first option (`browser`) is the default and the browser path
//!   works without further prompt input — the `manual_code` prompt is
//!   raced against the callback server and a no-op submit lets the
//!   callback win.
//! * `kimi-coding` never prompts — the device-code path is the only
//!   step.
//!
//! On success the [`OAuthCredential`] is persisted through the
//! [`CredentialStore`] the [`InteractiveOptions`] carries.

use std::sync::Arc;

use async_trait::async_trait;
use pi_ai::auth::oauth::oauth_for;
use pi_ai::auth::{
    AuthError, AuthEvent, AuthInteraction, AuthPrompt, AuthPromptOption, Credential,
    CredentialStore, OAuthAuth, OAuthCredential,
};
use pi_ai::types::AbortSignal;
use pi_tui::app::App;
use tokio::sync::Mutex as AsyncMutex;

#[cfg(not(target_arch = "wasm32"))]
use pi_ai::providers::registry::{OAuthKindSpec, ProviderSpec};

/// Reason a `/login <provider>` invocation settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginOutcome {
    /// Credential was written to the store.
    Stored {
        provider_id: String,
        /// Whether the OAuth credential carries a subscription
        /// (Copilot / ChatGPT subscription flows).
        subscription: bool,
    },
    /// Provider id is not registered as OAuth-first.
    NotOAuthProvider(String),
    /// Provider id is unknown to the registry.
    UnknownProvider(String),
    /// The OAuth flow aborted (`signal` cancelled, browser closed,
    /// device-code expired, etc.).
    Aborted,
    /// The OAuth flow failed with a coded or store error.
    Failed(String),
}

/// Driver-side [`AuthInteraction`] that pipes events to the TUI info
/// pane and answers prompts with a conservative default.
///
/// The full TUI dialog wired into the upstream TypeScript port is a
/// larger follow-up; the default answers are sufficient for every
/// OAuth flow the registry ships today. See the module docs for the
/// per-flow rationale.
pub(crate) struct LoginInteraction {
    /// Per-flow cancellation token. Optional — the OAuth flow treats
    /// `None` as "never cancel".
    signal: Option<AbortSignal>,
    /// Sink for progress events. Held behind an `AsyncMutex` so the
    /// `notify` callback can borrow it across the `&self` reference
    /// without violating `Send + Sync`.
    sink: Arc<AsyncMutex<App>>,
}

impl LoginInteraction {
    pub(crate) fn new(sink: Arc<AsyncMutex<App>>) -> Self {
        Self {
            signal: None,
            sink,
        }
    }

    pub(crate) fn with_signal(sink: Arc<AsyncMutex<App>>, signal: AbortSignal) -> Self {
        Self {
            signal: Some(signal),
            sink,
        }
    }

    fn render_event(event: &AuthEvent) -> String {
        match event {
            AuthEvent::Info { message, links } => {
                let mut body = message.clone();
                if !links.is_empty() {
                    body.push('\n');
                    for link in links {
                        match &link.label {
                            Some(label) => {
                                body.push_str(&format!("  {label}: {}\n", link.url));
                            }
                            None => {
                                body.push_str(&format!("  {}\n", link.url));
                            }
                        }
                    }
                }
                body
            }
            AuthEvent::AuthUrl { url, instructions } => {
                let mut body = format!("Open this URL in your browser:\n  {url}");
                if let Some(extra) = instructions {
                    body.push_str(&format!("\n{extra}"));
                }
                body
            }
            AuthEvent::DeviceCode {
                user_code,
                verification_uri,
                interval_seconds,
                expires_in_seconds,
            } => {
                let mut body = format!(
                    "Device code: {user_code}\nVisit: {verification_uri}\nEnter the code above to authorize."
                );
                if let Some(secs) = interval_seconds {
                    body.push_str(&format!("\nPolling every {secs}s."));
                }
                if let Some(secs) = expires_in_seconds {
                    body.push_str(&format!("\nCode expires in {secs}s."));
                }
                body
            }
            AuthEvent::Progress { message } => format!("… {message}"),
        }
    }
}

#[async_trait]
impl AuthInteraction for LoginInteraction {
    fn signal(&self) -> Option<&AbortSignal> {
        self.signal.as_ref()
    }

    async fn prompt(&self, prompt: AuthPrompt) -> Result<String, AuthError> {
        // Default-answer fallback. The renderer that owns the actual
        // prompt dialog is a follow-up; until then this keeps the flow
        // driveable in unattended contexts (CI smoke tests, scripted
        // logins, …).
        let preview = match &prompt {
            AuthPrompt::Text { message, .. }
            | AuthPrompt::Secret { message, .. }
            | AuthPrompt::ManualCode { message, .. } => message.clone(),
            AuthPrompt::Select { message, .. } => message.clone(),
        };
        {
            let mut sink = self.sink.lock().await;
            sink.info_block(format!(
                "OAuth asks: {preview}\n(no interactive prompt yet — answering with default; cancel with Ctrl+C to abort)"
            ));
        }

        match prompt {
            AuthPrompt::Text { .. } | AuthPrompt::Secret { .. } | AuthPrompt::ManualCode { .. } => {
                Ok(String::new())
            }
            AuthPrompt::Select { options, .. } => options
                .first()
                .map(|opt: &AuthPromptOption| opt.id.clone())
                .ok_or_else(|| AuthError::Store("Select prompt had no options".into())),
        }
    }

    fn notify(&self, event: AuthEvent) {
        // `notify` is sync and the `App` is behind an `AsyncMutex`,
        // so we hand the work to a one-shot tokio task instead of
        // blocking the calling future on the lock. The lock
        // acquisition does not borrow `self` mutably after the call
        // returns, so we can move a clone of `Arc` into the task.
        let body = Self::render_event(&event);
        let sink = self.sink.clone();
        tokio::spawn(async move {
            let mut guard = sink.lock().await;
            guard.info_block(body);
        });
    }
}

/// Run the OAuth login flow for the provider identified by `spec` and
/// persist the resulting credential through `store`.
///
/// Returns the [`LoginOutcome`] describing what happened so the caller
/// can render the user-visible summary line.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn run_oauth_login(
    app: Arc<AsyncMutex<App>>,
    store: Arc<dyn CredentialStore>,
    spec: &ProviderSpec,
) -> LoginOutcome {
    let provider_id = spec.id.to_string();
    let Some(oauth_spec) = spec.oauth.as_ref() else {
        return LoginOutcome::NotOAuthProvider(provider_id);
    };
    let OAuthKindSpec::DeviceCode { interval_ms } = oauth_spec.kind else {
        return LoginOutcome::NotOAuthProvider(provider_id);
    };
    let _ = interval_ms;
    let Some(oauth) = oauth_for(&provider_id, oauth_spec.kind.clone()) else {
        return LoginOutcome::UnknownProvider(provider_id);
    };

    let interaction = LoginInteraction::new(app.clone());
    let credential = match oauth.login(&interaction).await {
        Ok(credential) => credential,
        Err(AuthError::Aborted) => return LoginOutcome::Aborted,
        Err(err) => return LoginOutcome::Failed(format!("{err}")),
    };
    persist_credential(store, &provider_id, credential).await
}

async fn persist_credential(
    store: Arc<dyn CredentialStore>,
    provider_id: &str,
    credential: OAuthCredential,
) -> LoginOutcome {
    let id = provider_id.to_string();
    let stored = store
        .modify(
            provider_id,
            Box::new(move |_current| {
                Box::pin(async move { Ok(Some(Credential::OAuth(credential))) })
            }),
            None,
        )
        .await;
    match stored {
        Ok(Some(_)) => LoginOutcome::Stored {
            provider_id: id,
            subscription: true,
        },
        Ok(None) => {
            LoginOutcome::Failed("credential store rejected the OAuth credential".into())
        }
        Err(err) => LoginOutcome::Failed(format!("credential store: {err}")),
    }
}

/// Build the user-visible summary line for a successful `/login`.
pub(crate) fn describe_outcome(outcome: &LoginOutcome) -> String {
    match outcome {
        LoginOutcome::Stored {
            provider_id,
            subscription,
        } => {
            if *subscription {
                format!(
                    "Credentials saved for {provider_id}. Subscription-based providers may need a model refresh; restart the model selector with /model."
                )
            } else {
                format!("Credentials saved for {provider_id}.")
            }
        }
        LoginOutcome::NotOAuthProvider(provider_id) => format!(
            "/login {provider_id}: provider does not use OAuth — set the API key via the env vars listed in /extensions or in the README."
        ),
        LoginOutcome::UnknownProvider(provider_id) => {
            format!("/login {provider_id}: unknown provider id.")
        }
        LoginOutcome::Aborted => "/login cancelled.".into(),
        LoginOutcome::Failed(message) => format!("/login failed: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::providers::registry::{OAuthSpec, ProviderSpec};

    fn dummy_spec() -> ProviderSpec {
        ProviderSpec {
            id: "github-copilot",
            display_name: "GitHub Copilot",
            api: pi_protocol::Api::OpenAiChatCompletions,
            default_base_url: "https://api.githubcopilot.com",
            api_key_env: &[],
            base_url_env: &[],
            models: &[],
            oauth: Some(OAuthSpec {
                login_label: "Sign in with GitHub",
                auth_url: "https://github.com/login/device",
                kind: OAuthKindSpec::DeviceCode { interval_ms: 5000 },
            }),
        }
    }

    #[test]
    fn describe_outcome_renders_each_variant() {
        let stored = LoginOutcome::Stored {
            provider_id: "github-copilot".into(),
            subscription: true,
        };
        assert!(describe_outcome(&stored).contains("github-copilot"));
        assert!(describe_outcome(&LoginOutcome::Aborted).contains("cancelled"));
        assert!(describe_outcome(&LoginOutcome::UnknownProvider("nope".into())).contains("unknown"));
        assert!(describe_outcome(&LoginOutcome::NotOAuthProvider("anthropic".into())).contains("does not use OAuth"));
        assert!(describe_outcome(&LoginOutcome::Failed("nope".into())).contains("nope"));
    }

    #[test]
    fn render_event_device_code_includes_user_code_and_uri() {
        let event = AuthEvent::DeviceCode {
            user_code: "ABCD-1234".into(),
            verification_uri: "https://github.com/login/device".into(),
            interval_seconds: Some(5),
            expires_in_seconds: Some(900),
        };
        let body = LoginInteraction::render_event(&event);
        assert!(body.contains("ABCD-1234"));
        assert!(body.contains("https://github.com/login/device"));
        assert!(body.contains("Polling every 5s"));
        assert!(body.contains("Code expires in 900s"));
    }

    #[test]
    fn render_event_auth_url_includes_instructions() {
        let event = AuthEvent::AuthUrl {
            url: "https://auth.openai.com/...".into(),
            instructions: Some("Sign in with your ChatGPT account".into()),
        };
        let body = LoginInteraction::render_event(&event);
        assert!(body.contains("https://auth.openai.com/..."));
        assert!(body.contains("Sign in with your ChatGPT account"));
    }

    #[test]
    fn render_event_info_includes_links() {
        let event = AuthEvent::Info {
            message: "Copilot needs an OAuth token".into(),
            links: vec![pi_ai::auth::AuthInfoLink {
                url: "https://github.com/settings/tokens".into(),
                label: Some("Tokens".into()),
            }],
        };
        let body = LoginInteraction::render_event(&event);
        assert!(body.contains("Copilot needs an OAuth token"));
        assert!(body.contains("https://github.com/settings/tokens"));
    }

    #[test]
    fn dummy_spec_has_device_code_oauth() {
        let spec = dummy_spec();
        assert!(spec.oauth.is_some());
        assert!(matches!(
            spec.oauth.unwrap().kind,
            OAuthKindSpec::DeviceCode { interval_ms: 5000 }
        ));
    }
}
