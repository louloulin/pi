//! Deferred tool splitting — port of `packages/ai/src/utils/deferred-tools.ts`.
//!
//! The provider-side "tools loaded mid-transcript" feature (Anthropic's
//! `defer_loading`, OpenAI's additional tools) needs to split the current tool
//! set into definitions that go in the request prefix and definitions a
//! transcript tool result said it just added.
//!
//! # Deliberate differences from the TypeScript implementation
//!
//! **Not wired to any provider.** `pi-ai`'s provider compat layer has no
//! `supportsAdditionalTools` flag (`grep -rn "supports_additional"
//! crates/pi-ai` is empty), so nothing calls this yet; it is the util + tests
//! half of the port. Wiring precondition: a `Compat::supports_additional_tools`
//! field plus the provider request-side split (a separate stage).
//!
//! **Added tool names are supplied by the caller.** Upstream reads
//! `message.addedToolNames` from `toolResult` messages. Rust
//! [`ToolResult`](pi_protocol::ToolResult) has no such field, and adding one is
//! not a leaf change: `ToolResult` has ~20 struct-literal construction sites
//! across `pi-agent-core`, `pi-coding-agent`, `pi-tui`, `pi-evals` and the
//! `pi-protocol` wire tests, all of which would have to grow the field. Since
//! this issue does not wire the util, the extraction stays with the caller and
//! [`split_deferred_tools`] takes the already-extracted names. When the
//! protocol field lands, the caller passes `message.content`'s names and this
//! signature is unchanged.
//!
//! One semantic consequence: upstream checks each added name against the
//! assistant tool calls seen *so far* while walking the transcript, so a name
//! added before an assistant call to it stays deferred. This port subtracts
//! the union of all assistant tool-call names from the union of added names,
//! which only differs in that (pathological) ordering.

use std::collections::BTreeSet;

use pi_protocol::{Content, Context, Role, ToolDefinition};

/// Result of [`split_deferred_tools`].
///
/// Upstream returns `{ immediate: Tool[]; deferred: Map<string, Tool> }`. The
/// deferred half is an ordered `Vec` of `(normalized name, definition)` pairs
/// instead of a map: it keeps upstream's `Map` insertion order (first
/// occurrence in the tool list) without a new dependency, and is still a
/// one-liner to collect into a `HashMap` when a caller wants lookups.
#[derive(Debug, Clone, PartialEq)]
pub struct SplitDeferredTools<'a> {
    /// Definitions to send in the request prefix, in first-occurrence order.
    pub immediate: Vec<&'a ToolDefinition>,
    /// Definitions a transcript tool result added, keyed by normalized name.
    pub deferred: Vec<(String, &'a ToolDefinition)>,
}

/// `identityToolName` — the default [`split_deferred_tools`] normalizer.
pub fn identity_tool_name(name: &str) -> String {
    name.to_string()
}

/// Split the context's tools into prefix and transcript-loaded definitions.
///
/// Mirrors `splitDeferredTools(context, enabled, normalizeName)`, except that
/// `added_tool_names` is passed in instead of read off the transcript (see the
/// module docs) and a normalizer is always required — pass
/// [`identity_tool_name`] for upstream's default.
///
/// Semantics:
///
/// * tools are deduplicated by normalized name, the later definition winning;
/// * `enabled == false` returns every tool immediately and nothing deferred;
/// * a name already used by an assistant `toolCall` in the transcript is
///   **not** deferred (the model already saw it);
/// * otherwise a name in `added_tool_names` is deferred.
///
/// Names in `added_tool_names` that match no current tool are ignored, like
/// upstream's `context.tools.filter(...)`.
pub fn split_deferred_tools<'a>(
    context: &'a Context,
    enabled: bool,
    added_tool_names: &[String],
    normalize_name: impl Fn(&str) -> String,
) -> SplitDeferredTools<'a> {
    // Upstream's `uniqueTools` Map: first occurrence fixes the position, a
    // later duplicate overwrites the value in place. Tool sets are small, so
    // the linear scan is cheaper than allocating a second map.
    let mut unique_tools: Vec<(String, &'a ToolDefinition)> = Vec::new();
    for tool in &context.tools {
        let name = normalize_name(&tool.name);
        match unique_tools.iter_mut().find(|(seen, _)| *seen == name) {
            Some(slot) => slot.1 = tool,
            None => unique_tools.push((name, tool)),
        }
    }

    if !enabled {
        return SplitDeferredTools {
            immediate: unique_tools.into_iter().map(|(_, tool)| tool).collect(),
            deferred: Vec::new(),
        };
    }

    let mut used_names = BTreeSet::new();
    for message in &context.messages {
        if message.role != Role::Assistant {
            continue;
        }
        for block in &message.content {
            if let Content::ToolCall(call) = block {
                used_names.insert(normalize_name(&call.name));
            }
        }
    }

    let mut deferred_names = BTreeSet::new();
    for name in added_tool_names {
        let normalized = normalize_name(name);
        if !used_names.contains(&normalized) {
            deferred_names.insert(normalized);
        }
    }

    let mut immediate = Vec::new();
    let mut deferred = Vec::new();
    for (name, tool) in unique_tools {
        if deferred_names.contains(&name) {
            deferred.push((name, tool));
        } else {
            immediate.push(tool);
        }
    }
    SplitDeferredTools {
        immediate,
        deferred,
    }
}
