//! System prompt assembly.
//!
//! Rust port of `packages/coding-agent/src/core/system-prompt.ts`. The
//! prompt is rebuilt from live state (which tools are registered, which
//! context files and skills the resource loader found) rather than being
//! a constant, so a project's `AGENTS.md` and skills reach the model.
//!
//! Two shapes, matching the TS builder:
//!
//! - `custom_prompt` set (from `~/.pi/agent/SYSTEM.md`): the custom text
//!   *replaces* the built-in prompt; only the appended text, project
//!   context, skills, and the working directory are added around it;
//! - otherwise the built-in coding-assistant prompt is emitted with the
//!   available-tools list, guidelines, pi documentation pointers, and the
//!   same trailing sections.
//!
//! ```
//! use pi_coding_agent::system_prompt::{build_system_prompt, SystemPromptOptions};
//!
//! let prompt = build_system_prompt(&SystemPromptOptions {
//!     cwd: "/work".into(),
//!     selected_tools: vec!["read".to_string(), "bash".to_string()],
//!     ..Default::default()
//! });
//! assert!(prompt.contains("Current working directory: /work"));
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::context_files::ContextFile;
use crate::skills::{format_skills_for_prompt, Skill, SkillReadTool};

/// Tools listed in the prompt when the caller passes no explicit set —
/// mirrors the TS default of `[read, bash, edit, write]`.
pub const DEFAULT_SELECTED_TOOLS: [&str; 4] = ["read", "bash", "edit", "write"];

/// Documentation locations advertised in the built-in prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiDocs {
    /// Path to the main README.
    pub readme: PathBuf,
    /// Path to the `docs/` directory.
    pub docs: PathBuf,
    /// Path to the `examples/` directory.
    pub examples: PathBuf,
}

/// One built-in tool's system-prompt contribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolPromptContribution {
    /// One-line summary shown in the `Available tools` list.
    pub snippet: &'static str,
    /// Guideline bullets contributed by the tool.
    pub guidelines: &'static [&'static str],
}

/// Prompt contributions of the built-in tools, ported verbatim from the
/// `*ToolSystemPromptContribution` constants in the TS tool modules.
pub const BUILTIN_TOOL_CONTRIBUTIONS: [(&str, ToolPromptContribution); 7] = [
    (
        "read",
        ToolPromptContribution {
            snippet: "Read file contents",
            guidelines: &["Use read to examine files instead of cat or sed."],
        },
    ),
    (
        "write",
        ToolPromptContribution {
            snippet: "Create or overwrite files",
            guidelines: &["Use write only for new files or complete rewrites."],
        },
    ),
    (
        "edit",
        ToolPromptContribution {
            snippet: "Make precise file edits with exact text replacement, including multiple disjoint edits in one call",
            guidelines: &[
                "Use edit for precise changes (edits[].oldText must match exactly)",
                "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls",
                "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.",
                "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.",
            ],
        },
    ),
    (
        "bash",
        ToolPromptContribution {
            snippet: "Execute bash commands (ls, grep, find, etc.)",
            guidelines: &["You can inspect PI_* environment variables for current model and session details."],
        },
    ),
    (
        "find",
        ToolPromptContribution {
            snippet: "Find files by glob pattern (respects .gitignore)",
            guidelines: &[],
        },
    ),
    (
        "grep",
        ToolPromptContribution {
            snippet: "Search file contents for patterns (respects .gitignore)",
            guidelines: &[],
        },
    ),
    (
        "ls",
        ToolPromptContribution {
            snippet: "List directory contents",
            guidelines: &[],
        },
    ),
];

/// The prompt contribution of a built-in tool, if it has one.
pub fn builtin_tool_contribution(tool: &str) -> Option<ToolPromptContribution> {
    BUILTIN_TOOL_CONTRIBUTIONS
        .iter()
        .find(|(name, _)| *name == tool)
        .map(|(_, contribution)| *contribution)
}

