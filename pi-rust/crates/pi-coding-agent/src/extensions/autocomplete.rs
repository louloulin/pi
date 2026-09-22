//! Extension-injected autocomplete providers.
//!
//! `ctx.ui.addAutocompleteProvider(factory)` stacks an extension's provider on
//! top of the built-in one (upstream
//! `packages/coding-agent/src/modes/interactive/interactive-mode.ts:2449`,
//! `docs/extensions.md` → "Autocomplete Providers"). The wrapper *chain* lives
//! in the shim, because the wrappers are JS closures; this module is the two
//! Rust ends of it:
//!
//! * [`SessionAutocompleteBase`] — the built-in `CombinedAutocompleteProvider`
//!   the chain delegates to. `pi-extensions` owns the
//!   [`AutocompleteBaseProvider`] trait (it cannot depend on `pi-tui`) and the
//!   interactive adapter registers this implementation with the host.
//! * [`JsAutocompleteProvider`] — the editor-facing provider. It implements
//!   `pi-tui`'s [`AutocompleteProvider`] and forwards each call into the JS
//!   chain over the host's `_pi_autocomplete_call` entry point.
//!
//! ## Threading
//!
//! `pi-tui`'s provider trait is synchronous (a documented deviation from
//! upstream's `async getSuggestions`), and the editor calls it from inside the
//! render loop. The JS chain, however, is driven by the extension host's own
//! task. `JsAutocompleteProvider` therefore bridges the two with a **bounded
//! blocking call**: `block_in_place` + `Handle::block_on`, capped at
//! [`AUTOCOMPLETE_BRIDGE_TIMEOUT`]. The JS side answers in the same turn
//! (phase-1 providers are synchronous), so the usual wait is a few
//! microseconds, and nothing can hang the UI for longer than the cap — a
//! timeout or a bridge error falls back to the built-in provider.
//!
//! On a `current_thread` runtime (or outside a runtime) `block_in_place` is
//! not available; the provider then answers from the built-in fallback and the
//! extension chain is not consulted. The real binary runs on a multi-thread
//! runtime (`src/main.rs`), so only unit tests are affected.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use pi_extensions::{
    AutocompleteBaseProvider, AutocompleteCompletion, AutocompleteItem as HostItem,
    AutocompleteRequest as HostRequest, AutocompleteSuggestions as HostSuggestions,
    JsExtensionHost,
};
use pi_tui::autocomplete::{
    AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions, CompletionResult,
};

/// Upper bound on one extension autocomplete call.
///
/// The callback is text-in / text-out and cannot await IO (phase 1), so this
/// is a safety net for a pathological provider, not a latency budget: a
/// timeout falls back to the built-in provider rather than freezing the frame.
pub const AUTOCOMPLETE_BRIDGE_TIMEOUT: Duration = Duration::from_millis(250);

/// The built-in provider, published to the extension host.
///
/// The interactive loop installs the `CombinedAutocompleteProvider` it built
/// (slash commands + file paths) once the `App` exists — *after* extensions
/// loaded — so the slot starts empty and is filled by
/// [`SessionAutocompleteBase::set`]. A chain rebuilt before that answers
/// `None` through this delegation, which is exactly what a host without a
/// built-in provider does.
#[derive(Default)]
pub struct SessionAutocompleteBase {
    provider: RwLock<Option<Arc<dyn AutocompleteProvider>>>,
}

impl SessionAutocompleteBase {
    /// An empty slot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish (or replace) the built-in provider.
    pub fn set(&self, provider: Arc<dyn AutocompleteProvider>) {
        *self.provider.write() = Some(provider);
    }

    /// The built-in provider, when one has been published.
    pub fn get(&self) -> Option<Arc<dyn AutocompleteProvider>> {
        self.provider.read().clone()
    }
}

impl std::fmt::Debug for SessionAutocompleteBase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionAutocompleteBase")
            .field("installed", &self.provider.read().is_some())
            .finish()
    }
}

impl AutocompleteBaseProvider for SessionAutocompleteBase {
    fn get_suggestions(&self, request: &HostRequest) -> Option<HostSuggestions> {
        let provider = self.get()?;
        let suggestions = provider.get_suggestions(
            &request.lines,
            request.cursor_line,
            request.cursor_col,
            request.force,
        )?;
        Some(HostSuggestions {
            items: suggestions.items.into_iter().map(to_host_item).collect(),
            prefix: suggestions.prefix,
        })
    }

    fn apply_completion(
        &self,
        request: &HostRequest,
        item: &HostItem,
        prefix: &str,
    ) -> AutocompleteCompletion {
        let Some(provider) = self.get() else {
            // No built-in provider: leave the buffer untouched.
            return AutocompleteCompletion {
                lines: request.lines.clone(),
                cursor_line: request.cursor_line,
                cursor_col: request.cursor_col,
            };
        };
        let result = provider.apply_completion(
            &request.lines,
            request.cursor_line,
            request.cursor_col,
            &AutocompleteItem {
                value: item.value.clone(),
                label: item.label.clone(),
                description: item.description.clone(),
            },
            prefix,
        );
        AutocompleteCompletion {
            lines: result.lines,
            cursor_line: result.cursor_line,
            cursor_col: result.cursor_col,
        }
    }

