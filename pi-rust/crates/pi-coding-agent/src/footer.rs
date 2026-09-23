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
//! loose `HEAD`), and no filesystem watcher — the interactive driver polls
//! through [`BranchTracker`] once per frame instead of watching `HEAD`
//! (`footer-data-provider.ts:139-196`). The observable contract is the same:
//! a `git checkout` in another terminal repaints both the built-in footer and
//! every `footerData.getGitBranch()` reader.

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

/// The branch `cwd` sits on, re-read on every [`poll`](Self::poll).
///
/// Upstream resolves the branch once and then watches `HEAD` (`and`, for
/// reftable repos, the reftable directory) with `fs.watch`, repainting the
/// footer on change (`footer-data-provider.ts:51-59,139-196`). The port does
/// the cheap half of that: the repository's `HEAD` path is resolved once and
/// only its contents are re-read per poll, so an idle session costs one
/// `stat` + one small read per frame and never re-walks the ancestors of a
/// repository it already found. A path that stops resolving (HEAD deleted, the
/// directory moved, the session started outside a repo) makes the next poll
/// walk the ancestors again, so `git init` in a session's cwd is picked up
/// without a restart.
#[derive(Debug, Default)]
pub struct BranchTracker {
    /// `HEAD` of the repository the previous poll resolved, when it still
    /// exists.
    head: Option<PathBuf>,
    /// Branch the previous poll read; `None` outside a repo or on a detached
    /// `HEAD`.
    branch: Option<String>,
    /// Whether [`poll`](Self::poll) has ever run.
    resolved: bool,
}

impl BranchTracker {
    /// A tracker that has not read anything yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The value the last [`poll`](Self::poll) resolved.
    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }

    /// Re-read the branch for `cwd`.
    ///
    /// Returns `true` when the value is **new to this tracker**: on the first
    /// call (the initial value is reported so the caller can seed both the
    /// footer and the `footerData` snapshot) and on every call that observes a
    /// moved `HEAD`. A session that keeps resolving the same branch — the
    /// overwhelming common case — reports `false` and costs nothing but the
    /// read.
    pub fn poll(&mut self, cwd: &Path) -> bool {
        let head = match self.head.clone().filter(|path| path.is_file()) {
            Some(cached) => Some(cached),
            // Nothing cached, or the cached `HEAD` is gone: the session may have
            // started outside a repo (or a worktree's gitdir moved), so resolve
            // again from the cwd instead of staying stuck on `None`.
            None => head_path(cwd),
        };
        let branch = head
            .as_ref()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|content| branch_from_head(&content));
        self.head = head;
        let changed = !self.resolved || branch != self.branch;
        self.branch = branch;
        self.resolved = true;
        changed
    }
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
    fn tracker_reports_the_initial_value_then_only_real_moves() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let nested = repo.join("crates").join("pi-tui");
        fs::create_dir_all(&nested).unwrap();

        let mut tracker = BranchTracker::new();
        assert!(
            tracker.poll(&nested),
            "the first poll is the initial value, which the caller must seed from"
        );
        assert_eq!(tracker.branch(), Some("main"));
        assert!(
            !tracker.poll(&nested),
            "an unchanged HEAD is not a transition (upstream's onBranchChange must stay quiet)"
        );

        // `git checkout -b side`, from another terminal: the driver re-reads
        // HEAD each frame, so the very next poll sees it.
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/side\n").unwrap();
        assert!(tracker.poll(&nested));
        assert_eq!(tracker.branch(), Some("side"));
        assert!(!tracker.poll(&nested));

        // Detached HEAD: no branch, and that *is* a transition.
        fs::write(
            repo.join(".git/HEAD"),
            "9f2c7e92853f9f2c7e92853f9f2c7e92853f9f2c\n",
        )
        .unwrap();
        assert!(tracker.poll(&repo));
        assert_eq!(tracker.branch(), None);
        assert!(!tracker.poll(&repo));
    }

    #[test]
    fn tracker_notices_a_repository_appearing_after_a_non_repo_start() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let dir = temp.path().join("empty");
        fs::create_dir_all(&dir).unwrap();

        let mut tracker = BranchTracker::new();
        assert!(tracker.poll(&dir), "the initial value is reported");
        assert_eq!(tracker.branch(), None);
        assert!(!tracker.poll(&dir), "still no repo: no transition");

        // `git init` inside the session's cwd, with a HEAD written before the
        // next frame. The tracker must walk the ancestors again instead of
        // caching "no repo" forever.
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join(".git/HEAD"), "ref: refs/heads/fresh\n").unwrap();
        assert!(tracker.poll(&dir));
        assert_eq!(tracker.branch(), Some("fresh"));
    }

    #[test]
    fn tracker_follows_a_worktree_gitfile() {
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

        let mut tracker = BranchTracker::new();
        assert!(tracker.poll(&worktree));
        assert_eq!(tracker.branch(), Some("topic"));

        // A worktree checkout rewrites the *common* git dir's HEAD through the
        // gitfile indirection, which is exactly the path the tracker caches.
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/other\n").unwrap();
        assert!(tracker.poll(&worktree));
        assert_eq!(tracker.branch(), Some("other"));
    }

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
