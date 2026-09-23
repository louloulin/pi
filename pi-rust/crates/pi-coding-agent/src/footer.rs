//! Footer data the interactive TUI cannot compute for itself.
//!
//! Upstream splits this in two: `packages/coding-agent/src/core/footer-data-provider.ts`
//! resolves the git branch (and watches `HEAD` for changes) while
//! `modes/interactive/components/footer.ts` renders it. The Rust port keeps
//! the *rendering* in `pi-tui` (`pi_tui::status::StatusData::location_line`,
//! `StatusBar::render_lines`) and the *facts* here, because only the
//! coding-agent knows which directory the session was launched in.
//!
//! # Branch resolution
//!
//! `git_branch` reads `.git/HEAD` instead of shelling out to
//! `git symbolic-ref --short HEAD` (which is what upstream does,
//! `footer-data-provider.ts:51-59`). The observable contract is the same and
//! is the one upstream documents on `getGitBranch()`:
//!
//! * inside a repo on a branch → `Some("main")`
//! * detached HEAD (`.git/HEAD` holds a raw object id) → `None`
//! * not inside a repo, unreadable metadata, or git worktrees → handled:
//!   worktrees are supported because `.git` may be a *file* holding
//!   `gitdir: …`, which is exactly the path upstream's `findGitPaths` walks
//!   (`footer-data-provider.ts:14-47`).
//!
//! Deliberate deviations: no `packed-refs` lookup (a branch ref always has a
//! loose `HEAD`), no per-frame re-resolution, and no filesystem watcher —
//! upstream repaints the footer when `HEAD` changes
//! (`footer-data-provider.ts:139-196`), the Rust driver resolves the branch
//! once at startup. See `docs/LUM1466_TWO_LINE_FOOTER.md` §6.

use std::fs;
use std::path::{Path, PathBuf};

/// The current git branch for `cwd`, or `None` outside a repository or on a
/// detached HEAD.
///
/// Walks up from `cwd` to the filesystem root, so running the TUI from a
/// subdirectory of a repo reports that repo's branch.
pub fn git_branch(cwd: &Path) -> Option<String> {
    let head = head_path(cwd)?;
    branch_from_head(&fs::read_to_string(head).ok()?)
}

/// The `HEAD` file of the repository `cwd` sits in, walking up its ancestors.
///
/// Mirrors upstream's `findGitPaths`: `.git` is a directory in the common
/// case, and a file containing `gitdir: <path>` in a linked worktree
/// (`footer-data-provider.ts:19-46`).
fn head_path(cwd: &Path) -> Option<PathBuf> {
    let mut dir = cwd;
    loop {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            let head = dot_git.join("HEAD");
            if head.is_file() {
                return Some(head);
            }
            return None;
        }
        if dot_git.is_file() {
            let content = fs::read_to_string(&dot_git).ok()?;
            let target = content.strip_prefix("gitdir:")?.trim();
            if target.is_empty() {
                return None;
            }
            let git_dir = if Path::new(target).is_absolute() {
                PathBuf::from(target)
            } else {
                dir.join(target)
            };
            let head = git_dir.join("HEAD");
            return head.is_file().then_some(head);
        }
        dir = dir.parent()?;
    }
}

/// Parse a `HEAD` file's contents: `ref: refs/heads/<branch>` → `<branch>`.
///
/// A raw object id (detached HEAD) yields `None`, as does any other ref
/// namespace — `symbolic-ref --short HEAD` would report those, but upstream's
/// footer only ever shows a branch, so the extra names would be noise.
fn branch_from_head(content: &str) -> Option<String> {
    let rest = content.trim().strip_prefix("ref:")?.trim();
    let branch = rest.strip_prefix("refs/heads/")?;
    (!branch.is_empty()).then(|| branch.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_a_branch_head() {
        assert_eq!(
            branch_from_head("ref: refs/heads/main\n").as_deref(),
            Some("main")
        );
        assert_eq!(
            branch_from_head("ref: refs/heads/feature/pi.rs\n").as_deref(),
            Some("feature/pi.rs")
        );
    }

    #[test]
    fn a_detached_head_has_no_branch() {
        // Upstream's `symbolic-ref --quiet` exits non-zero here, so the
        // footer shows no `(branch)` suffix.
        assert_eq!(
            branch_from_head("9f2c7e92853f9f2c7e92853f9f2c7e92853f9f2c\n"),
            None
        );
        assert_eq!(branch_from_head("ref: refs/remotes/origin/main\n"), None);
        assert_eq!(branch_from_head(""), None);
        assert_eq!(branch_from_head("ref: refs/heads/\n"), None);
    }

    #[test]
    fn finds_the_branch_from_a_nested_directory() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let repo = temp.path().join("repo");
        let nested = repo.join("crates").join("pi-tui");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(&nested).unwrap();
        let mut head = fs::File::create(repo.join(".git/HEAD")).unwrap();
        head.write_all(b"ref: refs/heads/main\n").unwrap();

        assert_eq!(git_branch(&nested).as_deref(), Some("main"));
        assert_eq!(git_branch(&repo).as_deref(), Some("main"));
        // The TempDir root itself is not in a repo.
        assert_eq!(git_branch(temp.path()), None);
    }

    #[test]
    fn supports_a_worktree_gitfile() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let worktree = temp.path().join("worktree");
        let git_dir = temp.path().join("main/.git/worktrees/worktree");
        fs::create_dir_all(&worktree).unwrap();
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", git_dir.display()),
        )
        .unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/topic\n").unwrap();

        assert_eq!(git_branch(&worktree).as_deref(), Some("topic"));
    }
}
