//! Package spec parsing — mirrors `parseSource` in
//! `packages/coding-agent/src/core/package-manager.ts` and the source
//! syntax documented in `packages/coding-agent/docs/packages.md`.
//!
//! Supported forms:
//!
//! | Form | Example |
//! | ---- | ------- |
//! | npm | `npm:@scope/pkg@1.2.3`, `npm:pkg` |
//! | git | `git:github.com/user/repo@v1`, `git:git@github.com:user/repo`, `git:https://host/user/repo#ref` |
//! | https | `https://github.com/user/repo`, `https://github.com/user/repo@v1` |
//! | local | `file:./pkg`, `./pkg`, `../pkg`, `/abs/pkg` |
//!
//! Anything else is rejected with [`SpecError::Unsupported`] so the CLI
//! can exit with `EX_USAGE` (64).

use std::path::PathBuf;

use thiserror::Error;

/// Error returned when a package spec cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SpecError {
    /// The spec was empty (or only whitespace).
    #[error("empty package spec")]
    Empty,
    /// An `npm:` spec was malformed.
    #[error("invalid npm spec `{0}`")]
    Npm(String),
    /// A `git:` spec was malformed.
    #[error("invalid git spec `{0}`")]
    Git(String),
    /// An `https://` / `http://` spec was malformed.
    #[error("invalid https spec `{0}`")]
    Https(String),
    /// The spec did not match any supported source type.
    #[error(
        "unsupported package spec `{0}` (expected npm:, git:, file:, https://, or a local path like ./pkg)"
    )]
    Unsupported(String),
}

/// A parsed package source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageSpec {
    /// `npm:<name>[@<version>]`.
    Npm {
        /// The raw spec after the `npm:` prefix.
        spec: String,
        /// The package name (`@scope/pkg` or `pkg`).
        name: String,
        /// The requested version / range, when present.
        version: Option<String>,
    },
    /// `git:<url>[#ref]` (shorthand or protocol URL).
    Git {
        /// URL passed to `git clone`.
        url: String,
        /// Host portion (used for the install layout).
        host: String,
        /// Repository path portion.
        path: String,
        /// Pinned ref (tag / branch / commit), when present.
        reference: Option<String>,
    },
    /// A bare `https://` / `http://` URL (git repo or tarball).
    Https {
        /// URL to fetch.
        url: String,
        /// Host portion.
        host: String,
        /// Path portion.
        path: String,
        /// Pinned ref, when present.
        reference: Option<String>,
    },
    /// A local file or directory.
    File {
        /// Path exactly as written in the spec (may be relative).
        path: PathBuf,
    },
}

impl PackageSpec {
    /// Parse a raw spec string.
    pub fn parse(raw: &str) -> Result<Self, SpecError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(SpecError::Empty);
        }

        if let Some(rest) = trimmed.strip_prefix("npm:") {
            return parse_npm(rest, trimmed);
        }
        if let Some(rest) = trimmed.strip_prefix("file:") {
            if rest.trim().is_empty() {
                return Err(SpecError::Unsupported(trimmed.to_string()));
            }
            return Ok(PackageSpec::File {
                path: PathBuf::from(rest.trim()),
            });
        }
        if let Some(rest) = trimmed.strip_prefix("git:") {
            return parse_git(rest, trimmed);
        }
        if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
            return parse_https(trimmed);
        }
        if looks_like_local_path(trimmed) {
            return Ok(PackageSpec::File {
                path: PathBuf::from(trimmed),
            });
        }
        Err(SpecError::Unsupported(trimmed.to_string()))
    }

    /// Human-readable package name. Used by `pi list` and as the
    /// registry identity.
    pub fn name(&self) -> String {
        match self {
            PackageSpec::Npm { name, .. } => name.clone(),
            PackageSpec::Git { host, path, .. } => format!("{host}/{path}"),
            PackageSpec::Https { host, path, .. } => {
                let trimmed = path.trim_end_matches(".git");
                if trimmed.is_empty() {
                    host.clone()
                } else {
                    format!("{host}/{trimmed}")
                }
            }
            PackageSpec::File { path } => path
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("package")
                .to_string(),
        }
    }

    /// Filesystem-safe directory name under `<root>/packages/`.
    pub fn install_dir_name(&self) -> String {
        sanitize_segment(&self.name())
    }

    /// The source string as it should be echoed by `pi list`.
    pub fn display(&self) -> String {
        match self {
            PackageSpec::Npm { spec, .. } => format!("npm:{spec}"),
            PackageSpec::Git { url, reference, .. } => match reference {
                Some(reference) => format!("git:{url}#{reference}"),
                None => format!("git:{url}"),
            },
            PackageSpec::Https { url, reference, .. } => match reference {
                Some(reference) => format!("{url}#{reference}"),
                None => url.clone(),
            },
            PackageSpec::File { path } => path.display().to_string(),
        }
    }
}

