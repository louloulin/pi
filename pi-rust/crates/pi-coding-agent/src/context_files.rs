//! Project instructional context files (`AGENTS.md` / `CLAUDE.md`).
//!
//! Rust port of the `loadProjectContextFiles` half of
//! `packages/coding-agent/src/core/resource-loader.ts`. pi stitches
//! project instructions into the system prompt so a repository can steer
//! the agent without the user repeating itself every session.
//!
//! Search order:
//!
//! 1. the global agent directory (`~/.pi/agent/AGENTS.md`, …), first;
//! 2. every ancestor directory of the working directory, **outermost
//!    first**, so the closest file is the last thing the model reads.
//!
//! In each directory the first of
//! `AGENTS.override.md`, `AGENTS.md`, `AGENTS.MD`, `CLAUDE.md`,
//! `CLAUDE.MD` wins.
//!
//! One subtlety is ported deliberately: when the working directory is a
//! linked git worktree nested inside the main repository, the ancestor
//! walk would pick up *both* copies of the same logical context file (the
//! worktree's own `AGENTS.md` and the main repo's), applying that context
//! twice. [`load_project_context_files`] detects that layout and skips the
//! shadowed main-repo copy. Multica checkouts are exactly this layout, so
//! the behaviour matters in practice.
//!
//! Unlike the TS version this module never prints; unreadable candidate
//! files are skipped silently so callers can decide how noisy to be.

use std::fs;
use std::path::{Path, PathBuf};

use crate::paths::{absolute, canonicalize, strip_bom, CONFIG_DIR_NAME};

/// File names tried in order inside a directory, most specific first.
pub const CONTEXT_FILE_CANDIDATES: [&str; 5] = [
    "AGENTS.override.md",
    "AGENTS.md",
    "AGENTS.MD",
    "CLAUDE.md",
    "CLAUDE.MD",
];

/// One loaded context file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFile {
    /// Path the content was read from.
    pub path: PathBuf,
    /// File content, BOM stripped.
    pub content: String,
}

/// Load the first context file found in `dir`, if any.
///
/// Directories are skipped (a directory literally named `AGENTS.md` is
/// not a context file), as are unreadable candidates.
pub fn load_context_file_from_dir(dir: &Path) -> Option<ContextFile> {
    for filename in CONTEXT_FILE_CANDIDATES {
        let file_path = dir.join(filename);
        if !file_path.is_file() {
            continue;
        }
        match fs::read(&file_path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(content) => {
                    return Some(ContextFile {
                        path: file_path,
                        content: strip_bom(&content).to_string(),
                    })
                }
                Err(_) => continue,
            },
            Err(_) => continue,
        }
    }
    None
}

/// Git metadata paths for the repository containing `cwd`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPaths {
    /// Working tree root (`git rev-parse --show-toplevel`).
    pub repo_dir: PathBuf,
    /// Shared `.git` directory — for a linked worktree this is the main
    /// repository's `.git`, reached through the `commondir` file.
    pub common_git_dir: PathBuf,
    /// `HEAD` file inside the per-worktree git dir.
    pub head_path: PathBuf,
}

/// Walk up from `cwd` looking for `.git`, handling both a regular
/// repository (`.git` is a directory) and a linked worktree (`.git` is a
/// file holding `gitdir: …`).
pub fn find_git_paths(cwd: &Path) -> Option<GitPaths> {
    let mut dir = absolute(cwd);

    loop {
        let git_path = dir.join(".git");
        if let Ok(metadata) = fs::metadata(&git_path) {
            if metadata.is_file() {
                let content = fs::read_to_string(&git_path).ok()?;
                let content = content.trim();
                if let Some(target) = content.strip_prefix("gitdir: ") {
                    let git_dir = absolute(&dir.join(target.trim()));
                    let head_path = git_dir.join("HEAD");
                    if !head_path.exists() {
                        return None;
                    }
                    let common_dir_file = git_dir.join("commondir");
                    let common_git_dir = match fs::read_to_string(&common_dir_file) {
                        Ok(raw) => absolute(&git_dir.join(raw.trim())),
                        Err(_) => git_dir,
                    };
                    return Some(GitPaths {
                        repo_dir: dir,
                        common_git_dir,
                        head_path,
                    });
                }
                return None;
            }

            if metadata.is_dir() {
                let head_path = git_path.join("HEAD");
                if !head_path.exists() {
                    return None;
                }
                return Some(GitPaths {
                    repo_dir: dir,
                    common_git_dir: git_path,
                    head_path,
                });
            }
        }

        match dir.parent() {
            Some(parent) if parent != dir => dir = parent.to_path_buf(),
            _ => return None,
        }
    }
}