/// Build the `tool_snippets` / `prompt_guidelines` pair for a tool set.
///
/// Tools without a known contribution contribute nothing; the prompt
/// builder then leaves them out of the available-tools list, which is how
/// the TS side keeps extension tools from being described wrongly.
pub fn builtin_prompt_contributions(
    selected_tools: &[String],
) -> (BTreeMap<String, String>, Vec<String>) {
    let mut snippets = BTreeMap::new();
    let mut guidelines = Vec::new();

    for tool in selected_tools {
        if let Some(contribution) = builtin_tool_contribution(tool) {
            snippets.insert(tool.clone(), contribution.snippet.to_string());
            guidelines.extend(contribution.guidelines.iter().map(|line| line.to_string()));
        }
    }

    (snippets, guidelines)
}

/// Options for [`build_system_prompt`].
#[derive(Debug, Clone, Default)]
pub struct SystemPromptOptions {
    /// Custom prompt that replaces the built-in one.
    pub custom_prompt: Option<String>,
    /// Tool names to list. Defaults to [`DEFAULT_SELECTED_TOOLS`].
    pub selected_tools: Vec<String>,
    /// One-line tool summaries, keyed by tool name.
    pub tool_snippets: BTreeMap<String, String>,
    /// Extra guideline bullets, appended to the tool-derived ones.
    pub prompt_guidelines: Vec<String>,
    /// Text appended after the prompt body (loader + `--append-system-prompt`).
    pub append_system_prompt: Option<String>,
    /// Working directory, printed with forward slashes.
    pub cwd: PathBuf,
    /// Pre-loaded `AGENTS.md` / `CLAUDE.md` files.
    pub context_files: Vec<ContextFile>,
    /// Pre-loaded skills.
    pub skills: Vec<Skill>,
    /// Documentation roots; the documentation section is omitted when
    /// `None` (see [`pi_docs_paths`]).
    pub docs: Option<PiDocs>,
}

/// Build the system prompt.
pub fn build_system_prompt(options: &SystemPromptOptions) -> String {
    let prompt_cwd = options.cwd.to_string_lossy().replace('\\', "/");
    let append_section = options
        .append_system_prompt
        .as_deref()
        .filter(|text| !text.is_empty())
        .map(|text| format!("\n\n{text}"))
        .unwrap_or_default();

    let tools: Vec<String> = if options.selected_tools.is_empty() {
        DEFAULT_SELECTED_TOOLS
            .iter()
            .map(|tool| tool.to_string())
            .collect()
    } else {
        options.selected_tools.clone()
    };

    // The skills block is only useful when the model has a tool that can
    // open a skill file.
    let skill_read_tool = tools.iter().find_map(|tool| match tool.as_str() {
        "read" => Some(SkillReadTool::Read),
        "bash" => Some(SkillReadTool::Bash),
        _ => None,
    });

    let mut prompt = match options.custom_prompt.as_deref() {
        Some(custom) => custom.to_string(),
        None => default_prompt_body(options, &tools),
    };

    prompt.push_str(&append_section);
    prompt.push_str(&render_context_files(&options.context_files));

    if let Some(read_tool) = skill_read_tool {
        if !options.skills.is_empty() {
            prompt.push_str(&format_skills_for_prompt(&options.skills, read_tool));
        }
    }

    prompt.push_str(&format!("\nCurrent working directory: {prompt_cwd}"));
    prompt
}

/// Render the `<project_context>` block (empty when no files loaded).
fn render_context_files(context_files: &[ContextFile]) -> String {
    if context_files.is_empty() {
        return String::new();
    }

    let mut out = String::from("\n\n<project_context>\n\n");
    out.push_str("Project-specific instructions and guidelines:\n\n");
    for file in context_files {
        out.push_str(&format!(
            "<project_instructions path=\"{}\">\n{}\n</project_instructions>\n\n",
            file.path.to_string_lossy(),
            file.content
        ));
    }
    out.push_str("</project_context>\n");
    out
}