/// Whether `path` looks like a local filesystem path rather than a
/// remote source. Bare names are *not* treated as local paths (the TS
/// CLI requires `./`), so `pi install some-fixture` stays ambiguous and
/// is rejected.
fn looks_like_local_path(s: &str) -> bool {
    s == "."
        || s == ".."
        || s.starts_with("./")
        || s.starts_with("../")
        || s.starts_with('/')
        || s.starts_with('~')
        || s.starts_with(".\\")
        || s.starts_with("..\\")
        || (s.len() > 2 && s.as_bytes()[1] == b':')
}

fn parse_npm(rest: &str, raw: &str) -> Result<PackageSpec, SpecError> {
    let spec = rest.trim();
    if spec.is_empty() {
        return Err(SpecError::Npm(raw.to_string()));
    }

    // Scoped names start with `@`, so the version separator is the
    // *second* `@` for `@scope/pkg@1.0.0`.
    let (name, version) = if let Some(after_at) = spec.strip_prefix('@') {
        match after_at.find('@') {
            Some(idx) => (
                format!("@{}", &after_at[..idx]),
                Some(after_at[idx + 1..].to_string()),
            ),
            None => (format!("@{after_at}"), None),
        }
    } else {
        match spec.find('@') {
            Some(idx) => (spec[..idx].to_string(), Some(spec[idx + 1..].to_string())),
            None => (spec.to_string(), None),
        }
    };

    if name.is_empty() || name.contains(char::is_whitespace) {
        return Err(SpecError::Npm(raw.to_string()));
    }
    if let Some(version) = &version {
        if version.is_empty() {
            return Err(SpecError::Npm(raw.to_string()));
        }
    }

    Ok(PackageSpec::Npm {
        spec: spec.to_string(),
        name,
        version,
    })
}

fn parse_git(rest: &str, raw: &str) -> Result<PackageSpec, SpecError> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return Err(SpecError::Git(raw.to_string()));
    }
    let (base, reference) = split_reference(trimmed);
    let (host, path) = git_host_path(&base).ok_or_else(|| SpecError::Git(raw.to_string()))?;
    let url = git_clone_url(&base);
    Ok(PackageSpec::Git {
        url,
        host,
        path,
        reference,
    })
}

fn parse_https(raw: &str) -> Result<PackageSpec, SpecError> {
    let (base, reference) = split_reference(raw);
    let after_scheme = base
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base.as_str());
    let host_path = after_scheme
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(after_scheme);
    let (host, path) = host_path
        .split_once('/')
        .ok_or_else(|| SpecError::Https(raw.to_string()))?;
    if host.is_empty() || path.is_empty() {
        return Err(SpecError::Https(raw.to_string()));
    }
    let host = host.to_string();
    let path = path.to_string();
    Ok(PackageSpec::Https {
        url: base,
        host,
        path,
        reference,
    })
}

/// Split `url#ref` or `url@ref` into `(url, Some(ref))`.
///
/// The `@` separator must come after the last `/` so that userinfo
/// (`https://user@host/path`) and the `git@host:path` shorthand are not
/// mistaken for a ref.
fn split_reference(input: &str) -> (String, Option<String>) {
    if let Some(idx) = input.find('#') {
        if idx > 0 && idx + 1 < input.len() {
            return (input[..idx].to_string(), Some(input[idx + 1..].to_string()));
        }
    }
    let last_slash = input.rfind('/');
    if let Some(idx) = input.rfind('@') {
        let is_git_shorthand = idx == 3 && input.starts_with("git@");
        let after_last_slash = match last_slash {
            Some(slash) => idx > slash,
            None => true,
        };
        if !is_git_shorthand && after_last_slash && idx > 0 && idx + 1 < input.len() {
            return (input[..idx].to_string(), Some(input[idx + 1..].to_string()));
        }
    }
    (input.to_string(), None)
}

/// Normalize the URL passed to `git clone` (protocol URLs pass through,
/// shorthands are prefixed with `https://`).
fn git_clone_url(base: &str) -> String {
    if base.contains("://") || base.starts_with("git@") {
        base.to_string()
    } else {
        format!("https://{base}")
    }
}

