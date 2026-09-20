//! Offline tests for the image-provider runtime collection — the port of
//! `packages/ai/src/images-models.ts`.
//!
//! Vectors cover the collection surface (`set`/`delete`/`clear`, best-effort
//! model reads), auth resolution through the shared `resolve_provider_auth`
//! policy, the field-wise merge in `generateImages`, the "never reject"
//! contract, the `model_source` / best-effort refresh semantics and the
//! in-flight de-duplication of `createImagesProvider`'s `refreshModels`.
//!
//! Nothing here touches the network: ad‑hoc `ImagesProvider` / `ProviderImages`
//! doubles stand in for real adapters.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use pi_ai::auth::types::{
    ApiKeyAuth, ApiKeyCredential, ApiKeyResolveInput, AuthContext, AuthResult, Credential,
    CredentialModifyFn, CredentialStore, ModelAuth, ProviderAuth,
};
use pi_ai::auth::{AuthError, InMemoryCredentialStore, ModelsError, ModelsErrorCode};
use pi_ai::images::{
    create_images_models, create_images_provider, AssistantImages, AuthTarget,
    CreateImagesModelsOptions, CreateImagesProviderOptions, ImagesContext, ImagesModel,
    ImagesOptions, ImagesProvider, ImagesRefreshError, ImagesStopReason, MutableImagesModels,
    ProviderImages, RefreshModelsFn,
};
use pi_ai::types::AbortSignal;
use tokio_util::sync::CancellationToken;

const API: &str = "test-images-api";

fn model(provider: &str, id: &str, base_url: &str) -> ImagesModel {
    ImagesModel::new(provider, id, API, base_url)
}

fn env_of(pairs: &[(&str, &str)]) -> pi_ai::auth::ProviderEnv {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

fn headers_of(pairs: &[(&str, Option<&str>)]) -> pi_ai::auth::ProviderHeaders {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), value.map(str::to_string)))
        .collect()
}

async fn put(store: &InMemoryCredentialStore, provider_id: &str, credential: Credential) {
    let modify: CredentialModifyFn =
        Box::new(move |_| Box::pin(async move { Ok(Some(credential)) }));
    store.modify(provider_id, modify, None).await.unwrap();
}

// ---------------------------------------------------------------------------
// Doubles
// ---------------------------------------------------------------------------

/// Environment context backed by an explicit map (never the process env).
struct FakeContext {
    values: pi_ai::auth::ProviderEnv,
}

impl FakeContext {
    fn new(pairs: &[(&str, &str)]) -> Self {
        Self {
            values: env_of(pairs),
        }
    }

    fn empty() -> Self {
        Self {
            values: Default::default(),
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

/// Api-key strategy with a resolvable key (stored credential wins, else an
/// env var) plus per-credential base URL / headers, like the real
/// subscription-capable providers.
struct TestApiKeyAuth {
    name: String,
    key_env: String,
    base_url: Option<String>,
    headers: Option<pi_ai::auth::ProviderHeaders>,
}

impl TestApiKeyAuth {
    fn new(name: &str, key_env: &str) -> Self {
        Self {
            name: name.to_string(),
            key_env: key_env.to_string(),
            base_url: None,
            headers: None,
        }
    }

    fn with_base_url(mut self, base_url: &str) -> Self {
        self.base_url = Some(base_url.to_string());
        self
    }

    fn with_headers(mut self, headers: pi_ai::auth::ProviderHeaders) -> Self {
        self.headers = Some(headers);
        self
    }
}

#[async_trait]
impl ApiKeyAuth for TestApiKeyAuth {
    fn name(&self) -> &str {
        &self.name
    }

    async fn resolve(
        &self,
        input: ApiKeyResolveInput<'_>,
    ) -> Result<Option<AuthResult>, AuthError> {
        let stored = input
            .credential
            .and_then(|credential| credential.key.clone())
            .filter(|key| !key.is_empty());
        let key = match stored {
            Some(key) => Some(key),
            None => input
                .ctx
                .env(&self.key_env)
                .await
                .filter(|value| !value.is_empty()),
        };
        if key.is_none() && self.base_url.is_none() {
            return Ok(None);
        }
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: key,
                headers: self.headers.clone(),
                base_url: self.base_url.clone(),
            },
            env: input
                .credential
                .and_then(|credential| credential.env.clone()),
            source: Some(self.name.clone()),
        }))
    }
}