    fn should_trigger_file_completion(&self, request: &HostRequest) -> bool {
        match self.get() {
            Some(provider) => provider.should_trigger_file_completion(
                &request.lines,
                request.cursor_line,
                request.cursor_col,
            ),
            // Upstream's default when a provider does not override it.
            None => true,
        }
    }
}

/// Project one `pi-tui` candidate onto the wire shape.
fn to_host_item(item: AutocompleteItem) -> HostItem {
    HostItem {
        value: item.value,
        label: item.label,
        description: item.description,
    }
}

/// The editor-facing provider for an extension wrapper chain.
///
/// Every method forwards into the JS chain and falls back to `fallback` (the
/// built-in provider this wrapper was stacked on) when the bridge cannot
/// answer — a timeout, a dead host, or a `current_thread` runtime.
pub struct JsAutocompleteProvider {
    host: JsExtensionHost,
    fallback: Arc<dyn AutocompleteProvider>,
    trigger_characters: Vec<char>,
}

impl std::fmt::Debug for JsAutocompleteProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsAutocompleteProvider")
            .field("trigger_characters", &self.trigger_characters)
            .finish_non_exhaustive()
    }
}

impl JsAutocompleteProvider {
    /// Wrap `fallback` with the extension chain hosted by `host`.
    ///
    /// `trigger_characters` is the chain's deduplicated table, as reported by
    /// [`JsExtensionHost::autocomplete_rebuild`].
    pub fn new(
        host: JsExtensionHost,
        fallback: Arc<dyn AutocompleteProvider>,
        trigger_characters: Vec<char>,
    ) -> Self {
        Self {
            host,
            fallback,
            trigger_characters,
        }
    }

    /// Drive one JS-side autocomplete op, or `None` when the bridge is
    /// unavailable / timed out / failed.
    fn call(&self, op: &str, payload: serde_json::Value) -> Option<serde_json::Value> {
        let handle = tokio::runtime::Handle::try_current().ok()?;
        if handle.runtime_flavor() != tokio::runtime::RuntimeFlavor::MultiThread {
            // `block_in_place` is not available; the extension chain is
            // skipped rather than deadlocking the render loop.
            return None;
        }
        let host = self.host.clone();
        let op = op.to_string();
        let payload = payload.to_string();
        let raw = tokio::task::block_in_place(|| {
            handle.block_on(async move {
                tokio::time::timeout(
                    AUTOCOMPLETE_BRIDGE_TIMEOUT,
                    host.autocomplete_call(&op, &payload),
                )
                .await
                .ok()
                .and_then(Result::ok)
            })
        })?;
        serde_json::from_str(&raw).ok()
    }
}

impl AutocompleteProvider for JsAutocompleteProvider {
    fn trigger_characters(&self) -> &[char] {
        &self.trigger_characters
    }

    fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        force: bool,
    ) -> Option<AutocompleteSuggestions> {
        let request = HostRequest::new(lines, cursor_line, cursor_col, force);
        let Ok(payload) = serde_json::to_value(&request) else {
            return self
                .fallback
                .get_suggestions(lines, cursor_line, cursor_col, force);
        };
        let Some(reply) = self.call("getSuggestions", payload) else {
            return self
                .fallback
                .get_suggestions(lines, cursor_line, cursor_col, force);
        };
        if reply.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            return self
                .fallback
                .get_suggestions(lines, cursor_line, cursor_col, force);
        }
        // A `null` suggestions value is a real answer ("nothing to complete"),
        // not a bridge failure: the wrapper declined and the chain's own
        // delegation decided there is nothing to show.
        let value = reply.get("suggestions")?;
        if value.is_null() {
            return None;
        }
        let suggestions: HostSuggestions = serde_json::from_value(value.clone()).ok()?;
        if suggestions.items.is_empty() {
            return None;
        }
        Some(AutocompleteSuggestions {
            items: suggestions
                .items
                .into_iter()
                .map(|item| AutocompleteItem {
                    value: item.value,
                    label: item.label,
                    description: item.description,
                })
                .collect(),
            prefix: suggestions.prefix,
        })
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> CompletionResult {
        let request = HostRequest::new(lines, cursor_line, cursor_col, false);
        let payload = serde_json::json!({
            "lines": request.lines,
            "cursorLine": request.cursor_line,
            "cursorCol": request.cursor_col,
            "force": request.force,
            "item": {
                "value": item.value,
                "label": item.label,
                "description": item.description,
            },
            "prefix": prefix,
        });
        let Some(reply) = self.call("applyCompletion", payload) else {
            return self
                .fallback
                .apply_completion(lines, cursor_line, cursor_col, item, prefix);
        };
        if reply.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            return self
                .fallback
                .apply_completion(lines, cursor_line, cursor_col, item, prefix);
        }
        let Some(completion) = reply.get("completion") else {
            return self
                .fallback
                .apply_completion(lines, cursor_line, cursor_col, item, prefix);
        };
        match serde_json::from_value::<AutocompleteCompletion>(completion.clone()) {
            Ok(completion) if !completion.lines.is_empty() => CompletionResult {
                lines: completion.lines,
                cursor_line: completion.cursor_line,
                cursor_col: completion.cursor_col,
            },
            // An empty rewrite would blank the draft; keep the built-in one.
            _ => self
                .fallback
                .apply_completion(lines, cursor_line, cursor_col, item, prefix),
        }
    }

    fn should_trigger_file_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> bool {
        let request = HostRequest::new(lines, cursor_line, cursor_col, false);
        let Ok(payload) = serde_json::to_value(&request) else {
            return self
                .fallback
                .should_trigger_file_completion(lines, cursor_line, cursor_col);
        };
        let Some(reply) = self.call("shouldTriggerFileCompletion", payload) else {
            return self
                .fallback
                .should_trigger_file_completion(lines, cursor_line, cursor_col);
        };
        if reply.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            return self
                .fallback
                .should_trigger_file_completion(lines, cursor_line, cursor_col);
        }
        reply
            .get("value")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true)
    }
}

