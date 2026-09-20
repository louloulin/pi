//! Built-in image API adapter registration — port of
//! `packages/ai/src/providers/images/register-builtins.ts`.
//!
//! Upstream registers `openrouter-images` at import time, behind a dynamic
//! `import()` so the OpenAI SDK is only loaded when an image call happens.
//! The Rust port registers the compiled-in adapter from
//! [`register_builtin_images_api_providers`], and the module-level
//! [`super::generate_images`] facade calls
//! [`ensure_registered`] lazily so callers never have to remember to.

use std::sync::{Arc, Once};

use super::openrouter::{OpenRouterImagesProvider, OPENROUTER_IMAGES_API};
use super::registry::register_images_api_provider;

/// Register every built-in image adapter. Idempotent: re-registering simply
/// replaces the existing entry.
pub fn register_builtin_images_api_providers() {
    register_images_api_provider(
        OPENROUTER_IMAGES_API,
        Arc::new(OpenRouterImagesProvider::new()),
        Some("builtin".to_string()),
    );
}

static REGISTER_BUILTINS: Once = Once::new();

/// Run [`register_builtin_images_api_providers`] at most once per process.
///
/// Called by the facade before every lookup. Note that
/// [`super::registry::clear_images_api_providers`] therefore makes the
/// built-ins unavailable to later facade calls in the same process; hosts
/// that clear the registry must call
/// [`register_builtin_images_api_providers`] again.
pub(crate) fn ensure_registered() {
    REGISTER_BUILTINS.call_once(register_builtin_images_api_providers);
}
