//! Extension lifecycle events that fire *inside* an agent run.
//!
//! [`ExtensionEventMapper`](crate::extensions::events::ExtensionEventMapper)
//! covers the events that fall out of the agent's synchronous
//! [`AgentEvent`](pi_agent_core::AgentEvent) fan-out (`turn_start`,
//! `message_update`, …). The events here are different: upstream delivers
//! them as **request/response** hooks around the run
//! (`packages/coding-agent/src/core/extensions/runner.ts`), so a plugin can
//! rewrite the conversation (`context`), replace the system prompt
//! (`before_agent_start`), or observe the provider request boundary.
//!
//! The synchronous fan-out cannot await a plugin's answer, so these ride
//! [`LifecycleHooks`], the async seam `pi-agent-core` invokes at the matching
//! loop points. This module implements that trait on top of the loaded
//! [`ExtensionRuntime`] and owns the `ExtensionEvent` construction, because
//! the JS host lives in this crate.
//!
//! ## Fidelity notes
//!
//! * `before_provider_request` / `before_provider_headers`: the Rust
//!   `StreamFn` takes a typed `Context` and the `pi-ai` adapters build their
//!   own HTTP request, so neither a replacement payload nor a mutated header
//!   map has anywhere to go. The events are constructed and delivered with
//!   the data this port has; handler return values are folded in where they
//!   *can* be applied and otherwise documented as gaps. See
//!   `docs/LUM1432_EXTENSION_EVENTS.md` §3.
//! * `context`: upstream chains handlers (each sees the previous rewrite).
//!   The shim dispatches one event to every handler at once, so the fold
//!   here is "the last non-null `messages` result wins", which is equivalent
//!   whenever handlers do not depend on each other's output.
//! * `after_provider_response`: the adapters do not surface the HTTP status,
//!   so the event carries `200` when a stream was established and `0` when
//!   the call failed.
//! * `agent_settled`: emitted on the error path too, so cleanup handlers
//!   always run.

use std::sync::Arc;

use async_trait::async_trait;
use pi_agent_core::LifecycleHooks;
use pi_protocol::{ExtensionEvent, Message, UiPromptKind};

use crate::extensions::wiring::ExtensionRuntime;

/// [`LifecycleHooks`] backed by the loaded JS extensions.
///
/// The runtime is held by `Arc` so the hooks can be installed on an `Agent`
/// that outlives the load pass; a process with no extensions never installs
/// one, so every method is skipped entirely.
#[derive(Clone)]
pub struct ExtensionLifecycleHooks {
    runtime: Arc<ExtensionRuntime>,
}

impl std::fmt::Debug for ExtensionLifecycleHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionLifecycleHooks")
            .field("active", &self.runtime.has_any_subscriber())
            .finish()
    }
}

impl ExtensionLifecycleHooks {
    /// Wrap a loaded runtime.
    pub fn new(runtime: Arc<ExtensionRuntime>) -> Self {
        Self { runtime }
    }

    /// Deliver one event and return its dispatch summary, if any handler ran.
    async fn dispatch(&self, event: &ExtensionEvent) -> Option<pi_extensions::DispatchOutcome> {
        self.runtime.dispatch_event(event).await
    }
}

#[async_trait]
impl LifecycleHooks for ExtensionLifecycleHooks {
    async fn before_agent_start(&self, prompt: &str, system_prompt: &str) -> Option<String> {
        let event = ExtensionEvent::BeforeAgentStart {
            prompt: prompt.to_string(),
            system_prompt: system_prompt.to_string(),
        };
        let outcome = self.dispatch(&event).await?;
        // Upstream chains `systemPrompt` across handlers; the last one wins
        // (see the module docs).
        outcome
            .results
            .iter()
            .filter_map(|result| result.get("systemPrompt").and_then(|v| v.as_str()))
            .next_back()
            .map(str::to_string)
    }

    async fn context(&self, messages: Vec<Message>) -> Vec<Message> {
        let event = ExtensionEvent::Context {
            messages: messages.clone(),
        };
        let Some(outcome) = self.dispatch(&event).await else {
            return messages;
        };
        let mut current = messages;
        for result in &outcome.results {
            let Some(replacement) = result.get("messages") else {
                continue;
            };
            // A handler that returns the event's own `messages` array (the
            // documented no-op) must not be treated as a rewrite failure.
            if let Ok(parsed) = serde_json::from_value::<Vec<Message>>(replacement.clone()) {
                current = parsed;
            }
        }
        current
    }

    async fn agent_settled(&self) {
        let event = ExtensionEvent::AgentSettled;
        self.dispatch(&event).await;
    }

