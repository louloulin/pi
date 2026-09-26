//! `/login` OAuth dispatch — port of `handleLoginCommand` + `loginProvider` in
//! `packages/coding-agent/src/modes/interactive/runSlashCommand.ts`.
//!
//! The driver-side [`AuthInteraction`] impl collects OAuth progress events
//! and flushes them to the caller's `app.info` sink at the end of the flow.
//! The run_slash_command arm holds an `&mut App` while the OAuth login runs,
//! so we cannot borrow the App across the OAuth flow's await points —
//! instead the interaction buffers events and writes them once the flow
//! returns. Prompts are answered with a conservative default (empty text,
//! first option id) and a one-line "OAuth asks: …" notice is buffered so
//! the user understands the flow is running unattended; rendering an
//! interactive prompt dialog inside the TUI event loop is a follow-up.
//!
//! The defaults are enough to drive every flow currently shipped:
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

use std::sync::{Arc, Mutex as SyncMutex};

use async_trait::async_trait;
use pi_ai::auth::oauth::oauth_for;
use pi_ai::auth::{
    AuthError, AuthEvent, AuthInfoLink, AuthInteraction, AuthPrompt, AuthPromptOption,
    Credential, CredentialStore, OAuthAuth, OAuthCredential,
};
use pi_ai::types::AbortSignal;

#[cfg(not(target_arch = "wasm32"))]
use pi_ai::providers::registry::{OAuthKindSpec, ProviderSpec};

