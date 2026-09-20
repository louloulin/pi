//! Image-provider runtime collection — port of
//! `packages/ai/src/images-models.ts`.
//!
//! Upstream's `ImagesProvider` is the image-side counterpart of the chat
//! `Provider`: it owns `id`/`name` metadata, auth, a sync model listing and
//! the generation entry point. `ImagesModels` is the runtime collection that
//! holds those providers, resolves request auth through the shared
//! [`resolve_provider_auth`] policy, and offers a single
//! `generateImages(model, context, options)` facade that **never rejects**:
//! every failure (unknown provider, auth failure, adapter error) is returned
//! as an [`AssistantImages`] with [`crate::images::ImagesStopReason::Error`].
//!
//! [`create_images_provider`] builds a provider from parts, with the
//! in-flight de-duplication upstream implements for `refreshModels()`:
//! concurrent calls share one fetch, a later call retries after a failure.
//!
//! # Deliberate differences from the TypeScript implementation
//!
//! * `getModels()` returns an owned `Vec<ImagesModel>` snapshot instead of a
//!   live `readonly` array: a dynamic provider keeps its list behind interior
//!   mutability, so it cannot lend a slice.
//! * Rust has no `throw`, so "`getModels()` must not throw" becomes the
//!   infallible return type rather than a `try`/`catch`. A provider that
//!   *panics* is outside the contract; `ImagesModels::get_models` is
//!   best-effort in the sense that an unknown provider yields no models and
//!   every provider's own list is concatenated without an error channel.
//! * `get_auth` takes an [`AuthTarget`] instead of upstream's
//!   `string | ImagesModel` overload, keeping the trait object-safe.
//! * Upstream's optional `refreshModels?` member becomes a defaulted
//!   [`ImagesProvider::refresh_models`] returning `Ok(())` — the no-op is the
//!   observable behaviour of "member absent". Its rejection is modelled as
//!   [`ImagesRefreshError`], which distinguishes an already-coded
//!   [`ModelsError`] (re-raised unchanged, per upstream's
//!   `error instanceof ModelsError`) from an uncoded failure that
//!   [`ImagesModels::refresh`] wraps as `model_source`.
//! * `CreateModelsOptions.modelsStore` has no image-side meaning and is
//!   omitted; only `credentials` and `authContext` are carried over.
//! * `create_images_models` returns `Box<dyn MutableImagesModels>` so the
//!   concrete implementation stays private, like upstream's unexported
//!   `ImagesModelsImpl`.

use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use futures::future::{join_all, BoxFuture, Shared};
use futures::FutureExt;
use indexmap::IndexMap;

use crate::auth::{
    default_provider_auth_context, resolve_provider_auth, AuthContext, AuthError,
    AuthResolutionOverrides, AuthResult, CredentialStore, InMemoryCredentialStore, ModelsError,
    ModelsErrorCode, ProviderAuth, ProviderEnv, ProviderHeaders,
};

use super::types::{AssistantImages, ImagesContext, ImagesModel, ImagesOptions, ProviderImages};

/// Failure raised by [`ImagesProvider::refresh_models`].
///
/// Upstream's optional `refreshModels()` may reject with anything;
/// [`ImagesModels::refresh`] re-raises an already-coded [`ModelsError`]
/// unchanged and wraps everything else as a `model_source` failure, matching
/// upstream's `instanceof ModelsError` check.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ImagesRefreshError {
    /// Already-coded failure ([`ModelsError`]); re-raised unchanged by
    /// [`ImagesModels::refresh`]. Held behind an [`Arc`] because the provider
    /// builder shares one in-flight fetch's result between concurrent
    /// callers, which requires a `Clone` error.
    #[error("{0}")]
    Models(Arc<ModelsError>),
    /// Uncoded failure. [`ImagesModels::refresh`] turns it into a
    /// [`ModelsError`] with code `model_source` and this text as cause.
    #[error("{0}")]
    Other(String),
}

impl ImagesRefreshError {
    /// Wrap an arbitrary failure (upstream's `throw new Error(...)`).
    pub fn other(error: impl std::fmt::Display) -> Self {
        Self::Other(error.to_string())
    }
}

