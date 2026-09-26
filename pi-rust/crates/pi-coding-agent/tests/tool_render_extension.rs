//! P1-1 — `AgentTool::prompt_snippet` / `prompt_guidelines` reach the system
//! prompt (plan §6 P1-1).
//!
//! The TS `ToolDefinition` shape carries a `promptSnippet` (one-line summary
//! embedded before the parameter schema) and `promptGuidelines` (a list of
//! imperative bullets embedded after). Rust's [`AgentTool`](pi_coding_agent::tools::AgentTool)
//! trait gained the same two hooks so extension tools can customise their
//! prompt contributions without forcing the user to rebuild the static
//! [`BUILTIN_TOOL_CONTRIBUTIONS`](pi_coding_agent::system_prompt::BUILTIN_TOOL_CONTRIBUTIONS)
//! table. This test covers both halves:
//!
//! 1. A tool that overrides the trait methods surfaces its snippet +
//!    guidelines through [`system_prompt::tool_prompt_contributions_from`].
//! 2. A tool that does *not* override them falls back to the static
//!    `BUILTIN_TOOL_CONTRIBUTIONS` table — the seven built-in tools keep
//!    their hand-curated copy without having to override the trait.
//! 3. The full system prompt assembly includes both halves — i.e. the new
//!    helper is the one resource_loader actually uses.
//!
//! The test deliberately builds a tiny `DynAgentTool` for the override
//! case; reusing a real built-in would muddle "did this tool's snippet
//! land" with "did the static fallback also land", and the static path is
//! already covered by the existing `system_prompt::tests` suite.

#![cfg(test)]

use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::system_prompt::{
    build_system_prompt, tool_prompt_contributions_from, SystemPromptOptions,
};
use pi_coding_agent::tools::{AgentTool, DynAgentTool, ToolError, ToolOutput};
use pi_protocol::ToolExecutionMode;
use serde_json::json;

/// Tiny `AgentTool` whose `prompt_snippet` / `prompt_guidelines` overrides
/// prove the new trait methods propagate through the system-prompt builder.
struct PromptOverrideTool;

#[async_trait]
impl AgentTool for PromptOverrideTool {
    fn name(&self) -> &str {
        "prompt_override"
    }

    fn label(&self) -> &str {
        "Prompt override tool"
    }

    fn description(&self) -> &str {
        "Test tool: drives the P1-1 wiring."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
        })
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _abort: pi_coding_agent::tools::AbortLike,
    ) -> Result<ToolOutput, ToolError> {
        unreachable!("this test never runs the tool")
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some("OVERRIDE_SNIPPET_MARKER")
    }

    fn prompt_guidelines(&self) -> &[&str] {
        &[
            "OVERRIDE_GUIDELINE_ONE",
            "OVERRIDE_GUIDELINE_TWO",
        ]
    }
}

/// `AgentTool` that *does not* override the new methods, so the helper
/// has to fall back to the static `BUILTIN_TOOL_CONTRIBUTIONS` table.
/// Reusing `read` here would force the test to import the real `read`
/// module; building an inline tool with the same name routes through
/// the static lookup the same way the live built-in would.
struct ReadLikeTool;

#[async_trait]
impl AgentTool for ReadLikeTool {
    fn name(&self) -> &str {
        "read"
    }

    fn label(&self) -> &str {
        "ReadLike"
    }

    fn description(&self) -> &str {
        "Stub."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object"})
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _abort: pi_coding_agent::tools::AbortLike,
    ) -> Result<ToolOutput, ToolError> {
        unreachable!()
    }
}

#[test]
fn override_tool_surfaces_its_own_snippet_and_guidelines() {
    let tools: Vec<DynAgentTool> = vec![Arc::new(PromptOverrideTool)];
    let (snippets, guidelines) = tool_prompt_contributions_from(&tools);

    assert_eq!(
        snippets.get("prompt_override").map(String::as_str),
        Some("OVERRIDE_SNIPPET_MARKER"),
        "the trait method must drive the prompt snippet; the static table \
         must not overwrite an override",
    );
    assert!(
        guidelines
            .iter()
            .any(|g| g == "OVERRIDE_GUIDELINE_ONE"),
        "guideline one must reach the prompt"
    );
    assert!(
        guidelines
            .iter()
            .any(|g| g == "OVERRIDE_GUIDELINE_TWO"),
        "guideline two must reach the prompt"
    );
}

