//! Application-layer wiring for `pi.registerProvider`: an extension-declared
//! provider becomes a `ProviderRouter` adapter plus model-catalog entries.
//!
//! Every test injects the environment (no process mutation) and uses a fake
//! `baseUrl`; nothing here touches the network.

use pi_ai::models::Models;
use pi_coding_agent::extensions::wiring::apply_registered_providers_with_env;
use pi_coding_agent::provider::{resolve_extension_api_key, ProviderRouter};
use pi_extensions::RegisteredProviderConfig;
use pi_protocol::{Api, ProviderId};

fn provider_config(name: &str, api: &str, api_key: &str) -> RegisteredProviderConfig {
    RegisteredProviderConfig {
        name: name.to_string(),
        display_name: Some(name.to_string()),
        base_url: Some("https://proxy.example.com/v1".to_string()),
        api_key: Some(api_key.to_string()),
        api: Some(api.to_string()),
        models: serde_json::json!([
            {
                "id": "proxy-model",
                "name": "Proxy Model",
                "contextWindow": 128000,
                "maxTokens": 4096,
            },
        ]),
    }
}

#[test]
fn registering_an_extension_provider_makes_it_resolvable() {
    let mut router = ProviderRouter::from_env_with(|_| None);
    let mut models = Models::new();
    let env = |name: &str| (name == "PROXY_KEY").then(|| "sk-test".to_string());
    let providers = vec![provider_config(
        "my-proxy",
        "openai-completions",
        "$PROXY_KEY",
    )];

    let applied = apply_registered_providers_with_env(&mut router, &mut models, &providers, &env);
    assert_eq!(applied, vec!["my-proxy".to_string()]);
    assert!(router.provider_ids().contains(&"my-proxy"));
    assert!(router.has_provider("my-proxy"));

    let model = models
        .get_model(&ProviderId::new("my-proxy"), "proxy-model")
        .expect("extension model registered");
    assert_eq!(model.api, Api::OpenAiChatCompletions);
    assert_eq!(model.context_window, 128_000);
    assert_eq!(model.max_output_tokens, 4_096);

    router
        .require(model)
        .expect("registered provider resolves without UnsupportedProvider");
}

#[test]
fn unresolved_api_key_env_skips_the_provider() {
    let mut router = ProviderRouter::from_env_with(|_| None);
    let mut models = Models::new();
    let providers = vec![provider_config(
        "my-proxy",
        "anthropic-messages",
        "$MISSING_KEY",
    )];

    let applied =
        apply_registered_providers_with_env(&mut router, &mut models, &providers, &|_| None);
    assert!(applied.is_empty());
    assert!(!router.has_provider("my-proxy"));
    assert!(models
        .get_model(&ProviderId::new("my-proxy"), "proxy-model")
        .is_none());
}

#[test]
fn base_url_only_override_adopts_the_builtin_api_family() {
    let mut router = ProviderRouter::from_env_with(|_| None);
    let mut models = Models::new();
    let providers = vec![RegisteredProviderConfig {
        name: "anthropic".to_string(),
        base_url: Some("https://gw.example.com".to_string()),
        ..RegisteredProviderConfig::default()
    }];

    let applied =
        apply_registered_providers_with_env(&mut router, &mut models, &providers, &|_| None);
    assert_eq!(applied, vec!["anthropic".to_string()]);
    assert!(router.has_provider("anthropic"));
}

#[test]
fn unknown_api_is_skipped_without_touching_the_router() {
    let mut router = ProviderRouter::from_env_with(|_| None);
    let mut models = Models::new();
    let providers = vec![provider_config(
        "my-proxy",
        "bedrock-converse",
        "$PROXY_KEY",
    )];

    let applied =
        apply_registered_providers_with_env(&mut router, &mut models, &providers, &|name| {
            (name == "PROXY_KEY").then(|| "sk-test".to_string())
        });
    assert!(applied.is_empty());
    assert!(!router.has_provider("my-proxy"));
}

#[test]
fn unregister_removes_the_adapter_and_the_models() {
    let mut router = ProviderRouter::from_env_with(|_| None);
    let mut models = Models::new();
    let providers = vec![provider_config(
        "my-proxy",
        "openai-completions",
        "literal-key",
    )];

    apply_registered_providers_with_env(&mut router, &mut models, &providers, &|_| None);
    assert!(router.has_provider("my-proxy"));

    assert!(router.unregister_provider("my-proxy"));
    models.set_provider(ProviderId::new("my-proxy"), Vec::new());

    assert!(!router.has_provider("my-proxy"));
    assert!(models
        .get_model(&ProviderId::new("my-proxy"), "proxy-model")
        .is_none());
}

#[test]
fn resolve_extension_api_key_handles_literal_and_env_forms() {
    let env = |name: &str| (name == "PROXY_KEY").then(|| "sk-env".to_string());

    assert_eq!(
        resolve_extension_api_key(Some("sk-literal"), &env).unwrap(),
        Some("sk-literal".to_string())
    );
    assert_eq!(
        resolve_extension_api_key(Some("$PROXY_KEY"), &env).unwrap(),
        Some("sk-env".to_string())
    );
    assert_eq!(
        resolve_extension_api_key(Some("${PROXY_KEY}"), &env).unwrap(),
        Some("sk-env".to_string())
    );
    // Blank / absent means "no key".
    assert_eq!(resolve_extension_api_key(None, &env).unwrap(), None);
    assert_eq!(resolve_extension_api_key(Some("   "), &env).unwrap(), None);
    // A missing env var and the unsupported `!command` form are errors the
    // wiring layer logs before skipping the provider.
    assert!(resolve_extension_api_key(Some("$MISSING"), &env).is_err());
    assert!(resolve_extension_api_key(Some("!op read secret"), &env).is_err());
}
