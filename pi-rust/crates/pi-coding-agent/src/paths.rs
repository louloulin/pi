//! Path helpers shared by the resource loaders.
//!
//! Rust port of the parts of `utils/paths.ts` and `config.ts` the skill /
//! context-file loaders need. Everything here is lexical (no globbing, no
//! filesystem walks) so the helpers are cheap and testable.

use std::path::{Component, Path, PathBuf};

/// The config directory pi keeps its project-local state in (`.pi`).
pub const CONFIG_DIR_NAME: &str = ".pi";

/// The user's home directory (`$HOME`, else `%USERPROFILE%`).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

/// The global agent config directory (`~/.pi/agent`).
pub fn agent_dir() -> Option<PathBuf> {
    home_dir().map(|home| home.join(CONFIG_DIR_NAME).join("agent"))
}

/// The global agent config directory, falling back to `./.pi/agent` when
/// no home directory is known.
pub fn agent_dir_or_default() -> PathBuf {
    agent_dir().unwrap_or_else(|| PathBuf::from(CONFIG_DIR_NAME).join("agent"))
}

/// Collapse `.` / `..` components without touching the filesystem.
pub fn normalize_lexically(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() {
                    result.push("..");
                }
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

/// Resolve `path` against the process working directory when relative,
/// then normalise it lexically.
pub fn absolute(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    normalize_lexically(&joined)
}

/// Resolve `path` against `base` when relative (upstream `resolvePath`).
pub fn resolve_against(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize_lexically(path)
    } else {
        normalize_lexically(&base.join(path))
    }
}

/// Expand a leading `~` / `~/` to the home directory.
///
/// Falls back to `base` when no home directory is known so the caller
/// still produces a path it can report.
pub fn expand_tilde(path: &Path, base: &Path) -> PathBuf {
    let Some(raw) = path.to_str() else {
        return path.to_path_buf();
    };

    if raw == "~" {
        return home_dir().unwrap_or_else(|| base.to_path_buf());
    }

    let Some(rest) = raw.strip_prefix("~/") else {
        return path.to_path_buf();
    };

    match home_dir() {
        Some(home) => home.join(rest),
        None => base.join(rest),
    }
}

/// Real path of `path`, falling back to a lexical absolute path when the
/// target does not exist (`canonicalizePath` on the TS side).
pub fn canonicalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| absolute(path))
}

/// Strip a leading UTF-8 BOM.
pub fn strip_bom(content: &str) -> &str {
    content.strip_prefix('\u{feff}').unwrap_or(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_lexically() {
        assert_eq!(
            normalize_lexically(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
        assert_eq!(
            normalize_lexically(Path::new("a/./b")),
            PathBuf::from("a/b")
        );
    }

    #[test]
    fn resolves_relative_paths_against_a_base() {
        assert_eq!(
            resolve_against(Path::new("/base"), Path::new("skills")),
            PathBuf::from("/base/skills")
        );
        assert_eq!(
            resolve_against(Path::new("/base"), Path::new("/abs")),
            PathBuf::from("/abs")
        );
    }

    #[test]
    fn expands_tilde_and_keeps_other_paths() {
        if let Some(home) = home_dir() {
            assert_eq!(
                expand_tilde(Path::new("~/.pi/agent"), Path::new("/fallback")),
                home.join(".pi/agent")
            );
        }
        assert_eq!(
            expand_tilde(Path::new("/abs/path"), Path::new("/fallback")),
            PathBuf::from("/abs/path")
        );
    }

    #[test]
    fn strips_a_leading_bom() {
        assert_eq!(strip_bom("\u{feff}hello"), "hello");
        assert_eq!(strip_bom("hello"), "hello");
    }

    #[test]
    fn absolute_paths_stay_absolute() {
        assert_eq!(absolute(Path::new("/a/b")), PathBuf::from("/a/b"));
    }
}