impl From<ModelsError> for ImagesRefreshError {
    fn from(error: ModelsError) -> Self {
        Self::Models(Arc::new(error))
    }
}

/// A fetcher for a dynamic provider's model list (upstream
/// `CreateImagesProviderOptions.refreshModels`).
///
/// The returned future is `'static` and `Send`, so providers work on both
/// native and WASM targets.
pub type RefreshModelsFn =
    Arc<dyn Fn() -> BoxFuture<'static, Result<Vec<ImagesModel>, ImagesRefreshError>> + Send + Sync>;

/// An image-generation provider: the image-side counterpart of the chat
/// `Provider` (upstream `ImagesProvider`).
///
/// Implementations must be safe to share across tasks (`Send + Sync`) because
/// [`ImagesModels`] hands them out as [`Arc`]s.
#[async_trait]
pub trait ImagesProvider: Send + Sync {
    /// Provider id; unique within an [`ImagesModels`] collection.
    fn id(&self) -> &str;

    /// Human-readable label.
    fn name(&self) -> &str;

    /// Auth strategies. At least one of `api_key` / `oauth` is expected: even
    /// ambient-credential and keyless providers report through `api_key`.
    fn auth(&self) -> &ProviderAuth;

    /// Current known models, sync. Static providers return their catalog;
    /// dynamic providers return the list as of the last
    /// [`refresh_models`](ImagesProvider::refresh_models) (empty before the
    /// first). Must not panic or block; [`ImagesModels`] treats the list as
    /// best-effort data.
    fn get_models(&self) -> Vec<ImagesModel>;

    /// Dynamic providers only: fetch and update the model list.
    ///
    /// The default implementation is a no-op, which is upstream's "member
    /// absent" (static provider) behaviour. On failure the last-known list is
    /// kept, and a later call retries.
    async fn refresh_models(&self) -> Result<(), ImagesRefreshError> {
        Ok(())
    }

    /// Generate images. Must not reject: failures come back inside the
    /// returned [`AssistantImages`].
    async fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: &ImagesOptions,
    ) -> AssistantImages;
}

/// Lookup key accepted by [`ImagesModels::get_auth`] — upstream's
/// `string | ImagesModel` overload.
#[derive(Debug, Clone, Copy)]
pub enum AuthTarget<'a> {
    /// Resolve by provider id.
    Provider(&'a str),
    /// Resolve through the model's owning provider.
    Model(&'a ImagesModel),
}

impl<'a> AuthTarget<'a> {
    /// The provider id this target resolves through.
    pub fn provider_id(self) -> &'a str {
        match self {
            Self::Provider(id) => id,
            Self::Model(model) => model.provider.0.as_str(),
        }
    }
}

impl<'a> From<&'a str> for AuthTarget<'a> {
    fn from(provider_id: &'a str) -> Self {
        Self::Provider(provider_id)
    }
}

impl<'a> From<&'a ImagesModel> for AuthTarget<'a> {
    fn from(model: &'a ImagesModel) -> Self {
        Self::Model(model)
    }
}

/// Runtime collection of image-generation providers (upstream
/// `ImagesModels`).
#[async_trait]
pub trait ImagesModels: Send + Sync {
    /// Every provider, in insertion order.
    fn get_providers(&self) -> Vec<Arc<dyn ImagesProvider>>;

    /// Look up a provider by id.
    fn get_provider(&self, id: &str) -> Option<Arc<dyn ImagesProvider>>;

    /// Sync read of last-known models from one provider or all providers, in
    /// provider insertion order. Best-effort: an unknown provider yields no
    /// models.
    fn get_models(&self, provider: Option<&str>) -> Vec<ImagesModel>;

    /// Sync runtime model lookup against the last-known lists.
    fn get_model(&self, provider: &str, id: &str) -> Option<ImagesModel>;

