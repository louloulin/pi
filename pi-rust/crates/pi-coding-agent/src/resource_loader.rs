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

use pi_extensions::{DiscoveredResources, RegisteredToolPrompt};

use crate::cli::Cli;
use crate::context_files::{
    discover_append_system_prompt_file, discover_system_prompt_file, load_project_context_files,
    read_prompt_file, ContextFile,
};
use crate::paths::agent_dir_or_default;
use crate::prompt_templates::{
    load_prompt_templates, LoadPromptTemplatesOptions, PromptTemplate, PromptTemplateDiagnostic,
    PromptTemplatesLoadResult,
};
use crate::skills::{load_skills, LoadSkillsOptions, Skill, SkillDiagnostic};
use crate::system_prompt::{
    build_system_prompt, builtin_prompt_contributions, pi_docs_paths, SystemPromptOptions,
};
use crate::trust::{
    has_trust_requiring_project_resources, resolve_project_trusted, DefaultProjectTrust,
    ProjectTrustStore,
};

/// Where resources are loaded from.
#[derive(Debug, Clone)]
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
    /// Extra prompt template files / directories (`--prompt-template`).
    pub prompt_template_paths: Vec<PathBuf>,
    /// `--no-prompt-templates`: skip `~/.pi/agent/prompts` and
    /// `.pi/prompts` discovery (explicit paths still load).
    pub no_prompt_templates: bool,
    /// Whether `<cwd>/.pi` resources (`.pi/SYSTEM.md`, `.pi/skills`,
    /// `.pi/prompts`) may be read. See [`crate::trust`] for how the CLI
    /// resolves this; it is never inferred from an untrusted project.
    pub project_trusted: bool,
}

impl Default for ResourceLoadOptions {
    /// Library-friendly defaults; the CLI always fills `project_trusted`
    /// from the trust store and the `--approve` / `--no-approve` flags.
    fn default() -> Self {
        Self {
            cwd: PathBuf::new(),
            agent_dir: PathBuf::new(),
            skill_paths: Vec::new(),
            no_skills: false,
            no_context_files: false,
            append_system_prompt: Vec::new(),
            prompt_template_paths: Vec::new(),
            no_prompt_templates: false,
            project_trusted: true,
        }
    }
}

/// Everything the system prompt is built from.
#[derive(Debug, Clone, Default)]
pub struct LoadedResources {
    /// `~/.pi/agent/SYSTEM.md` (or a trusted `.pi/SYSTEM.md`), which
    /// replaces the built-in prompt.
    pub custom_prompt: Option<String>,
    /// Concatenated append segments (loader file first, then CLI).
    pub append_system_prompt: Option<String>,
    /// Loaded project context files.
    pub context_files: Vec<ContextFile>,
    /// Loaded skills.
    pub skills: Vec<Skill>,
    /// Loaded prompt templates (`/<name>` invocations).
    pub prompt_templates: Vec<PromptTemplate>,
    /// Non-fatal skill problems worth reporting on stderr.
    pub diagnostics: Vec<SkillDiagnostic>,
    /// Non-fatal prompt template problems worth reporting on stderr.
    pub prompt_diagnostics: Vec<PromptTemplateDiagnostic>,
    /// Whether project `.pi` resources were allowed for this load.
    pub project_trusted: bool,
    /// Whether this project has resources that required a trust decision.
    pub trust_requiring_resources: bool,
}

impl LoadedResources {
    /// Assemble the system prompt for `cwd`.
    pub fn build_system_prompt(&self, cwd: &Path) -> String {
        self.build_system_prompt_with_extension_tools(cwd, &[])
    }

    /// Assemble the system prompt for `cwd`, folding in the
    /// `promptSnippet` / `promptGuidelines` contributions declared by
    /// extension-registered tools.
    ///
    /// Extension tools appear in the `Available tools` list only when
    /// they declared a non-empty `promptSnippet`; their guidelines are
    /// appended to the tool-derived ones.
    pub fn build_system_prompt_with_extension_tools(
        &self,
        cwd: &Path,
        extension_tools: &[RegisteredToolPrompt],
    ) -> String {
        // The prompt describes exactly the tools the agent can call: the
        // built-in bundle in registration order, plus every extension
        // tool that declared a prompt contribution.
        let bundle = crate::tools::default_tool_bundle();
        let mut selected_tools: Vec<String> = bundle
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();
        // Walk the live tool objects so each tool's
        // `prompt_snippet` / `prompt_guidelines` trait methods take
        // effect (plan §6 P1-1). Tools that do not override the trait
        // methods fall back to the static `BUILTIN_TOOL_CONTRIBUTIONS`
        // table via [`system_prompt::tool_prompt_contributions_from`].
        let (mut tool_snippets, mut prompt_guidelines) =
            crate::system_prompt::tool_prompt_contributions_from(&bundle);

        for tool in extension_tools {
            if let Some(snippet) = tool
                .snippet
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                tool_snippets.insert(tool.name.clone(), snippet.to_string());
            }
            prompt_guidelines.extend(
                tool.guidelines
                    .iter()
                    .map(|line| line.trim())
                    .filter(|line| !line.is_empty())
                    .map(str::to_string),
            );
            if !selected_tools.contains(&tool.name) {
                selected_tools.push(tool.name.clone());
            }
        }

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

