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
        // String overload: no native `Provider` object, no `streamSimple`
        // handler, no `oauth` block.
        native: false,
        has_stream_simple: false,
        oauth: None,
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


#[test]
fn registering_with_snake_case_model_fields_is_normalised() {
    // The Rust port's native catalog format is snake_case (`context_window` /
    // `max_output_tokens`). The bridge accepts both spellings because plugin
    // authors writing JSON manually sometimes use one or the other — TS
    // extensions tend to use camelCase to match the upstream type.
    let mut router = ProviderRouter::from_env_with(|_| None);
    let mut models = Models::new();
    let env = |name: &str| (name == "PROXY_KEY").then(|| "sk-test".to_string());
    let providers = vec![RegisteredProviderConfig {
        name: "snake-proxy".to_string(),
        display_name: Some("Snake Proxy".to_string()),
        base_url: Some("https://snake.example.com/v1".to_string()),
        api_key: Some("$PROXY_KEY".to_string()),
        api: Some("openai-completions".to_string()),
        models: serde_json::json!([
            {
                "id": "snake-model",
                "label": "Snake Model",
                "context_window": 64000,
                "max_output_tokens": 2048,
            },
        ]),
        native: false,
        has_stream_simple: false,
        oauth: None,
    }];

    let applied =
        apply_registered_providers_with_env(&mut router, &mut models, &providers, &env);
    assert_eq!(applied, vec!["snake-proxy".to_string()]);
    let m = models
        .get_model(&ProviderId::new("snake-proxy"), "snake-model")
        .expect("snake_model");
    assert_eq!(m.label.as_deref(), Some("Snake Model"));
    assert_eq!(m.context_window, 64_000);
    assert_eq!(m.max_output_tokens, 2_048);
    assert_eq!(m.api, Api::OpenAiChatCompletions);
}

#[test]
fn registering_multiple_models_in_one_provider_makes_each_resolvable() {
    let mut router = ProviderRouter::from_env_with(|_| None);
    let mut models = Models::new();
    let env = |name: &str| (name == "PROXY_KEY").then(|| "sk-test".to_string());
    let providers = vec![RegisteredProviderConfig {
        name: "multi-proxy".to_string(),
        display_name: Some("Multi".to_string()),
        base_url: Some("https://multi.example.com/v1".to_string()),
        api_key: Some("$PROXY_KEY".to_string()),
        api: Some("openai-completions".to_string()),
        models: serde_json::json!([
            {"id": "alpha", "name": "Alpha", "contextWindow": 32000, "maxTokens": 1024},
            {"id": "beta",  "name": "Beta",  "contextWindow": 64000, "maxTokens": 2048},
            {"id": "gamma", "name": "Gamma", "contextWindow": 128000, "maxTokens": 4096},
        ]),
        native: false,
        has_stream_simple: false,
        oauth: None,
    }];

    apply_registered_providers_with_env(&mut router, &mut models, &providers, &env);
    for id in ["alpha", "beta", "gamma"] {
        assert!(
            models
                .get_model(&ProviderId::new("multi-proxy"), id)
                .is_some(),
            "expected model {id} to be registered"
        );
    }
    let gamma = models
        .get_model(&ProviderId::new("multi-proxy"), "gamma")
        .unwrap();
    assert_eq!(gamma.context_window, 128_000);
}

#[test]
fn per_model_api_hint_overrides_the_provider_family() {
    // An extension can mix wire protocols inside one provider — e.g. an
    // OpenAI-compatible base that also exposes a Responses endpoint.
    let mut router = ProviderRouter::from_env_with(|_| None);
    let mut models = Models::new();
    let env = |name: &str| (name == "PROXY_KEY").then(|| "sk-test".to_string());
    let providers = vec![RegisteredProviderConfig {
        name: "mixed-proxy".to_string(),
        display_name: Some("Mixed".to_string()),
        base_url: Some("https://mixed.example.com".to_string()),
        api_key: Some("$PROXY_KEY".to_string()),
        api: Some("openai-completions".to_string()),
        models: serde_json::json!([
            {"id": "chat", "name": "Chat", "contextWindow": 32000, "maxTokens": 1024},
            {"id": "resp", "name": "Resp", "contextWindow": 32000, "maxTokens": 1024, "api": "openai-responses"},
        ]),
        native: false,
        has_stream_simple: false,
        oauth: None,
    }];

    apply_registered_providers_with_env(&mut router, &mut models, &providers, &env);
    assert_eq!(
        models
            .get_model(&ProviderId::new("mixed-proxy"), "chat")
            .unwrap()
            .api,
        Api::OpenAiChatCompletions,
    );
    assert_eq!(
        models
            .get_model(&ProviderId::new("mixed-proxy"), "resp")
            .unwrap()
            .api,
        Api::OpenAiResponses,
    );
}

#[test]
fn empty_models_array_is_a_silent_no_op_for_the_catalog() {
    // Pure base-URL override with no models — the provider still gets
    // registered with the router (so `pi --model my-proxy/foo` errors),
    // and the catalog simply has no entries for it.
    let mut router = ProviderRouter::from_env_with(|_| None);
    let mut models = Models::new();
    let env = |name: &str| (name == "PROXY_KEY").then(|| "sk-test".to_string());
    let providers = vec![RegisteredProviderConfig {
        name: "no-models".to_string(),
        display_name: Some("No Models".to_string()),
        base_url: Some("https://empty.example.com".to_string()),
        api_key: Some("$PROXY_KEY".to_string()),
        api: Some("openai-completions".to_string()),
        models: serde_json::json!([]),
        native: false,
        has_stream_simple: false,
        oauth: None,
    }];

    let applied =
        apply_registered_providers_with_env(&mut router, &mut models, &providers, &env);
    assert_eq!(applied, vec!["no-models".to_string()]);
    assert!(router.has_provider("no-models"));
    let entries: Vec<_> = models
        .iter()
        .filter(|(p, _)| p.0 == "no-models")
        .collect();
    assert!(entries.is_empty(), "no entries expected, got {entries:?}");
}

#[test]
fn apply_registered_providers_can_be_called_twice_with_different_overrides() {
    // Idempotency: re-applying with a fresh provider (different name) must
    // not stomp on prior entries — the catalog should carry both.
    let mut router = ProviderRouter::from_env_with(|_| None);
    let mut models = Models::new();
    let env = |name: &str| (name == "PROXY_KEY").then(|| "sk-test".to_string());
    let providers = vec![
        provider_config("first-proxy", "openai-completions", "$PROXY_KEY"),
        provider_config("second-proxy", "openai-completions", "$PROXY_KEY"),
    ];

    apply_registered_providers_with_env(&mut router, &mut models, &providers, &env);
    apply_registered_providers_with_env(&mut router, &mut models, &providers, &env);
    assert!(router.has_provider("first-proxy"));
    assert!(router.has_provider("second-proxy"));
    assert!(models
        .get_model(&ProviderId::new("first-proxy"), "proxy-model")
        .is_some());
    assert!(models
        .get_model(&ProviderId::new("second-proxy"), "proxy-model")
        .is_some());
}