/// Stack the extension chain's provider on top of `base` and return the
/// composed provider.
///
/// This is the production composition path (the interactive loop calls it
/// once per install) and it is public so an integration test can drive the
/// same composition with a real host and a real fixture without standing up
/// the whole CLI. `trigger_characters` is the chain's deduplicated table from
/// [`JsExtensionHost::autocomplete_rebuild`].
pub fn compose_provider(
    host: JsExtensionHost,
    base: Arc<dyn AutocompleteProvider>,
    trigger_characters: Vec<char>,
) -> Arc<dyn AutocompleteProvider> {
    let factory: pi_tui::autocomplete::AutocompleteProviderFactory =
        Arc::new(move |current: Arc<dyn AutocompleteProvider>| {
            Arc::new(JsAutocompleteProvider::new(
                host.clone(),
                current,
                trigger_characters.clone(),
            )) as Arc<dyn AutocompleteProvider>
        });
    pi_tui::autocomplete::compose_autocomplete_providers(base, &[factory])
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_tui::autocomplete::CombinedAutocompleteProvider;

    /// Outside a runtime the bridge is unavailable, so every method must fall
    /// back to the built-in provider — including `apply_completion`, which is
    /// what keeps `Tab` working when the host is gone.
    #[test]
    fn without_a_runtime_the_builtin_provider_answers() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let base: Arc<dyn AutocompleteProvider> = Arc::new(CombinedAutocompleteProvider::new(
            vec![pi_tui::autocomplete::SlashCommand::new("help")],
            ".",
        ));
        let base_slot = SessionAutocompleteBase::new();
        base_slot.set(base.clone());
        assert!(base_slot.get().is_some());

        let host = runtime.block_on(async { JsExtensionHost::new().await.expect("host") });
        let provider = JsAutocompleteProvider::new(host, base.clone(), vec!['#']);
        assert_eq!(provider.trigger_characters(), &['#']);

        let lines = vec!["/he".to_string()];
        // Called from a non-runtime thread: the fallback answers.
        let suggestions = provider.get_suggestions(&lines, 0, 3, false).unwrap();
        assert_eq!(suggestions.items[0].value, "help");
        // The built-in rule: a `/`-token with no space never falls through to
        // file completion (`CombinedAutocompleteProvider`).
        assert!(!provider.should_trigger_file_completion(&lines, 0, 3));
        let plain = vec!["hello ".to_string()];
        assert!(provider.should_trigger_file_completion(&plain, 0, 6));
        let applied =
            provider.apply_completion(&lines, 0, 3, &AutocompleteItem::new("help", "help"), "/he");
        assert_eq!(applied.lines, vec!["/help ".to_string()]);
    }

    #[test]
    fn host_request_maps_the_editor_buffer() {
        let slot = SessionAutocompleteBase::new();
        let request = HostRequest::new(&["#29".to_string()], 0, 3, true);
        // No built-in provider published yet: an empty slot answers nothing.
        assert!(slot.get_suggestions(&request).is_none());
        assert!(slot.should_trigger_file_completion(&request));
        let completion = slot.apply_completion(
            &request,
            &HostItem {
                value: "#2983".into(),
                label: "#2983".into(),
                description: None,
            },
            "#29",
        );
        assert_eq!(completion.lines, vec!["#29".to_string()]);
    }
}