    /// Ask dynamic providers to re-fetch their model lists. With a provider
    /// id, fails with a [`ModelsError`] (code `model_source`, unless the
    /// provider raised an already-coded error) when that provider's fetch
    /// fails; without one, refreshes all providers concurrently best-effort,
    /// where a single failure does not affect the others. Unknown and static
    /// providers are no-ops.
    async fn refresh(&self, provider: Option<&str>) -> Result<(), ModelsError>;

    /// Resolve request auth by provider id or image model. `Ok(None)` when
    /// the provider is unknown or unconfigured; real failures surface as
    /// [`AuthError::Models`] (`auth` / `oauth` code).
    async fn get_auth(
        &self,
        target: AuthTarget<'_>,
        overrides: Option<AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>, AuthError>;

    /// Generate images through the owning provider with auth resolved and
    /// merged (explicit options win per field, `headers` / `env` merge per
    /// key). Never rejects: failures are returned as an [`AssistantImages`]
    /// with [`crate::images::ImagesStopReason::Error`].
    async fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: &ImagesOptions,
    ) -> AssistantImages;
}

/// Writable half of [`ImagesModels`] (upstream `MutableImagesModels`).
pub trait MutableImagesModels: ImagesModels {
    /// Upsert a provider by `provider.id`. Provider ids are unique; replacing
    /// an existing id keeps its position.
    fn set_provider(&mut self, provider: Arc<dyn ImagesProvider>);

    /// Remove a provider by id. Unknown ids are ignored.
    fn delete_provider(&mut self, id: &str);

    /// Remove every provider.
    fn clear_providers(&mut self);
}

/// Options for [`create_images_models`] (upstream `CreateModelsOptions`,
/// minus the chat-only `modelsStore`).
#[derive(Default)]
pub struct CreateImagesModelsOptions {
    /// Credential storage used by [`ImagesModels::get_auth`]. Defaults to a
    /// fresh [`InMemoryCredentialStore`].
    pub credentials: Option<Arc<dyn CredentialStore>>,
    /// Environment / file access used by auth resolution. Defaults to
    /// [`default_provider_auth_context`].
    pub auth_context: Option<Arc<dyn AuthContext>>,
}

struct ImagesModelsImpl {
    providers: IndexMap<String, Arc<dyn ImagesProvider>>,
    credentials: Arc<dyn CredentialStore>,
    auth_context: Arc<dyn AuthContext>,
}

impl ImagesModelsImpl {
    fn new(options: CreateImagesModelsOptions) -> Self {
        Self {
            providers: IndexMap::new(),
            credentials: options
                .credentials
                .unwrap_or_else(|| Arc::new(InMemoryCredentialStore::new())),
            auth_context: options
                .auth_context
                .unwrap_or_else(default_provider_auth_context),
        }
    }
}

impl MutableImagesModels for ImagesModelsImpl {
    fn set_provider(&mut self, provider: Arc<dyn ImagesProvider>) {
        // `IndexMap::insert` keeps the original slot for an existing id, like
        // JS `Map.set`.
        self.providers.insert(provider.id().to_string(), provider);
    }

    fn delete_provider(&mut self, id: &str) {
        self.providers.shift_remove(id);
    }

    fn clear_providers(&mut self) {
        self.providers.clear();
    }
}

#[async_trait]
impl ImagesModels for ImagesModelsImpl {
    fn get_providers(&self) -> Vec<Arc<dyn ImagesProvider>> {
        self.providers.values().cloned().collect()
    }

    fn get_provider(&self, id: &str) -> Option<Arc<dyn ImagesProvider>> {
        self.providers.get(id).cloned()
    }

    fn get_models(&self, provider: Option<&str>) -> Vec<ImagesModel> {
        match provider {
            Some(id) => self
                .providers
                .get(id)
                .map(|entry| entry.get_models())
                .unwrap_or_default(),
            None => {
                let mut models = Vec::new();
                for entry in self.providers.values() {
                    models.extend(entry.get_models());
                }
                models
            }
        }
    }

    fn get_model(&self, provider: &str, id: &str) -> Option<ImagesModel> {
        self.get_models(Some(provider))
            .into_iter()
            .find(|model| model.id == id)
    }

