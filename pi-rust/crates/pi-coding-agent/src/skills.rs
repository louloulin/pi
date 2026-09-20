//! Agent Skills discovery and prompt formatting.
//!
//! Rust port of `packages/coding-agent/src/core/skills.ts`. pi loads
//! *skills* — directories that carry a `SKILL.md` with YAML frontmatter —
//! from the global agent directory (`~/.pi/agent/skills`), the project
//! config directory (`.pi/skills`), and any directory named with
//! `--skill`. Every visible skill is advertised to the model in the
//! system prompt as an `<available_skills>` block; the model then reads
//! the skill file with the `read` tool when a task matches.
//!
//! Discovery rules (identical to the TS side):
//!
//! - a directory containing `SKILL.md` is a skill root — it is loaded and
//!   **not** recursed into;
//! - otherwise the directory's own `*.md` children load as skills, and
//!   subdirectories are searched for `SKILL.md` (their loose `.md` files
//!   are ignored);
//! - candidates git reports as ignored are skipped.
//! - the first name wins on collision, later ones report a diagnostic.
//!
//! The TS side builds its own matcher from `.gitignore` / `.ignore` /
//! `.fdignore` files via the `ignore` npm package. The Rust port instead
//! asks `git check-ignore` once per discovered directory tree — a single
//! subprocess for the whole tree. Two consequences are deliberate: git's
//! own configuration (`$GIT_DIR/info/exclude`, `core.excludesFile`, the
//! index) participates, while `.ignore` / `.fdignore` do not; and outside
//! a git work tree nothing is ignored. Ignoring is advisory throughout —
//! when `git` is missing or the command fails, every candidate still
//! loads.
//!
//! Still missing compared to the TS implementation: the
//! `resources_discover` extension hook.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::frontmatter::parse_frontmatter;
use crate::paths::{absolute, expand_tilde};

pub use crate::paths::CONFIG_DIR_NAME;

/// Max skill name length per the Agent Skills spec.
pub const MAX_SKILL_NAME_LENGTH: usize = 64;

/// Max skill description length per the Agent Skills spec.
pub const MAX_SKILL_DESCRIPTION_LENGTH: usize = 1024;

/// Name of the marker file that turns a directory into a skill root.
pub const SKILL_FILE_NAME: &str = "SKILL.md";

/// Where a skill was discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    /// `~/.pi/agent/skills`.
    User,
    /// `<cwd>/.pi/skills`.
    Project,
    /// A path given explicitly on the command line (`--skill`).
    Path,
}

impl SkillSource {
    /// Stable label used in diagnostics and session logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Path => "path",
        }
    }
}

/// A discovered skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// Frontmatter `name`, or the skill directory's basename.
    pub name: String,
    /// Frontmatter `description` — required for a skill to load.
    pub description: String,
    /// Absolute path to `SKILL.md` (or the loose `.md` file).
    pub file_path: PathBuf,
    /// Directory the skill's relative paths resolve against.
    pub base_dir: PathBuf,
    /// Where the skill was found.
    pub source: SkillSource,
    /// `disable-model-invocation: true` keeps the skill out of the system
    /// prompt; it can still be invoked explicitly by name.
    pub disable_model_invocation: bool,
}

/// Severity / kind of a resource diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    /// The skill loaded (or was skipped) but something is off-spec.
    Warning,
    /// Two skills claimed the same name; the first one won.
    Collision,
}

/// A non-fatal problem found while loading skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDiagnostic {
    /// Severity of the diagnostic.
    pub kind: DiagnosticKind,
    /// Human-readable message.
    pub message: String,
    /// Resource the diagnostic refers to.
    pub path: Option<PathBuf>,
}

/// Result of one skill load pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillsLoadResult {
    /// Loaded skills, in discovery order (global first, then project).
    pub skills: Vec<Skill>,
    /// Non-fatal problems.
    pub diagnostics: Vec<SkillDiagnostic>,
}

