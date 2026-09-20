//! `pi.registerProvider` / `pi.unregisterProvider` end-to-end through the
//! JS shim: an extension registers one or more providers during load and the
//! host exposes the raw configs for the application layer to resolve.
//!
//! The host-op validation cases (unknown `api`, `models` without `api`,
//! overwrite / unregister semantics) live in `tests/host.rs`; this file
//! covers the load-pipeline shape across several extensions.

use std::path::PathBuf;

use pi_extensions::{ExtensionEntry, JsExtensionHost};

fn entry(id: &str) -> ExtensionEntry {
    ExtensionEntry {
        source: PathBuf::from(format!("/tmp/pi_extensions_register_provider/{id}.js")),
        id: id.to_string(),
        label: None,
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime")
}

#[test]
fn register_provider_round_trips_through_the_load_pipeline() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        host.load(
            entry("proxy_ext"),
            r#"
                module.exports = function (pi) {
                    pi.registerProvider("my-proxy", {
                        name: "My Proxy",
                        baseUrl: "https://proxy.example.com/v1",
                        apiKey: "${MY_PROXY_KEY}",
                        api: "openai-completions",
                        models: [
                            { id: "proxy-small", name: "Proxy Small", contextWindow: 64000, maxTokens: 2048 },
                        ],
                    });
                };
            "#,
        )
        .await
        .expect("load proxy extension");

        // A second extension with a baseUrl-only override (no api / models).
        host.load(
            entry("base_url_ext"),
            r#"
                module.exports = function (pi) {
                    pi.registerProvider("corp-gateway", { baseUrl: "https://gw.example.com" });
                };
            "#,
        )
        .await
        .expect("load base-url extension");

        let providers = host.registered_providers();
        assert_eq!(
            providers.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["my-proxy", "corp-gateway"],
            "load order is preserved: {providers:?}"
        );

        let proxy = &providers[0];
        assert_eq!(proxy.display_name.as_deref(), Some("My Proxy"));
        assert_eq!(
            proxy.base_url.as_deref(),
            Some("https://proxy.example.com/v1")
        );
        assert_eq!(proxy.api_key.as_deref(), Some("${MY_PROXY_KEY}"));
        assert_eq!(proxy.api.as_deref(), Some("openai-completions"));
        assert_eq!(proxy.models[0]["id"], "proxy-small");

        let gateway = &providers[1];
        assert!(gateway.api.is_none());
        assert!(gateway.models.is_null(), "{gateway:?}");
        assert_eq!(gateway.base_url.as_deref(), Some("https://gw.example.com"));
    });
}

#[test]
fn no_extensions_means_no_registered_providers() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        assert!(host.registered_providers().is_empty());
    });
}