fn git_host_path(base: &str) -> Option<(String, String)> {
    if let Some((_, after_scheme)) = base.split_once("://") {
        let host_path = after_scheme
            .rsplit_once('@')
            .map(|(_, h)| h)
            .unwrap_or(after_scheme);
        let (host, path) = host_path.split_once('/')?;
        if host.is_empty() || path.is_empty() {
            return None;
        }
        return Some((host.to_string(), path.to_string()));
    }
    if let Some(rest) = base.strip_prefix("git@") {
        let (host, path) = rest
            .split_once(':')
            .or_else(|| rest.split_once('/'))
            .filter(|(h, p)| !h.is_empty() && !p.is_empty())?;
        return Some((host.to_string(), path.to_string()));
    }
    let (host, path) = base.split_once('/')?;
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some((host.to_string(), path.to_string()))
}

pub(crate) fn sanitize_segment(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('-'),
            _ => out.push(ch),
        }
    }
    let trimmed = out.trim_matches(['-', '.', ' ']);
    if trimmed.is_empty() {
        "package".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_npm_spec() {
        let spec = PackageSpec::parse("npm:pkg").unwrap();
        assert_eq!(
            spec,
            PackageSpec::Npm {
                spec: "pkg".into(),
                name: "pkg".into(),
                version: None
            }
        );
    }

    #[test]
    fn parses_versioned_scoped_npm_spec() {
        let spec = PackageSpec::parse("npm:@foo/bar@1.0.0").unwrap();
        assert_eq!(spec.name(), "@foo/bar");
        match spec {
            PackageSpec::Npm { version, .. } => assert_eq!(version.as_deref(), Some("1.0.0")),
            other => panic!("unexpected spec: {other:?}"),
        }
    }

    #[test]
    fn parses_git_shorthand_with_ref() {
        let spec = PackageSpec::parse("git:github.com/user/repo@v1").unwrap();
        match &spec {
            PackageSpec::Git {
                url,
                host,
                path,
                reference,
            } => {
                assert_eq!(url, "https://github.com/user/repo");
                assert_eq!(host, "github.com");
                assert_eq!(path, "user/repo");
                assert_eq!(reference.as_deref(), Some("v1"));
            }
            other => panic!("unexpected spec: {other:?}"),
        }
        assert_eq!(spec.install_dir_name(), "github.com-user-repo");
    }

    #[test]
    fn parses_git_ssh_shorthand_without_ref() {
        let spec = PackageSpec::parse("git:git@github.com:user/repo").unwrap();
        match &spec {
            PackageSpec::Git { url, reference, .. } => {
                assert_eq!(url, "git@github.com:user/repo");
                assert_eq!(*reference, None);
            }
            other => panic!("unexpected spec: {other:?}"),
        }
    }

    #[test]
    fn parses_git_hash_ref() {
        let spec = PackageSpec::parse("git:https://example.com/u/r#main").unwrap();
        match &spec {
            PackageSpec::Git { reference, .. } => assert_eq!(reference.as_deref(), Some("main")),
            other => panic!("unexpected spec: {other:?}"),
        }
    }

    #[test]
    fn parses_https_with_ref() {
        let spec = PackageSpec::parse("https://github.com/user/repo@v2").unwrap();
        match &spec {
            PackageSpec::Https { url, reference, .. } => {
                assert_eq!(url, "https://github.com/user/repo");
                assert_eq!(reference.as_deref(), Some("v2"));
            }
            other => panic!("unexpected spec: {other:?}"),
        }
    }

    #[test]
    fn parses_local_and_file_specs() {
        assert_eq!(
            PackageSpec::parse("./fixture").unwrap(),
            PackageSpec::File {
                path: PathBuf::from("./fixture")
            }
        );
        assert_eq!(
            PackageSpec::parse("file:/abs/pkg").unwrap(),
            PackageSpec::File {
                path: PathBuf::from("/abs/pkg")
            }
        );
    }

    #[test]
    fn rejects_bare_and_unknown_specs() {
        assert!(matches!(PackageSpec::parse(""), Err(SpecError::Empty)));
        assert!(matches!(
            PackageSpec::parse("some-fixture"),
            Err(SpecError::Unsupported(_))
        ));
        assert!(matches!(PackageSpec::parse("npm:"), Err(SpecError::Npm(_))));
        assert!(matches!(PackageSpec::parse("git:"), Err(SpecError::Git(_))));
    }
}