#[test]
fn tools_without_overrides_fall_back_to_the_static_table() {
    let tools: Vec<DynAgentTool> = vec![Arc::new(ReadLikeTool)];
    let (snippets, guidelines) = tool_prompt_contributions_from(&tools);

    // `read` is in BUILTIN_TOOL_CONTRIBUTIONS with snippet "Read file
    // contents" and a single guideline. The fallback must surface both.
    assert_eq!(
        snippets.get("read").map(String::as_str),
        Some("Read file contents"),
        "a tool that does not override must still get the static snippet",
    );
    assert!(
        guidelines
            .iter()
            .any(|g| g.contains("Use read to examine files")),
        "the static guideline must reach the prompt",
    );
}

#[test]
fn override_tool_with_no_snippet_but_with_guidelines() {
    /// Override `prompt_guidelines` only. The static fallback must NOT
    /// overwrite an explicit (even if partial) contribution.
    struct GuidelineOnly;

    #[async_trait]
    impl AgentTool for GuidelineOnly {
        fn name(&self) -> &str {
            "read"
        }
        fn label(&self) -> &str {
            "GuidelineOnly"
        }
        fn description(&self) -> &str {
            "Stub."
        }
        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object"})
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
            _abort: pi_coding_agent::tools::AbortLike,
        ) -> Result<ToolOutput, ToolError> {
            unreachable!()
        }
        fn prompt_guidelines(&self) -> &[&str] {
            &["GUIDELINE_ONLY_BULLET"]
        }
    }

    let tools: Vec<DynAgentTool> = vec![Arc::new(GuidelineOnly)];
    let (snippets, guidelines) = tool_prompt_contributions_from(&tools);

    // Static fallback for snippet (no override), override for guidelines.
    assert_eq!(snippets.get("read").map(String::as_str), Some("Read file contents"));
    assert!(
        guidelines.iter().any(|g| g == "GUIDELINE_ONLY_BULLET"),
        "the explicit override must reach the prompt"
    );
    assert!(
        !guidelines.iter().any(|g| g.contains("Use read to examine files")),
        "the static guideline must not double up when the tool overrode the field"
    );
}

#[test]
fn no_tools_produces_empty_contributions() {
    let (snippets, guidelines) = tool_prompt_contributions_from(&[]);
    assert!(snippets.is_empty(), "no tools ⇒ no snippets");
    assert!(guidelines.is_empty(), "no tools ⇒ no guidelines");
}

#[test]
fn override_snippet_lands_in_the_assembled_system_prompt() {
    // End-to-end: the snippet must appear in the assembled prompt body.
    // The system-prompt builder keeps `selected_tools` filter, so we list
    // only the override tool here.
    let tools: Vec<DynAgentTool> = vec![Arc::new(PromptOverrideTool)];
    let (snippets, guidelines) = tool_prompt_contributions_from(&tools);

    let prompt = build_system_prompt(&SystemPromptOptions {
        selected_tools: vec!["prompt_override".to_string()],
        tool_snippets: snippets,
        prompt_guidelines: guidelines,
        cwd: "/tmp".into(),
        ..SystemPromptOptions::default()
    });

    assert!(
        prompt.contains("OVERRIDE_SNIPPET_MARKER"),
        "the snippet must reach the assembled prompt; got:\n{prompt}",
    );
    assert!(
        prompt.contains("OVERRIDE_GUIDELINE_ONE") && prompt.contains("OVERRIDE_GUIDELINE_TWO"),
        "both guidelines must reach the assembled prompt; got:\n{prompt}",
    );
}

/// Sanity check: an extension tool's `execution_mode` default is
/// preserved when the new prompt hooks are added. The trait gains two
/// methods; nothing else should change.
#[test]
fn default_execution_mode_still_returns_none() {
    let mode: Option<ToolExecutionMode> = PromptOverrideTool.execution_mode();
    assert!(
        mode.is_none(),
        "the default `execution_mode` must remain None; got {mode:?}"
    );
}