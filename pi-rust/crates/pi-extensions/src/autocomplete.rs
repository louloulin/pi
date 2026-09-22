//! Wire types and the base-provider trait behind
//! `ctx.ui.addAutocompleteProvider`.
//!
//! Upstream lets an extension stack a provider on top of the built-in one
//! (`packages/coding-agent/docs/extensions.md` → "Autocomplete Providers"):
//!
//! ```typescript
//! ctx.ui.addAutocompleteProvider((current) => ({
//!   triggerCharacters: ["#"],
//!   getSuggestions(lines, cursorLine, cursorCol, options) { … },
//!   applyCompletion(lines, cursorLine, cursorCol, item, prefix) { … },
//!   shouldTriggerFileCompletion(lines, cursorLine, cursorCol) { … },
//! }));
//! ```
//!
//! The JS side of the port (the shim) owns the wrapper *chain*: each factory
//! receives the provider it stacks on, exactly like
//! `setupAutocompleteProvider` (`interactive-mode.ts:734-745`). The chain's
//! innermost link is the Rust `CombinedAutocompleteProvider` the interactive
//! mode installs, which cannot be a JS object — so the shim reaches it
//! through the synchronous `host_ui_autocomplete` import and the host
//! forwards each delegation to an injected [`AutocompleteBaseProvider`].
//!
//! `pi-extensions` must not depend on `pi-tui` (the base provider lives
//! there), so the types that cross the ABI are owned here and the adapter in
//! `pi-coding-agent` maps them onto the editor's provider types.

use serde::{Deserialize, Serialize};

/// One autocomplete request — upstream's `(lines, cursorLine, cursorCol,
/// options)` argument list collapsed into one snapshot.
///
/// The snapshot carries the buffer text and nothing else: a provider must
/// not be able to reach into the TUI, and the extension callback runs on the
/// host's task, never on the render thread.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutocompleteRequest {
    /// The editor buffer, one entry per logical line.
    #[serde(default)]
    pub lines: Vec<String>,
    /// Line the cursor sits on.
    #[serde(default)]
    pub cursor_line: usize,
    /// Byte offset of the cursor within `lines[cursor_line]`.
    #[serde(default)]
    pub cursor_col: usize,
    /// Upstream's `options.force` — an explicit Tab that skips the
    /// "does this look completable" heuristics.
    #[serde(default)]
    pub force: bool,
}

impl AutocompleteRequest {
    /// Build a request from a buffer snapshot.
    pub fn new(lines: &[String], cursor_line: usize, cursor_col: usize, force: bool) -> Self {
        Self {
            lines: lines.to_vec(),
            cursor_line,
            cursor_col,
            force,
        }
    }

    /// The text of the cursor's line before the cursor.
    pub fn text_before_cursor(&self) -> &str {
        let Some(line) = self.lines.get(self.cursor_line) else {
            return "";
        };
        line.get(..self.cursor_col.min(line.len())).unwrap_or("")
    }
}

/// One candidate — upstream `AutocompleteItem`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutocompleteItem {
    /// Text inserted when the candidate is accepted.
    pub value: String,
    /// Dropdown label.
    pub label: String,
    /// Optional secondary text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The candidate list plus the prefix it was computed for — upstream
/// `AutocompleteSuggestions`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutocompleteSuggestions {
    /// Candidates, best match first.
    #[serde(default)]
    pub items: Vec<AutocompleteItem>,
    /// The buffer prefix the items complete.
    #[serde(default)]
    pub prefix: String,
}

/// The result of applying one candidate — upstream's inline
/// `{ lines, cursorLine, cursorCol }` return type.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutocompleteCompletion {
    /// Buffer lines after the completion.
    #[serde(default)]
    pub lines: Vec<String>,
    /// Cursor line after the completion.
    #[serde(default)]
    pub cursor_line: usize,
    /// Cursor column (byte offset) after the completion.
    #[serde(default)]
    pub cursor_col: usize,
}

/// The built-in provider an extension wrapper delegates to.
///
/// `pi-extensions` cannot depend on `pi-tui`, so the interactive adapter
/// implements this over the `CombinedAutocompleteProvider` it installs and
/// injects it through
/// [`HostOptions::with_autocomplete_base`](crate::HostOptions::with_autocomplete_base).
/// Every method is synchronous: the shim reaches it from inside a JS
/// callback, so there is nothing to await, and the port's editor-side
/// provider is synchronous too.
pub trait AutocompleteBaseProvider: Send + Sync + 'static {
    /// Candidates for `request`, or `None` when the built-in provider has
    /// nothing to offer — upstream's `null` return.
    fn get_suggestions(&self, request: &AutocompleteRequest) -> Option<AutocompleteSuggestions>;

    /// Apply `item` (computed for `prefix`) — upstream `applyCompletion`.
    fn apply_completion(
        &self,
        request: &AutocompleteRequest,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> AutocompleteCompletion;

    /// Whether an explicit Tab may still fall through to file completion —
    /// upstream `shouldTriggerFileCompletion`.
    fn should_trigger_file_completion(&self, request: &AutocompleteRequest) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serialises_with_upstream_camel_case_keys() {
        let request = AutocompleteRequest::new(&["#29".to_string()], 0, 3, false);
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "lines": ["#29"],
                "cursorLine": 0,
                "cursorCol": 3,
                "force": false,
            })
        );
    }

    #[test]
    fn text_before_cursor_clamps_to_the_line() {
        let request = AutocompleteRequest::new(&["hello".to_string()], 0, 99, false);
        assert_eq!(request.text_before_cursor(), "hello");
        let missing = AutocompleteRequest::new(&["a".to_string()], 7, 1, false);
        assert_eq!(missing.text_before_cursor(), "");
    }

    #[test]
    fn suggestions_round_trip_through_json() {
        let raw = serde_json::json!({
            "items": [{"value": "#2983", "label": "#2983", "description": "issue"}],
            "prefix": "#29",
        });
        let parsed: AutocompleteSuggestions = serde_json::from_value(raw).unwrap();
        assert_eq!(parsed.prefix, "#29");
        assert_eq!(parsed.items[0].value, "#2983");
        assert_eq!(parsed.items[0].description.as_deref(), Some("issue"));
    }
}