impl SkillDiagnostic {
    fn warning(message: impl Into<String>, path: Option<PathBuf>) -> Self {
        Self {
            kind: DiagnosticKind::Warning,
            message: message.into(),
            path,
        }
    }

    fn collision(message: impl Into<String>, path: PathBuf) -> Self {
        Self {
            kind: DiagnosticKind::Collision,
            message: message.into(),
            path: Some(path),
        }
    }
}

/// Options for [`load_skills`].
#[derive(Debug, Clone)]
pub struct LoadSkillsOptions {
    /// Working directory; `<cwd>/.pi/skills` is searched when
    /// `include_defaults` is set.
    pub cwd: PathBuf,
    /// Agent config directory (`~/.pi/agent`); `<agent_dir>/skills` is
    /// searched when `include_defaults` is set.
    pub agent_dir: PathBuf,
    /// Extra skill files / directories (`--skill`).
    pub skill_paths: Vec<PathBuf>,
    /// Whether the two default locations are searched.
    pub include_defaults: bool,
    /// Whether `<cwd>/.pi/skills` may be loaded. Project skills are a
    /// trust-requiring resource: a cloned repository must not be able to
    /// steer the agent through a checked-in `SKILL.md` before the user
    /// trusts the directory. `~/.pi/agent/skills` is never gated.
    pub project_trusted: bool,
}

/// Which tool the prompt advertises for loading a skill file.
///
/// Mirrors `formatSkillsForPrompt(skills, fileReadTool)`: the instruction
/// line has to name a tool the model actually has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillReadTool {
    /// `read` is registered — the default.
    Read,
    /// `read` is unavailable but `bash` is.
    Bash,
}

impl SkillReadTool {
    /// Instruction line telling the model how to open a skill file.
    fn instruction(self) -> &'static str {
        match self {
            Self::Read => {
                "Use the read tool to load a skill's file when the task matches its description."
            }
            Self::Bash => "Use bash to load a skill's file when the task matches its description.",
        }
    }
}

/// Validate a skill name against the Agent Skills spec.
///
/// Returns one message per violated rule; an empty vector means valid.
pub fn validate_skill_name(name: &str) -> Vec<String> {
    let mut errors = Vec::new();

    if name.chars().count() > MAX_SKILL_NAME_LENGTH {
        errors.push(format!(
            "name exceeds {MAX_SKILL_NAME_LENGTH} characters ({})",
            name.chars().count()
        ));
    }

    if !name
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        errors.push(
            "name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)"
                .to_string(),
        );
    }

    if name.starts_with('-') || name.ends_with('-') {
        errors.push("name must not start or end with a hyphen".to_string());
    }

    if name.contains("--") {
        errors.push("name must not contain consecutive hyphens".to_string());
    }

    errors
}

/// Validate a skill description against the Agent Skills spec.
pub fn validate_skill_description(description: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let length = description.chars().count();

    if description.trim().is_empty() {
        errors.push("description is required".to_string());
    } else if length > MAX_SKILL_DESCRIPTION_LENGTH {
        errors.push(format!(
            "description exceeds {MAX_SKILL_DESCRIPTION_LENGTH} characters ({length})"
        ));
    }

    errors
}

/// Load every skill from `dir` (see the module docs for the walk rules).
pub fn load_skills_from_dir(dir: &Path, source: SkillSource) -> SkillsLoadResult {
    // Git reads `.gitignore` files itself while descending, so one
    // `check-ignore --stdin` call covers the whole tree. Candidates are
    // enumerated with the same structural pruning as the walk below (dot
    // entries, `node_modules`, and “a `SKILL.md` makes its directory the
    // skill root”), so the batch never misses a path the walk would test.
    let mut candidates = Vec::new();
    collect_skill_candidates(dir, &mut candidates);
    let ignored = git_ignored_paths(dir, &candidates);

    load_skills_from_dir_internal(dir, source, true, &ignored)
}