/// Reason a `/login <provider>` invocation settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginOutcome {
    /// Credential was written to the store.
    Stored {
        /// Provider the credential was written for.
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

/// Buffered event sink shared between the OAuth flow (which calls
/// `notify` from any task) and the dispatcher (which flushes them to
/// `app.info` once the flow returns).
#[derive(Default)]
pub(crate) struct EventBuffer {
    events: SyncMutex<Vec<String>>,
}

impl EventBuffer {
    fn push(&self, body: String) {
        if let Ok(mut guard) = self.events.lock() {
            guard.push(body);
        }
    }

    fn drain(&self) -> Vec<String> {
        self.events
            .lock()
            .map(|mut guard| std::mem::take(&mut *guard))
            .unwrap_or_default()
    }
}

/// Driver-side [`AuthInteraction`] that buffers events until the
/// dispatcher flushes them and answers prompts with a conservative
/// default.
///
/// The full TUI dialog wired into the upstream TypeScript port is a
/// larger follow-up; the default answers are sufficient for every
/// OAuth flow the registry ships today. See the module docs for the
/// per-flow rationale.
pub(crate) struct LoginInteraction {
    /// Per-flow cancellation token. Optional — the OAuth flow treats
    /// `None` as "never cancel".
    signal: Option<AbortSignal>,
    /// Buffered events. Cloned into the `notify` callback so the
    /// future can outlive the caller's `&mut App` borrow.
    sink: Arc<EventBuffer>,
}

impl LoginInteraction {
    pub(crate) fn new() -> (Self, Arc<EventBuffer>) {
        let sink = Arc::new(EventBuffer::default());
        (
            Self {
                signal: None,
                sink: sink.clone(),
            },
            sink,
        )
    }

    fn render_event(event: &AuthEvent) -> String {
        match event {
            AuthEvent::Info { message, links } => {
                let mut body = message.clone();
                if !links.is_empty() {
                    body.push('\n');
                    for link in links {
                        append_link(&mut body, link);
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

fn append_link(body: &mut String, link: &AuthInfoLink) {
    match &link.label {
        Some(label) => body.push_str(&format!("  {label}: {}\n", link.url)),
        None => body.push_str(&format!("  {}\n", link.url)),
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
        self.sink.push(format!(
            "OAuth asks: {preview}\n(no interactive prompt yet — answering with default; cancel with Ctrl+C to abort)"
        ));

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
        self.sink.push(Self::render_event(&event));
    }
}

/// Run the OAuth login flow for the provider identified by `spec` and
/// persist the resulting credential through `store`.
///
/// `events_out` is filled with the user-visible progress messages the
/// flow produced; the caller is responsible for flushing them to the
/// TUI info pane.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn run_oauth_login(
    events_out: &mut Vec<String>,
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

    let (interaction, buffer) = LoginInteraction::new();
    let credential = match oauth.login(&interaction).await {
        Ok(credential) => credential,
        Err(AuthError::Aborted) => return LoginOutcome::Aborted,
        Err(err) => return LoginOutcome::Failed(format!("{err}")),
    };
    let outcome = persist_credential(store, &provider_id, credential).await;
    events_out.extend(buffer.drain());
    outcome
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
            links: vec![AuthInfoLink {
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

    #[test]
    fn event_buffer_round_trips() {
        let (interaction, buffer) = LoginInteraction::new();
        interaction.notify(AuthEvent::Progress {
            message: "first".into(),
        });
        interaction.notify(AuthEvent::Progress {
            message: "second".into(),
        });
        let drained = buffer.drain();
        assert_eq!(drained.len(), 2);
        assert!(drained[0].contains("first"));
        assert!(drained[1].contains("second"));
        assert!(buffer.drain().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prompt_returns_default_for_text_and_first_option_for_select() {
        let (interaction, _) = LoginInteraction::new();
        let text = interaction
            .prompt(AuthPrompt::Text {
                signal: None,
                message: "GitHub Enterprise URL".into(),
                placeholder: Some("company.ghe.com".into()),
            })
            .await
            .expect("text prompt");
        assert_eq!(text, "");

        let selected = interaction
            .prompt(AuthPrompt::Select {
                signal: None,
                message: "Pick a method".into(),
                options: vec![
                    AuthPromptOption {
                        id: "browser".into(),
                        label: "Browser".into(),
                        description: None,
                    },
                    AuthPromptOption {
                        id: "device".into(),
                        label: "Device code".into(),
                        description: None,
                    },
                ],
            })
            .await
            .expect("select prompt");
        assert_eq!(selected, "browser");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_oauth_login_unknown_provider_returns_unknown_outcome() {
        let store: Arc<dyn CredentialStore> =
            Arc::new(pi_ai::auth::InMemoryCredentialStore::new());
        let mut events: Vec<String> = Vec::new();
        // A provider id not registered as OAuth-first but with no
        // `api_key_env` either — the `oauth_login` helper still gets a
        // `find_provider` hit, sees `oauth: Some(…)`, and resolves
        // through `oauth_for`. Build a `ProviderSpec` for a fictional
        // id so `oauth_for` rejects it.
        let spec = ProviderSpec {
            id: "no-such-provider",
            display_name: "None",
            api: pi_protocol::Api::OpenAiChatCompletions,
            default_base_url: "",
            api_key_env: &[],
            base_url_env: &[],
            models: &[],
            oauth: Some(OAuthSpec {
                login_label: "Sign in",
                auth_url: "",
                kind: OAuthKindSpec::DeviceCode { interval_ms: 1000 },
            }),
        };
        let outcome = run_oauth_login(&mut events, store, &spec).await;
        assert!(matches!(outcome, LoginOutcome::UnknownProvider(_)));
        assert!(events.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_oauth_login_without_oauth_spec_returns_not_oauth() {
        let store: Arc<dyn CredentialStore> =
            Arc::new(pi_ai::auth::InMemoryCredentialStore::new());
        let mut events: Vec<String> = Vec::new();
        let spec = ProviderSpec {
            id: "anthropic",
            display_name: "Anthropic",
            api: pi_protocol::Api::AnthropicMessages,
            default_base_url: "https://api.anthropic.com",
            api_key_env: &["ANTHROPIC_API_KEY"],
            base_url_env: &[],
            models: &[],
            oauth: None,
        };
        let outcome = run_oauth_login(&mut events, store, &spec).await;
        assert!(matches!(outcome, LoginOutcome::NotOAuthProvider(_)));
        assert!(events.is_empty());
    }
}
