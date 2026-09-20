//! Tests for the provider-auth registry bridge
//! (`crates/pi-ai/src/auth/provider_registry.rs`).
//!
//! The bridge wires the static provider registry's `api_key_env` table into
//! the auth subsystem and exposes the api-key resolution entry point the
//! `pi-coding-agent` router consumes. These vectors pin the table's coverage
//! (every keyed provider, nothing else) and the resolution policy
//! (stored credential first, scoped env second) without touching the network
//! or the process environment.

use std::collections::BTreeMap;

use async_trait::async_trait;
use pi_ai::auth::resolve::AuthResolutionOverrides;
use pi_ai::auth::types::CredentialModifyFn;
use pi_ai::auth::{provider_auth_for, resolve_api_key_for_provider};
use pi_ai::providers::registry::BUILTIN_PROVIDERS;
use pi_ai::{
    ApiKeyCredential, AuthContext, Credential, CredentialStore, InMemoryCredentialStore,
    ProviderEnv,
};

/// Environment/file context backed by an explicit map (no process env), so a
/// host environment variable can never influence an assertion.
struct FakeContext {
    values: ProviderEnv,
}

impl FakeContext {
    fn new(pairs: &[(&str, &str)]) -> Self {
        Self {
            values: pairs
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
        }
    }

    fn empty() -> Self {
        Self {
            values: BTreeMap::new(),
        }
    }
}

#[async_trait]
impl AuthContext for FakeContext {
    async fn env(&self, name: &str) -> Option<String> {
        self.values
            .get(name)
            .filter(|value| !value.is_empty())
            .cloned()
    }

    async fn file_exists(&self, _path: &str) -> bool {
        false
    }
}

fn scoped_env(pairs: &[(&str, &str)]) -> Option<AuthResolutionOverrides> {
    Some(AuthResolutionOverrides {
        env: Some(
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
        ),
        ..AuthResolutionOverrides::default()
    })
}

async fn put(store: &InMemoryCredentialStore, provider_id: &str, credential: Credential) {
    let modify: CredentialModifyFn = Box::new(move |_current| {
        let credential = credential.clone();
        Box::pin(async move { Ok(Some(credential)) })
    });
    store
        .modify(provider_id, modify, None)
        .await
        .expect("seeding the in-memory store must not fail");
}

fn api_key(key: &str) -> Credential {
    Credential::ApiKey(ApiKeyCredential {
        key: Some(key.to_string()),
        env: None,
    })
}

#[tokio::test]
async fn the_registry_covers_exactly_the_keyed_providers() {
    for spec in BUILTIN_PROVIDERS {
        let registered = provider_auth_for(spec.id);
        if spec.api_key_env.is_empty() {
            assert!(
                registered.is_none(),
                "keyless provider `{}` must not be registered",
                spec.id
            );
        } else {
            let auth = registered
                .unwrap_or_else(|| panic!("keyed provider `{}` must be registered", spec.id));
            let api_key_auth = auth
                .api_key
                .as_ref()
                .unwrap_or_else(|| panic!("provider `{}` must have api-key auth", spec.id));
            assert_eq!(api_key_auth.name(), spec.display_name);
            assert!(auth.oauth.is_none(), "OAuth is not wired this round");
        }
    }

    // The examples the issue calls out, plus the deliberate OAuth-only gap.
    assert!(provider_auth_for("anthropic").is_some());
    assert!(provider_auth_for("minimax").is_some());
    assert!(provider_auth_for("vercel-ai-gateway").is_some());
    assert!(provider_auth_for("faux").is_none());
    assert!(provider_auth_for("github-copilot").is_none());
    assert!(provider_auth_for("openai-codex").is_none());
    assert!(provider_auth_for("kimi-coding").is_none());
    assert!(provider_auth_for("not-a-provider").is_none());
}

#[tokio::test]
async fn a_stored_api_key_wins_over_the_scoped_env() {
    let store = InMemoryCredentialStore::new();
    put(&store, "anthropic", api_key("stored-key")).await;

    let resolved = resolve_api_key_for_provider(
        "anthropic",
        &store,
        &FakeContext::empty(),
        scoped_env(&[("ANTHROPIC_API_KEY", "env-key")]),
    )
    .await
    .expect("resolution must not fail")
    .expect("the stored credential must resolve");

    assert_eq!(resolved.key.as_deref(), Some("stored-key"));
}

