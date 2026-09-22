//! `ctx.ui.addAutocompleteProvider` — the host half of LUM-1448.
//!
//! These tests drive the *real* wrapper chain in QuickJS: the fixture is the
//! on-disk `examples/issue-autocomplete.mjs` (a port of upstream's
//! `github-issue-autocomplete.ts` example, see its header for the two
//! documented deltas), loaded through the same loader path the binary uses.
//!
//! Two directions cross the ABI and both are covered:
//!
//! 1. Rust → JS: [`JsExtensionHost::autocomplete_rebuild`] and
//!    [`JsExtensionHost::autocomplete_call`] invoke the chain
//!    (`_pi_autocomplete_call`).
//! 2. JS → Rust: the chain's `current` delegate answers through the
//!    synchronous `host_ui_autocomplete` import, which forwards to the
//!    injected [`AutocompleteBaseProvider`] — the stand-in for the built-in
//!    `CombinedAutocompleteProvider` here.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use pi_extensions::{
    AutocompleteBaseProvider, AutocompleteCompletion, AutocompleteItem, AutocompleteRequest,
    AutocompleteSuggestions, ExtensionEntry, ExtensionSearchPaths, HostOptions, JsExtensionHost,
};
use pi_protocol::ExtensionEvent;

/// A recording stand-in for the built-in provider.
///
/// It answers exactly what `CombinedAutocompleteProvider` answers for a
/// `#123` token: nothing for `#…` (the base has no `#` branch — see
/// `docs/LUM1436_AUTOCOMPLETE_WHEEL.md` §1.2) and a plain-prefix rewrite for
/// `applyCompletion`. Recording the calls is how the test proves the JS
/// wrapper *delegated* rather than answered itself.
#[derive(Default)]
struct RecordingBase {
    suggestions: AtomicUsize,
    applies: AtomicUsize,
    file_completion: AtomicUsize,
}

impl AutocompleteBaseProvider for RecordingBase {
    fn get_suggestions(&self, request: &AutocompleteRequest) -> Option<AutocompleteSuggestions> {
        self.suggestions.fetch_add(1, Ordering::SeqCst);
        // `/` command completion: the one thing the built-in provider offers.
        if request.text_before_cursor().trim_start().starts_with('/') {
            return Some(AutocompleteSuggestions {
                items: vec![AutocompleteItem {
                    value: "help".to_string(),
                    label: "help".to_string(),
                    description: Some("show help".to_string()),
                }],
                prefix: "/".to_string(),
            });
        }
        None
    }