/// The main repo's context file that a nested linked worktree shadows.
///
/// Returns `None` for an ordinary repository and for a sibling worktree
/// (whose main repo is not an ancestor), leaving normal ancestor
/// inheritance untouched.
fn find_shadowed_context_file(cwd: &Path) -> Option<PathBuf> {
    let git_paths = find_git_paths(cwd)?;
    let common_git_dir = canonicalize(&git_paths.common_git_dir);
    let worktree_root = canonicalize(&git_paths.repo_dir);
    let main_repo_root = common_git_dir.parent()?.to_path_buf();

    // Only a strict descendant can shadow: for an ordinary repo the two
    // roots are the same directory.
    if worktree_root == main_repo_root || !worktree_root.starts_with(&main_repo_root) {
        return None;
    }

    // In a bare layout (`proj/.bare` + `proj/main`) the parent of the
    // common git dir merely holds `.bare`; a submodule's gitdir has no
    // `commondir` and lands under `.git/modules`.
    if canonicalize(&main_repo_root.join(".git")) != common_git_dir {
        return None;
    }

    let worktree_context_file = load_context_file_from_dir(&worktree_root)?;
    Some(main_repo_root.join(worktree_context_file.path.file_name()?))
}

/// Load the global context file plus every ancestor context file.
pub fn load_project_context_files(cwd: &Path, agent_dir: &Path) -> Vec<ContextFile> {
    let resolved_cwd = absolute(cwd);
    let resolved_agent_dir = absolute(agent_dir);

    let mut context_files: Vec<ContextFile> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();

    if let Some(global) = load_context_file_from_dir(&resolved_agent_dir) {
        seen.push(global.path.clone());
        context_files.push(global);
    }

    let shadowed = find_shadowed_context_file(&resolved_cwd);
    let mut current_dir = resolved_cwd;
    let mut ancestors: Vec<ContextFile> = Vec::new();

    loop {
        if let Some(context_file) = load_context_file_from_dir(&current_dir) {
            let is_shadowed = shadowed
                .as_ref()
                .is_some_and(|shadowed| canonicalize(&context_file.path) == *shadowed);
            if !is_shadowed && !seen.contains(&context_file.path) {
                seen.push(context_file.path.clone());
                ancestors.push(context_file);
            }
        }

        match current_dir.parent() {
            Some(parent) if parent != current_dir => current_dir = parent.to_path_buf(),
            _ => break,
        }
    }

    // Outermost first: the closest directory's instructions are applied
    // last, which is what makes them win in practice.
    ancestors.reverse();
    context_files.extend(ancestors);
    context_files
}

/// The `SYSTEM.md` that replaces the built-in system prompt.
///
/// Precedence matches upstream `discoverSystemPromptFile`: a trusted
/// project's `.pi/SYSTEM.md` wins, otherwise the agent-directory copy is
/// used. Passing `project_trusted = false` makes a checked-in
/// `.pi/SYSTEM.md` invisible, so a cloned repository cannot replace the
/// system prompt before the user runs `/trust`.
pub fn discover_system_prompt_file(
    cwd: &Path,
    agent_dir: &Path,
    project_trusted: bool,
) -> Option<PathBuf> {
    if project_trusted {
        let project = absolute(cwd).join(CONFIG_DIR_NAME).join("SYSTEM.md");
        if project.is_file() {
            return Some(project);
        }
    }
    let global = absolute(agent_dir).join("SYSTEM.md");
    global.is_file().then_some(global)
}

/// The `APPEND_SYSTEM.md`, appended after the built-in prompt.
///
/// Same trust gate and precedence as [`discover_system_prompt_file`].
pub fn discover_append_system_prompt_file(
    cwd: &Path,
    agent_dir: &Path,
    project_trusted: bool,
) -> Option<PathBuf> {
    if project_trusted {
        let project = absolute(cwd).join(CONFIG_DIR_NAME).join("APPEND_SYSTEM.md");
        if project.is_file() {
            return Some(project);
        }
    }
    let global = absolute(agent_dir).join("APPEND_SYSTEM.md");
    global.is_file().then_some(global)
}