#[derive(Clone)]
struct RecordedCall {
    model: ImagesModel,
    context: ImagesContext,
    options: ImagesOptions,
}

/// Adapter double that records exactly what the collection handed it.
struct RecordingImages {
    calls: Mutex<Vec<RecordedCall>>,
}

impl RecordingImages {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
        })
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn last_call(&self) -> RecordedCall {
        self.calls
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("adapter was called")
    }
}

#[async_trait]
impl ProviderImages for RecordingImages {
    async fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: &ImagesOptions,
    ) -> AssistantImages {
        self.calls.lock().unwrap().push(RecordedCall {
            model: model.clone(),
            context: context.clone(),
            options: options.clone(),
        });
        AssistantImages::new(
            model.api.clone(),
            model.provider.to_string(),
            model.id.clone(),
            ImagesStopReason::Stop,
        )
    }
}

/// A counting fetcher that succeeds after a delay, optionally after one
/// failure.
fn counting_refresher(
    calls: Arc<AtomicUsize>,
    delay_ms: u64,
    fail_first: Arc<AtomicBool>,
    models: Vec<ImagesModel>,
) -> RefreshModelsFn {
    Arc::new(move || {
        let calls = calls.clone();
        let fail_first = fail_first.clone();
        let models = models.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            if fail_first.swap(false, Ordering::SeqCst) {
                return Err(ImagesRefreshError::other("network down"));
            }
            Ok(models)
        }
        .boxed()
    })
}

fn collection_of(providers: Vec<Arc<dyn ImagesProvider>>) -> Box<dyn MutableImagesModels> {
    let mut models = create_images_models(CreateImagesModelsOptions::default());
    for provider in providers {
        models.set_provider(provider);
    }
    models
}

// ---------------------------------------------------------------------------
// Collection surface
// ---------------------------------------------------------------------------

#[tokio::test]
async fn provider_name_defaults_to_id_and_lists_models_in_order() {
    let provider = create_images_provider(
        CreateImagesProviderOptions::new("acme", ProviderAuth::default(), RecordingImages::new())
            .models(vec![
                model("acme", "first", "https://acme.example"),
                model("acme", "second", "https://acme.example"),
            ]),
    );
    assert_eq!(provider.id(), "acme");
    assert_eq!(provider.name(), "acme");

    let models = collection_of(vec![provider]);
    assert_eq!(models.get_providers().len(), 1);
    assert_eq!(models.get_models(Some("acme")).len(), 2);
    assert_eq!(
        models
            .get_model("acme", "second")
            .map(|model| model.id)
            .as_deref(),
        Some("second")
    );
    assert!(models.get_model("acme", "missing").is_none());
}

#[tokio::test]
async fn get_models_is_best_effort_for_unknown_and_all_providers() {
    let first = create_images_provider(
        CreateImagesProviderOptions::new("a", ProviderAuth::default(), RecordingImages::new())
            .models(vec![model("a", "a-1", "https://a.example")]),
    );
    let second = create_images_provider(
        CreateImagesProviderOptions::new("b", ProviderAuth::default(), RecordingImages::new())
            .models(vec![
                model("b", "b-1", "https://b.example"),
                model("b", "b-2", "https://b.example"),
            ]),
    );
    let models = collection_of(vec![first, second]);

    assert!(models.get_models(Some("unknown")).is_empty());
    assert!(models.get_models(Some("a")).len() == 1);
    let all: Vec<String> = models
        .get_models(None)
        .into_iter()
        .map(|model| model.id)
        .collect();
    // Provider insertion order, then per-provider registration order.
    assert_eq!(all, vec!["a-1", "b-1", "b-2"]);
}

#[tokio::test]
async fn set_replaces_in_place_and_delete_clear_remove() {
    let make = |id: &str| {
        create_images_provider(
            CreateImagesProviderOptions::new(id, ProviderAuth::default(), RecordingImages::new())
                .name(format!("{id} display")),
        )
    };
    let mut models = collection_of(vec![make("a"), make("b")]);

    // Upserting an existing id keeps its slot; a new id appends.
    models.set_provider(make("a"));
    models.set_provider(make("c"));
    let ids: Vec<String> = models
        .get_providers()
        .into_iter()
        .map(|provider| provider.id().to_string())
        .collect();
    assert_eq!(ids, vec!["a", "b", "c"]);
    assert_eq!(models.get_provider("a").unwrap().name(), "a display");

    models.delete_provider("b");
    assert!(models.get_provider("b").is_none());
    // Deleting an unknown id is a no-op.
    models.delete_provider("zzz");

    models.clear_providers();
    assert!(models.get_providers().is_empty());
    assert!(models.get_models(None).is_empty());
}

