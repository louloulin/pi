//! `wasm-bindgen` exports for the `pi-ai` crate.
//!
//! The JS host calls [`register_faux_provider`] once on startup to seed
//! the in-memory model catalog with a faux provider and the
//! [`StreamFn`](crate::StreamFn) implementation that backs it. Both
//! pieces live in [`thread_local!`] cells because `wasm32-unknown-unknown`
//! is single-threaded — every call into the WASM module runs on the same
//! thread, so per-thread storage is sufficient (and avoids the
//! `Send + 'static` constraints `OnceLock` would impose).
//!
//! Consumers (notably `pi-agent-core`'s `AgentHandle`) read the
//! catalog and the stream function from the same cells, so a single
//! `register_faux_provider` call at host startup lights up the entire
//! agent runtime.

// The entire module is `wasm32`-only — the deps it pulls in
// (`wasm-bindgen`, `js-sys`) are linked only on the wasm target. The
// `wasm` feature is the user-facing opt-in flag but the conditional
// compilation is gated on the target so native builds skip the whole
// module entirely.
#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use js_sys::Object;
use pi_protocol::{Model, ProviderId};
use wasm_bindgen::prelude::*;

use crate::models::Models;
use crate::providers::faux::FauxProvider;
use crate::stream::StreamFn;

/// Catalog of models the JS host has registered.
type Catalog = Rc<RefCell<Option<Models>>>;

/// Stream function the JS host registered for `faux:` models.
type SharedFaux = Rc<RefCell<Option<Arc<FauxProvider>>>>;

thread_local! {
    static MODELS: RefCell<Catalog> = RefCell::new(Rc::new(RefCell::new(None)));
    static FAUX: RefCell<SharedFaux> = RefCell::new(Rc::new(RefCell::new(None)));
}

/// Borrow the live catalog. Returns [`None`] when no provider has been
/// registered yet.
pub fn with_catalog<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&Models) -> R,
{
    let catalog = MODELS.with(|cell| cell.borrow().clone());
    let guard = catalog.borrow();
    guard.as_ref().map(f)
}

/// Borrow the registered faux provider (if any). Returned as a
/// `StreamFn`-trait-object so consumers can drop it into an
/// `Arc<dyn StreamFn>`.
pub fn with_faux<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&Arc<FauxProvider>) -> R,
{
    let faux = FAUX.with(|cell| cell.borrow().clone());
    let guard = faux.borrow();
    guard.as_ref().map(f)
}

/// Look up a model by its `(provider, id)` pair.
///
/// The provider is parsed from the leading `provider:` segment of
/// `model_id` (e.g. `"faux:faux-model"`). When no prefix is present,
/// the lookup falls back to scanning every registered provider.
#[wasm_bindgen]
pub fn lookup_model(model_id: String) -> Result<JsValue, JsValue> {
    let (provider_hint, id) = match model_id.split_once(':') {
        Some((p, i)) => (Some(p.to_string()), i.to_string()),
        None => (None, model_id.clone()),
    };
    let model = with_catalog(|catalog| {
        if let Some(provider) = &provider_hint {
            catalog.get_model(&ProviderId::new(provider), &id).cloned()
        } else {
            catalog
                .iter()
                .find(|(_, m)| m.id == id)
                .map(|(_, m)| m.clone())
        }
    });
    match model {
        Some(m) => serde_wasm_bindgen::to_value(&m)
            .map_err(|e| JsValue::from_str(&format!("serialize model: {e}"))),
        None => Err(JsValue::from_str(&format!("model not found: {model_id}"))),
    }
}

/// Register a faux provider in the global [`Models`] catalog.
///
/// The `responses` argument is a JS array of strings — each string is
/// streamed as a single faux assistant text reply. The first reply is
/// returned for the first `prompt`, the second for the second, and so on.
/// The last reply is reused for any subsequent prompts.
///
/// Calling this replaces any previously-registered catalog — the JS
/// host is expected to call it once at startup.
#[wasm_bindgen]
pub fn register_faux_provider(responses: Option<Vec<JsValue>>) -> Result<JsValue, JsValue> {
    let scripts = match responses {
        Some(values) => values
            .into_iter()
            .map(|value| {
                if value.is_undefined() || value.is_null() {
                    Ok(String::new())
                } else {
                    value.as_string().ok_or_else(|| {
                        JsValue::from_str("register_faux_provider: response must be a string")
                    })
                }
            })
            .collect::<Result<Vec<_>, _>>()?,
        None => Vec::new(),
    };

    let faux = FauxProvider::with_scripts(scripts);
    let faux_arc = Arc::new(faux.clone());

    let mut models = Models::new();
    models.set_provider(faux.provider_id(), faux.models());

    MODELS.with(|cell| {
        *cell.borrow().borrow_mut() = Some(models);
    });
    FAUX.with(|cell| {
        *cell.borrow().borrow_mut() = Some(faux_arc);
    });

    // Surface a summary object the JS host can introspect.
    let summary = Object::new();
    let provider_id = faux.provider_id();
    let _ = js_sys::Reflect::set(
        &summary,
        &JsValue::from_str("provider"),
        &JsValue::from_str(&provider_id.0),
    );
    let model_ids: Vec<JsValue> = faux
        .models()
        .iter()
        .map(|m| JsValue::from_str(&m.id))
        .collect();
    let model_array = js_sys::Array::from_iter(model_ids);
    let _ = js_sys::Reflect::set(&summary, &JsValue::from_str("models"), &model_array);
    Ok(summary.into())
}

/// Snapshot the registered models as a JS object.
///
/// Useful for the JS host's debug overlay — the value is the same shape
/// as the `Models` table, with one entry per provider.
#[wasm_bindgen]
pub fn list_models() -> Result<JsValue, JsValue> {
    let snapshot = with_catalog(|catalog| {
        let mut grouped: HashMap<String, Vec<Model>> = HashMap::new();
        for (provider, model) in catalog.iter() {
            grouped
                .entry(provider.0.clone())
                .or_default()
                .push(model.clone());
        }
        grouped
    })
    .ok_or_else(|| JsValue::from_str("no provider registered"))?;

    let out = Object::new();
    for (provider, models) in snapshot {
        let array = js_sys::Array::from_iter(
            models
                .iter()
                .map(|m| serde_wasm_bindgen::to_value(m).unwrap_or(JsValue::NULL)),
        );
        let _ = js_sys::Reflect::set(&out, &JsValue::from_str(&provider), array.as_ref());
    }
    Ok(out.into())
}

/// Public hook for `pi-agent-core`'s `AgentHandle` to fetch the registered
/// faux provider's [`StreamFn`] trait-object. Returns `None` when no
/// provider has been registered.
pub fn faux_stream_fn() -> Option<Arc<dyn StreamFn>> {
    with_faux(|faux| faux.clone() as Arc<dyn StreamFn>)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Quick smoke check — calling `register_faux_provider` from native
    /// Rust sets the catalog so `with_catalog` returns `Some(_)`.
    #[test]
    fn register_seeds_catalog() {
        let faux = FauxProvider::with_scripts(vec!["hi".into()]);
        let mut models = Models::new();
        models.set_provider(faux.provider_id(), faux.models());
        MODELS.with(|cell| {
            *cell.borrow().borrow_mut() = Some(models);
        });
        with_catalog(|c| {
            assert!(c
                .get_model(&ProviderId::new("faux"), "faux-model")
                .is_some());
        });
    }
}
