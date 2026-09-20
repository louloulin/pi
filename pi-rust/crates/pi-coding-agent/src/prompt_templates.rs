//! Prompt template discovery and expansion.
//!
//! Rust port of `packages/coding-agent/src/core/prompt-templates.ts`.
//! A prompt template is a markdown file (usually under
//! `~/.pi/agent/prompts/` or `.pi/prompts/`) that can be invoked from
//! the prompt line as `/<name> [args]`. Invoking it expands to the file
//! body with the arguments substituted bash-style.
//!
//! Discovery order matches the TS loader:
//!
//! 1. global `~/.pi/agent/prompts/`,
//! 2. project `<cwd>/.pi/prompts/`,
//! 3. explicit `--prompt-template` files / directories.
//!
//! The first definition of a name wins; later ones are reported as
//! collisions. `--no-prompt-templates` skips the two default locations
//! but still honours explicit `--prompt-template` paths, mirroring the
//! upstream `noPromptTemplates` handling.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

use crate::frontmatter::parse_frontmatter;
use crate::paths::{absolute, expand_tilde};

pub use crate::paths::CONFIG_DIR_NAME;

/// Max characters kept from the first body line when a template has no
/// `description` frontmatter field.
const MAX_GENERATED_DESCRIPTION: usize = 60;

/// Where a prompt template was discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptTemplateSource {
    /// `~/.pi/agent/prompts`.
    User,
    /// `<cwd>/.pi/prompts`.
    Project,
    /// A path given explicitly on the command line (`--prompt-template`).
    Path,
}

impl PromptTemplateSource {
    /// Stable label used in diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Path => "path",
        }
    }
}

/// A prompt template loaded from a markdown file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplate {
    /// File stem — the name typed after `/`.
    pub name: String,
    /// Frontmatter `description`, or the first non-empty body line.
    pub description: String,
    /// Frontmatter `argument-hint`, shown by completions.
    pub argument_hint: Option<String>,
    /// Template body (frontmatter stripped).
    pub content: String,
    /// Absolute path to the template file.
    pub file_path: PathBuf,
    /// Where the template was found.
    pub source: PromptTemplateSource,
}

/// A non-fatal problem found while loading templates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplateDiagnostic {
    /// Human-readable message.
    pub message: String,
    /// File the diagnostic refers to.
    pub path: Option<PathBuf>,
}

/// Result of one template load pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptTemplatesLoadResult {
    /// Loaded templates, in discovery order.
    pub templates: Vec<PromptTemplate>,
    /// Non-fatal problems.
    pub diagnostics: Vec<PromptTemplateDiagnostic>,
}

/// Options for [`load_prompt_templates`].
#[derive(Debug, Clone, Default)]
pub struct LoadPromptTemplatesOptions {
    /// Working directory; `<cwd>/.pi/prompts` is searched when
    /// `include_defaults` is set.
    pub cwd: PathBuf,
    /// Agent config directory (`~/.pi/agent`).
    pub agent_dir: PathBuf,
    /// Explicit template files / directories (`--prompt-template`).
    pub prompt_paths: Vec<PathBuf>,
    /// Whether the two default locations are searched.
    pub include_defaults: bool,
    /// Whether `<cwd>/.pi/prompts` may be loaded. Project prompt
    /// templates are trust-requiring: an untrusted clone must not be able
    /// to install a `/command`. `~/.pi/agent/prompts` is never gated.
    /// Defaults to `false` (safe).
    pub project_trusted: bool,
}

/// Split a command argument string into arguments, respecting quotes.
///
/// Mirrors `parseCommandArgs` on the TS side: whitespace separates
/// arguments, `'` and `"` group them, and quote characters are dropped.
pub fn parse_command_args(args_string: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;

    for ch in args_string.chars() {
        if let Some(open) = quote {
            if ch == open {
                quote = None;
            } else {
                current.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                args.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }

    if !current.is_empty() {
        args.push(current);
    }

    args
}

fn substitution_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\$\{(\d+|ARGUMENTS|@):-([^}]*)\}|\$\{@:(\d+)(?::(\d+))?\}|\$(ARGUMENTS|@|\d+)")
            .expect("valid substitution regex")
    })
}