// ---------------------------------------------------------------------------
// Auth resolution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_auth_returns_none_for_unknown_and_unconfigured_providers() {
    let unconfigured = create_images_provider(CreateImagesProviderOptions::new(
        "unconfigured",
        ProviderAuth {
            api_key: Some(Arc::new(TestApiKeyAuth::new("Test", "MISSING_KEY"))),
            oauth: None,
        },
        RecordingImages::new(),
    ));
    let models = collection_of(vec![unconfigured]);

    assert!(models
        .get_auth(AuthTarget::Provider("unknown"), None)
        .await
        .unwrap()
        .is_none());
    assert!(models
        .get_auth(AuthTarget::Provider("unconfigured"), None)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn get_auth_resolves_by_provider_id_and_by_model() {
    let store = Arc::new(InMemoryCredentialStore::new());
    put(
        &store,
        "acme",
        Credential::ApiKey(ApiKeyCredential {
            key: Some("stored-key".into()),
            env: None,
        }),
    )
    .await;

    let provider = create_images_provider(CreateImagesProviderOptions::new(
        "acme",
        ProviderAuth {
            api_key: Some(Arc::new(TestApiKeyAuth::new("Test", "ACME_KEY"))),
            oauth: None,
        },
        RecordingImages::new(),
    ));
    let mut models = create_images_models(CreateImagesModelsOptions {
        credentials: Some(store),
        auth_context: Some(Arc::new(FakeContext::empty())),
    });
    models.set_provider(provider);

    let by_id = models
        .get_auth(AuthTarget::Provider("acme"), None)
        .await
        .unwrap()
        .expect("configured provider resolves");
    assert_eq!(by_id.auth.api_key.as_deref(), Some("stored-key"));

    let runtime_model = model("acme", "acme-image", "https://acme.example");
    let by_model = models
        .get_auth(AuthTarget::Model(&runtime_model), None)
        .await
        .unwrap()
        .expect("model resolves through its provider");
    assert_eq!(by_model.auth.api_key.as_deref(), Some("stored-key"));
}

// ---------------------------------------------------------------------------
// generate_images
// ---------------------------------------------------------------------------

#[tokio::test]
async fn generate_images_injects_auth_base_url_and_key() {
    let auth = ProviderAuth {
        api_key: Some(Arc::new(
            TestApiKeyAuth::new("Test", "ACME_KEY").with_base_url("https://auth.example/v1"),
        )),
        oauth: None,
    };
    let api = RecordingImages::new();
    let provider =
        create_images_provider(CreateImagesProviderOptions::new("acme", auth, api.clone()));
    let mut models = create_images_models(CreateImagesModelsOptions {
        credentials: None,
        auth_context: Some(Arc::new(FakeContext::new(&[("ACME_KEY", "env-key")]))),
    });
    models.set_provider(provider);

    let runtime_model = model("acme", "acme-image", "https://model.example/v1");
    let result = models
        .generate_images(
            &runtime_model,
            &ImagesContext::text("a red panda"),
            &ImagesOptions::default(),
        )
        .await;

    assert_eq!(result.stop_reason, ImagesStopReason::Stop);
    let call = api.last_call();
    // The auth base URL overrides the model's.
    assert_eq!(call.model.base_url, "https://auth.example/v1");
    assert_eq!(call.options.api_key.as_deref(), Some("env-key"));
    assert_eq!(call.context, ImagesContext::text("a red panda"));
}

#[tokio::test]
async fn generate_images_keeps_model_base_url_without_auth_override() {
    let auth = ProviderAuth {
        api_key: Some(Arc::new(TestApiKeyAuth::new("Test", "ACME_KEY"))),
        oauth: None,
    };
    let api = RecordingImages::new();
    let provider =
        create_images_provider(CreateImagesProviderOptions::new("acme", auth, api.clone()));
    let mut models = create_images_models(CreateImagesModelsOptions {
        credentials: None,
        auth_context: Some(Arc::new(FakeContext::new(&[("ACME_KEY", "env-key")]))),
    });
    models.set_provider(provider);

    let runtime_model = model("acme", "acme-image", "https://model.example/v1");
    models
        .generate_images(
            &runtime_model,
            &ImagesContext::text("hi"),
            &ImagesOptions::default(),
        )
        .await;

    assert_eq!(api.last_call().model.base_url, "https://model.example/v1");
}

#[tokio::test]
async fn generate_images_merges_headers_and_env_per_key() {
    let store = Arc::new(InMemoryCredentialStore::new());
    put(
        &store,
        "acme",
        Credential::ApiKey(ApiKeyCredential {
            key: Some("stored-key".into()),
            env: Some(env_of(&[("A", "provider"), ("B", "provider")])),
        }),
    )
    .await;

    let auth = ProviderAuth {
        api_key: Some(Arc::new(
            TestApiKeyAuth::new("Test", "ACME_KEY").with_headers(headers_of(&[
                ("X-Provider", Some("provider")),
                ("X-Both", Some("provider")),
            ])),
        )),
        oauth: None,
    };
    let api = RecordingImages::new();
    let provider =
        create_images_provider(CreateImagesProviderOptions::new("acme", auth, api.clone()));
    let mut models = create_images_models(CreateImagesModelsOptions {
        credentials: Some(store),
        auth_context: Some(Arc::new(FakeContext::empty())),
    });
    models.set_provider(provider);

    let runtime_model = model("acme", "acme-image", "https://acme.example");
    models
        .generate_images(
            &runtime_model,
            &ImagesContext::text("hi"),
            &ImagesOptions {
                headers: headers_of(&[
                    ("X-Both", Some("request")),
                    ("X-Request", Some("request")),
                    // A `None` value suppresses the provider default.
                    ("X-Provider", None),
                ]),
                env: env_of(&[("A", "request"), ("C", "request")]),
                ..ImagesOptions::default()
            },
        )
        .await;

    let call = api.last_call();
    let headers = &call.options.headers;
    assert_eq!(headers.get("X-Both"), Some(&Some("request".to_string())));
    assert_eq!(headers.get("X-Request"), Some(&Some("request".to_string())));
    assert_eq!(headers.get("X-Provider"), Some(&None::<String>));
    assert_eq!(headers.len(), 3);

    let env = &call.options.env;
    assert_eq!(env.get("A"), Some(&"request".to_string()));
    assert_eq!(env.get("B"), Some(&"provider".to_string()));
    assert_eq!(env.get("C"), Some(&"request".to_string()));
    assert_eq!(env.len(), 3);
    assert_eq!(call.options.api_key.as_deref(), Some("stored-key"));
}

#[tokio::test]
async fn generate_images_explicit_api_key_wins_over_resolved_auth() {
    let auth = ProviderAuth {
        api_key: Some(Arc::new(TestApiKeyAuth::new("Test", "ACME_KEY"))),
        oauth: None,
    };
    let api = RecordingImages::new();
    let provider =
        create_images_provider(CreateImagesProviderOptions::new("acme", auth, api.clone()));
    let mut models = create_images_models(CreateImagesModelsOptions {
        credentials: None,
        auth_context: Some(Arc::new(FakeContext::new(&[("ACME_KEY", "env-key")]))),
    });
    models.set_provider(provider);

    let runtime_model = model("acme", "acme-image", "https://acme.example");
    models
        .generate_images(
            &runtime_model,
            &ImagesContext::text("hi"),
            &ImagesOptions {
                api_key: Some("explicit".into()),
                ..ImagesOptions::default()
            },
        )
        .await;

    assert_eq!(api.last_call().options.api_key.as_deref(), Some("explicit"));
}

#[tokio::test]
async fn generate_images_delegates_untouched_when_unconfigured() {
    let auth = ProviderAuth {
        api_key: Some(Arc::new(TestApiKeyAuth::new("Test", "MISSING_KEY"))),
        oauth: None,
    };
    let api = RecordingImages::new();
    let provider =
        create_images_provider(CreateImagesProviderOptions::new("acme", auth, api.clone()));
    let mut models = create_images_models(CreateImagesModelsOptions {
        credentials: None,
        auth_context: Some(Arc::new(FakeContext::empty())),
    });
    models.set_provider(provider);

    let runtime_model = model("acme", "acme-image", "https://model.example/v1");
    let result = models
        .generate_images(
            &runtime_model,
            &ImagesContext::text("hi"),
            &ImagesOptions {
                max_retries: Some(3),
                ..ImagesOptions::default()
            },
        )
        .await;

    assert_eq!(result.stop_reason, ImagesStopReason::Stop);
    let call = api.last_call();
    assert_eq!(call.model.base_url, "https://model.example/v1");
    assert!(call.options.api_key.is_none());
    assert!(call.options.headers.is_empty());
    assert!(call.options.env.is_empty());
    assert_eq!(call.options.max_retries, Some(3));
}

#[tokio::test]
async fn generate_images_returns_error_for_unknown_provider() {
    let models = create_images_models(CreateImagesModelsOptions::default());
    let runtime_model = model("ghost", "ghost-image", "https://ghost.example");

    let result = models
        .generate_images(
            &runtime_model,
            &ImagesContext::text("hi"),
            &ImagesOptions::default(),
        )
        .await;

    assert_eq!(result.stop_reason, ImagesStopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Unknown provider: ghost")
    );
    assert_eq!(result.api, API);
    assert_eq!(result.provider.to_string(), "ghost");
    assert_eq!(result.model, "ghost-image");
    assert!(result.output.is_empty());
}

#[tokio::test]
async fn generate_images_turns_auth_failures_into_error_results() {
    let auth = ProviderAuth {
        api_key: Some(Arc::new(TestApiKeyAuth::new("Test", "ACME_KEY"))),
        oauth: None,
    };
    let api = RecordingImages::new();
    let provider =
        create_images_provider(CreateImagesProviderOptions::new("acme", auth, api.clone()));
    let mut models = create_images_models(CreateImagesModelsOptions {
        credentials: None,
        auth_context: Some(Arc::new(FakeContext::new(&[("ACME_KEY", "env-key")]))),
    });
    models.set_provider(provider);

    let token = CancellationToken::new();
    token.cancel();
    let runtime_model = model("acme", "acme-image", "https://acme.example");
    let result = models
        .generate_images(
            &runtime_model,
            &ImagesContext::text("hi"),
            &ImagesOptions {
                signal: Some(AbortSignal::native(token)),
                ..ImagesOptions::default()
            },
        )
        .await;

    assert_eq!(result.stop_reason, ImagesStopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("the operation was aborted")
    );
    // The adapter is never reached once auth fails.
    assert_eq!(api.call_count(), 0);
}

// ---------------------------------------------------------------------------
// refresh
// ---------------------------------------------------------------------------

#[tokio::test]
async fn refresh_wraps_uncoded_failures_as_model_source() {
    let failing: RefreshModelsFn =
        Arc::new(|| async move { Err(ImagesRefreshError::other("network down")) }.boxed());
    let provider = create_images_provider(
        CreateImagesProviderOptions::new("acme", ProviderAuth::default(), RecordingImages::new())
            .refresh_models(failing),
    );
    let models = collection_of(vec![provider]);

    let error = models
        .refresh(Some("acme"))
        .await
        .expect_err("refresh must fail");
    assert_eq!(error.code(), ModelsErrorCode::ModelSource);
    assert!(error.message().contains("Model refresh failed for acme"));
    assert!(error.message().contains("network down"));
}

#[tokio::test]
async fn refresh_passes_through_an_already_coded_error() {
    let coded: RefreshModelsFn = Arc::new(|| {
        async move {
            Err(ImagesRefreshError::from(ModelsError::new(
                ModelsErrorCode::OAuth,
                "denied",
            )))
        }
        .boxed()
    });
    let provider = create_images_provider(
        CreateImagesProviderOptions::new("acme", ProviderAuth::default(), RecordingImages::new())
            .refresh_models(coded),
    );
    let models = collection_of(vec![provider]);

    let error = models
        .refresh(Some("acme"))
        .await
        .expect_err("refresh must fail");
    assert_eq!(error.code(), ModelsErrorCode::OAuth);
    assert_eq!(error.message(), "denied");
}

#[tokio::test]
async fn refresh_unknown_and_static_providers_are_noops() {
    let static_provider = create_images_provider(CreateImagesProviderOptions::new(
        "static",
        ProviderAuth::default(),
        RecordingImages::new(),
    ));
    let models = collection_of(vec![static_provider]);

    assert!(models.refresh(Some("missing")).await.is_ok());
    assert!(models.refresh(Some("static")).await.is_ok());
}

#[tokio::test]
async fn refresh_all_is_best_effort_and_updates_successful_providers() {
    let failing_calls = Arc::new(AtomicUsize::new(0));
    let failing: RefreshModelsFn = {
        let calls = failing_calls.clone();
        Arc::new(move || {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(ImagesRefreshError::other("boom"))
            }
            .boxed()
        })
    };
    let ok_calls = Arc::new(AtomicUsize::new(0));
    let ok_models = vec![model("ok", "fresh", "https://ok.example")];
    let ok = counting_refresher(
        ok_calls.clone(),
        0,
        Arc::new(AtomicBool::new(false)),
        ok_models.clone(),
    );

    let failing_provider = create_images_provider(
        CreateImagesProviderOptions::new("bad", ProviderAuth::default(), RecordingImages::new())
            .refresh_models(failing),
    );
    let ok_provider = create_images_provider(
        CreateImagesProviderOptions::new("ok", ProviderAuth::default(), RecordingImages::new())
            .refresh_models(ok),
    );
    let models = collection_of(vec![failing_provider, ok_provider]);

    // `refresh(None)` never rejects: one provider's failure is swallowed.
    assert!(models.refresh(None).await.is_ok());
    assert_eq!(failing_calls.load(Ordering::SeqCst), 1);
    assert_eq!(ok_calls.load(Ordering::SeqCst), 1);
    assert_eq!(models.get_models(Some("ok")), ok_models);
    assert!(models.get_models(Some("bad")).is_empty());
}