    async fn refresh(&self, provider: Option<&str>) -> Result<(), ModelsError> {
        let Some(provider) = provider else {
            // `Promise.allSettled` equivalent: run every refresh concurrently
            // and drop the individual results, so one failure cannot stop the
            // rest.
            join_all(self.providers.values().map(|entry| entry.refresh_models())).await;
            return Ok(());
        };

        // Unknown and static providers are no-ops upstream.
        let Some(entry) = self.providers.get(provider) else {
            return Ok(());
        };
        match entry.refresh_models().await {
            Ok(()) => Ok(()),
            // An already-coded failure keeps its own code.
            Err(ImagesRefreshError::Models(error)) => {
                Err(ModelsError::new(error.code(), error.message()))
            }
            Err(error) => Err(ModelsError::with_cause(
                ModelsErrorCode::ModelSource,
                format!("Model refresh failed for {provider}"),
                error,
            )),
        }
    }

    async fn get_auth(
        &self,
        target: AuthTarget<'_>,
        overrides: Option<AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>, AuthError> {
        let provider_id = target.provider_id();
        let Some(provider) = self.providers.get(provider_id) else {
            return Ok(None);
        };
        resolve_provider_auth(
            provider_id,
            provider.auth(),
            self.credentials.as_ref(),
            self.auth_context.as_ref(),
            overrides,
        )
        .await
    }

    async fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: &ImagesOptions,
    ) -> AssistantImages {
        let Some(provider) = self.providers.get(model.provider.0.as_str()) else {
            return AssistantImages::error(
                model.api.clone(),
                model.provider.to_string(),
                model.id.clone(),
                format!("Unknown provider: {}", model.provider),
            );
        };

        let overrides = AuthResolutionOverrides {
            api_key: options.api_key.clone(),
            env: (!options.env.is_empty()).then(|| options.env.clone()),
            signal: options.signal.clone(),
            ..AuthResolutionOverrides::default()
        };
        let resolution = match self
            .get_auth(AuthTarget::Model(model), Some(overrides))
            .await
        {
            Ok(resolution) => resolution,
            // Upstream catches every throw here and returns an error result.
            Err(error) => {
                return AssistantImages::error(
                    model.api.clone(),
                    model.provider.to_string(),
                    model.id.clone(),
                    error.to_string(),
                )
            }
        };

        // Unconfigured provider: delegate with the caller's options untouched.
        let Some(resolution) = resolution else {
            return provider.generate_images(model, context, options).await;
        };

        let auth = resolution.auth;
        let request_model = auth.base_url.as_ref().map(|base_url| {
            let mut model = model.clone();
            model.base_url = base_url.clone();
            model
        });
        let request_model = request_model.as_ref().unwrap_or(model);

        // Explicit request options win per field; headers/env merge per key.
        let api_key = options.api_key.clone().or_else(|| auth.api_key.clone());
        let headers: ProviderHeaders = {
            let mut merged = auth.headers.clone().unwrap_or_default();
            merged.extend(options.headers.clone());
            merged
        };
        let env: ProviderEnv = {
            let mut merged = resolution.env.clone().unwrap_or_default();
            merged.extend(options.env.clone());
            merged
        };
        let request_options = ImagesOptions {
            api_key,
            headers,
            env,
            ..options.clone()
        };

        provider
            .generate_images(request_model, context, &request_options)
            .await
    }
}

/// Build an empty mutable image-model collection (upstream
/// `createImagesModels`).
pub fn create_images_models(options: CreateImagesModelsOptions) -> Box<dyn MutableImagesModels> {
    Box::new(ImagesModelsImpl::new(options))
}

/// Input for [`create_images_provider`] (upstream
/// `CreateImagesProviderOptions`).
pub struct CreateImagesProviderOptions {
    /// Provider id. Must be unique within a collection.
    pub id: String,
    /// Display name. Defaults to `id`.
    pub name: Option<String>,
    /// Auth strategies — every provider has auth semantics, even
    /// ambient/keyless ones.
    pub auth: ProviderAuth,
    /// Initial model list (empty for purely dynamic providers).
    pub models: Vec<ImagesModel>,
    /// Dynamic providers: fetch the current list. Stored on success;
    /// concurrent calls share one in-flight fetch. May fail: the stored list
    /// then stays at its last-known state and a later call retries.
    pub refresh_models: Option<RefreshModelsFn>,
    /// The adapter that performs generation.
    pub api: Arc<dyn ProviderImages>,
}

