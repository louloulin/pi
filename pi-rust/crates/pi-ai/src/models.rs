//! Model catalog loader — typed wrapper over the catalog JSON file.
//!
//! Stage 0 ships an in-memory stub populated from a JSON literal. Stage 1
//! replaces it with a deserialised loader that reads from
//! `models.generated.json` (parity with `packages/ai/src/models.generated.ts`).

use std::collections::HashMap;

use pi_protocol::{Api, Model, ProviderId};

/// In-memory model catalog.
#[derive(Debug, Clone, Default)]
pub struct Models {
    by_provider: HashMap<ProviderId, Vec<Model>>,
}

impl Models {
    /// Construct an empty catalog.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a provider's models. Replaces any prior registration.
    pub fn set_provider(&mut self, provider: ProviderId, models: Vec<Model>) {
        self.by_provider.insert(provider, models);
    }

    /// Look up a model by `(provider, id)`.
    pub fn get_model(&self, provider: &ProviderId, id: &str) -> Option<&Model> {
        self.by_provider
            .get(provider)
            .and_then(|models| models.iter().find(|m| m.id == id))
    }

    /// Iterator over all registered models.
    pub fn iter(&self) -> impl Iterator<Item = (&ProviderId, &Model)> {
        self.by_provider
            .iter()
            .flat_map(|(p, ms)| ms.iter().map(move |m| (p, m)))
    }

    /// Build a stub catalog with the faux provider registered.
    ///
    /// Lets `pi-agent-core` tests compile against `pi-ai` before Stage 1
    /// finishes the real provider ports.
    pub fn with_faux() -> Self {
        let mut models = Self::new();
        models.set_provider(
            ProviderId::new("faux"),
            vec![Model {
                provider: ProviderId::new("faux"),
                id: "faux-model".into(),
                api: Api::Faux,
                label: Some("Faux test model".into()),
                context_window: 8192,
                max_output_tokens: 1024,
            }],
        );
        models
    }
}
