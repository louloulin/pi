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

    /// Iterator over all registered tools (across extensions).
    pub fn tools(&self) -> impl Iterator<Item = &ToolDefinition> {
        self.by_id.values().flat_map(|c| c.tools.iter())
    }

    /// Iterator over registered extension entries.
    pub fn extensions(&self) -> impl Iterator<Item = &ExtensionEntry> {
        self.entries.iter()
    }
}
