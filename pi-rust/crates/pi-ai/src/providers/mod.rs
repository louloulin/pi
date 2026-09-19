//! Provider stubs. Stage 1 replaces these with real streaming adapters.

pub mod anthropic;
pub mod faux;
pub mod google;
pub mod openai;
pub mod registry;

pub use registry::{find_provider, ProviderSpec, BUILTIN_PROVIDERS};
