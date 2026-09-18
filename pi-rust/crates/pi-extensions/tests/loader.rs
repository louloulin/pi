//! Smoke tests for `pi-extensions` loader.

use std::path::PathBuf;

use pi_extensions::loader::ExtensionSearchPaths;

#[test]
fn candidates_skip_missing_dirs() {
    let paths = ExtensionSearchPaths {
        global: None,
        project: Some(PathBuf::from("/tmp/__nonexistent_pi_extensions__")),
    };
    let cands = paths.candidates();
    assert!(cands.is_empty(), "expected no candidates, got {cands:?}");
}

#[test]
fn entry_for_uses_file_stem() {
    let paths = ExtensionSearchPaths::default();
    let entry = paths.entry_for(PathBuf::from("/tmp/extensions/summarize.ts").as_path());
    assert_eq!(entry.id, "summarize");
    assert_eq!(entry.source, PathBuf::from("/tmp/extensions/summarize.ts"));
}

#[test]
fn registry_records_tools() {
    use pi_extensions::registry::ExtensionRegistry;
    use pi_protocol::ToolDefinition;
    use serde_json::json;

    let mut reg = ExtensionRegistry::new();
    reg.register(
        pi_extensions::api::ExtensionEntry {
            source: PathBuf::from("/tmp/x.ts"),
            id: "x".into(),
            label: None,
        },
        pi_extensions::api::ExtensionCapabilities {
            tools: vec![ToolDefinition {
                name: "echo".into(),
                label: "Echo".into(),
                description: "Echo a string".into(),
                parameters: json!({"type": "object"}),
                metadata: None,
            }],
        },
    );

    assert!(reg.contains("x"));
    let tools: Vec<_> = reg.tools().map(|t| t.name.clone()).collect();
    assert_eq!(tools, vec!["echo".to_string()]);
}