// ---------------------------------------------------------------------------
// create_images_provider: in-flight de-duplication
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_refresh_calls_share_one_fetch() {
    let calls = Arc::new(AtomicUsize::new(0));
    let refreshed = vec![model("acme", "dynamic", "https://acme.example")];
    let refresh = counting_refresher(
        calls.clone(),
        50,
        Arc::new(AtomicBool::new(false)),
        refreshed.clone(),
    );
    let provider = create_images_provider(
        CreateImagesProviderOptions::new("acme", ProviderAuth::default(), RecordingImages::new())
            .refresh_models(refresh),
    );

    let mut tasks = Vec::new();
    for _ in 0..5 {
        let provider = provider.clone();
        tasks.push(tokio::spawn(async move { provider.refresh_models().await }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.get_models(), refreshed);
}

#[tokio::test]
async fn refresh_starts_a_new_fetch_after_the_previous_settled() {
    let calls = Arc::new(AtomicUsize::new(0));
    let refreshed = vec![model("acme", "dynamic", "https://acme.example")];
    let refresh = counting_refresher(
        calls.clone(),
        0,
        Arc::new(AtomicBool::new(false)),
        refreshed.clone(),
    );
    let provider = create_images_provider(
        CreateImagesProviderOptions::new("acme", ProviderAuth::default(), RecordingImages::new())
            .refresh_models(refresh),
    );

    provider.refresh_models().await.unwrap();
    provider.refresh_models().await.unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider.get_models(), refreshed);
}

#[tokio::test]
async fn refresh_retries_after_a_failure_and_keeps_last_known_models() {
    let calls = Arc::new(AtomicUsize::new(0));
    let refreshed = vec![model("acme", "dynamic", "https://acme.example")];
    let refresh = counting_refresher(
        calls.clone(),
        0,
        Arc::new(AtomicBool::new(true)),
        refreshed.clone(),
    );
    let provider = create_images_provider(
        CreateImagesProviderOptions::new("acme", ProviderAuth::default(), RecordingImages::new())
            .models(vec![model("acme", "stale", "https://acme.example")])
            .refresh_models(refresh),
    );

    assert!(provider.refresh_models().await.is_err());
    // The last-known list survives a failed fetch.
    assert_eq!(provider.get_models()[0].id, "stale");

    provider.refresh_models().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider.get_models(), refreshed);
}