#[tokio::test]
async fn without_a_stored_credential_the_scoped_env_resolves() {
    let store = InMemoryCredentialStore::new();
    let resolved = resolve_api_key_for_provider(
        "anthropic",
        &store,
        &FakeContext::empty(),
        scoped_env(&[("ANTHROPIC_API_KEY", "env-key")]),
    )
    .await
    .expect("resolution must not fail")
    .expect("the env var must resolve");

    assert_eq!(resolved.key.as_deref(), Some("env-key"));
}

#[tokio::test]
async fn the_registry_env_priority_order_is_preserved() {
    // `ANTHROPIC_API_KEY` outranks the bearer-only token, exactly as the
    // registry lists them.
    let store = InMemoryCredentialStore::new();
    let resolved = resolve_api_key_for_provider(
        "anthropic",
        &store,
        &FakeContext::empty(),
        scoped_env(&[
            ("ANTHROPIC_OAUTH_TOKEN", "oauth-token"),
            ("ANTHROPIC_API_KEY", "env-key"),
        ]),
    )
    .await
    .expect("resolution must not fail")
    .expect("a key must resolve");

    assert_eq!(resolved.key.as_deref(), Some("env-key"));
}

#[tokio::test]
async fn provider_scoped_env_from_the_credential_is_preserved() {
    let store = InMemoryCredentialStore::new();
    put(
        &store,
        "anthropic",
        Credential::ApiKey(ApiKeyCredential {
            key: Some("stored-key".to_string()),
            env: Some(BTreeMap::from([("REGION".to_string(), "eu".to_string())])),
        }),
    )
    .await;

    let resolved = resolve_api_key_for_provider("anthropic", &store, &FakeContext::empty(), None)
        .await
        .expect("resolution must not fail")
        .expect("the stored credential must resolve");

    assert_eq!(resolved.key.as_deref(), Some("stored-key"));
    assert_eq!(
        resolved.env.as_ref().and_then(|env| env.get("REGION")),
        Some(&"eu".to_string())
    );
}

#[tokio::test]
async fn nothing_configured_resolves_to_none() {
    let store = InMemoryCredentialStore::new();
    for provider in ["anthropic", "minimax", "vercel-ai-gateway"] {
        let resolved =
            resolve_api_key_for_provider(provider, &store, &FakeContext::empty(), scoped_env(&[]))
                .await
                .expect("resolution must not fail");
        assert!(resolved.is_none(), "`{provider}` must be unconfigured");
    }
}

#[tokio::test]
async fn an_unknown_or_keyless_provider_resolves_to_none() {
    let store = InMemoryCredentialStore::new();
    // Even a stored credential does not invent an auth strategy.
    put(&store, "github-copilot", api_key("stored-key")).await;
    for provider in ["github-copilot", "faux", "not-a-provider"] {
        let resolved = resolve_api_key_for_provider(
            provider,
            &store,
            &FakeContext::empty(),
            scoped_env(&[("ANTHROPIC_API_KEY", "env-key")]),
        )
        .await
        .expect("resolution must not fail");
        assert!(resolved.is_none(), "`{provider}` has no api-key strategy");
    }
}

#[tokio::test]
async fn an_ambient_auth_context_still_backs_the_scoped_env() {
    // A caller that supplies a real context (the router's process-backed one)
    // gets ambient lookup for the vars the scoped overrides do not carry.
    let store = InMemoryCredentialStore::new();
    let context = FakeContext::new(&[("GEMINI_API_KEY", "ambient-key")]);
    let resolved = resolve_api_key_for_provider(
        "google",
        &store,
        &context,
        scoped_env(&[("GOOGLE_API_KEY", "")]),
    )
    .await
    .expect("resolution must not fail")
    .expect("the ambient key must resolve");

    assert_eq!(resolved.key.as_deref(), Some("ambient-key"));
}