/// Every path [`load_skills_from_dir_internal`] may inspect under `dir`,
/// in the order it walks them.
fn collect_skill_candidates(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    let mut entry_paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    entry_paths.sort();

    if dir.join(SKILL_FILE_NAME).is_file() {
        out.push(dir.join(SKILL_FILE_NAME));
        return;
    }

    for path in entry_paths {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };

        out.push(path.clone());
        if metadata.is_dir() {
            collect_skill_candidates(&path, out);
        }
    }
}

/// Paths git reports as ignored, resolved by a single
/// `git -C <root> check-ignore --stdin -z` invocation.
///
/// Every failure mode — `git` absent, `root` outside a work tree, a write
/// or wait error — yields an empty set, so ignore handling can only ever
/// narrow, never break, discovery. `git check-ignore` exits 1 when no
/// path matched and 0 otherwise, which is why the exit status is not
/// treated as an error.
fn git_ignored_paths(root: &Path, candidates: &[PathBuf]) -> HashSet<PathBuf> {
    let relative: Vec<(PathBuf, String)> = candidates
        .iter()
        .filter_map(|path| {
            let rel = path.strip_prefix(root).ok()?;
            let rel = rel.to_string_lossy().replace('\\', "/");
            if rel.is_empty() {
                return None;
            }
            Some((path.clone(), rel))
        })
        .collect();
    if relative.is_empty() {
        return HashSet::new();
    }

    let mut child = match Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "--stdin", "-z"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return HashSet::new(),
    };

    let payload: Vec<u8> = relative
        .iter()
        .flat_map(|(_, rel)| rel.as_bytes().iter().copied().chain(std::iter::once(0)))
        .collect();
    let wrote = child
        .stdin
        .take()
        .map(|mut stdin| stdin.write_all(&payload).is_ok())
        .unwrap_or(false);
    if !wrote {
        let _ = child.wait();
        return HashSet::new();
    }

    let Ok(output) = child.wait_with_output() else {
        return HashSet::new();
    };
    if !output.status.success() {
        return HashSet::new();
    }

    let reported: HashSet<String> = String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect();
    if reported.is_empty() {
        return HashSet::new();
    }

    relative
        .into_iter()
        .filter(|(_, rel)| reported.contains(rel))
        .map(|(path, _)| path)
        .collect()
}

fn load_skills_from_dir_internal(
    dir: &Path,
    source: SkillSource,
    include_root_files: bool,
    ignored: &HashSet<PathBuf>,
) -> SkillsLoadResult {
    let mut result = SkillsLoadResult::default();

    let Ok(entries) = fs::read_dir(dir) else {
        return result;
    };

    let mut entry_paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    entry_paths.sort();

    // A `SKILL.md` directly in `dir` makes `dir` itself the skill root.
    let marker = dir.join(SKILL_FILE_NAME);
    if marker.is_file() {
        if !ignored.contains(&marker) {
            let loaded = load_skill_from_file(&marker, source);
            if let Some(skill) = loaded.skill {
                result.skills.push(skill);
            }
            result.diagnostics.extend(loaded.diagnostics);
        }
        return result;
    }

    for path in entry_paths {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }

        // `file_type` follows symlinks, so a link to a skill directory is
        // walked exactly like the target.
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };

        if ignored.contains(&path) {
            continue;
        }

        if metadata.is_dir() {
            let nested = load_skills_from_dir_internal(&path, source, false, ignored);
            result.skills.extend(nested.skills);
            result.diagnostics.extend(nested.diagnostics);
            continue;
        }

        if !metadata.is_file() || !include_root_files || !name.ends_with(".md") {
            continue;
        }

        let loaded = load_skill_from_file(&path, source);
        if let Some(skill) = loaded.skill {
            result.skills.push(skill);
        }
        result.diagnostics.extend(loaded.diagnostics);
    }

    result
}

struct LoadedSkill {
    skill: Option<Skill>,
    diagnostics: Vec<SkillDiagnostic>,
}