/// Read a prompt file, returning `None` when it cannot be read.
pub fn read_prompt_file(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    Some(strip_bom(&content).to_string())
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
                "pi-context-{label}-{}-{}",
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
    fn picks_the_first_candidate_in_priority_order() {
        let temp = TempDir::new("candidates");
        temp.write("AGENTS.md", "agents");
        temp.write("CLAUDE.md", "claude");

        let loaded = load_context_file_from_dir(&temp.path).expect("loads");
        assert_eq!(loaded.content, "agents");
        assert_eq!(loaded.path, temp.path.join("AGENTS.md"));

        temp.write("AGENTS.override.md", "override");
        let loaded = load_context_file_from_dir(&temp.path).expect("loads");
        assert_eq!(loaded.content, "override");
    }

    #[test]
    fn accepts_uppercase_extensions_and_claude_fallbacks() {
        let temp = TempDir::new("fallbacks");
        temp.write("CLAUDE.MD", "claude uppercase");
        let loaded = load_context_file_from_dir(&temp.path).expect("loads");
        assert_eq!(loaded.content, "claude uppercase");

        let temp = TempDir::new("uppercase");
        temp.write("AGENTS.MD", "agents uppercase");
        let loaded = load_context_file_from_dir(&temp.path).expect("loads");
        assert_eq!(loaded.content, "agents uppercase");
    }

    #[test]
    fn strips_a_bom() {
        let temp = TempDir::new("bom");
        temp.write("AGENTS.md", "\u{feff}hello");
        let loaded = load_context_file_from_dir(&temp.path).expect("loads");
        assert_eq!(loaded.content, "hello");
    }

    #[test]
    fn missing_directories_load_nothing() {
        assert!(load_context_file_from_dir(Path::new("/non/existent/dir")).is_none());
    }

    #[test]
    fn directories_named_like_context_files_are_skipped() {
        let temp = TempDir::new("dir-not-file");
        fs::create_dir_all(temp.path.join("AGENTS.md")).expect("create dir");
        assert!(load_context_file_from_dir(&temp.path).is_none());
    }

    #[test]
    fn loads_global_first_then_ancestors_outermost_first() {
        let temp = TempDir::new("ancestors");
        temp.write("agent/AGENTS.md", "global");
        temp.write("repo/AGENTS.md", "repo");
        temp.write("repo/pkg/AGENTS.md", "pkg");
        fs::create_dir_all(temp.path.join("repo/pkg/src")).expect("create cwd");

        let loaded = load_project_context_files(
            &temp.path.join("repo/pkg/src"),
            &temp.path.join("agent"),
        );

        let contents: Vec<&str> = loaded.iter().map(|file| file.content.as_str()).collect();
        assert_eq!(contents, vec!["global", "repo", "pkg"]);
    }

    #[test]
    fn skips_duplicate_paths_from_a_symlinked_agent_dir() {
        let temp = TempDir::new("dedup");
        temp.write("agent/AGENTS.md", "global");

        let loaded = load_project_context_files(&temp.path.join("agent"), &temp.path.join("agent"));
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content, "global");
    }

    #[test]
    fn detects_git_paths_for_a_regular_repository() {
        let temp = TempDir::new("git-plain");
        fs::create_dir_all(temp.path.join(".git")).expect("create .git");
        fs::write(temp.path.join(".git/HEAD"), "ref: refs/heads/main\n").expect("write HEAD");
        fs::create_dir_all(temp.path.join("nested/deep")).expect("create nested");

        let git_paths = find_git_paths(&temp.path.join("nested/deep")).expect("finds .git");
        assert_eq!(git_paths.repo_dir, temp.path);
        assert_eq!(git_paths.common_git_dir, temp.path.join(".git"));
    }

    #[test]
    fn detects_worktree_git_paths() {
        let temp = TempDir::new("git-worktree");
        // Main repository.
        fs::create_dir_all(temp.path.join("main/.git/worktrees/feat")).expect("create git dir");
        fs::write(temp.path.join("main/.git/HEAD"), "ref: refs/heads/main\n").expect("HEAD");
        fs::write(
            temp.path.join("main/.git/worktrees/feat/HEAD"),
            "ref: refs/heads/feat\n",
        )
        .expect("worktree HEAD");
        // Linked worktree nested inside the main repo.
        fs::create_dir_all(temp.path.join("main/wt/src")).expect("create worktree");
        fs::write(
            temp.path.join("main/wt/.git"),
            "gitdir: ../.git/worktrees/feat\n",
        )
        .expect("git file");
        fs::write(
            temp.path.join("main/.git/worktrees/feat/commondir"),
            "../..\n",
        )
        .expect("commondir");

        let git_paths = find_git_paths(&temp.path.join("main/wt/src")).expect("finds worktree");
        assert_eq!(git_paths.repo_dir, temp.path.join("main/wt"));
        assert_eq!(git_paths.common_git_dir, temp.path.join("main/.git"));
    }

    #[test]
    fn shadowed_main_repo_context_file_is_skipped() {
        let temp = TempDir::new("shadow");
        // Main repository with its own context file.
        fs::create_dir_all(temp.path.join("main/.git/worktrees/feat")).expect("create git dir");
        fs::write(temp.path.join("main/.git/HEAD"), "ref: refs/heads/main\n").expect("HEAD");
        fs::write(
            temp.path.join("main/.git/worktrees/feat/HEAD"),
            "ref: refs/heads/feat\n",
        )
        .expect("worktree HEAD");
        fs::write(temp.path.join("main/.git/worktrees/feat/commondir"), "../..\n").expect("commondir");
        fs::write(temp.path.join("main/AGENTS.md"), "main repo rules").expect("main context");
        // Nested linked worktree with the same logical context file.
        fs::create_dir_all(temp.path.join("main/wt/src")).expect("create worktree");
        fs::write(temp.path.join("main/wt/.git"), "gitdir: ../.git/worktrees/feat\n").expect("git file");
        fs::write(temp.path.join("main/wt/AGENTS.md"), "worktree rules").expect("worktree context");

        let loaded = load_project_context_files(
            &temp.path.join("main/wt/src"),
            &temp.path.join("agent"),
        );

        let contents: Vec<&str> = loaded.iter().map(|file| file.content.as_str()).collect();
        assert_eq!(contents, vec!["worktree rules"], "{loaded:?}");
    }

    #[test]
    fn sibling_worktrees_do_not_inherit_the_main_repo_context() {
        let temp = TempDir::new("sibling");
        fs::create_dir_all(temp.path.join("main/.git/worktrees/feat")).expect("create git dir");
        fs::write(temp.path.join("main/.git/HEAD"), "ref: refs/heads/main\n").expect("HEAD");
        fs::write(
            temp.path.join("main/.git/worktrees/feat/HEAD"),
            "ref: refs/heads/feat\n",
        )
        .expect("worktree HEAD");
        fs::write(temp.path.join("main/.git/worktrees/feat/commondir"), "../..\n").expect("commondir");
        fs::write(temp.path.join("main/AGENTS.md"), "main repo rules").expect("main context");
        // Sibling worktree: `main` is not an ancestor of the cwd, so its
        // context file is neither inherited nor considered shadowed.
        fs::create_dir_all(temp.path.join("feat/src")).expect("create worktree");
        fs::write(temp.path.join("feat/.git"), "gitdir: ../main/.git/worktrees/feat\n").expect("git file");

        let loaded = load_project_context_files(&temp.path.join("feat/src"), &temp.path.join("agent"));
        assert!(loaded.is_empty(), "{loaded:?}");
    }

    #[test]
    fn discovers_system_prompt_files_only_when_present() {
        let temp = TempDir::new("system-md");
        let agent = temp.path.join("agent");
        let cwd = temp.path.join("project");
        fs::create_dir_all(&cwd).expect("cwd");
        assert_eq!(discover_system_prompt_file(&cwd, &agent, true), None);
        assert_eq!(discover_append_system_prompt_file(&cwd, &agent, true), None);

        fs::create_dir_all(&agent).expect("agent");
        temp.write("agent/SYSTEM.md", "custom prompt");
        temp.write("agent/APPEND_SYSTEM.md", "appended\n");
        assert_eq!(
            discover_system_prompt_file(&cwd, &agent, false),
            Some(agent.join("SYSTEM.md"))
        );

        let read = read_prompt_file(&agent.join("APPEND_SYSTEM.md")).expect("reads");
        assert_eq!(read.trim(), "appended");
    }

    #[test]
    fn a_trusted_project_system_md_shadows_the_global_one() {
        let temp = TempDir::new("project-system-md");
        let agent = temp.path.join("agent");
        let cwd = temp.path.join("project");
        temp.write("agent/SYSTEM.md", "global prompt");
        temp.write("agent/APPEND_SYSTEM.md", "global append");
        temp.write("project/.pi/SYSTEM.md", "project prompt");
        temp.write("project/.pi/APPEND_SYSTEM.md", "project append");

        assert_eq!(
            discover_system_prompt_file(&cwd, &agent, true),
            Some(cwd.join(".pi/SYSTEM.md"))
        );
        assert_eq!(
            discover_append_system_prompt_file(&cwd, &agent, true),
            Some(cwd.join(".pi/APPEND_SYSTEM.md"))
        );

        // Untrusted: the project files are invisible, the global ones win.
        assert_eq!(
            discover_system_prompt_file(&cwd, &agent, false),
            Some(agent.join("SYSTEM.md"))
        );
        assert_eq!(
            discover_append_system_prompt_file(&cwd, &agent, false),
            Some(agent.join("APPEND_SYSTEM.md"))
        );
    }
}