/// Substitute argument placeholders in template content.
///
/// Supports `$1`, `$@`, `$ARGUMENTS`, `${N:-default}`,
/// `${@:-default}`, `${@:N}` and `${@:N:L}`. Substitution is applied to
/// the template body only; argument/default values are never recursively
/// substituted.
pub fn substitute_args(content: &str, args: &[String]) -> String {
    let all_args = args.join(" ");

    substitution_regex()
        .replace_all(content, |caps: &regex::Captures<'_>| {
            if let Some(target) = caps.get(1) {
                // `${target:-default}`.
                let target = target.as_str();
                let default = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                let value = if target == "@" || target == "ARGUMENTS" {
                    Some(all_args.clone())
                } else {
                    target
                        .parse::<usize>()
                        .ok()
                        .and_then(|index| index.checked_sub(1))
                        .and_then(|index| args.get(index).cloned())
                };
                return match value {
                    Some(value) if !value.is_empty() => value,
                    _ => default.to_string(),
                };
            }

            if let Some(slice_start) = caps.get(3) {
                // `${@:N}` / `${@:N:L}`.
                let mut start = slice_start
                    .as_str()
                    .parse::<usize>()
                    .unwrap_or(0)
                    .saturating_sub(1);
                if start > args.len() {
                    start = args.len();
                }
                if let Some(length) = caps.get(4) {
                    let length = length.as_str().parse::<usize>().unwrap_or(0);
                    let end = start.saturating_add(length).min(args.len());
                    return args[start..end].join(" ");
                }
                return args[start..].join(" ");
            }

            let simple = caps.get(5).map(|m| m.as_str()).unwrap_or("");
            if simple == "ARGUMENTS" || simple == "@" {
                return all_args.clone();
            }
            simple
                .parse::<usize>()
                .ok()
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| args.get(index).cloned())
                .unwrap_or_default()
        })
        .into_owned()
}

fn load_template_from_file(
    file_path: &Path,
    source: PromptTemplateSource,
) -> Option<PromptTemplate> {
    let raw = fs::read_to_string(file_path).ok()?;
    let parsed = parse_frontmatter(&raw).ok()?;

    let name = file_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_string();

    let frontmatter_description = parsed
        .frontmatter
        .get_str("description")
        .unwrap_or_default()
        .to_string();

    let description = if !frontmatter_description.is_empty() {
        frontmatter_description
    } else {
        let first_line = parsed
            .body
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default();
        let mut description: String = first_line.chars().take(MAX_GENERATED_DESCRIPTION).collect();
        if first_line.chars().count() > MAX_GENERATED_DESCRIPTION {
            description.push_str("...");
        }
        description
    };

    let argument_hint = parsed
        .frontmatter
        .get_str("argument-hint")
        .map(str::to_string)
        .filter(|hint| !hint.is_empty());

    Some(PromptTemplate {
        name,
        description,
        argument_hint,
        content: parsed.body,
        file_path: file_path.to_path_buf(),
        source,
    })
}

/// Scan a directory for top-level `.md` files and load them.
fn load_templates_from_dir(dir: &Path, source: PromptTemplateSource) -> Vec<PromptTemplate> {
    let mut templates = Vec::new();

    let Ok(entries) = fs::read_dir(dir) else {
        return templates;
    };

    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    paths.sort();

    for path in paths {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with(".md") {
            continue;
        }

        // Follow symlinks so a link to a template file loads; a broken
        // link is skipped without error.
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }

        if let Some(template) = load_template_from_file(&path, source) {
            templates.push(template);
        }
    }

    templates
}

/// Load prompt templates from every configured location.
///
/// Global templates come first, then project, then explicit paths; the
/// first definition of a name wins and later ones are reported.
pub fn load_prompt_templates(options: &LoadPromptTemplatesOptions) -> PromptTemplatesLoadResult {
    let cwd = absolute(&options.cwd);
    let agent_dir = absolute(&options.agent_dir);

    let mut result = PromptTemplatesLoadResult::default();
    let mut seen_names: Vec<String> = Vec::new();
    let mut add = |templates: Vec<PromptTemplate>, result: &mut PromptTemplatesLoadResult| {
        for template in templates {
            if seen_names.contains(&template.name) {
                result.diagnostics.push(PromptTemplateDiagnostic {
                    message: format!("prompt template \"{}\" collision", template.name),
                    path: Some(template.file_path.clone()),
                });
                continue;
            }
            seen_names.push(template.name.clone());
            result.templates.push(template);
        }
    };

    if options.include_defaults {
        let global_dir = agent_dir.join("prompts");
        add(
            load_templates_from_dir(&global_dir, PromptTemplateSource::User),
            &mut result,
        );
        if options.project_trusted {
            let project_dir = cwd.join(CONFIG_DIR_NAME).join("prompts");
            add(
                load_templates_from_dir(&project_dir, PromptTemplateSource::Project),
                &mut result,
            );
        }
    }

    for raw_path in &options.prompt_paths {
        let resolved = absolute(&expand_tilde(raw_path, &cwd));
        if !resolved.exists() {
            result.diagnostics.push(PromptTemplateDiagnostic {
                message: "prompt template path does not exist".to_string(),
                path: Some(resolved),
            });
            continue;
        }
        let Ok(metadata) = fs::metadata(&resolved) else {
            result.diagnostics.push(PromptTemplateDiagnostic {
                message: "failed to read prompt template path".to_string(),
                path: Some(resolved),
            });
            continue;
        };

        if metadata.is_dir() {
            add(
                load_templates_from_dir(&resolved, PromptTemplateSource::Path),
                &mut result,
            );
        } else if metadata.is_file()
            && resolved.extension().and_then(|ext| ext.to_str()) == Some("md")
        {
            if let Some(template) = load_template_from_file(&resolved, PromptTemplateSource::Path) {
                add(vec![template], &mut result);
            }
        } else {
            result.diagnostics.push(PromptTemplateDiagnostic {
                message: "prompt template path is not a markdown file".to_string(),
                path: Some(resolved),
            });
        }
    }

    result
}