    async fn before_provider_request(&self, payload: serde_json::Value) -> serde_json::Value {
        let event = ExtensionEvent::BeforeProviderRequest {
            payload: payload.clone(),
        };
        let Some(outcome) = self.dispatch(&event).await else {
            return payload;
        };
        // Upstream replaces the payload with the last non-undefined return.
        // The Rust provider adapters take a typed `Context`, so the caller has
        // nowhere to put a replacement yet; returning it keeps the seam honest
        // about what the handlers answered even though the loop ignores it.
        outcome
            .results
            .iter()
            .rfind(|result| !result.is_null())
            .cloned()
            .unwrap_or(payload)
    }

    async fn before_provider_headers(
        &self,
        headers: serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let event = ExtensionEvent::BeforeProviderHeaders {
            headers: headers.clone(),
        };
        let outcome = self.dispatch(&event).await;
        // Upstream handlers mutate `headers` in place and only the mutation
        // matters; the shim does not echo the event object back yet, so this
        // returns the input unchanged. Documented gap.
        let _ = outcome;
        headers
    }

    async fn after_provider_response(&self, ok: bool) {
        let event = ExtensionEvent::AfterProviderResponse {
            status: if ok { 200 } else { 0 },
            headers: serde_json::Map::new(),
        };
        self.dispatch(&event).await;
    }
}

/// Emit `ui_prompt_start` / `ui_prompt_end` around a blocking dialog.
///
/// Upstream's runner brackets every blocking `ctx.ui.*` prompt
/// (`runner.ts:457-482`). The Rust prompt surface is the
/// [`UiHandler`](pi_extensions::UiHandler) the host calls, so
/// [`TuiUiHandler`](crate::extensions::ui_bridge) forwards the two edges
/// here. The pair is purely observational — nothing consumes its result — so
/// it is a separate trait from [`LifecycleHooks`] rather than another method
/// the agent loop would have to call.
#[async_trait]
pub trait UiPromptObserver: Send + Sync {
    /// A blocking prompt is about to be shown.
    async fn prompt_started(&self, kind: UiPromptKind, title: Option<&str>);

    /// The blocking prompt was answered (or cancelled).
    async fn prompt_finished(&self, kind: UiPromptKind, title: Option<&str>);
}

#[async_trait]
impl UiPromptObserver for ExtensionLifecycleHooks {
    async fn prompt_started(&self, kind: UiPromptKind, title: Option<&str>) {
        let event = ExtensionEvent::UiPromptStart {
            kind,
            title: title.map(str::to_string),
        };
        self.dispatch(&event).await;
    }

