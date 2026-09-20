//! Provider stubs. Stage 1 replaces these with real streaming adapters.

pub mod anthropic;
pub mod faux;
pub mod google;
pub mod mistral;
pub mod openai;
pub mod openai_responses;
pub mod registry;

pub use registry::{find_provider, ProviderSpec, BUILTIN_PROVIDERS};
