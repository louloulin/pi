//! Provider stubs. Stage 1 replaces these with real streaming adapters.

pub mod anthropic;
pub mod azure_openai_responses;
pub mod faux;
pub mod google;
pub mod mistral;
pub mod openai;
pub mod openai_responses;
pub mod registry;

pub use azure_openai_responses::{
    parse_deployment_name_map, AzureOpenAiResponsesProvider, DEFAULT_API_VERSION,
};
pub use registry::{find_provider, ProviderSpec, BUILTIN_PROVIDERS};
