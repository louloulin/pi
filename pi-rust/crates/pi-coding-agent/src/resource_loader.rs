//! Resource loading for the system prompt.
//!
//! Rust port of the prompt-relevant half of
//! `packages/coding-agent/src/core/resource-loader.ts`: it gathers the
//! custom prompt, append-prompt files, project context files, and skills
//! for a working directory and assembles them with
//! [`build_system_prompt`].
//!
//! [`load_resources`] is the testable core (explicit `cwd` / `agent_dir`);
//! [`build_cli_system_prompt`] is the thin wrapper the `pi` binary calls
//! with parsed CLI flags.
//!
//! ```
//! use pi_coding_agent::resource_loader::{load_resources, ResourceLoadOptions};
//!
//! let loaded = load_resources(&ResourceLoadOptions {
//!     cwd: std::env::current_dir().unwrap(),
//!     agent_dir: std::path::PathBuf::from("/nonexistent-agent-dir"),
//!     ..Default::default()
//! });
//! assert!(loaded.build_system_prompt(&std::env::current_dir().unwrap()).contains("coding agent"));
//! ```

use std::path::{Path, PathBuf};

use crate::cli::Cli;
use crate::context_files::{
    discover_append_system_prompt_file, discover_system_prompt_file, load_project_context_files,
    read_prompt_file, ContextFile,
};
use crate::paths::agent_dir_or_default;
use crate::skills::{
    load_skills, LoadSkillsOptions, Skill, SkillDiagnostic,
};
use crate::system_prompt::{
    build_system_prompt, builtin_prompt_contributions, pi_docs_paths, SystemPromptOptions,
};

/// Where resources are loaded from.
#[derive(Debug, Clone, Default)]
pub struct ResourceLoadOptions {
    /// Working directory — the base for `.pi/` discovery.
    pub cwd: PathBuf,
    /// Agent config directory, normally `~/.pi/agent`.
    pub agent_dir: PathBuf,
    /// Extra skill files / directories (`--skill`).
    pub skill_paths: Vec<PathBuf>,
    /// `--no-skills`: skip skill discovery entirely.
    pub no_skills: bool,
    /// `--no-context-files`: skip `AGENTS.md` / `CLAUDE.md` discovery.
    pub no_context_files: bool,
    /// `--append-system-prompt` segments, appended after
    /// `~/.pi/agent/APPEND_SYSTEM.md`.
    pub append_system_prompt: Vec<String>,
}

/// Everything the system prompt is built from.
#[derive(Debug, Clone, Default)]
pub struct LoadedResources {
    /// `~/.pi/agent/SYSTEM.md`, which replaces the built-in prompt.
    pub custom_prompt: Option<String>,
    /// Concatenated append segments (loader file first, then CLI).
    pub append_system_prompt: Option<String>,
    /// Loaded project context files.
    pub context_files: Vec<ContextFile>,
    /// Loaded skills.
    pub skills: Vec<Skill>,
    /// Non-fatal skill problems worth reporting on stderr.
    pub diagnostics: Vec<SkillDiagnostic>,
}

impl LoadedResources {
    /// Assemble the system prompt for `cwd`.
    pub fn build_system_prompt(&self, cwd: &Path) -> String {
        // The prompt describes exactly the tools the agent can call: the
        // built-in bundle in registration order. Extension tools are
        // advertised to the model through the tool registry itself.
        let selected_tools: Vec<String> = crate::tools::default_tool_bundle()
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();
        let (tool_snippets, prompt_guidelines) = builtin_prompt_contributions(&selected_tools);

        build_system_prompt(&SystemPromptOptions {
            custom_prompt: self.custom_prompt.clone(),
            selected_tools,
            tool_snippets,
            prompt_guidelines,
            append_system_prompt: self.append_system_prompt.clone(),
            cwd: cwd.to_path_buf(),
            context_files: self.context_files.clone(),
            skills: self.skills.clone(),
            docs: pi_docs_paths(),
        })
    }
}

/// Load the resources for one run.
pub fn load_resources(options: &ResourceLoadOptions) -> LoadedResources {
    let cwd = crate::paths::absolute(&options.cwd);
    let agent_dir = crate::paths::absolute(&options.agent_dir);

    let context_files = if options.no_context_files {
        Vec::new()
    } else {
        load_project_context_files(&cwd, &agent_dir)
    };

    let mut diagnostics = Vec::new();
    let skills = if options.no_skills {
        Vec::new()
    } else {
        let loaded = load_skills(&LoadSkillsOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            skill_paths: options.skill_paths.clone(),
            include_defaults: true,
        });
        diagnostics.extend(loaded.diagnostics);
        loaded.skills
    };

    let custom_prompt = discover_system_prompt_file(&agent_dir)
        .and_then(|path| read_prompt_file(&path))
        .filter(|content| !content.trim().is_empty());

    let mut append_segments: Vec<String> = Vec::new();
    if let Some(content) =
        discover_append_system_prompt_file(&agent_dir).and_then(|path| read_prompt_file(&path))
    {
        if !content.is_empty() {
            append_segments.push(content);
        }
    }
    append_segments.extend(options.append_system_prompt.iter().cloned());
    append_segments.retain(|segment| !segment.is_empty());

    LoadedResources {
        custom_prompt,
        append_system_prompt: (!append_segments.is_empty())
            .then(|| append_segments.join("\n\n")),
        context_files,
        skills,
        diagnostics,
    }
}