    async fn prompt_finished(&self, kind: UiPromptKind, title: Option<&str>) {
        let event = ExtensionEvent::UiPromptEnd {
            kind,
            title: title.map(str::to_string),
        };
        self.dispatch(&event).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_extensions::{ExtensionEntry, JsExtensionHost};

    /// Load one extension source into a real QuickJS host and wrap it in a
    /// runtime that reports exactly `subscribed`.
    async fn hooks_for(
        tag: &str,
        source: &str,
        subscribed: &[&str],
    ) -> (ExtensionLifecycleHooks, JsExtensionHost) {
        let dir = std::env::temp_dir().join(format!("pi-lifecycle-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let file = dir.join("extension.js");
        std::fs::write(&file, source).expect("write extension");
        let host = JsExtensionHost::new().await.expect("host");
        host.load(
            ExtensionEntry {
                source: file.clone(),
                id: tag.to_string(),
                label: None,
            },
            source,
        )
        .await
        .expect("load");
        let _ = std::fs::remove_dir_all(&dir);
        let runtime = Arc::new(ExtensionRuntime::for_test(host.clone(), subscribed));
        (ExtensionLifecycleHooks::new(runtime), host)
    }

    fn text_message(text: &str) -> Message {
        Message {
            role: pi_protocol::Role::User,
            content: vec![pi_protocol::Content::text(text)],
            model: None,
        }
    }

    /// `context` really reaches the JS handler and its `messages` result
    /// becomes the list the provider would receive.
    #[tokio::test]
    async fn context_rewrite_is_applied() {
        let (hooks, _host) = hooks_for(
            "context",
            r#"
module.exports = function (pi) {
  pi.on("context", function (event) {
    var msgs = event.messages.slice();
    msgs.push({ role: "user", content: [{ type: "text", text: "CTX-MARKER" }] });
    return { messages: msgs };
  });
};
"#,
            &["context"],
        )
        .await;

        let rewritten = hooks.context(vec![text_message("original")]).await;
        assert_eq!(rewritten.len(), 2, "the handler appended one message");
        let tail = rewritten.last().expect("tail");
        match tail.content.first() {
            Some(pi_protocol::Content::Text(text)) => assert_eq!(text.text, "CTX-MARKER"),
            other => panic!("unexpected {:?}", other),
        }
    }

    /// A handler that returns nothing must leave the message list alone.
    #[tokio::test]
    async fn context_without_result_keeps_messages() {
        let (hooks, _host) = hooks_for(
            "context-noop",
            "module.exports = function (pi) { pi.on('context', function () {}); };\n",
            &["context"],
        )
        .await;
        let original = vec![text_message("keep me")];
        assert_eq!(hooks.context(original.clone()).await, original);
    }

    /// `before_agent_start`'s `systemPrompt` replaces the run's prompt.
    #[tokio::test]
    async fn before_agent_start_overrides_the_system_prompt() {
        let (hooks, _host) = hooks_for(
            "before-agent-start",
            r#"
module.exports = function (pi) {
  pi.on("before_agent_start", function (event) {
    return { systemPrompt: "OVERRIDDEN:" + event.prompt };
  });
};
"#,
            &["before_agent_start"],
        )
        .await;

        let prompt = hooks.before_agent_start("hello", "base prompt").await;
        assert_eq!(prompt.as_deref(), Some("OVERRIDDEN:hello"));
    }

    /// `agent_settled` reaches the handler after a run.
    #[tokio::test]
    async fn agent_settled_fires() {
        let (hooks, host) = hooks_for(
            "agent-settled",
            r#"
module.exports = function (pi) {
  pi.on("agent_settled", function () { pi.appendEntry("settled-entry", { ok: true }); });
};
"#,
            &["agent_settled"],
        )
        .await;

        hooks.agent_settled().await;
        let log = host.log();
        assert!(
            log.entries
                .iter()
                .any(|entry| entry.custom_type == "settled-entry"),
            "the settled handler must have run: {:?}",
            log.entries.len()
        );
    }

    /// The provider-boundary events are constructed and delivered with the
    /// payload the port has.
    #[tokio::test]
    async fn provider_boundary_events_reach_the_handler() {
        let (hooks, host) = hooks_for(
            "provider-boundary",
            r#"
module.exports = function (pi) {
  pi.on("before_provider_request", function (event) {
    pi.appendEntry("saw-request", { model: event.payload && event.payload.model });
  });
  pi.on("before_provider_headers", function () { pi.appendEntry("saw-headers", {}); });
  pi.on("after_provider_response", function (event) {
    pi.appendEntry("saw-response", { status: event.status });
  });
};
"#,
            &[
                "before_provider_request",
                "before_provider_headers",
                "after_provider_response",
            ],
        )
        .await;

        let payload = serde_json::json!({ "model": { "id": "faux" } });
        hooks.before_provider_request(payload).await;
        hooks.before_provider_headers(serde_json::Map::new()).await;
        hooks.after_provider_response(true).await;

        let log = host.log();
        let kinds: Vec<&str> = log
            .entries
            .iter()
            .map(|entry| entry.custom_type.as_str())
            .collect();
        assert!(kinds.contains(&"saw-request"), "{kinds:?}");
        assert!(kinds.contains(&"saw-headers"), "{kinds:?}");
        assert!(kinds.contains(&"saw-response"), "{kinds:?}");
    }

    /// `ui_prompt_start` / `ui_prompt_end` bracket a blocking dialog.
    #[tokio::test]
    async fn ui_prompt_edges_reach_the_handler() {
        let (hooks, host) = hooks_for(
            "ui-prompt",
            r#"
module.exports = function (pi) {
  pi.on("ui_prompt_start", function (event) {
    pi.appendEntry("prompt-start", { kind: event.kind });
  });
  pi.on("ui_prompt_end", function (event) {
    pi.appendEntry("prompt-end", { kind: event.kind });
  });
};
"#,
            &["ui_prompt_start", "ui_prompt_end"],
        )
        .await;

        hooks
            .prompt_started(UiPromptKind::Confirm, Some("Trust?"))
            .await;
        hooks
            .prompt_finished(UiPromptKind::Confirm, Some("Trust?"))
            .await;

        let log = host.log();
        let kinds: Vec<&str> = log
            .entries
            .iter()
            .map(|entry| entry.custom_type.as_str())
            .collect();
        assert_eq!(kinds, vec!["prompt-start", "prompt-end"]);
    }
}
