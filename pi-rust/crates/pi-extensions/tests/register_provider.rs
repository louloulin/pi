//! `pi.registerProvider` / `pi.unregisterProvider` end-to-end through the
//! JS shim: an extension registers one or more providers during load and the
//! host exposes the raw configs for the application layer to resolve.
//!
//! The host-op validation cases (unknown `api`, `models` without `api`,
//! overwrite / unregister semantics) live in `tests/host.rs`; this file
//! covers the load-pipeline shape across several extensions.

use std::path::PathBuf;

use pi_extensions::{ExtensionEntry, JsExtensionHost};
use pi_protocol::{AssistantMessageEvent, StopReason};

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

#[test]
fn native_provider_object_overload_round_trips_through_the_load_pipeline() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        host.load(
            entry("native_ext"),
            r#"
                module.exports = function (pi) {
                    pi.registerProvider({
                        id: "native-proxy",
                        name: "Native Proxy",
                        baseUrl: "https://native.example.com/v1",
                        getModels: function () {
                            return [
                                { id: "native-small", name: "Native Small", api: "native-api", contextWindow: 32000, maxTokens: 1024 },
                            ];
                        },
                        oauth: {
                            name: "Native Proxy (subscription)",
                            isSubscription: true,
                            login: async function () { return { type: "oauth", access: "a" }; },
                            refreshToken: async function (credentials) { return credentials; },
                            getApiKey: function (credentials) { return credentials.access; },
                        },
                        streamSimple: function () { return []; },
                    });
                };
            "#,
        )
        .await
        .expect("load native provider extension");

        let providers = host.registered_providers();
        assert_eq!(providers.len(), 1, "{providers:?}");
        let provider = &providers[0];
        assert_eq!(provider.name, "native-proxy");
        assert!(provider.native, "the native overload must be marked");
        assert_eq!(provider.display_name.as_deref(), Some("Native Proxy"));
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://native.example.com/v1")
        );
        // No provider-level `api`: it is taken from the first catalog model.
        assert_eq!(provider.api.as_deref(), Some("native-api"));
        assert!(provider.has_stream_simple);
        assert_eq!(provider.models[0]["id"], "native-small");
        assert_eq!(provider.models[0]["contextWindow"], 32000);
        let oauth = provider.oauth.as_ref().expect("oauth block recorded");
        assert_eq!(oauth.name, "Native Proxy (subscription)");
        assert!(oauth.is_subscription);
        assert!(oauth.has_login && oauth.has_refresh_token && oauth.has_get_api_key);
        assert!(!oauth.has_modify_models);
        assert!(!oauth.uses_callback_server);
    });
}

#[test]
fn stream_simple_handler_streams_text_delta_and_done() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        host.load(
            entry("stream_ext"),
            r#"
                module.exports = function (pi) {
                    pi.registerProvider("my-proxy", {
                        api: "my-proxy-api",
                        baseUrl: "https://proxy.example.com/v1",
                        apiKey: "literal-key",
                        models: [{ id: "proxy-small", name: "Proxy Small", contextWindow: 64000, maxTokens: 2048 }],
                        streamSimple: function (model, context, options) {
                            if (model.id !== "proxy-small") throw new Error("unexpected model " + model.id);
                            const prompt = (context.messages || []).length;
                            const key = options && options.apiKey;
                            return (async function* () {
                                yield { type: "start", partial: { role: "assistant", content: [], model: model.id, api: model.api, provider: model.provider, usage: {}, stopReason: "pending", timestamp: 0 } };
                                yield { type: "text_start", contentIndex: 0 };
                                yield { type: "text_delta", contentIndex: 0, delta: "prox" + prompt + "-" + key };
                                yield { type: "text_end", contentIndex: 0 };
                                yield {
                                    type: "done",
                                    reason: "stop",
                                    message: {
                                        role: "assistant",
                                        content: [{ type: "text", text: "prox" + prompt + "-" + key }],
                                        stopReason: "stop",
                                        usage: { input: 7, output: 3, total: 10 },
                                        model: model.id,
                                    },
                                };
                            })();
                        },
                    });
                };
            "#,
        )
        .await
        .expect("load stream provider extension");

        let model = serde_json::json!({
            "id": "proxy-small",
            "provider": "my-proxy",
            "api": "my-proxy-api",
            "contextWindow": 64000,
            "maxTokens": 2048,
        });
        let context = serde_json::json!({
            "systemPrompt": "be brief",
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hi" }] }],
            "tools": [],
        });
        let options = serde_json::json!({ "apiKey": "literal-key", "baseUrl": "https://proxy.example.com/v1" });

        let events = host
            .invoke_provider_stream_simple(
                "my-proxy",
                &model.to_string(),
                &context.to_string(),
                &options.to_string(),
            )
            .await
            .expect("streamSimple handler runs");

        // The handler saw the model/context/options the host built.
        assert!(
            events.iter().any(|event| matches!(
                event,
                AssistantMessageEvent::TextDelta { delta } if delta == "prox1-literal-key"
            )),
            "{events:?}"
        );
        match events.last() {
            Some(AssistantMessageEvent::Done {
                content,
                stop_reason,
                usage,
            }) => {
                assert_eq!(*stop_reason, StopReason::Stop);
                assert_eq!(usage.total, 10);
                assert_eq!(content.len(), 1);
                match &content[0] {
                    pi_protocol::Content::Text(text) => assert_eq!(text.text, "prox1-literal-key"),
                    other => panic!("expected a text block, got {other:?}"),
                }
            }
            other => panic!("expected a terminal done event, got {other:?}"),
        }
    });
}

#[test]
fn stream_simple_handler_failures_surface_as_errors() {
    let runtime = rt();
    runtime.block_on(async {
        let host = JsExtensionHost::new().await.expect("host");
        host.load(
            entry("stream_ext_failure"),
            r#"
                module.exports = function (pi) {
                    pi.registerProvider("broken-proxy", {
                        api: "broken-api",
                        streamSimple: function () { throw new Error("upstream exploded"); },
                    });
                    pi.registerProvider("unterminated-proxy", {
                        api: "unterminated-api",
                        streamSimple: function () {
                            return (async function* () {
                                yield { type: "text_delta", contentIndex: 0, delta: "partial" };
                            })();
                        },
                    });
                };
            "#,
        )
        .await
        .expect("load failing provider extension");

        let err = host
            .invoke_provider_stream_simple("broken-proxy", "{}", "{}", "{}")
            .await
            .expect_err("a throwing handler must surface");
        assert!(err.to_string().contains("upstream exploded"), "{err}");

        // A handler that never emits a terminal event is closed out with the
        // `Error` + `Done { stop_reason: Error }` tail the StreamFn contract
        // requires.
        let events = host
            .invoke_provider_stream_simple("unterminated-proxy", "{}", "{}", "{}")
            .await
            .expect("unterminated handler still returns events");
        assert!(matches!(
            events.first(),
            Some(AssistantMessageEvent::TextDelta { .. })
        ));
        assert!(matches!(
            events.get(events.len() - 2),
            Some(AssistantMessageEvent::Error { .. })
        ));
        assert!(matches!(
            events.last(),
            Some(AssistantMessageEvent::Done {
                stop_reason: StopReason::Error,
                ..
            })
        ));

        // An unknown provider name is an error, not an empty stream.
        let err = host
            .invoke_provider_stream_simple("nope", "{}", "{}", "{}")
            .await
            .expect_err("unknown provider must reject");
        assert!(err.to_string().contains("no streamSimple handler"), "{err}");
    });
}
