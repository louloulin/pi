//! Tests for the ported auth subsystem
//! (`packages/ai/src/auth/*.ts` + `packages/ai/src/env-api-keys.ts`).
//!
//! The vectors pin the resolution policy and the in-memory store semantics:
//! stored-credential precedence, env fallback, OAuth refresh under the store
//! lock, the "failed refresh never falls back to env" invariant, per-provider
//! write serialization and the registry-backed env lookups.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use pi_ai::auth::resolve::AuthResolutionOverrides;
use pi_ai::auth::types::{
    ApiKeyAuth, ApiKeyCredential, ApiKeyResolveInput, AuthContext, AuthOperationOptions,
    AuthResult, Credential, CredentialModifyFn, CredentialStore, ModelAuth, OAuthAuth,
    OAuthCredential, ProviderAuth, ProviderEnv,
};
use pi_ai::auth::{
    env_api_key_auth, resolve_provider_auth, AuthError, InMemoryCredentialStore, ModelsErrorCode,
};
use pi_ai::env_api_keys::{find_env_keys, get_env_api_key};
use pi_ai::types::AbortSignal;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn env(pairs: &[(&str, &str)]) -> ProviderEnv {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

fn api_key(key: &str) -> Credential {
    Credential::ApiKey(ApiKeyCredential {
        key: Some(key.to_string()),
        env: None,
    })
}

fn oauth(access: &str, expires: i64) -> OAuthCredential {
    OAuthCredential {
        refresh: "refresh-token".to_string(),
        access: access.to_string(),
        expires,
        extra: BTreeMap::new(),
    }
}

/// Environment/file context backed by an explicit map (no process env).
struct FakeContext {
    values: ProviderEnv,
}

impl FakeContext {
    fn new(pairs: &[(&str, &str)]) -> Self {
        Self { values: env(pairs) }
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

/// OAuth strategy with counters so tests can observe refresh calls.
struct FakeOAuth {
    refreshes: AtomicUsize,
    fail_refresh: bool,
    refreshed_access: String,
    refreshed_expires_ms: i64,
    derived_key: String,
}

impl FakeOAuth {
    fn new(derived_key: &str) -> Self {
        Self {
            refreshes: AtomicUsize::new(0),
            fail_refresh: false,
            refreshed_access: "refreshed-access".to_string(),
            refreshed_expires_ms: 60 * 60 * 1000,
            derived_key: derived_key.to_string(),
        }
    }

    fn failing() -> Self {
        Self {
            fail_refresh: true,
            ..Self::new("unused")
        }
    }

    fn refresh_count(&self) -> usize {
        self.refreshes.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl OAuthAuth for FakeOAuth {
    fn name(&self) -> &str {
        "Fake OAuth"
    }

    async fn login(
        &self,
        _interaction: &dyn pi_ai::auth::types::AuthInteraction,
    ) -> Result<OAuthCredential, AuthError> {
        Err(AuthError::Store(
            "login is not exercised by these tests".into(),
        ))
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        _signal: &AbortSignal,
    ) -> Result<OAuthCredential, AuthError> {
        self.refreshes.fetch_add(1, Ordering::SeqCst);
        if self.fail_refresh {
            return Err(AuthError::Store("invalid_grant".into()));
        }
        Ok(OAuthCredential {
            refresh: format!("{}-rotated", credential.refresh),
            access: self.refreshed_access.clone(),
            expires: now_ms() + self.refreshed_expires_ms,
            extra: BTreeMap::new(),
        })
    }

    async fn to_auth(&self, _credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth {
            api_key: Some(self.derived_key.clone()),
            ..ModelAuth::default()
        })
    }
}

/// Api-key strategy that always fails, to check error wrapping.
struct FailingApiKeyAuth;

#[async_trait]
impl ApiKeyAuth for FailingApiKeyAuth {
    fn name(&self) -> &str {
        "Failing"
    }

    async fn resolve(
        &self,
        _input: ApiKeyResolveInput<'_>,
    ) -> Result<Option<AuthResult>, AuthError> {
        Err(AuthError::Store("boom".into()))
    }
}

async fn put(store: &InMemoryCredentialStore, provider_id: &str, credential: Credential) {
    let modify: CredentialModifyFn =
        Box::new(move |_| Box::pin(async move { Ok(Some(credential)) }));
    store.modify(provider_id, modify, None).await.unwrap();
}

fn provider_auth(
    api_key: Option<Arc<dyn ApiKeyAuth>>,
    oauth: Option<Arc<dyn OAuthAuth>>,
) -> ProviderAuth {
    ProviderAuth { api_key, oauth }
}

// ---------------------------------------------------------------------------
// Resolution matrix
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_credential_and_no_env_resolves_to_none() {
    let store = InMemoryCredentialStore::new();
    let context = FakeContext::empty();
    let auth = provider_auth(
        Some(env_api_key_auth("Test", vec!["TEST_API_KEY".into()])),
        None,
    );

    let resolved = resolve_provider_auth("test", &auth, &store, &context, None)
        .await
        .unwrap();
    assert!(resolved.is_none());
}

#[tokio::test]
async fn stored_api_key_wins_over_env() {
    let store = InMemoryCredentialStore::new();
    put(&store, "test", api_key("stored-key")).await;
    let context = FakeContext::new(&[("TEST_API_KEY", "env-key")]);
    let auth = provider_auth(
        Some(env_api_key_auth("Test", vec!["TEST_API_KEY".into()])),
        None,
    );

    let resolved = resolve_provider_auth("test", &auth, &store, &context, None)
        .await
        .unwrap()
        .expect("stored credential resolves");
    assert_eq!(resolved.auth.api_key.as_deref(), Some("stored-key"));
    assert_eq!(resolved.source.as_deref(), Some("stored credential"));
}

#[tokio::test]
async fn env_is_used_only_when_nothing_is_stored() {
    let store = InMemoryCredentialStore::new();
    let context = FakeContext::new(&[("TEST_API_KEY", "env-key")]);
    let auth = provider_auth(
        Some(env_api_key_auth("Test", vec!["TEST_API_KEY".into()])),
        None,
    );

    let resolved = resolve_provider_auth("test", &auth, &store, &context, None)
        .await
        .unwrap()
        .expect("env credential resolves");
    assert_eq!(resolved.auth.api_key.as_deref(), Some("env-key"));
    assert_eq!(resolved.source.as_deref(), Some("TEST_API_KEY"));
}

#[tokio::test]
async fn env_vars_resolve_in_priority_order() {
    let store = InMemoryCredentialStore::new();
    let auth = provider_auth(
        Some(env_api_key_auth(
            "Test",
            vec!["FIRST_KEY".into(), "SECOND_KEY".into()],
        )),
        None,
    );

    let both = FakeContext::new(&[("FIRST_KEY", "one"), ("SECOND_KEY", "two")]);
    let resolved = resolve_provider_auth("test", &auth, &store, &both, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.auth.api_key.as_deref(), Some("one"));
    assert_eq!(resolved.source.as_deref(), Some("FIRST_KEY"));

    let only_second = FakeContext::new(&[("SECOND_KEY", "two")]);
    let resolved = resolve_provider_auth("test", &auth, &store, &only_second, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.auth.api_key.as_deref(), Some("two"));
    assert_eq!(resolved.source.as_deref(), Some("SECOND_KEY"));
}

#[tokio::test]
async fn override_api_key_beats_stored_credential() {
    let store = InMemoryCredentialStore::new();
    put(&store, "test", api_key("stored-key")).await;
    let context = FakeContext::empty();
    let auth = provider_auth(
        Some(env_api_key_auth("Test", vec!["TEST_API_KEY".into()])),
        None,
    );
    let overrides = AuthResolutionOverrides {
        api_key: Some("override-key".into()),
        ..AuthResolutionOverrides::default()
    };

    let resolved = resolve_provider_auth("test", &auth, &store, &context, Some(overrides))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.auth.api_key.as_deref(), Some("override-key"));
}

#[tokio::test]
async fn override_env_is_merged_into_a_stored_api_key_credential() {
    let store = InMemoryCredentialStore::new();
    put(&store, "test", api_key("stored-key")).await;
    let context = FakeContext::empty();
    let auth = provider_auth(
        Some(env_api_key_auth("Test", vec!["TEST_API_KEY".into()])),
        None,
    );
    let overrides = AuthResolutionOverrides {
        env: Some(env(&[("ACCOUNT_ID", "acct-1")])),
        ..AuthResolutionOverrides::default()
    };

    let resolved = resolve_provider_auth("test", &auth, &store, &context, Some(overrides))
        .await
        .unwrap()
        .unwrap();
    let resolved_env = resolved.env.expect("env surfaces on the result");
    assert_eq!(
        resolved_env.get("ACCOUNT_ID").map(String::as_str),
        Some("acct-1")
    );
}

// ---------------------------------------------------------------------------
// OAuth resolution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn valid_oauth_token_is_used_without_refreshing() {
    let store = InMemoryCredentialStore::new();
    put(
        &store,
        "test",
        Credential::OAuth(oauth("live-access", now_ms() + 60 * 60 * 1000)),
    )
    .await;
    let context = FakeContext::empty();
    let fake = Arc::new(FakeOAuth::new("derived-key"));
    let auth = provider_auth(None, Some(fake.clone()));

    let resolved = resolve_provider_auth("test", &auth, &store, &context, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.auth.api_key.as_deref(), Some("derived-key"));
    assert_eq!(resolved.source.as_deref(), Some("OAuth"));
    assert_eq!(fake.refresh_count(), 0);
}

#[tokio::test]
async fn expired_oauth_token_is_refreshed_and_persisted() {
    let store = InMemoryCredentialStore::new();
    put(
        &store,
        "test",
        Credential::OAuth(oauth("stale-access", now_ms() - 1_000)),
    )
    .await;
    let context = FakeContext::empty();
    let fake = Arc::new(FakeOAuth::new("derived-key"));
    let auth = provider_auth(None, Some(fake.clone()));

    let resolved = resolve_provider_auth("test", &auth, &store, &context, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.auth.api_key.as_deref(), Some("derived-key"));
    assert_eq!(fake.refresh_count(), 1);

    let stored = store.read("test", None).await.unwrap().unwrap();
    let stored = stored.as_oauth().expect("still an OAuth credential");
    assert_eq!(stored.access, "refreshed-access");
    assert_eq!(stored.refresh, "refresh-token-rotated");
}

#[tokio::test]
async fn failed_refresh_does_not_fall_back_to_env_and_codes_oauth() {
    let store = InMemoryCredentialStore::new();
    put(
        &store,
        "test",
        Credential::OAuth(oauth("stale-access", now_ms() - 1_000)),
    )
    .await;
    // The ambient api key is present and would resolve if the code wrongly
    // fell back to env after the refresh failure.
    let context = FakeContext::new(&[("TEST_API_KEY", "env-key")]);
    let fake = Arc::new(FakeOAuth::failing());
    let auth = provider_auth(
        Some(env_api_key_auth("Test", vec!["TEST_API_KEY".into()])),
        Some(fake.clone()),
    );

    let error = resolve_provider_auth("test", &auth, &store, &context, None)
        .await
        .expect_err("refresh failure propagates");
    match error {
        AuthError::Models(models) => {
            assert_eq!(models.code(), ModelsErrorCode::OAuth);
            assert!(models.message().contains("OAuth refresh failed for test"));
        }
        other => panic!("expected a coded OAuth error, got {other:?}"),
    }
    assert_eq!(fake.refresh_count(), 1);

    // The stale credential is untouched: no silent downgrade to env.
    let stored = store.read("test", None).await.unwrap().unwrap();
    assert_eq!(stored.as_oauth().unwrap().access, "stale-access");
}

#[tokio::test]
async fn refresh_short_of_explicit_minimum_validity_is_rejected() {
    let store = InMemoryCredentialStore::new();
    put(
        &store,
        "test",
        Credential::OAuth(oauth("stale-access", now_ms() - 1_000)),
    )
    .await;
    let context = FakeContext::empty();
    let fake = Arc::new(FakeOAuth {
        refreshed_expires_ms: 10 * 60 * 1000,
        ..FakeOAuth::new("derived-key")
    });
    let auth = provider_auth(None, Some(fake.clone()));
    let overrides = AuthResolutionOverrides {
        min_oauth_validity_ms: Some(60 * 60 * 1000),
        ..AuthResolutionOverrides::default()
    };

    let error = resolve_provider_auth("test", &auth, &store, &context, Some(overrides))
        .await
        .expect_err("refreshed token expires too soon");
    match error {
        AuthError::Models(models) => assert_eq!(models.code(), ModelsErrorCode::OAuth),
        other => panic!("expected a coded OAuth error, got {other:?}"),
    }
}

#[tokio::test]
async fn stored_oauth_without_an_oauth_handler_does_not_fall_back_to_env() {
    let store = InMemoryCredentialStore::new();
    put(
        &store,
        "test",
        Credential::OAuth(oauth("live-access", now_ms() + 60 * 60 * 1000)),
    )
    .await;
    let context = FakeContext::new(&[("TEST_API_KEY", "env-key")]);
    let auth = provider_auth(
        Some(env_api_key_auth("Test", vec!["TEST_API_KEY".into()])),
        None,
    );

    let resolved = resolve_provider_auth("test", &auth, &store, &context, None)
        .await
        .unwrap();
    assert!(resolved.is_none());
}

#[tokio::test]
async fn stored_api_key_without_an_api_key_handler_does_not_fall_back_to_env() {
    let store = InMemoryCredentialStore::new();
    put(&store, "test", api_key("stored-key")).await;
    let context = FakeContext::empty();
    let fake = Arc::new(FakeOAuth::new("derived-key"));
    let auth = provider_auth(None, Some(fake));

    let resolved = resolve_provider_auth("test", &auth, &store, &context, None)
        .await
        .unwrap();
    assert!(resolved.is_none());
}

#[tokio::test]
async fn api_key_resolution_failures_are_coded_auth() {
    let store = InMemoryCredentialStore::new();
    let context = FakeContext::empty();
    let auth = provider_auth(Some(Arc::new(FailingApiKeyAuth)), None);

    let error = resolve_provider_auth("test", &auth, &store, &context, None)
        .await
        .expect_err("api key failure propagates");
    match error {
        AuthError::Models(models) => {
            assert_eq!(models.code(), ModelsErrorCode::Auth);
            assert!(models
                .message()
                .contains("API key auth failed for provider test"));
            // `withCauseDetail` folds the underlying reason into the message.
            assert!(models.message().contains("boom"));
        }
        other => panic!("expected a coded auth error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// In-memory credential store
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn modify_is_serialized_per_provider() {
    let store = Arc::new(InMemoryCredentialStore::new());
    let in_critical = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for index in 0..4usize {
        let store = store.clone();
        let in_critical = in_critical.clone();
        let max_concurrent = max_concurrent.clone();
        handles.push(tokio::spawn(async move {
            let modify: CredentialModifyFn = Box::new(move |_| {
                let in_critical = in_critical.clone();
                let max_concurrent = max_concurrent.clone();
                Box::pin(async move {
                    let concurrent = in_critical.fetch_add(1, Ordering::SeqCst) + 1;
                    max_concurrent.fetch_max(concurrent, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    in_critical.fetch_sub(1, Ordering::SeqCst);
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some(format!("key-{index}")),
                        env: None,
                    })))
                })
            });
            store.modify("test", modify, None).await.unwrap();
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }

    assert_eq!(
        max_concurrent.load(Ordering::SeqCst),
        1,
        "same-provider modifies must not overlap"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn modify_runs_concurrently_across_providers() {
    let store = Arc::new(InMemoryCredentialStore::new());
    let in_critical = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for provider in ["alpha", "beta"] {
        let store = store.clone();
        let in_critical = in_critical.clone();
        let max_concurrent = max_concurrent.clone();
        handles.push(tokio::spawn(async move {
            let modify: CredentialModifyFn = Box::new(move |_| {
                let in_critical = in_critical.clone();
                let max_concurrent = max_concurrent.clone();
                Box::pin(async move {
                    let concurrent = in_critical.fetch_add(1, Ordering::SeqCst) + 1;
                    max_concurrent.fetch_max(concurrent, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    in_critical.fetch_sub(1, Ordering::SeqCst);
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some(provider.to_string()),
                        env: None,
                    })))
                })
            });
            store.modify(provider, modify, None).await.unwrap();
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }

    assert_eq!(
        max_concurrent.load(Ordering::SeqCst),
        2,
        "different providers must be able to run concurrently"
    );
}

#[tokio::test]
async fn modify_returning_none_leaves_the_entry_and_yields_it() {
    let store = InMemoryCredentialStore::new();
    put(&store, "test", api_key("initial")).await;

    let modify: CredentialModifyFn = Box::new(|_| Box::pin(async { Ok(None) }));
    let current = store.modify("test", modify, None).await.unwrap();
    assert_eq!(
        current
            .as_ref()
            .and_then(Credential::as_api_key)
            .and_then(|c| c.key.as_deref()),
        Some("initial")
    );

    let stored = store.read("test", None).await.unwrap().unwrap();
    assert_eq!(
        stored.as_api_key().and_then(|c| c.key.as_deref()),
        Some("initial")
    );
}

#[tokio::test]
async fn list_reports_metadata_without_secrets() {
    let store = InMemoryCredentialStore::new();
    put(&store, "alpha", api_key("secret-a")).await;
    put(
        &store,
        "beta",
        Credential::OAuth(oauth("access-b", now_ms() + 10_000)),
    )
    .await;

    let mut listed = store.list(None).await.unwrap();
    listed.sort_by(|left, right| left.provider_id.cmp(&right.provider_id));
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].provider_id, "alpha");
    assert_eq!(listed[0].r#type, pi_ai::auth::AuthType::ApiKey);
    assert_eq!(listed[1].provider_id, "beta");
    assert_eq!(listed[1].r#type, pi_ai::auth::AuthType::OAuth);
}

#[tokio::test]
async fn delete_removes_the_credential() {
    let store = InMemoryCredentialStore::new();
    put(&store, "test", api_key("secret")).await;
    store.delete("test", None).await.unwrap();
    assert!(store.read("test", None).await.unwrap().is_none());
    assert!(store.list(None).await.unwrap().is_empty());
}

#[tokio::test]
async fn aborted_operations_fail_with_aborted() {
    let store = InMemoryCredentialStore::new();
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let options = Some(AuthOperationOptions {
        signal: Some(AbortSignal::native(token)),
    });

    let error = store
        .read("test", options.clone())
        .await
        .expect_err("aborted");
    assert!(matches!(error, AuthError::Aborted));
    let error = store.list(options).await.expect_err("aborted");
    assert!(matches!(error, AuthError::Aborted));
}

#[tokio::test]
async fn credentials_round_trip_through_serde() {
    let credential = Credential::ApiKey(ApiKeyCredential {
        key: Some("secret".into()),
        env: Some(env(&[("ACCOUNT_ID", "acct-1")])),
    });
    let json = serde_json::to_string(&credential).unwrap();
    let decoded: Credential = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, credential);

    let mut extra = BTreeMap::new();
    extra.insert("account_id".to_string(), serde_json::json!("acct-1"));
    let credential = Credential::OAuth(OAuthCredential {
        refresh: "r".into(),
        access: "a".into(),
        expires: 42,
        extra,
    });
    let json = serde_json::to_string(&credential).unwrap();
    let decoded: Credential = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, credential);
}

// ---------------------------------------------------------------------------
// env-api-keys (registry-backed)
// ---------------------------------------------------------------------------

#[test]
fn find_env_keys_reports_registry_vars_that_are_set() {
    let values = env(&[("DEEPSEEK_API_KEY", "key")]);
    assert_eq!(
        find_env_keys("deepseek", Some(&values)),
        Some(vec!["DEEPSEEK_API_KEY".to_string()])
    );

    // faux has no credential env vars, and unknown providers have no entry.
    assert_eq!(find_env_keys("faux", None), None);
    assert_eq!(find_env_keys("not-a-provider", None), None);
}

#[test]
fn find_env_keys_preserves_registry_priority_order() {
    let values = env(&[
        ("GEMINI_API_KEY", "primary"),
        ("GOOGLE_API_KEY", "secondary"),
    ]);
    assert_eq!(
        find_env_keys("google", Some(&values)),
        Some(vec![
            "GEMINI_API_KEY".to_string(),
            "GOOGLE_API_KEY".to_string()
        ])
    );
}

#[test]
fn get_env_api_key_returns_the_first_configured_var() {
    let only_secondary = env(&[("GOOGLE_API_KEY", "secondary")]);
    assert_eq!(
        get_env_api_key("google", Some(&only_secondary)).as_deref(),
        Some("secondary")
    );

    let both = env(&[
        ("GEMINI_API_KEY", "primary"),
        ("GOOGLE_API_KEY", "secondary"),
    ]);
    assert_eq!(
        get_env_api_key("google", Some(&both)).as_deref(),
        Some("primary")
    );
}

#[test]
fn get_env_api_key_skips_anthropics_bearer_only_token() {
    // ANTHROPIC_AUTH_TOKEN participates in discovery but requests must pass it
    // as `Authorization: Bearer`, so key selection skips it.
    let bearer_only = env(&[("ANTHROPIC_AUTH_TOKEN", "bearer")]);
    assert_eq!(
        find_env_keys("anthropic", Some(&bearer_only)),
        Some(vec!["ANTHROPIC_AUTH_TOKEN".to_string()])
    );
    assert_eq!(get_env_api_key("anthropic", Some(&bearer_only)), None);

    let bearer_and_oauth = env(&[
        ("ANTHROPIC_AUTH_TOKEN", "bearer"),
        ("ANTHROPIC_OAUTH_TOKEN", "oauth"),
    ]);
    assert_eq!(
        get_env_api_key("anthropic", Some(&bearer_and_oauth)).as_deref(),
        Some("oauth")
    );
}

#[test]
fn get_env_api_key_detects_bedrock_ambient_credentials() {
    let profile = env(&[("AWS_PROFILE", "default")]);
    assert_eq!(
        get_env_api_key("amazon-bedrock", Some(&profile)).as_deref(),
        Some("<authenticated>")
    );

    // A lone access key id is not a complete IAM credential pair.
    let partial = env(&[("AWS_ACCESS_KEY_ID", "id")]);
    assert_eq!(get_env_api_key("amazon-bedrock", Some(&partial)), None);
}