/// Parse one candidate skill file.
fn load_skill_from_file(file_path: &Path, source: SkillSource) -> LoadedSkill {
    let mut diagnostics = Vec::new();
    let is_declared_skill = file_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == SKILL_FILE_NAME);

    let raw = match fs::read_to_string(file_path) {
        Ok(raw) => raw,
        Err(error) => {
            diagnostics.push(SkillDiagnostic::warning(
                format!("failed to read skill file: {error}"),
                Some(file_path.to_path_buf()),
            ));
            return LoadedSkill {
                skill: None,
                diagnostics,
            };
        }
    };

    let frontmatter = match parse_frontmatter(&raw) {
        Ok(parsed) => parsed.frontmatter,
        Err(error) => {
            if is_declared_skill {
                diagnostics.push(SkillDiagnostic::warning(
                    error.to_string(),
                    Some(file_path.to_path_buf()),
                ));
            }
            return LoadedSkill {
                skill: None,
                diagnostics,
            };
        }
    };

    let description = frontmatter.get_str("description").map(str::to_string);
    let has_description = description
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());

    // Loose `.md` files in a skills directory are only skills when they
    // carry a description; declared `SKILL.md` files stay loadable.
    if !is_declared_skill && !has_description {
        return LoadedSkill {
            skill: None,
            diagnostics,
        };
    }

    let base_dir = file_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    match &description {
        Some(value) => {
            for error in validate_skill_description(value) {
                diagnostics.push(SkillDiagnostic::warning(
                    error,
                    Some(file_path.to_path_buf()),
                ));
            }
        }
        None => diagnostics.push(SkillDiagnostic::warning(
            "description is required",
            Some(file_path.to_path_buf()),
        )),
    }

    let Some(description) = description.filter(|value| !value.trim().is_empty()) else {
        return LoadedSkill {
            skill: None,
            diagnostics,
        };
    };

    let name = frontmatter
        .get_str("name")
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            base_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string()
        });

    for error in validate_skill_name(&name) {
        diagnostics.push(SkillDiagnostic::warning(
            error,
            Some(file_path.to_path_buf()),
        ));
    }

    LoadedSkill {
        skill: Some(Skill {
            name,
            description,
            file_path: file_path.to_path_buf(),
            base_dir,
            source,
            disable_model_invocation: frontmatter
                .get_bool("disable-model-invocation")
                .unwrap_or(false),
        }),
        diagnostics,
    }
}

/// Load skills from every configured location.
///
/// Global skills come first, then project skills, then explicit
/// `--skill` paths; the first definition of a name wins and duplicates
/// are reported as [`DiagnosticKind::Collision`].
pub fn load_skills(options: &LoadSkillsOptions) -> SkillsLoadResult {
    let cwd = absolute(&options.cwd);
    let agent_dir = absolute(&options.agent_dir);

    let mut by_name: BTreeMap<String, Skill> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut seen_files: HashSet<PathBuf> = HashSet::new();
    let mut diagnostics: Vec<SkillDiagnostic> = Vec::new();
    let mut collisions: Vec<SkillDiagnostic> = Vec::new();

    let mut add = |result: SkillsLoadResult,
                   diagnostics: &mut Vec<SkillDiagnostic>,
                   collisions: &mut Vec<SkillDiagnostic>| {
        diagnostics.extend(result.diagnostics);
        for skill in result.skills {
            // `canonicalize` resolves symlinks so the same file reached
            // through two locations loads once.
            let identity =
                fs::canonicalize(&skill.file_path).unwrap_or_else(|_| skill.file_path.clone());
            if !seen_files.insert(identity) {
                continue;
            }
            if by_name.contains_key(&skill.name) {
                collisions.push(SkillDiagnostic::collision(
                    format!("name \"{}\" collision", skill.name),
                    skill.file_path.clone(),
                ));
                continue;
            }
            order.push(skill.name.clone());
            by_name.insert(skill.name.clone(), skill);
        }
    };

    if options.include_defaults {
        let user_dir = agent_dir.join("skills");
        add(
            load_skills_from_dir(&user_dir, SkillSource::User),
            &mut diagnostics,
            &mut collisions,
        );
        if options.project_trusted {
            let project_dir = cwd.join(CONFIG_DIR_NAME).join("skills");
            add(
                load_skills_from_dir(&project_dir, SkillSource::Project),
                &mut diagnostics,
                &mut collisions,
            );
        }
    }

    for raw_path in &options.skill_paths {
        let resolved = absolute(&expand_tilde(raw_path, &cwd));
        if !resolved.exists() {
            diagnostics.push(SkillDiagnostic::warning(
                "skill path does not exist",
                Some(resolved),
            ));
            continue;
        }

        let Ok(metadata) = fs::metadata(&resolved) else {
            diagnostics.push(SkillDiagnostic::warning(
                "failed to read skill path",
                Some(resolved),
            ));
            continue;
        };

        if metadata.is_dir() {
            add(
                load_skills_from_dir(&resolved, SkillSource::Path),
                &mut diagnostics,
                &mut collisions,
            );
        } else if metadata.is_file() && resolved.extension().is_some_and(|ext| ext == "md") {
            let loaded = load_skill_from_file(&resolved, SkillSource::Path);
            if let Some(skill) = loaded.skill {
                add(
                    SkillsLoadResult {
                        skills: vec![skill],
                        diagnostics: loaded.diagnostics,
                    },
                    &mut diagnostics,
                    &mut collisions,
                );
            } else {
                diagnostics.extend(loaded.diagnostics);
            }
        } else {
            diagnostics.push(SkillDiagnostic::warning(
                "skill path is not a markdown file",
                Some(resolved),
            ));
        }
    }

    let skills = order
        .into_iter()
        .filter_map(|name| by_name.remove(&name))
        .collect();

    diagnostics.extend(collisions);
    SkillsLoadResult {
        skills,
        diagnostics,
    }
}