/// Find the template matching a `/<name> ...` invocation.
///
/// Returns the template plus the raw argument string; `None` when the
/// text is not a matching template invocation.
pub fn find_prompt_template<'a>(
    text: &str,
    templates: &'a [PromptTemplate],
) -> Option<(&'a PromptTemplate, String)> {
    if !text.starts_with('/') {
        return None;
    }
    let rest = &text[1..];
    if rest.is_empty() {
        return None;
    }
    let (name, args) = match rest.find(char::is_whitespace) {
        Some(index) => (&rest[..index], rest[index..].trim_start().to_string()),
        None => (rest, String::new()),
    };
    if name.is_empty() {
        return None;
    }
    templates
        .iter()
        .find(|template| template.name == name)
        .map(|template| (template, args))
}

/// Expand a prompt template if it matches a template name.
///
/// Returns the expanded content, or the original text unchanged when the
/// text is not a slash invocation or the name matches no template.
pub fn expand_prompt_template(text: &str, templates: &[PromptTemplate]) -> String {
    match find_prompt_template(text, templates) {
        Some((template, args)) => {
            let parsed = parse_command_args(&args);
            substitute_args(&template.content, &parsed)
        }
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "pi-prompts-{label}-{}-{}",
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

    #[test]
    fn parses_quoted_command_arguments() {
        assert_eq!(
            parse_command_args("a b  c"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert_eq!(
            parse_command_args("\"a b\" 'c d' e"),
            vec!["a b".to_string(), "c d".to_string(), "e".to_string()]
        );
        assert!(parse_command_args("   ").is_empty());
    }

    #[test]
    fn substitutes_positional_and_all_args() {
        let args = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        assert_eq!(substitute_args("$1-$2", &args), "one-two");
        assert_eq!(substitute_args("$@", &args), "one two three");
        assert_eq!(substitute_args("$ARGUMENTS", &args), "one two three");
        assert_eq!(substitute_args("$4", &args), "");
    }

    #[test]
    fn substitutes_defaults() {
        let args = vec!["one".to_string()];
        assert_eq!(substitute_args("${1:-fallback}", &args), "one");
        assert_eq!(substitute_args("${2:-fallback}", &args), "fallback");
        assert_eq!(substitute_args("${@:-none}", &args), "one");
        assert_eq!(substitute_args("${@:-none}", &[]), "none");
        assert_eq!(substitute_args("${ARGUMENTS:-none}", &[]), "none");
    }

    #[test]
    fn substitutes_slices() {
        let args = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        assert_eq!(substitute_args("${@:2}", &args), "b c d");
        assert_eq!(substitute_args("${@:2:2}", &args), "b c");
        assert_eq!(substitute_args("${@:1:1}", &args), "a");
        assert_eq!(substitute_args("${@:9}", &args), "");
    }

    #[test]
    fn does_not_recursively_substitute_values() {
        let args = vec!["$1".to_string()];
        assert_eq!(substitute_args("$1", &args), "$1");
    }

    #[test]
    fn loads_templates_from_a_directory() {
        let temp = TempDir::new("dir");
        temp.write(
            "prompts/greet.md",
            "---\ndescription: Greets someone.\nargument-hint: <name>\n---\nHello $1!\n",
        );
        temp.write("prompts/plain.md", "First line description.\n\nBody.\n");
        temp.write("prompts/ignored.txt", "not a template");

        let result = load_prompt_templates(&LoadPromptTemplatesOptions {
            cwd: temp.path.clone(),
            agent_dir: temp.path.join("agent"),
            prompt_paths: vec![temp.path.join("prompts")],
            include_defaults: false,
            project_trusted: true,
        });

        assert_eq!(result.templates.len(), 2, "{:?}", result.diagnostics);
        let greet = result
            .templates
            .iter()
            .find(|template| template.name == "greet")
            .expect("greet");
        assert_eq!(greet.description, "Greets someone.");
        assert_eq!(greet.argument_hint.as_deref(), Some("<name>"));
        assert_eq!(greet.content, "Hello $1!");
        assert_eq!(greet.source, PromptTemplateSource::Path);

        let plain = result
            .templates
            .iter()
            .find(|template| template.name == "plain")
            .expect("plain");
        assert_eq!(plain.description, "First line description.");
    }

    #[test]
    fn truncates_generated_descriptions() {
        let temp = TempDir::new("truncate");
        let long = "x".repeat(100);
        temp.write("prompts/long.md", &format!("{long}\n"));

        let result = load_prompt_templates(&LoadPromptTemplatesOptions {
            cwd: temp.path.clone(),
            agent_dir: temp.path.join("agent"),
            prompt_paths: vec![temp.path.join("prompts")],
            include_defaults: false,
            project_trusted: true,
        });

        let template = &result.templates[0];
        assert_eq!(
            template.description.chars().count(),
            MAX_GENERATED_DESCRIPTION + 3
        );
        assert!(template.description.ends_with("..."));
    }

    #[test]
    fn defaults_then_explicit_paths_and_first_name_wins() {
        let temp = TempDir::new("precedence");
        temp.write(
            "agent/prompts/shared.md",
            "---\ndescription: From agent.\n---\nagent\n",
        );
        temp.write(
            "project/.pi/prompts/shared.md",
            "---\ndescription: From project.\n---\nproject\n",
        );
        temp.write("extra.md", "---\ndescription: Explicit.\n---\nexplicit\n");

        let result = load_prompt_templates(&LoadPromptTemplatesOptions {
            cwd: temp.path.join("project"),
            agent_dir: temp.path.join("agent"),
            prompt_paths: vec![temp.path.join("extra.md")],
            include_defaults: true,
            project_trusted: true,
        });

        assert_eq!(result.templates.len(), 2);
        assert_eq!(result.templates[0].name, "shared");
        assert_eq!(result.templates[0].description, "From agent.");
        assert_eq!(result.templates[1].name, "extra");
        assert!(result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("collision")));
    }

    #[test]
    fn an_untrusted_project_hides_project_templates() {
        let temp = TempDir::new("untrusted-project");
        temp.write(
            "agent/prompts/global.md",
            "---\ndescription: From agent.\n---\nagent\n",
        );
        temp.write(
            "project/.pi/prompts/local.md",
            "---\ndescription: From project.\n---\nproject\n",
        );

        let result = load_prompt_templates(&LoadPromptTemplatesOptions {
            cwd: temp.path.join("project"),
            agent_dir: temp.path.join("agent"),
            prompt_paths: Vec::new(),
            include_defaults: true,
            project_trusted: false,
        });

        assert_eq!(result.templates.len(), 1);
        assert_eq!(result.templates[0].name, "global");
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn missing_explicit_path_reports_a_diagnostic() {
        let temp = TempDir::new("missing");
        let result = load_prompt_templates(&LoadPromptTemplatesOptions {
            cwd: temp.path.clone(),
            agent_dir: temp.path.join("agent"),
            prompt_paths: vec![temp.path.join("nope.md")],
            include_defaults: false,
            project_trusted: true,
        });

        assert!(result.templates.is_empty());
        assert_eq!(result.diagnostics.len(), 1);
        assert!(result.diagnostics[0].message.contains("does not exist"));
    }

    fn template(name: &str, content: &str) -> PromptTemplate {
        PromptTemplate {
            name: name.to_string(),
            description: String::new(),
            argument_hint: None,
            content: content.to_string(),
            file_path: PathBuf::from(format!("/prompts/{name}.md")),
            source: PromptTemplateSource::User,
        }
    }

    #[test]
    fn expands_a_matching_template() {
        let templates = vec![template("greet", "Hello $1 from $2!")];
        assert_eq!(
            expand_prompt_template("/greet alice bob", &templates),
            "Hello alice from bob!"
        );
    }

    #[test]
    fn leaves_non_templates_untouched() {
        let templates = vec![template("greet", "Hello $1!")];
        assert_eq!(expand_prompt_template("hello", &templates), "hello");
        assert_eq!(expand_prompt_template("/help", &templates), "/help");
        assert_eq!(expand_prompt_template("/greet", &[]), "/greet");
    }
}