    /// Append the skills / prompt templates an extension advertised
    /// through the `resources_discover` event.
    ///
    /// Discovery runs after the extension host has loaded its plugins
    /// (upstream `AgentSession.extendResourcesFromExtensions`), so these
    /// paths arrive too late for [`load_resources`]'s own pass and are
    /// folded in here instead. Loading is additive: a skill or template
    /// already in the bundle wins over one with the same name, so an
    /// extension cannot silently displace the project's own resources.
    /// Paths are resolved the same way the CLI flags are — relative
    /// entries against `cwd`.
    /// `project_trusted` only reaches the two default-directory passes,
    /// which this call disables: extension-provided paths are explicit
    /// (`skill_paths` / `prompt_paths`), not project discovery, so they
    /// are not gated on trust.
    pub fn extend_extension_resources(
        &mut self,
        cwd: &Path,
        agent_dir: &Path,
        project_trusted: bool,
        resources: &DiscoveredResources,
    ) {
        if !resources.skill_paths.is_empty() {
            let loaded = load_skills(&LoadSkillsOptions {
                cwd: cwd.to_path_buf(),
                agent_dir: agent_dir.to_path_buf(),
                skill_paths: resources.skill_paths.clone(),
                include_defaults: false,
                project_trusted,
            });
            self.diagnostics.extend(loaded.diagnostics);
            for skill in loaded.skills {
                if !self
                    .skills
                    .iter()
                    .any(|existing| existing.name == skill.name)
                {
                    self.skills.push(skill);
                }
            }
        }

        if !resources.prompt_paths.is_empty() {
            let loaded = load_prompt_templates(&LoadPromptTemplatesOptions {
                cwd: cwd.to_path_buf(),
                agent_dir: agent_dir.to_path_buf(),
                prompt_paths: resources.prompt_paths.clone(),
                include_defaults: false,
                project_trusted,
            });
            self.prompt_diagnostics.extend(loaded.diagnostics);
            for template in loaded.templates {
                if !self
                    .prompt_templates
                    .iter()
                    .any(|existing| existing.name == template.name)
                {
                    self.prompt_templates.push(template);
                }
            }
        }
        // `themePaths` is deliberately unconsumed: the Rust TUI has no
        // theme system yet, so there is nothing to resolve them into.
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
            project_trusted: options.project_trusted,
        });
        diagnostics.extend(loaded.diagnostics);
        loaded.skills
    };

    let custom_prompt = discover_system_prompt_file(&cwd, &agent_dir, options.project_trusted)
        .and_then(|path| read_prompt_file(&path))
        .filter(|content| !content.trim().is_empty());

    let mut append_segments: Vec<String> = Vec::new();
    if let Some(content) =
        discover_append_system_prompt_file(&cwd, &agent_dir, options.project_trusted)
            .and_then(|path| read_prompt_file(&path))
    {
        if !content.is_empty() {
            append_segments.push(content);
        }
    }
    append_segments.extend(options.append_system_prompt.iter().cloned());
    append_segments.retain(|segment| !segment.is_empty());

    let prompt_templates = load_prompt_templates(&LoadPromptTemplatesOptions {
        cwd: cwd.clone(),
        agent_dir: agent_dir.clone(),
        prompt_paths: options.prompt_template_paths.clone(),
        include_defaults: !options.no_prompt_templates,
        project_trusted: options.project_trusted,
    });

    LoadedResources {
        custom_prompt,
        append_system_prompt: (!append_segments.is_empty()).then(|| append_segments.join("\n\n")),
        context_files,
        skills,
        prompt_templates: prompt_templates.templates,
        diagnostics,
        prompt_diagnostics: prompt_templates.diagnostics,
        project_trusted: options.project_trusted,
        trust_requiring_resources: has_trust_requiring_project_resources(&cwd),
    }
}