/// Format skills for the system prompt (Agent Skills XML block).
///
/// Returns an empty string when no skill is visible — skills with
/// `disable-model-invocation: true` never reach the model through the
/// prompt, they can only be invoked explicitly.
pub fn format_skills_for_prompt(skills: &[Skill], file_read_tool: SkillReadTool) -> String {
    let visible: Vec<&Skill> = skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .collect();

    if visible.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = vec![
        String::new(),
        String::new(),
        "The following skills provide specialized instructions for specific tasks.".to_string(),
        file_read_tool.instruction().to_string(),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ];

    for skill in visible {
        lines.push("  <skill>".to_string());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        lines.push(format!(
            "    <description>{}</description>",
            escape_xml(&skill.description)
        ));
        lines.push(format!(
            "    <location>{}</location>",
            escape_xml(&skill.file_path.to_string_lossy())
        ));
        lines.push("  </skill>".to_string());
    }

    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// `$HOME` (or `%USERPROFILE%` on Windows).
pub fn home_dir() -> Option<PathBuf> {
    crate::paths::home_dir()
}

/// The global agent config directory (`~/.pi/agent`).
pub fn agent_dir() -> Option<PathBuf> {
    crate::paths::agent_dir()
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
                "pi-skills-{label}-{}-{}",
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

    fn load(dir: &Path) -> SkillsLoadResult {
        load_skills_from_dir(dir, SkillSource::Path)
    }

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn loads_a_valid_skill() {
        let temp = TempDir::new("valid");
        temp.write(
            "valid-skill/SKILL.md",
            "---\nname: valid-skill\ndescription: A valid skill for testing purposes.\n---\n\n# Valid\n",
        );

        let result = load(&temp.path.join("valid-skill"));
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].name, "valid-skill");
        assert_eq!(
            result.skills[0].description,
            "A valid skill for testing purposes."
        );
        assert!(!result.skills[0].disable_model_invocation);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    }

    #[test]
    fn falls_back_to_the_directory_name() {
        let temp = TempDir::new("fallback-name");
        temp.write(
            "my-skill/SKILL.md",
            "---\ndescription: Description without a name.\n---\n",
        );

        let result = load(&temp.path.join("my-skill"));
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].name, "my-skill");
    }

    #[test]
    fn skips_skills_without_a_description() {
        let temp = TempDir::new("no-description");
        temp.write(
            "missing-description/SKILL.md",
            "---\nname: missing-description\n---\n",
        );
        temp.write("no-frontmatter/SKILL.md", "# No frontmatter\n");

        let result = load(&temp.path.join("missing-description"));
        assert!(result.skills.is_empty());
        assert!(result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("description is required")));

        let result = load(&temp.path.join("no-frontmatter"));
        assert!(result.skills.is_empty());
        assert!(result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("description is required")));
    }

    #[test]
    fn warns_but_keeps_off_spec_names() {
        let temp = TempDir::new("off-spec-names");
        temp.write(
            "bad--name/SKILL.md",
            "---\nname: bad--name\ndescription: Consecutive hyphens.\n---\n",
        );
        temp.write(
            "invalid/SKILL.md",
            "---\nname: Invalid_Name\ndescription: Invalid characters.\n---\n",
        );
        temp.write(
            "long/SKILL.md",
            &format!(
                "---\nname: {}\ndescription: Too long a name.\n---\n",
                "a".repeat(65)
            ),
        );

        let result = load(&temp.path.join("bad--name"));
        assert_eq!(result.skills.len(), 1);
        assert!(result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("consecutive hyphens")));

        let result = load(&temp.path.join("invalid"));
        assert_eq!(result.skills.len(), 1);
        assert!(result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("invalid characters")));

        let result = load(&temp.path.join("long"));
        assert_eq!(result.skills.len(), 1);
        assert!(result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("exceeds 64 characters")));
    }

    #[test]
    fn warns_on_invalid_frontmatter() {
        let temp = TempDir::new("invalid-yaml");
        temp.write(
            "invalid-yaml/SKILL.md",
            "---\nname: invalid-yaml\ndescription: [unclosed bracket\n---\n",
        );

        let result = load(&temp.path.join("invalid-yaml"));
        assert!(result.skills.is_empty());
        assert!(result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("frontmatter")));
    }

    #[test]
    fn prefers_a_root_skill_over_nested_ones() {
        let temp = TempDir::new("root-preferred");
        temp.write(
            "SKILL.md",
            "---\ndescription: Root skill should win.\n---\n",
        );
        temp.write(
            "nested-child/SKILL.md",
            "---\ndescription: Nested skill should be ignored.\n---\n",
        );

        let result = load(&temp.path);
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].description, "Root skill should win.");
        // No `name` in frontmatter: the name falls back to the root
        // directory's basename.
        assert_eq!(
            result.skills[0].name,
            temp.path.file_name().unwrap().to_string_lossy()
        );
    }

    #[test]
    fn recurses_into_subdirectories_and_loads_root_markdown_files() {
        let temp = TempDir::new("recursive");
        temp.write(
            "notes.md",
            "---\nname: notes\ndescription: A loose markdown skill.\n---\n",
        );
        temp.write("ignored.md", "# no frontmatter\n");
        temp.write(
            "nested/child-skill/SKILL.md",
            "---\nname: child-skill\ndescription: A nested skill.\n---\n",
        );
        temp.write(
            "nested/loose.md",
            "---\ndescription: Not loaded when nested.\n---\n",
        );

        let result = load(&temp.path);
        let names: Vec<&str> = result
            .skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.contains(&"child-skill"));
        assert!(names.contains(&"notes"));
        assert!(!names.contains(&"loose"));
    }

    #[test]
    fn skips_dot_dirs_and_node_modules() {
        let temp = TempDir::new("skip-dirs");
        temp.write(".hidden/SKILL.md", "---\ndescription: Hidden skill.\n---\n");
        temp.write(
            "node_modules/pkg/SKILL.md",
            "---\ndescription: Dependency skill.\n---\n",
        );

        let result = load(&temp.path);
        assert!(result.skills.is_empty(), "{:?}", result.skills);
    }

    #[test]
    fn missing_directory_is_not_an_error() {
        let result = load(Path::new("/non/existent/path"));
        assert!(result.skills.is_empty());
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn load_skills_prefers_global_then_project() {
        let temp = TempDir::new("precedence");
        temp.write(
            "agent/skills/shared/SKILL.md",
            "---\nname: shared\ndescription: From the agent dir.\n---\n",
        );
        temp.write(
            "project/.pi/skills/shared/SKILL.md",
            "---\nname: shared\ndescription: From the project.\n---\n",
        );
        temp.write(
            "project/.pi/skills/local/SKILL.md",
            "---\nname: local\ndescription: Project-only skill.\n---\n",
        );

        let result = load_skills(&LoadSkillsOptions {
            cwd: temp.path.join("project"),
            agent_dir: temp.path.join("agent"),
            skill_paths: Vec::new(),
            include_defaults: true,
            project_trusted: true,
        });

        assert_eq!(result.skills.len(), 2);
        assert_eq!(result.skills[0].name, "shared");
        assert_eq!(result.skills[0].description, "From the agent dir.");
        assert_eq!(result.skills[0].source, SkillSource::User);
        assert_eq!(result.skills[1].name, "local");
        assert_eq!(result.skills[1].source, SkillSource::Project);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].kind, DiagnosticKind::Collision);
        assert!(result.diagnostics[0].message.contains("collision"));
    }

    #[test]
    fn an_untrusted_project_hides_project_skills() {
        let temp = TempDir::new("untrusted-project");
        temp.write(
            "agent/skills/global/SKILL.md",
            "---\nname: global\ndescription: From the agent dir.\n---\n",
        );
        temp.write(
            "project/.pi/skills/local/SKILL.md",
            "---\nname: local\ndescription: Project-only skill.\n---\n",
        );

        let result = load_skills(&LoadSkillsOptions {
            cwd: temp.path.join("project"),
            agent_dir: temp.path.join("agent"),
            skill_paths: Vec::new(),
            include_defaults: true,
            project_trusted: false,
        });

        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].name, "global");
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn explicit_skill_paths_are_added_after_defaults() {
        let temp = TempDir::new("explicit");
        temp.write(
            "extra/extra-skill/SKILL.md",
            "---\ndescription: Explicitly loaded.\n---\n",
        );

        let result = load_skills(&LoadSkillsOptions {
            cwd: temp.path.clone(),
            agent_dir: temp.path.join("agent"),
            skill_paths: vec![temp.path.join("extra")],
            include_defaults: false,
            project_trusted: true,
        });

        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].source, SkillSource::Path);

        let result = load_skills(&LoadSkillsOptions {
            cwd: temp.path.clone(),
            agent_dir: temp.path.join("agent"),
            skill_paths: vec![PathBuf::from("/non/existent/path")],
            include_defaults: false,
            project_trusted: true,
        });
        assert!(result.skills.is_empty());
        assert!(result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("does not exist")));
    }

    #[test]
    fn formats_skills_as_an_xml_block() {
        let skill = Skill {
            name: "test-skill".to_string(),
            description: "A skill with <special> & \"characters\".".to_string(),
            file_path: PathBuf::from("/path/to/skill/SKILL.md"),
            base_dir: PathBuf::from("/path/to/skill"),
            source: SkillSource::Path,
            disable_model_invocation: false,
        };

        let formatted = format_skills_for_prompt(&[skill], SkillReadTool::Read);
        assert!(formatted.contains("<available_skills>"));
        assert!(formatted.contains("</available_skills>"));
        assert!(formatted.contains("<name>test-skill</name>"));
        assert!(formatted.contains("&lt;special&gt;"));
        assert!(formatted.contains("&amp;"));
        assert!(formatted.contains("&quot;characters&quot;"));
        assert!(formatted.contains("<location>/path/to/skill/SKILL.md</location>"));
        assert!(formatted.contains("Use the read tool to load a skill's file"));
        assert!(formatted.starts_with("\n\nThe following skills provide"));
    }

    #[test]
    fn hides_skills_marked_disable_model_invocation() {
        let visible = Skill {
            name: "visible".to_string(),
            description: "Visible.".to_string(),
            file_path: PathBuf::from("/visible/SKILL.md"),
            base_dir: PathBuf::from("/visible"),
            source: SkillSource::Path,
            disable_model_invocation: false,
        };
        let hidden = Skill {
            name: "hidden".to_string(),
            description: "Hidden.".to_string(),
            file_path: PathBuf::from("/hidden/SKILL.md"),
            base_dir: PathBuf::from("/hidden"),
            source: SkillSource::Path,
            disable_model_invocation: true,
        };

        let formatted =
            format_skills_for_prompt(&[visible.clone(), hidden.clone()], SkillReadTool::Bash);
        assert!(formatted.contains("<name>visible</name>"));
        assert!(!formatted.contains("<name>hidden</name>"));
        assert!(formatted.contains("Use bash to load a skill's file"));

        assert_eq!(format_skills_for_prompt(&[hidden], SkillReadTool::Read), "");
        assert_eq!(format_skills_for_prompt(&[], SkillReadTool::Read), "");
    }

    #[test]
    fn parses_disable_model_invocation_from_a_realistic_file() {
        let temp = TempDir::new("manual-only");
        temp.write(
            "manual/SKILL.md",
            "---\nname: manual-only\ndescription: Requires an explicit invocation.\ndisable-model-invocation: true\n---\n\n# Manual\n",
        );

        let result = load(&temp.path.join("manual"));
        assert_eq!(result.skills.len(), 1);
        assert!(result.skills[0].disable_model_invocation);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    }

    #[test]
    fn git_ignored_skill_files_are_skipped() {
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }

        let temp = TempDir::new("gitignore");
        temp.write("project/.gitignore", "ignored/\nloose.md\n");
        temp.write(
            "project/kept/SKILL.md",
            "---\nname: kept\ndescription: Kept.\n---\n",
        );
        temp.write(
            "project/ignored/SKILL.md",
            "---\nname: ignored\ndescription: Ignored.\n---\n",
        );
        temp.write(
            "project/loose.md",
            "---\nname: loose\ndescription: Loose.\n---\n",
        );
        run_git(&temp.path.join("project"), &["init", "-q", "."]);

        let result = load(&temp.path.join("project"));
        let names: Vec<&str> = result
            .skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect();
        assert_eq!(names, vec!["kept"], "{:?}", result.diagnostics);
    }

    #[test]
    fn skills_load_normally_outside_a_git_work_tree() {
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }

        let temp = TempDir::new("no-git");
        temp.write("project/.gitignore", "ignored/\n");
        temp.write(
            "project/kept/SKILL.md",
            "---\nname: kept\ndescription: Kept.\n---\n",
        );
        temp.write(
            "project/ignored/SKILL.md",
            "---\nname: ignored\ndescription: Ignored.\n---\n",
        );

        // No `git init`: the ignore rules are inert rather than an error.
        let result = load(&temp.path.join("project"));
        let mut names: Vec<&str> = result
            .skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect();
        names.sort();
        assert_eq!(names, vec!["ignored", "kept"]);
    }

    #[test]
    fn git_ignored_paths_are_empty_without_git() {
        // A directory that is not a work tree must never yield ignores, so
        // discovery keeps working on machines without git.
        let temp = TempDir::new("ignore-empty");
        temp.write(
            "project/SKILL.md",
            "---\nname: only\ndescription: Only.\n---\n",
        );
        let candidates = vec![
            temp.path.join("project/SKILL.md"),
            temp.path.join("project/nested/SKILL.md"),
        ];
        assert!(git_ignored_paths(&temp.path.join("project"), &candidates).is_empty());
        assert!(git_ignored_paths(&temp.path.join("project"), &[]).is_empty());
    }

    #[test]
    fn expands_tilde_in_skill_paths() {
        let Some(home) = home_dir() else {
            return;
        };
        assert_eq!(
            expand_tilde(Path::new("~/.pi/agent/skills"), Path::new("/cwd")),
            home.join(".pi/agent/skills")
        );
        assert_eq!(
            expand_tilde(Path::new("/abs/path"), Path::new("/cwd")),
            PathBuf::from("/abs/path")
        );
    }
}