impl CreateImagesProviderOptions {
    /// Required fields; `models` defaults to empty and `refresh_models` to
    /// static.
    pub fn new(id: impl Into<String>, auth: ProviderAuth, api: Arc<dyn ProviderImages>) -> Self {
        Self {
            id: id.into(),
            name: None,
            auth,
            models: Vec::new(),
            refresh_models: None,
            api,
        }
    }

    /// Override the display name (defaults to `id`).
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the initial model list.
    pub fn models(mut self, models: Vec<ImagesModel>) -> Self {
        self.models = models;
        self
    }

    /// Mark the provider dynamic and install its model-list fetcher.
    pub fn refresh_models(mut self, refresh: RefreshModelsFn) -> Self {
        self.refresh_models = Some(refresh);
        self
    }
}

/// One shared in-flight fetch; clones await the same underlying future.
type SharedRefresh = Shared<BoxFuture<'static, Result<Vec<ImagesModel>, ImagesRefreshError>>>;

struct CreatedImagesProvider {
    id: String,
    name: String,
    auth: ProviderAuth,
    api: Arc<dyn ProviderImages>,
    models: RwLock<Vec<ImagesModel>>,
    refresh_models: Option<RefreshModelsFn>,
    inflight: Mutex<Option<SharedRefresh>>,
}

impl CreatedImagesProvider {
    /// Return the in-flight fetch, starting one when none is registered.
    fn shared_refresh(&self) -> SharedRefresh {
        let mut inflight = self
            .inflight
            .lock()
            .expect("images provider refresh lock poisoned");
        match inflight.as_ref() {
            Some(shared) => shared.clone(),
            None => {
                let shared = (self
                    .refresh_models
                    .as_ref()
                    .expect("shared_refresh called without a fetcher"))(
                )
                .shared();
                *inflight = Some(shared.clone());
                shared
            }
        }
    }

    /// Drop the registration once this fetch settles, so the next call
    /// starts a fresh one. Every waiter calls this; only the registration
    /// still pointing at this future is cleared.
    fn clear_inflight(&self, shared: &SharedRefresh) {
        let mut inflight = self
            .inflight
            .lock()
            .expect("images provider refresh lock poisoned");
        if inflight
            .as_ref()
            .is_some_and(|current| current.ptr_eq(shared))
        {
            *inflight = None;
        }
    }
}

#[async_trait]
impl ImagesProvider for CreatedImagesProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<ImagesModel> {
        self.models
            .read()
            .expect("images provider models lock poisoned")
            .clone()
    }

    async fn refresh_models(&self) -> Result<(), ImagesRefreshError> {
        if self.refresh_models.is_none() {
            return Ok(());
        }
        let shared = self.shared_refresh();
        let result = shared.clone().await;
        self.clear_inflight(&shared);

        let models = result?;
        *self
            .models
            .write()
            .expect("images provider models lock poisoned") = models;
        Ok(())
    }

    async fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: &ImagesOptions,
    ) -> AssistantImages {
        self.api.generate_images(model, context, options).await
    }
}

/// Build an image-generation provider from parts (upstream
/// `createImagesProvider`).
///
/// When `refresh_models` is provided, concurrent `refresh_models()` calls
/// share one in-flight fetch; the next call after it settles starts a fresh
/// one.
pub fn create_images_provider(input: CreateImagesProviderOptions) -> Arc<dyn ImagesProvider> {
    let name = input.name.unwrap_or_else(|| input.id.clone());
    Arc::new(CreatedImagesProvider {
        id: input.id,
        name,
        auth: input.auth,
        api: input.api,
        models: RwLock::new(input.models),
        refresh_models: input.refresh_models,
        inflight: Mutex::new(None),
    })
}
