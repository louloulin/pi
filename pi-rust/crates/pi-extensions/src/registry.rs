//! In-memory registry of loaded extensions + their registered tools.

use std::collections::HashMap;

use pi_protocol::ToolDefinition;

use crate::api::{ExtensionCapabilities, ExtensionEntry};

/// Registry mapping extension IDs to their registered capabilities.
#[derive(Debug, Default)]
pub struct ExtensionRegistry {
    by_id: HashMap<String, ExtensionCapabilities>,
    entries: Vec<ExtensionEntry>,
}

impl ExtensionRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an extension and its capabilities.
    pub fn register(&mut self, entry: ExtensionEntry, capabilities: ExtensionCapabilities) {
        self.by_id.insert(entry.id.clone(), capabilities);
        self.entries.push(entry);
    }

    /// True when an extension with the given id is registered.
    pub fn contains(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    /// Replace the tool list of one extension, leaving every other
    /// entry (and its tools) untouched.
    ///
    /// Loading a second extension must not drop the first one's tools,
    /// so [`JsExtensionHost::load`](crate::JsExtensionHost::load) folds
    /// each load's registrations into its own id instead of rebuilding
    /// the whole registry.
    pub fn set_tools(&mut self, id: &str, tools: Vec<ToolDefinition>) {
        self.by_id.entry(id.to_string()).or_default().tools = tools;
    }

    /// Iterator over all registered tools (across extensions).
    pub fn tools(&self) -> impl Iterator<Item = &ToolDefinition> {
        self.by_id.values().flat_map(|c| c.tools.iter())
    }

    /// Find the extension id that owns a tool by its tool `name`.
    /// Used by `extension_isolation::execute_tool` to route a tool
    /// call to the per-extension runtime that registered it.
    pub fn tool_owner(&self, name: &str) -> Option<String> {
        self.by_id
            .iter()
            .find(|(_, caps)| caps.tools.iter().any(|t| t.name == name))
            .map(|(id, _)| id.clone())
    }

    /// Iterator over `(tool_name, owning_extension_id)` pairs.
    /// Used by `extension_isolation::registered_tool_names` and
    /// `registered_tools` to attribute every tool to the extension
    /// that registered it.
    pub fn tool_pairs(&self) -> impl Iterator<Item = (String, String)> + '_ {
        self.by_id.iter().flat_map(|(id, caps)| {
            let id = id.clone();
            caps.tools
                .iter()
                .map(move |t| (t.name.clone(), id.clone()))
        })
    }

    /// Iterator over registered extension entries.
    pub fn extensions(&self) -> impl Iterator<Item = &ExtensionEntry> {
        self.entries.iter()
    }
}
