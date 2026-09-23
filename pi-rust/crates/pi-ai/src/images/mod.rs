//! Image generation — the Rust port of `packages/ai/src/images.ts`, plus the
//! image slice of `types.ts`, `images-api-registry.ts`, `image-models.ts`,
//! `images-models.ts`, `providers/images/register-builtins.ts` and
//! `api/openrouter-images.ts`.
//!
//! Upstream exposes a single `generateImages(model, context, options)`
//! facade that resolves the adapter registered for `model.api` and delegates
//! to it. The lifecycle mirrors the chat side (`stream()`), but the result is
//! a plain [`AssistantImages`] value instead of an event stream:
//!
//! ```no_run
//! use pi_ai::images::{
//!     generate_images, ImagesContext, ImagesModel, ImagesOptions, OPENROUTER_IMAGES_API,
//! };
//!
//! # async fn example() -> Result<(), pi_ai::images::ImagesError> {
//! let model = ImagesModel::new(
//!     "openrouter",
//!     "google/gemini-2.5-flash-image-preview",
//!     OPENROUTER_IMAGES_API,
//!     "https://openrouter.ai/api/v1",
//! );
//! let context = ImagesContext::text("a red panda in a spacesuit");
//! let options = ImagesOptions {
//!     api_key: Some("sk-...".to_string()),
//!     ..Default::default()
//! };
//! let result = generate_images(&model, &context, &options).await?;
//! assert!(matches!(result.stop_reason, pi_ai::images::ImagesStopReason::Stop));
//! # Ok(())
//! # }
//! ```
//!
//! # Port status and deliberate differences
//!
//! Ported: the request/response contract, the adapter registry (including the
//! upstream "Mismatched api" guard), the OpenRouter adapter (params, usage
//! math, data-URL response parsing, retry/error-body handling), the model
//! catalog surface, built-in registration, and the auth-aware provider
//! runtime collection ([`ImagesProvider`] / [`ImagesModels`] / [`runtime`]).
//!
//! Not ported, on purpose or pending a follow-up round:
//!
//! * The bundled `image-models.generated.ts` catalog — the host loads it via
//!   [`ImageModels::register_provider_json`] (see [`models`]).
//! * `onPayload` / `onResponse` hooks and `fetch` injection: the Rust
//!   options struct is narrowed to what the adapters use.
//! * OpenRouter is the only image adapter upstream ships, so it is the only
//!   one here. A second adapter would only need
//!   [`register_images_api_provider`].

pub mod builtins;
pub mod models;
pub mod openrouter;
pub mod registry;
pub mod runtime;
pub mod types;

pub use models::ImageModels;
pub use openrouter::{
    build_params, OpenRouterImagesProvider, OPENROUTER_IMAGES_API, OPENROUTER_IMAGES_BASE_URL,
    OPENROUTER_PROVIDER_ID,
};
pub use registry::{
    clear_images_api_providers, get_images_api_provider, image_api_provider_source_id,
    register_images_api_provider, registered_api_of, registered_images_api_providers,
};
pub use runtime::{
    create_images_models, create_images_provider, AuthTarget, CreateImagesModelsOptions,
    CreateImagesProviderOptions, ImagesModels, ImagesProvider, ImagesRefreshError,
    MutableImagesModels, RefreshModelsFn,
};
pub use types::{
    AssistantImages, ImageModality, ImageModelCost, ImagesApi, ImagesContext, ImagesError,
    ImagesInputContent, ImagesModel, ImagesOptions, ImagesOutputContent, ImagesStopReason,
    ImagesUsage, ProviderImages, UsageCost,
};

/// Resolve the adapter for `model.api` and generate images.
///
/// Returns [`ImagesError::NoApiProvider`] when nothing is registered for the
/// api. Every adapter-level failure (missing key, HTTP error, cancellation)
/// is reported inside the returned [`AssistantImages`], matching upstream's
/// "image providers never reject" contract.
///
/// Triggers built-in adapter registration on first use, so callers do not
/// have to call
/// [`builtins::register_builtin_images_api_providers`] themselves.
pub async fn generate_images(
    model: &ImagesModel,
    context: &ImagesContext,
    options: &ImagesOptions,
) -> Result<AssistantImages, ImagesError> {
    builtins::ensure_registered();
    let provider = registry::get_images_api_provider(&model.api)
        .ok_or_else(|| ImagesError::NoApiProvider(model.api.clone()))?;
    Ok(provider.generate_images(model, context, options).await)
}
