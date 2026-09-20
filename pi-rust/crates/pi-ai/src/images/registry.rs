//! Image API adapter registry — port of
//! `packages/ai/src/images-api-registry.ts`.
//!
//! Upstream keeps a module-global `Map<api, provider>` populated by
//! `providers/images/register-builtins.ts` at import time. The Rust port uses
//! a process-global [`RwLock`] and an explicit
//! [`crate::images::builtins::register_builtin_images_api_providers`] call
//! (the module-level facade triggers it lazily), because Rust has no
//! import-time side effects.
//!
//! # Deliberate differences
//!
//! * Upstream takes a provider object (`{ api, generateImages }`) plus an
//!   optional `sourceId`; the Rust signature is
//!   `(api, Arc<dyn ProviderImages>, Option<String>)`.
//! * Mismatch handling differs (see below).
//!
//! # Deliberate difference: mismatch handling
//!
//! Upstream's `registerImagesApiProvider` wraps `generateImages` in a
//! function that **throws** `Mismatched api: …` when `model.api` is not the
//! api the adapter was registered under; the promise rejection propagates out
//! of the facade. Because [`ProviderImages::generate_images`] is infallible
//! here, the wrapper instead returns an [`AssistantImages`] with
//! [`ImagesStopReason::Error`] and the same message. Callers that need to
//! distinguish "misconfigured adapter" from "provider error" must inspect the
//! message.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;

use super::types::{
    AssistantImages, ImagesApi, ImagesContext, ImagesModel, ImagesOptions, ProviderImages,
};

/// A registered image adapter plus the id it was registered under, kept so
/// tests and diagnostics can list what is available.
#[derive(Clone)]
struct Registered {
    provider: Arc<dyn ProviderImages>,
    api: ImagesApi,
    source_id: Option<String>,
}

/// Wraps an adapter so it can only ever be invoked for the api it was
/// registered under.
struct ApiCheckedAdapter {
    api: ImagesApi,
    inner: Arc<dyn ProviderImages>,
}

#[async_trait]
impl ProviderImages for ApiCheckedAdapter {
    async fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: &ImagesOptions,
    ) -> AssistantImages {
        if model.api != self.api {
            return AssistantImages::error(
                model.api.clone(),
                model.provider.to_string(),
                model.id.clone(),
                format!("Mismatched api: {} expected {}", model.api, self.api),
            );
        }
        self.inner.generate_images(model, context, options).await
    }
}

fn registry() -> &'static RwLock<HashMap<ImagesApi, Registered>> {
    static REGISTRY: OnceLock<RwLock<HashMap<ImagesApi, Registered>>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register (or replace) the adapter for `api`.
///
/// `source_id` is upstream's optional provenance tag (used by the TUI to show
/// which provider source registered an api); the Rust port stores it for
/// diagnostics.
pub fn register_images_api_provider(
    api: impl Into<ImagesApi>,
    generate_images: Arc<dyn ProviderImages>,
    source_id: Option<String>,
) {
    let api = api.into();
    let entry = Registered {
        provider: Arc::new(ApiCheckedAdapter {
            api: api.clone(),
            inner: generate_images,
        }),
        api: api.clone(),
        source_id,
    };
    registry()
        .write()
        .expect("images api registry lock poisoned")
        .insert(api, entry);
}

/// Look up the adapter registered for `api`.
pub fn get_images_api_provider(api: &str) -> Option<Arc<dyn ProviderImages>> {
    registry()
        .read()
        .expect("images api registry lock poisoned")
        .get(api)
        .map(|entry| entry.provider.clone())
}

/// Every registered api id, in unspecified order.
pub fn registered_images_api_providers() -> Vec<ImagesApi> {
    let mut apis: Vec<ImagesApi> = registry()
        .read()
        .expect("images api registry lock poisoned")
        .keys()
        .cloned()
        .collect();
    apis.sort();
    apis
}

/// Remove every registration. Exposed for tests and for hosts that want to
/// rebuild the registry from scratch.
pub fn clear_images_api_providers() {
    registry()
        .write()
        .expect("images api registry lock poisoned")
        .clear();
}

/// Provenance tag recorded when `api` was registered (upstream's optional
/// `sourceId`).
pub fn image_api_provider_source_id(api: &str) -> Option<String> {
    registry()
        .read()
        .expect("images api registry lock poisoned")
        .get(api)
        .and_then(|entry| entry.source_id.clone())
}

/// The api an adapter was registered under, for a provider handle obtained
/// from [`get_images_api_provider`].
pub fn registered_api_of(api: &str) -> Option<ImagesApi> {
    registry()
        .read()
        .expect("images api registry lock poisoned")
        .get(api)
        .map(|entry| entry.api.clone())
}