    fn apply_completion(
        &self,
        request: &AutocompleteRequest,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> AutocompleteCompletion {
        self.applies.fetch_add(1, Ordering::SeqCst);
        let line = request
            .lines
            .get(request.cursor_line)
            .cloned()
            .unwrap_or_default();
        let head = &line[..request.cursor_col.saturating_sub(prefix.len())];
        let after = &line[request.cursor_col.min(line.len())..];
        AutocompleteCompletion {
            lines: vec![format!("{head}{}{after}", item.value)],
            cursor_line: request.cursor_line,
            cursor_col: head.len() + item.value.len(),
        }
    }

    fn should_trigger_file_completion(&self, _request: &AutocompleteRequest) -> bool {
        self.file_completion.fetch_add(1, Ordering::SeqCst);
        true
    }
}

/// The fixture, loaded from disk exactly the way the loader does it.
fn fixture() -> (ExtensionEntry, String) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("issue-autocomplete.mjs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let entry = ExtensionSearchPaths::default().entry_for(&path);
    (entry, source)
}

/// Load the fixture, emit `session_start`, and return the live host + base.
async fn loaded_host() -> (JsExtensionHost, Arc<RecordingBase>) {
    let base = Arc::new(RecordingBase::default());
    let host = JsExtensionHost::with_options(
        HostOptions::default()
            .with_autocomplete_base(base.clone() as Arc<dyn AutocompleteBaseProvider>),
    )
    .await
    .expect("host");
    let (entry, source) = fixture();
    host.load(entry, &source).await.expect("load fixture");
    host.emit_event_with(&ExtensionEvent::SessionStart, Some("tui"), true, "/tmp")
        .await
        .expect("session_start");
    (host, base)
}

fn request(lines: &[&str], cursor_col: usize) -> String {
    serde_json::json!({
        "lines": lines,
        "cursorLine": 0,
        "cursorCol": cursor_col,
        "force": false,
    })
    .to_string()
}

#[tokio::test]
async fn fixture_registers_a_hash_trigger() {
    let (host, _base) = loaded_host().await;
    // The wrapper chain is live and declares `#`; the base declares none, so
    // the table is exactly what the extension contributed.
    assert_eq!(
        host.autocomplete_rebuild().await.expect("rebuild"),
        vec!['#']
    );
    assert!(
        host.autocomplete_generation() > 0,
        "registration not recorded"
    );
}

#[tokio::test]
async fn hash_token_returns_the_extensions_own_candidates() {
    let (host, base) = loaded_host().await;
    let _ = host.autocomplete_rebuild().await.expect("rebuild");

    let raw = host
        .autocomplete_call("getSuggestions", &request(&["#29"], 3))
        .await
        .expect("getSuggestions");
    let reply: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(reply["ok"], true, "{reply}");
    let suggestions = &reply["suggestions"];
    assert_eq!(suggestions["prefix"], "#29", "{reply}");
    let items = suggestions["items"].as_array().expect("items");
    // Upstream's numeric branch is a *prefix* match on the number, so `#29`
    // selects #2983 alone.
    assert_eq!(items.len(), 1, "{reply}");
    assert_eq!(items[0]["value"], "#2983");
    assert_eq!(items[0]["label"], "#2983");
    assert_eq!(
        items[0]["description"],
        "[open] Extension API for autocomplete"
    );

    // A non-numeric query falls through to the title fuzzy match.
    let raw = host
        .autocomplete_call("getSuggestions", &request(&["#reload"], 7))
        .await
        .expect("getSuggestions");
    let reply: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(reply["suggestions"]["prefix"], "#reload", "{reply}");
    assert_eq!(
        reply["suggestions"]["items"][0]["value"], "#2753",
        "{reply}"
    );

    // The wrapper answered itself: it never asked the built-in provider.
    assert_eq!(base.suggestions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn non_hash_token_delegates_to_the_builtin_provider() {
    let (host, base) = loaded_host().await;
    let _ = host.autocomplete_rebuild().await.expect("rebuild");

    // `/` is not a `#` token, so the fixture delegates to `current` — which is
    // the Rust provider reached through the synchronous host import.
    let raw = host
        .autocomplete_call("getSuggestions", &request(&["/he"], 3))
        .await
        .expect("getSuggestions");
    let reply: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(reply["suggestions"]["items"][0]["value"], "help", "{reply}");
    assert_eq!(base.suggestions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn apply_completion_round_trips_through_the_builtin_provider() {
    let (host, base) = loaded_host().await;
    let _ = host.autocomplete_rebuild().await.expect("rebuild");

    let payload = serde_json::json!({
        "lines": ["#29"],
        "cursorLine": 0,
        "cursorCol": 3,
        "force": false,
        "item": {"value": "#2983", "label": "#2983"},
        "prefix": "#29",
    })
    .to_string();
    let raw = host
        .autocomplete_call("applyCompletion", &payload)
        .await
        .expect("applyCompletion");
    let reply: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["completion"]["lines"][0], "#2983", "{reply}");
    assert_eq!(reply["completion"]["cursorCol"], 5, "{reply}");
    // The fixture's `applyCompletion` is a pure delegation, so the built-in
    // provider performed the rewrite.
    assert_eq!(base.applies.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn should_trigger_file_completion_delegates_with_upstream_default() {
    let (host, base) = loaded_host().await;
    let _ = host.autocomplete_rebuild().await.expect("rebuild");

    let raw = host
        .autocomplete_call("shouldTriggerFileCompletion", &request(&["hello "], 6))
        .await
        .expect("shouldTriggerFileCompletion");
    let reply: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["value"], true, "{reply}");
    assert_eq!(base.file_completion.load(Ordering::SeqCst), 1);
}

/// Without a wrapper registered the chain is the built-in provider alone:
/// rebuilding reports no trigger characters and `getSuggestions` still
/// delegates.
#[tokio::test]
async fn empty_chain_still_delegates_to_the_builtin_provider() {
    let base = Arc::new(RecordingBase::default());
    let host = JsExtensionHost::with_options(
        HostOptions::default()
            .with_autocomplete_base(base.clone() as Arc<dyn AutocompleteBaseProvider>),
    )
    .await
    .expect("host");
    assert!(host
        .autocomplete_rebuild()
        .await
        .expect("rebuild")
        .is_empty());
    let raw = host
        .autocomplete_call("getSuggestions", &request(&["/he"], 3))
        .await
        .expect("getSuggestions");
    let reply: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(reply["suggestions"]["items"][0]["value"], "help", "{reply}");
}

/// A host with no built-in provider answers `null` instead of failing: a
/// print-mode host must not break the chain.
#[tokio::test]
async fn missing_builtin_provider_degrades_to_no_candidates() {
    let host = JsExtensionHost::new().await.expect("host");
    let (entry, source) = fixture();
    host.load(entry, &source).await.expect("load fixture");
    host.emit_event_with(&ExtensionEvent::SessionStart, Some("tui"), true, "/tmp")
        .await
        .expect("session_start");

    let raw = host
        .autocomplete_call("getSuggestions", &request(&["/he"], 3))
        .await
        .expect("getSuggestions");
    let reply: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(reply["suggestions"].is_null(), "{reply}");
}

/// A wrapper that answers with a Promise is not awaited: the built-in provider
/// answers that call instead, and the async deviation is warned once.
#[tokio::test]
async fn async_provider_callbacks_fall_back_to_the_builtin_provider() {
    let base = Arc::new(RecordingBase::default());
    let host = JsExtensionHost::with_options(
        HostOptions::default()
            .with_autocomplete_base(base.clone() as Arc<dyn AutocompleteBaseProvider>),
    )
    .await
    .expect("host");
    let source = r#"
export default function (pi) {
  pi.on("session_start", (_event, ctx) => {
    ctx.ui.addAutocompleteProvider((current) => ({
      triggerCharacters: ["$"],
      async getSuggestions(lines, cursorLine, cursorCol, options) {
        return { items: [{ value: "$never", label: "$never" }], prefix: "$" };
      },
      applyCompletion(lines, cursorLine, cursorCol, item, prefix) {
        return current.applyCompletion(lines, cursorLine, cursorCol, item, prefix);
      },
    }));
  });
}
"#;
    host.load(
        ExtensionEntry {
            source: PathBuf::from("async-provider.mjs"),
            id: "async-provider".into(),
            label: None,
        },
        source,
    )
    .await
    .expect("load");
    host.emit_event_with(&ExtensionEvent::SessionStart, Some("tui"), true, "/tmp")
        .await
        .expect("session_start");

    assert_eq!(
        host.autocomplete_rebuild().await.expect("rebuild"),
        vec!['$']
    );
    let raw = host
        .autocomplete_call("getSuggestions", &request(&["/he"], 3))
        .await
        .expect("getSuggestions");
    let reply: serde_json::Value = serde_json::from_str(&raw).unwrap();
    // The Promise was not awaited: `$never` never appeared, and the built-in
    // provider answered instead.
    assert_eq!(reply["suggestions"]["items"][0]["value"], "help", "{reply}");
    assert_eq!(base.suggestions.load(Ordering::SeqCst), 1);
}
