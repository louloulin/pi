//! Image-model catalog — port of `packages/ai/src/image-models.ts`.
//!
//! Upstream seeds the registry from `image-models.generated.ts` at module
//! load. The Rust port keeps the same lookup surface but **does not ship the
//! generated catalog**: consistent with the chat-side
//! [`crate::models`] decision, the static data is loaded by the host through
//! [`ImageModels::register_provider`] / [`ImageModels::register_provider_json`],
//! which makes the port independent of upstream's generated-file churn.
//!
//! # Deliberate differences from upstream
//!
//! * No bundled generated catalog (see above).
//! * Entries are stored per provider in insertion order rather than as one
//!   flat `Map<provider/model, model>`, which is what every caller needs
//!   (`getImageProviders`, `getImageModels`, `getImageModel`).

use std::collections::HashMap;

use super::types::ImagesModel;

/// A provider-keyed image-model registry.
///
/// ```no_run
/// use pi_ai::images::{ImageModels, ImagesModel, OPENROUTER_IMAGES_API};
///
/// let mut models = ImageModels::new();
/// models.register_provider(
///     "openrouter",
///     vec![ImagesModel::new(
///         "openrouter",
///         "google/gemini-2.5-flash-image-preview",
///         OPENROUTER_IMAGES_API,
///         "https://openrouter.ai/api/v1",
///     )],
/// );
/// assert!(models.get_image_model("openrouter", "google/gemini-2.5-flash-image-preview").is_some());
/// ```
#[derive(Debug, Clone, Default)]
pub struct ImageModels {
    by_provider: HashMap<String, Vec<ImagesModel>>,
}

impl ImageModels {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the models registered for `provider`. A later call for the
    /// same provider overwrites the earlier list, matching upstream's
    /// `registerProvider` overwrite semantics.
    pub fn register_provider(&mut self, provider: impl Into<String>, models: Vec<ImagesModel>) {
        self.by_provider.insert(provider.into(), models);
    }

    /// Parse the JSON array upstream stores per provider (the body of
    /// `image-models.generated.ts`) and register it for `provider`.
    ///
    /// Both a bare array (`[{…}, …]`) and an envelope (`{"models": […]}`)
    /// are accepted. Entry fields mirror upstream's generated records; `id`
    /// and `api` are required, everything else has a default.
    pub fn register_provider_json(
        &mut self,
        provider: impl Into<String>,
        json: &str,
    ) -> Result<(), serde_json::Error> {
        let provider = provider.into();
        let value: serde_json::Value = serde_json::from_str(json)?;
        let list = match value {
            serde_json::Value::Array(list) => list,
            serde_json::Value::Object(mut object) => object
                .remove("models")
                .and_then(|models| models.as_array().cloned())
                .unwrap_or_default(),
            _ => Vec::new(),
        };

        let mut models = Vec::with_capacity(list.len());
        for entry in list {
            let id = entry["id"].as_str().unwrap_or_default().to_string();
            if id.is_empty() {
                continue;
            }
            let api = entry["api"].as_str().unwrap_or_default().to_string();
            let base_url = entry["base_url"]
                .as_str()
                .or_else(|| entry["baseUrl"].as_str())
                .unwrap_or_default()
                .to_string();
            let mut model = ImagesModel::new(provider.clone(), id, api, base_url);
            model.label = entry["name"]
                .as_str()
                .or_else(|| entry["label"].as_str())
                .map(str::to_string);
            models.push(model);
        }
        self.register_provider(provider, models);
        Ok(())
    }

    /// Look up one model by provider and id.
    pub fn get_image_model(&self, provider: &str, id: &str) -> Option<&ImagesModel> {
        self.by_provider
            .get(provider)?
            .iter()
            .find(|model| model.id == id)
    }

    /// Every model registered for `provider`, in registration order.
    pub fn get_image_models(&self, provider: &str) -> &[ImagesModel] {
        self.by_provider
            .get(provider)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Every provider with at least one model, sorted.
    pub fn get_image_providers(&self) -> Vec<String> {
        let mut providers: Vec<String> = self
            .by_provider
            .iter()
            .filter(|(_, models)| !models.is_empty())
            .map(|(provider, _)| provider.clone())
            .collect();
        providers.sort();
        providers
    }

    /// Total number of registered models.
    pub fn len(&self) -> usize {
        self.by_provider.values().map(Vec::len).sum()
    }

    /// Whether no model is registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