/// Resolve the effective project trust for one CLI invocation.
///
/// Returns `(project_trusted, has_trust_requiring_resources)` so callers
/// can explain to the user why project resources were skipped. Resolution
/// order lives in [`resolve_project_trusted`]: `--approve` /
/// `--no-approve`, then the saved `.pi` decision, then "untrusted"
/// because this entry point has no interactive prompt.
pub fn resolve_cli_project_trust(cli: &Cli) -> (bool, bool) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    resolve_cli_project_trust_for(cli, &cwd, &agent_dir_or_default())
}

/// [`resolve_cli_project_trust`] against explicit directories.
pub fn resolve_cli_project_trust_for(cli: &Cli, cwd: &Path, agent_dir: &Path) -> (bool, bool) {
    let has_trust_requiring = has_trust_requiring_project_resources(cwd);
    let store = ProjectTrustStore::new(agent_dir);
    let trusted =
        resolve_project_trusted(cwd, &store, cli.trust_override(), DefaultProjectTrust::Ask);
    (trusted, has_trust_requiring)
}

/// Build the system prompt from parsed CLI flags.
///
/// Returns the prompt plus the diagnostics the caller should report; the
/// loader itself never writes to stderr.
pub fn build_cli_system_prompt(cli: &Cli) -> (String, Vec<SkillDiagnostic>) {
    build_cli_system_prompt_with_extension_tools(cli, &[])
}

/// Like [`build_cli_system_prompt`], but also folds in the prompt
/// contributions declared by extension-registered tools.
pub fn build_cli_system_prompt_with_extension_tools(
    cli: &Cli,
    extension_tools: &[RegisteredToolPrompt],
) -> (String, Vec<SkillDiagnostic>) {
    build_cli_system_prompt_with_extensions(cli, extension_tools, &DiscoveredResources::default())
}

/// Like [`build_cli_system_prompt_with_extension_tools`], but also folds
/// in the skills an extension advertised from a `resources_discover`
/// handler.
pub fn build_cli_system_prompt_with_extensions(
    cli: &Cli,
    extension_tools: &[RegisteredToolPrompt],
    extension_resources: &DiscoveredResources,
) -> (String, Vec<SkillDiagnostic>) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let agent_dir = agent_dir_or_default();
    let (project_trusted, _) = resolve_cli_project_trust(cli);
    let mut loaded = load_resources(&ResourceLoadOptions {
        cwd: cwd.clone(),
        agent_dir: agent_dir.clone(),
        skill_paths: cli.skill.clone(),
        no_skills: cli.no_skills,
        no_context_files: cli.no_context_files,
        append_system_prompt: cli.append_system_prompt.clone(),
        prompt_template_paths: cli.prompt_template.clone(),
        no_prompt_templates: cli.no_prompt_templates,
        project_trusted,
    });
    // `--no-skills` disables skill discovery outright, extension paths
    // included: the flag is the user's "do not put skills in the prompt"
    // switch, and a plugin must not be able to override it.
    if !extension_resources.skill_paths.is_empty() && !cli.no_skills {
        loaded.extend_extension_resources(&cwd, &agent_dir, project_trusted, extension_resources);
    }

    let prompt = loaded.build_system_prompt_with_extension_tools(&cwd, extension_tools);
    (prompt, loaded.diagnostics)
}

/// Load the prompt templates selected by CLI flags.
///
/// Kept separate from [`build_cli_system_prompt`] because templates are
/// expanded at prompt-submission time, not baked into the system prompt.
pub fn build_cli_prompt_templates(cli: &Cli) -> PromptTemplatesLoadResult {
    build_cli_prompt_templates_with_extensions(cli, &DiscoveredResources::default())
}