/// The built-in (non-custom) prompt body.
fn default_prompt_body(options: &SystemPromptOptions, tools: &[String]) -> String {
    let visible_tools: Vec<&String> = tools
        .iter()
        .filter(|tool| options.tool_snippets.contains_key(tool.as_str()))
        .collect();

    let tools_list = if visible_tools.is_empty() {
        "(none)".to_string()
    } else {
        visible_tools
            .iter()
            .map(|tool| {
                format!(
                    "- {tool}: {}",
                    options
                        .tool_snippets
                        .get(tool.as_str())
                        .map(String::as_str)
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let guidelines = render_guidelines(options, tools);

    let mut prompt = format!(
        "You are an expert coding assistant operating inside pi, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.\n\nAvailable tools:\n{tools_list}\n\nIn addition to the tools above, you may have access to other custom tools depending on the project.\n\nGuidelines:\n{guidelines}"
    );

    if let Some(docs) = &options.docs {
        prompt.push_str(&format!(
            "\n\nPi documentation (read only when the user asks about pi itself, its SDK, extensions, themes, skills, or TUI):\n- Main documentation: {}\n- Additional docs: {}\n- Examples: {} (extensions, custom tools, SDK)\n- When reading pi docs or examples, resolve docs/... under Additional docs and examples/... under Examples, not the current working directory\n- When asked about: extensions (docs/extensions.md, examples/extensions/), themes (docs/themes.md), skills (docs/skills.md), prompt templates (docs/prompt-templates.md), TUI components (docs/tui.md), keybindings (docs/keybindings.md), SDK integrations (docs/sdk.md), custom providers (docs/custom-provider.md), adding models (docs/models.md), pi packages (docs/packages.md), environment variables (docs/environment-variables.md)\n- When working on pi topics, read the docs and examples, and follow .md cross-references before implementing\n- Always read pi .md files completely and follow links to related docs (e.g., tui.md for TUI API details)",
            docs.readme.to_string_lossy(),
            docs.docs.to_string_lossy(),
            docs.examples.to_string_lossy()
        ));
    }

    prompt
}

/// Tool-aware guidelines, de-duplicated and in first-seen order.
fn render_guidelines(options: &SystemPromptOptions, tools: &[String]) -> String {
    let mut guidelines: Vec<String> = Vec::new();
    let mut push = |guideline: String| {
        if !guidelines.contains(&guideline) {
            guidelines.push(guideline);
        }
    };

    let has = |tool: &str| tools.iter().any(|candidate| candidate == tool);
    let has_shell = has("bash") || has("powershell");
    let has_navigation = has("grep") || has("find") || has("ls");

    // Without the dedicated navigation tools, the shell is the only way
    // to explore the tree.
    if has_shell && !has_navigation {
        if has("bash") && has("powershell") {
            push("Use bash or PowerShell for file operations like listing, searching, and finding files".to_string());
        } else if has("powershell") {
            push(
                "Use PowerShell for file operations like listing, searching, and finding files"
                    .to_string(),
            );
        } else {
            push("Use bash for file operations like ls, rg, find".to_string());
        }
    }

    for guideline in &options.prompt_guidelines {
        let normalized = guideline.trim();
        if !normalized.is_empty() {
            push(normalized.to_string());
        }
    }

    push("Be concise in your responses".to_string());
    push("Show file paths clearly when working with files".to_string());

    guidelines
        .iter()
        .map(|guideline| format!("- {guideline}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Resolve the documentation roots for the built-in prompt.
///
/// Upstream resolves them relative to the installed npm package. The Rust
/// binary looks, in order, at:
///
/// 1. `$PI_PACKAGE_DIR` (same override as upstream),
/// 2. `<exe dir>/../share/pi` (installed layout),
/// 3. `<exe dir>/../../..` — the `pi-rust` directory when running from
///    `target/<profile>/pi`.
///
/// The first candidate that has both a `README.md` and a `docs/` directory
/// wins; when none does the documentation section is omitted rather than
/// pointing the model at paths that do not exist.
pub fn pi_docs_paths() -> Option<PiDocs> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(dir) = std::env::var_os("PI_PACKAGE_DIR") {
        candidates.push(PathBuf::from(dir));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            candidates.push(exe_dir.join("..").join("share").join("pi"));
            if let Some(repo_dir) = exe_dir.parent().and_then(Path::parent) {
                candidates.push(repo_dir.to_path_buf());
            }
        }
    }

    for candidate in candidates {
        let docs_paths = PiDocs {
            readme: candidate.join("README.md"),
            docs: candidate.join("docs"),
            examples: candidate.join("examples"),
        };
        if docs_paths.readme.is_file() && docs_paths.docs.is_dir() {
            return Some(docs_paths);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::SkillSource;

    fn options() -> SystemPromptOptions {
        SystemPromptOptions {
            cwd: PathBuf::from("/work/project"),
            ..Default::default()
        }
    }

    fn skill(name: &str, description: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            file_path: PathBuf::from(format!("/skills/{name}/SKILL.md")),
            base_dir: PathBuf::from(format!("/skills/{name}")),
            source: SkillSource::User,
            disable_model_invocation: false,
        }
    }

    #[test]
    fn default_prompt_lists_tools_and_guidelines() {
        let (snippets, guidelines) = builtin_prompt_contributions(
            &["read", "bash", "edit", "write", "find", "grep", "ls"]
                .iter()
                .map(|tool| tool.to_string())
                .collect::<Vec<_>>(),
        );

        let prompt = build_system_prompt(&SystemPromptOptions {
            selected_tools: snippets.keys().cloned().collect(),
            tool_snippets: snippets,
            prompt_guidelines: guidelines,
            ..options()
        });

        assert!(prompt.starts_with("You are an expert coding assistant operating inside pi"));
        assert!(prompt.contains("- read: Read file contents"));
        assert!(prompt.contains("- bash: Execute bash commands (ls, grep, find, etc.)"));
        assert!(prompt.contains("- ls: List directory contents"));
        assert!(prompt.contains("- Use read to examine files instead of cat or sed."));
        assert!(prompt.contains("- Be concise in your responses"));
        assert!(prompt.contains("- Show file paths clearly when working with files"));
        // Navigation tools are present, so the shell-exploration fallback
        // guideline must not be added.
        assert!(!prompt.contains("Use bash for file operations like ls, rg, find"));
        assert!(prompt.ends_with("Current working directory: /work/project"));
    }

    #[test]
    fn bash_only_selection_gets_the_exploration_guideline() {
        let (snippets, guidelines) =
            builtin_prompt_contributions(&["read".to_string(), "bash".to_string()]);

        let prompt = build_system_prompt(&SystemPromptOptions {
            selected_tools: vec!["read".to_string(), "bash".to_string()],
            tool_snippets: snippets,
            prompt_guidelines: guidelines,
            ..options()
        });

        assert!(prompt.contains("- Use bash for file operations like ls, rg, find"));
    }

    #[test]
    fn tools_without_a_snippet_are_hidden_and_none_is_reported() {
        let prompt = build_system_prompt(&SystemPromptOptions {
            selected_tools: vec!["custom_extension_tool".to_string()],
            ..options()
        });

        assert!(prompt.contains("Available tools:\n(none)"));
    }

    #[test]
    fn default_tool_list_matches_the_ts_default() {
        let prompt = build_system_prompt(&options());
        assert!(prompt.contains("Available tools:\n(none)"));
        assert!(prompt.contains("- Be concise in your responses"));
    }

    #[test]
    fn omits_the_documentation_section_without_docs() {
        let prompt = build_system_prompt(&options());
        assert!(!prompt.contains("Pi documentation"));
    }

    #[test]
    fn includes_the_documentation_section_with_docs() {
        let prompt = build_system_prompt(&SystemPromptOptions {
            docs: Some(PiDocs {
                readme: PathBuf::from("/pkg/README.md"),
                docs: PathBuf::from("/pkg/docs"),
                examples: PathBuf::from("/pkg/examples"),
            }),
            ..options()
        });

        assert!(prompt.contains("- Main documentation: /pkg/README.md"));
        assert!(prompt.contains("- Additional docs: /pkg/docs"));
        assert!(prompt.contains("- Examples: /pkg/examples (extensions, custom tools, SDK)"));
        assert!(prompt.contains("docs/skills.md"));
    }

    #[test]
    fn appends_project_context_files_after_the_prompt() {
        let prompt = build_system_prompt(&SystemPromptOptions {
            context_files: vec![ContextFile {
                path: PathBuf::from("/work/project/AGENTS.md"),
                content: "Be careful.".to_string(),
            }],
            ..options()
        });

        assert!(prompt.contains("<project_context>"));
        assert!(prompt.contains(
            "<project_instructions path=\"/work/project/AGENTS.md\">\nBe careful.\n</project_instructions>"
        ));
        assert!(prompt.contains("</project_context>"));
        // Context files come before the working-directory line.
        let context = prompt.find("<project_context>").unwrap();
        let cwd = prompt.find("Current working directory").unwrap();
        assert!(context < cwd);
    }

    #[test]
    fn appends_skills_only_when_a_reading_tool_is_available() {
        let skills = vec![skill("demo", "A demo skill.")];

        let with_read = build_system_prompt(&SystemPromptOptions {
            selected_tools: vec!["read".to_string()],
            skills: skills.clone(),
            ..options()
        });
        assert!(with_read.contains("<available_skills>"));
        assert!(with_read.contains("<name>demo</name>"));

        let without_reader = build_system_prompt(&SystemPromptOptions {
            selected_tools: vec!["edit".to_string()],
            skills,
            ..options()
        });
        assert!(!without_reader.contains("<available_skills>"));
    }

    #[test]
    fn append_system_prompt_lands_before_the_context_files() {
        let prompt = build_system_prompt(&SystemPromptOptions {
            append_system_prompt: Some("EXTRA RULES".to_string()),
            context_files: vec![ContextFile {
                path: PathBuf::from("/work/project/AGENTS.md"),
                content: "context".to_string(),
            }],
            ..options()
        });

        let extra = prompt.find("EXTRA RULES").unwrap();
        let context = prompt.find("<project_context>").unwrap();
        assert!(extra < context);
    }

    #[test]
    fn custom_prompt_replaces_the_builtin_body() {
        let prompt = build_system_prompt(&SystemPromptOptions {
            custom_prompt: Some("You are a custom agent.".to_string()),
            context_files: vec![ContextFile {
                path: PathBuf::from("/work/project/AGENTS.md"),
                content: "Be careful.".to_string(),
            }],
            skills: vec![skill("demo", "A demo skill.")],
            selected_tools: vec!["read".to_string()],
            ..options()
        });

        assert!(prompt.starts_with("You are a custom agent."));
        assert!(!prompt.contains("Available tools:"));
        assert!(prompt.contains("<project_context>"));
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.ends_with("Current working directory: /work/project"));
    }

    #[test]
    fn custom_prompt_gets_append_and_cwd() {
        let prompt = build_system_prompt(&SystemPromptOptions {
            custom_prompt: Some("Custom.".to_string()),
            append_system_prompt: Some("Appended.".to_string()),
            ..options()
        });

        assert_eq!(
            prompt,
            "Custom.\n\nAppended.\nCurrent working directory: /work/project"
        );
    }

    #[test]
    fn windows_style_paths_are_printed_with_forward_slashes() {
        let prompt = build_system_prompt(&SystemPromptOptions {
            cwd: PathBuf::from("C:\\work\\project"),
            ..options()
        });

        assert!(prompt.ends_with("Current working directory: C:/work/project"));
    }

    #[test]
    fn documentation_paths_are_resolved_from_the_environment_override() {
        let temp = std::env::temp_dir().join(format!("pi-docs-{}", std::process::id()));
        std::fs::create_dir_all(temp.join("docs")).expect("create docs");
        std::fs::write(temp.join("README.md"), "# pi").expect("write readme");

        // `PI_PACKAGE_DIR` is process-wide; keep the assertion scoped to
        // this temporary directory and restore the previous value.
        let previous = std::env::var_os("PI_PACKAGE_DIR");
        std::env::set_var("PI_PACKAGE_DIR", &temp);
        let resolved = pi_docs_paths();
        match previous {
            Some(value) => std::env::set_var("PI_PACKAGE_DIR", value),
            None => std::env::remove_var("PI_PACKAGE_DIR"),
        }

        let resolved = resolved.expect("resolves docs from PI_PACKAGE_DIR");
        assert_eq!(resolved.readme, temp.join("README.md"));
        assert_eq!(resolved.docs, temp.join("docs"));

        let _ = std::fs::remove_dir_all(&temp);
    }
}