/// Build the system prompt from parsed CLI flags.
///
/// Returns the prompt plus the diagnostics the caller should report; the
/// loader itself never writes to stderr.
pub fn build_cli_system_prompt(cli: &Cli) -> (String, Vec<SkillDiagnostic>) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let loaded = load_resources(&ResourceLoadOptions {
        cwd: cwd.clone(),
        agent_dir: agent_dir_or_default(),
        skill_paths: cli.skill.clone(),
        no_skills: cli.no_skills,
        no_context_files: cli.no_context_files,
        append_system_prompt: cli.append_system_prompt.clone(),
    });

    let prompt = loaded.build_system_prompt(&cwd);
    (prompt, loaded.diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::absolute;
    use clap::Parser;
    use std::fs;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "pi-resources-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or(0)
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn write(&self, relative: &str, content: &str) {
            let path = self.path.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent");
            }
            fs::write(path, content).expect("write fixture");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn load(temp: &TempDir, cwd: &str) -> LoadedResources {
        load_resources(&ResourceLoadOptions {
            cwd: temp.path.join(cwd),
            agent_dir: temp.path.join("agent"),
            ..Default::default()
        })
    }

    #[test]
    fn prompt_contains_builtin_tools_context_and_skills() {
        let temp = TempDir::new("full");
        temp.write("project/AGENTS.md", "Be careful.\n");
        temp.write(
            "project/.pi/skills/demo/SKILL.md",
            "---\nname: demo\ndescription: Demo skill.\n---\n\n# Demo\n",
        );

        let loaded = load(&temp, "project");
        let prompt = loaded.build_system_prompt(&absolute(&temp.path.join("project")));

        assert!(prompt.contains("- read: Read file contents"));
        assert!(prompt.contains("- ls: List directory contents"));
        assert!(prompt.contains("Project-specific instructions and guidelines:"));
        assert!(prompt.contains("Be careful."));
        assert!(prompt.contains("<name>demo</name>"));
        assert!(prompt.contains("Demo skill."));
        assert!(prompt.contains(&format!(
            "Current working directory: {}",
            absolute(&temp.path.join("project")).display()
        )));
        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    }

    #[test]
    fn no_context_files_and_no_skills_disable_discovery() {
        let temp = TempDir::new("disabled");
        temp.write("project/AGENTS.md", "Be careful.\n");
        temp.write(
            "project/.pi/skills/demo/SKILL.md",
            "---\nname: demo\ndescription: Demo skill.\n---\n",
        );

        let loaded = load_resources(&ResourceLoadOptions {
            cwd: temp.path.join("project"),
            agent_dir: temp.path.join("agent"),
            no_skills: true,
            no_context_files: true,
            ..Default::default()
        });

        let prompt = loaded.build_system_prompt(&temp.path.join("project"));
        assert!(loaded.skills.is_empty());
        assert!(loaded.context_files.is_empty());
        assert!(!prompt.contains("Be careful."));
        assert!(!prompt.contains("<available_skills>"));
    }

    #[test]
    fn global_system_and_append_prompt_files_are_honoured() {
        let temp = TempDir::new("system-md");
        temp.write("agent/SYSTEM.md", "You are a custom agent.");
        temp.write("agent/APPEND_SYSTEM.md", "Loader rule.");

        let loaded = load_resources(&ResourceLoadOptions {
            cwd: temp.path.join("project"),
            agent_dir: temp.path.join("agent"),
            append_system_prompt: vec!["CLI rule.".to_string()],
            ..Default::default()
        });

        assert_eq!(loaded.custom_prompt.as_deref(), Some("You are a custom agent."));
        assert_eq!(
            loaded.append_system_prompt.as_deref(),
            Some("Loader rule.\n\nCLI rule.")
        );

        let prompt = loaded.build_system_prompt(&temp.path.join("project"));
        assert!(prompt.starts_with("You are a custom agent."));
        let loader_rule = prompt.find("Loader rule.").unwrap();
        let cli_rule = prompt.find("CLI rule.").unwrap();
        assert!(loader_rule < cli_rule);
    }

    #[test]
    fn explicit_skill_paths_are_loaded() {
        let temp = TempDir::new("skill-paths");
        temp.write(
            "extra/extra-skill/SKILL.md",
            "---\nname: extra-skill\ndescription: Extra.\n---\n",
        );

        let loaded = load_resources(&ResourceLoadOptions {
            cwd: temp.path.join("project"),
            agent_dir: temp.path.join("agent"),
            skill_paths: vec![temp.path.join("extra")],
            ..Default::default()
        });

        assert_eq!(loaded.skills.len(), 1);
        assert_eq!(loaded.skills[0].name, "extra-skill");
    }

    #[test]
    fn cli_flags_reach_the_loaded_resources() {
        let cli = Cli::try_parse_from([
            "pi",
            "--skill",
            "/tmp/skills",
            "--no-skills",
            "--no-context-files",
            "--append-system-prompt",
            "Extra.",
        ])
        .expect("parses");

        assert!(cli.no_skills);
        assert!(cli.no_context_files);
        assert_eq!(cli.skill, vec![PathBuf::from("/tmp/skills")]);
        assert_eq!(cli.append_system_prompt, vec!["Extra.".to_string()]);
    }

    #[test]
    fn cli_system_prompt_is_wired_to_the_loader() {
        // The wrapper resolves the real working directory and agent dir, so
        // this only asserts it produces a prompt and surfaces diagnostics
        // (empty here) instead of panicking on missing locations.
        let cli = Cli::try_parse_from(["pi", "--no-skills", "--no-context-files"]).expect("parses");
        let (prompt, diagnostics) = build_cli_system_prompt(&cli);
        assert!(!prompt.is_empty());
        assert!(diagnostics.is_empty());
    }
}
