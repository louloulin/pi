//! Shared helpers for the navigation tools (`find`, `grep`, `ls`).
//!
//! Centralises the directory ignore list that mirrors the TS port's
//! hardcoded skip list (`.git`, `node_modules`, `target`, etc.) so the
//! three tools stay consistent. The list is intentionally conservative;
//! more sophisticated `.gitignore` integration is left for a later stage.
//!
//! Also exposes [`relativize_for_search`], which canonicalises paths
//! relative to the agent's current working directory the same way every
//! navigation tool does it. Returning POSIX-style separators keeps the
//! output stable across Windows and Unix.

#![cfg(not(target_arch = "wasm32"))]

use std::path::{Component, Path, PathBuf};

/// Directory names that the navigation tools never descend into.
///
/// Mirrors `packages/coding-agent/src/core/tools/path-utils.ts`. The TS
/// port relies on `fd`'s `.gitignore` handling, but our pure-Rust
/// implementation has no such machinery — instead it skips the usual
/// build / VCS noise by name.
pub const DEFAULT_IGNORE_NAMES: &[&str] = &[
    ".git",
    ".pi",
    "node_modules",
    "target",
    "dist",
    "build",
];

/// `true` if `name` (a single path component) should be skipped during
/// directory walks.
pub fn is_ignored_dir_name(name: &str) -> bool {
    DEFAULT_IGNORE_NAMES.iter().any(|ignored| *ignored == name)
}

/// Build a search root relative to `cwd`, validating that the supplied
/// path stays inside the sandbox.
///
/// Returns the absolute path that `walkdir` / `std::fs::read_dir` should
/// use, plus the path-as-supplied-by-the-caller (which we keep around so
/// we can produce error messages with the exact input).
///
/// Behaviour:
/// - Empty / `"."` input resolves to `cwd` itself.
/// - Absolute paths are rejected with `ToolError::SandboxViolation`.
/// - Paths containing a `..` component are rejected for the same reason.
/// - Otherwise the input is joined onto `cwd` and returned as an absolute
///   [`PathBuf`].
pub fn relativize_for_search(
    raw: &str,
    cwd: &Path,
) -> Result<(PathBuf, String), super::ToolError> {
    use super::ToolError;

    let input = raw.trim();
    let display = if input.is_empty() { "." } else { input };

    let path = Path::new(display);
    if path.is_absolute() {
        return Err(ToolError::SandboxViolation(format!(
            "absolute paths are not allowed (got '{}')",
            display
        )));
    }

    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err(ToolError::SandboxViolation(format!(
                "path may not contain '..' (got '{}')",
                display
            )));
        }
    }

    let joined = if display == "." {
        cwd.to_path_buf()
    } else {
        cwd.join(display)
    };

    Ok((joined, display.to_string()))
}

/// Convert an absolute path produced during a walk back to the
/// search-root-relative form the model expects, using POSIX separators
/// regardless of host OS.
pub fn to_posix_relative(root: &Path, abs: &Path) -> String {
    let rel = abs.strip_prefix(root).unwrap_or(abs);
    let mut out = String::new();
    let mut first = true;
    for component in rel.components() {
        let piece = match component {
            Component::Normal(s) => s.to_string_lossy().into_owned(),
            Component::CurDir => ".".to_string(),
            _ => continue,
        };
        if !first {
            out.push('/');
        }
        first = false;
        out.push_str(&piece);
    }
    if out.is_empty() {
        ".".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolError;
    use std::path::PathBuf;

    #[test]
    fn default_relativize_is_cwd() {
        let cwd = PathBuf::from("/workspace");
        let (abs, display) = relativize_for_search(".", &cwd).unwrap();
        assert_eq!(abs, cwd);
        assert_eq!(display, ".");
    }

    #[test]
    fn empty_string_is_cwd() {
        let cwd = PathBuf::from("/workspace");
        let (abs, _) = relativize_for_search("", &cwd).unwrap();
        assert_eq!(abs, cwd);
    }

    #[test]
    fn nested_relative_joins_cwd() {
        let cwd = PathBuf::from("/workspace");
        let (abs, display) = relativize_for_search("src/lib", &cwd).unwrap();
        assert_eq!(abs, PathBuf::from("/workspace/src/lib"));
        assert_eq!(display, "src/lib");
    }

    #[test]
    fn absolute_paths_are_rejected() {
        let cwd = PathBuf::from("/workspace");
        let err = relativize_for_search("/etc", &cwd).unwrap_err();
        assert!(matches!(err, ToolError::SandboxViolation(_)));
    }

    #[test]
    fn parent_traversal_is_rejected() {
        let cwd = PathBuf::from("/workspace");
        let err = relativize_for_search("../etc", &cwd).unwrap_err();
        assert!(matches!(err, ToolError::SandboxViolation(_)));
        let err = relativize_for_search("a/../../b", &cwd).unwrap_err();
        assert!(matches!(err, ToolError::SandboxViolation(_)));
    }

    #[test]
    fn ignored_names() {
        assert!(is_ignored_dir_name(".git"));
        assert!(is_ignored_dir_name("node_modules"));
        assert!(is_ignored_dir_name("target"));
        assert!(!is_ignored_dir_name("src"));
        assert!(!is_ignored_dir_name(".github"));
    }

    #[test]
    fn posix_relative_skips_root() {
        let root = PathBuf::from("/workspace");
        let rel = to_posix_relative(&root, &PathBuf::from("/workspace/src/lib.rs"));
        assert_eq!(rel, "src/lib.rs");
    }
}