/// Like [`build_cli_prompt_templates`], but also exposes the prompt
/// templates an extension advertised from a `resources_discover`
/// handler, so `/name` resolves against the union of both sources.
pub fn build_cli_prompt_templates_with_extensions(
    cli: &Cli,
    extension_resources: &DiscoveredResources,
) -> PromptTemplatesLoadResult {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let agent_dir = agent_dir_or_default();
    let (project_trusted, _) = resolve_cli_project_trust(cli);
    let mut result = load_prompt_templates(&LoadPromptTemplatesOptions {
        cwd: cwd.clone(),
        agent_dir: agent_dir.clone(),
        prompt_paths: cli.prompt_template.clone(),
        include_defaults: !cli.no_prompt_templates,
        project_trusted,
    });
    // `--no-prompt-templates` keeps the CLI paths but drops the default
    // directories; the same flag silences extension-provided templates
    // for the same reason (`--no-skills` silences extension skills).
    if !extension_resources.prompt_paths.is_empty() && !cli.no_prompt_templates {
        let extra = load_prompt_templates(&LoadPromptTemplatesOptions {
            cwd,
            agent_dir,
            prompt_paths: extension_resources.prompt_paths.clone(),
            include_defaults: false,
            project_trusted,
        });
        result.diagnostics.extend(extra.diagnostics);
        for template in extra.templates {
            if !result.templates.iter().any(|t| t.name == template.name) {
                result.templates.push(template);
            }
        }
    }
    result
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

        assert_eq!(
            loaded.custom_prompt.as_deref(),
            Some("You are a custom agent.")
        );
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
    fn extension_tool_prompts_are_folded_into_the_system_prompt() {
        use pi_extensions::RegisteredToolPrompt;

        let temp = TempDir::new("ext-prompts");
        let loaded = load(&temp, "project");
        let cwd = absolute(&temp.path.join("project"));

        let tools = vec![
            RegisteredToolPrompt {
                name: "custom_search".to_string(),
                snippet: Some("Search the web".to_string()),
                guidelines: vec!["Cite sources".to_string(), "   ".to_string()],
            },
            RegisteredToolPrompt {
                name: "silent_tool".to_string(),
                snippet: None,
                guidelines: Vec::new(),
            },
        ];

        let prompt = loaded.build_system_prompt_with_extension_tools(&cwd, &tools);
        assert!(
            prompt.contains("- custom_search: Search the web"),
            "{prompt}"
        );
        assert!(prompt.contains("- Cite sources"), "{prompt}");
        // A tool with no snippet stays callable but out of the prompt.
        assert!(!prompt.contains("silent_tool"), "{prompt}");
    }

    #[test]
    fn project_prompt_templates_load_and_no_prompt_templates_disables_defaults() {
        let temp = TempDir::new("prompt-templates");
        temp.write(
            "project/.pi/prompts/greet.md",
            "---\ndescription: Greet someone.\n---\nHi $1",
        );

        let loaded = load_resources(&ResourceLoadOptions {
            cwd: temp.path.join("project"),
            agent_dir: temp.path.join("agent"),
            ..Default::default()
        });
        assert_eq!(
            loaded.prompt_templates.len(),
            1,
            "{:?}",
            loaded.prompt_diagnostics
        );
        assert_eq!(loaded.prompt_templates[0].name, "greet");
        assert_eq!(loaded.prompt_templates[0].description, "Greet someone.");

        let disabled = load_resources(&ResourceLoadOptions {
            cwd: temp.path.join("project"),
            agent_dir: temp.path.join("agent"),
            no_prompt_templates: true,
            ..Default::default()
        });
        assert!(disabled.prompt_templates.is_empty());
    }

    #[test]
    fn cli_prompt_template_flags_parse() {
        let cli = Cli::try_parse_from([
            "pi",
            "--prompt-template",
            "/tmp/prompts",
            "--no-prompt-templates",
        ])
        .expect("parses");

        assert_eq!(cli.prompt_template, vec![PathBuf::from("/tmp/prompts")]);
        assert!(cli.no_prompt_templates);
    }

    #[test]
    fn cli_project_trust_uses_flags_and_the_saved_store() {
        let temp = TempDir::new("cli-trust");
        temp.write("project/.pi/settings.json", "{}");
        let cwd = temp.path.join("project");
        let agent_dir = temp.path.join("agent");

        // Trust-requiring resources + no saved decision + no UI: untrusted.
        let plain = Cli::try_parse_from(["pi", "--print", "hi"]).expect("parses");
        assert_eq!(
            resolve_cli_project_trust_for(&plain, &cwd, &agent_dir),
            (false, true)
        );

        // `--approve` overrides for one run, `--no-approve` forces off.
        let approved = Cli::try_parse_from(["pi", "--approve", "--print", "hi"]).expect("parses");
        assert_eq!(
            resolve_cli_project_trust_for(&approved, &cwd, &agent_dir),
            (true, true)
        );
        let denied = Cli::try_parse_from(["pi", "--no-approve", "--print", "hi"]).expect("parses");
        assert_eq!(
            resolve_cli_project_trust_for(&denied, &cwd, &agent_dir),
            (false, true)
        );

        // A saved decision applies when neither flag is given.
        ProjectTrustStore::new(&agent_dir)
            .set(&cwd, Some(true))
            .expect("save trust");
        assert_eq!(
            resolve_cli_project_trust_for(&plain, &cwd, &agent_dir),
            (true, true)
        );
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

    /// The whole point of Stage 23: a skill and a prompt template that
    /// only an extension knows about land in the same bundle the CLI
    /// flags feed, so the system prompt and `/name` lookup see them.
    #[test]
    fn extension_discovered_resources_extend_the_bundle() {
        let temp = TempDir::new("ext-resources");
        temp.write(
            "project/.pi/skills/local/SKILL.md",
            "---\nname: local\ndescription: Local skill.\n---\n",
        );
        temp.write(
            "dynamic/skills/from-ext/SKILL.md",
            "---\nname: from-ext\ndescription: From an extension.\n---\n",
        );
        temp.write("dynamic/prompts/dyn.md", "EXT TEMPLATE\n");

        let mut loaded = load(&temp, "project");
        let project = temp.path.join("project");
        let extension_resources = DiscoveredResources {
            skill_paths: vec![temp.path.join("dynamic/skills/from-ext/SKILL.md")],
            prompt_paths: vec![temp.path.join("dynamic/prompts/dyn.md")],
            theme_paths: vec![temp.path.join("dynamic/theme.json")],
        };
        loaded.extend_extension_resources(
            &project,
            &temp.path.join("agent"),
            true,
            &extension_resources,
        );

        let prompt = loaded.build_system_prompt(&absolute(&project));
        assert!(prompt.contains("<name>local</name>"), "{prompt}");
        assert!(prompt.contains("<name>from-ext</name>"), "{prompt}");
        assert!(prompt.contains("From an extension."), "{prompt}");
        assert!(
            loaded.prompt_templates.iter().any(|t| t.name == "dyn"),
            "extension prompt template missing: {:?}",
            loaded.prompt_templates
        );
        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
        assert!(
            loaded.prompt_diagnostics.is_empty(),
            "{:?}",
            loaded.prompt_diagnostics
        );
    }

    /// Loading is additive: an extension cannot silently displace the
    /// project's own skill / template of the same name.
    #[test]
    fn extension_resources_never_shadow_existing_names() {
        let temp = TempDir::new("ext-shadow");
        temp.write(
            "project/.pi/skills/dup/SKILL.md",
            "---\nname: dup\ndescription: Project wins.\n---\n",
        );
        temp.write(
            "dynamic/SKILL.md",
            "---\nname: dup\ndescription: Extension loses.\n---\n",
        );
        temp.write("project/.pi/prompts/note.md", "PROJECT TEMPLATE\n");
        temp.write("dynamic/note.md", "EXTENSION TEMPLATE\n");

        let mut loaded = load(&temp, "project");
        let project = temp.path.join("project");
        loaded.extend_extension_resources(
            &project,
            &temp.path.join("agent"),
            true,
            &DiscoveredResources {
                skill_paths: vec![temp.path.join("dynamic/SKILL.md")],
                prompt_paths: vec![temp.path.join("dynamic/note.md")],
                theme_paths: Vec::new(),
            },
        );

        let prompt = loaded.build_system_prompt(&absolute(&project));
        assert!(!prompt.contains("Extension loses."), "{prompt}");
        assert!(prompt.contains("Project wins."), "{prompt}");
        let note = loaded
            .prompt_templates
            .iter()
            .find(|t| t.name == "note")
            .expect("project template present");
        assert_eq!(note.content.trim(), "PROJECT TEMPLATE");
    }

    /// A path the extension advertised that does not exist is a
    /// diagnostic, not a panic: discovery is best-effort.
    #[test]
    fn missing_extension_resource_path_is_a_diagnostic() {
        let temp = TempDir::new("ext-missing");
        let mut loaded = load(&temp, "project");
        loaded.extend_extension_resources(
            &temp.path.join("project"),
            &temp.path.join("agent"),
            true,
            &DiscoveredResources {
                skill_paths: vec![temp.path.join("nope/SKILL.md")],
                prompt_paths: vec![temp.path.join("nope/note.md")],
                theme_paths: Vec::new(),
            },
        );
        assert_eq!(loaded.diagnostics.len(), 1, "{:?}", loaded.diagnostics);
        assert_eq!(
            loaded.prompt_diagnostics.len(),
            1,
            "{:?}",
            loaded.prompt_diagnostics
        );
        assert!(loaded.skills.is_empty());
    }
}
