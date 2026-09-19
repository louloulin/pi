//! Package installation, removal, and extension wiring.
//!
//! Layout (all under the pi root — `~/.pi`, overridable with `PI_HOME`
//! or `--dir`):
//!
//! ```text
//! <root>/packages.json                 registry
//! <root>/packages/<dir-name>/          installed package
//! <root>/agent/extensions/<dir-name>/  per-file links the loader discovers
//! ```
//!
//! Remote sources (`npm:`, `git:`, `https:`) go through the
//! [`PackageFetcher`] trait so tests can inject an in-memory fetcher and
//! never touch the network. [`RealFetcher`] shells out to `npm` / `git`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

use super::registry::{PackageEntry, Registry, RegistryError};
use super::spec::{PackageSpec, SpecError};

/// Sub-directory holding installed packages.
pub const PACKAGES_DIR: &str = "packages";
/// Sub-directory the extension loader searches (`~/.pi/agent/extensions`).
pub const EXTENSIONS_DIR: &str = "agent/extensions";

const EXTENSION_EXTS: [&str; 4] = ["ts", "js", "mjs", "wasm"];

/// Errors raised by install / remove.
#[derive(Debug, Error)]
pub enum InstallError {
    /// The package spec could not be parsed.
    #[error(transparent)]
    Spec(#[from] SpecError),
    /// A local path did not exist.
    #[error("path does not exist: {0}")]
    NotFound(String),
    /// The source type is known but not supported in this build.
    #[error("unsupported package source: {0}")]
    Unsupported(String),
    /// A required external tool was not available (offline / missing
    /// binary).
    #[error("{0}")]
    Unavailable(String),
    /// An external fetch command failed.
    #[error("{0}")]
    Fetch(String),
    /// The registry could not be read or written.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// A filesystem operation failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl InstallError {
    /// `sysexits.h`-style exit code for the CLI.
    pub fn exit_code(&self) -> u8 {
        match self {
            InstallError::Spec(_) | InstallError::Unsupported(_) => 64, // EX_USAGE
            InstallError::NotFound(_) => 66,                            // EX_NOINPUT
            InstallError::Unavailable(_) | InstallError::Fetch(_) => 69, // EX_UNAVAILABLE
            InstallError::Registry(_) | InstallError::Io(_) => 74,      // EX_IOERR
        }
    }
}

/// Strategy for materializing a remote package under the install dir.
///
/// Implementations must create `dest` and return the package root inside
/// it (usually `dest` itself).
pub trait PackageFetcher: Send + Sync {
    /// Materialize `spec` under `dest` and return the package root.
    fn fetch(&self, spec: &PackageSpec, dest: &Path) -> Result<PathBuf, InstallError>;
}

/// Resolve the pi root (`~/.pi` by default).
///
/// Precedence: explicit `--dir` → `PI_HOME` → `$HOME/.pi` → `./.pi`.
pub fn resolve_root(explicit: Option<&Path>) -> PathBuf {
    if let Some(dir) = explicit {
        return dir.to_path_buf();
    }
    if let Some(value) = std::env::var_os("PI_HOME") {
        if !value.is_empty() {
            return PathBuf::from(value);
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".pi")
}

/// Install (or reinstall) `raw_spec` into `root`, updating the registry.
///
/// Reinstalling the same spec is idempotent: the previous install
/// directory and registry entry are replaced rather than appended.
pub fn install(
    root: &Path,
    cwd: &Path,
    raw_spec: &str,
    fetcher: &dyn PackageFetcher,
) -> Result<PackageEntry, InstallError> {
    let spec = PackageSpec::parse(raw_spec)?;
    let dir_name = spec.install_dir_name();
    let dest = root.join(PACKAGES_DIR).join(&dir_name);
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    fs::create_dir_all(&dest)?;

    let package_root = match &spec {
        PackageSpec::File { path } => {
            let source = resolve_local(cwd, path)?;
            materialize_local(&source, &dest)?
        }
        _ => {
            let fetched = fetcher.fetch(&spec, &dest)?;
            if fetched != dest {
                fs::create_dir_all(&dest)?;
            }
            fetched
        }
    };

    let package_root = fs::canonicalize(&package_root).unwrap_or(package_root);
    let extensions = collect_extensions(&package_root);
    let published = publish_extensions(root, &dir_name, &extensions)?;

    let entry = PackageEntry {
        spec: raw_spec.to_string(),
        name: spec.name(),
        resolved: package_root.display().to_string(),
        installed_at: chrono::Utc::now().to_rfc3339(),
        extensions: published
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
    };

    let mut registry = Registry::load(root)?;
    registry.upsert(entry.clone());
    registry.save()?;
    Ok(entry)
}

/// Remove a package by spec, name, or install directory name.
///
/// Returns `Ok(false)` when nothing matched.
pub fn remove(root: &Path, query: &str) -> Result<bool, InstallError> {
    let mut registry = Registry::load(root)?;
    let Some(entry) = registry.remove(query) else {
        return Ok(false);
    };

    let dir_name = Path::new(&entry.resolved)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| sanitize_dir_name(&entry.name));
    let install_dir = root.join(PACKAGES_DIR).join(&dir_name);
    if install_dir.is_dir() {
        fs::remove_dir_all(&install_dir)?;
    }
    let extension_dir = root.join(EXTENSIONS_DIR).join(&dir_name);
    if extension_dir.is_dir() {
        fs::remove_dir_all(&extension_dir)?;
    }

    registry.save()?;
    Ok(true)
}

/// Materialize a local file / directory into `dest` and return the
/// package root.
fn materialize_local(source: &Path, dest: &Path) -> Result<PathBuf, InstallError> {
    if source.is_dir() {
        copy_dir_recursive(source, dest)?;
        Ok(dest.to_path_buf())
    } else if source.is_file() {
        let file_name = source
            .file_name()
            .ok_or_else(|| InstallError::NotFound(source.display().to_string()))?;
        fs::copy(source, dest.join(file_name))?;
        Ok(dest.to_path_buf())
    } else {
        Err(InstallError::NotFound(source.display().to_string()))
    }
}

fn resolve_local(cwd: &Path, path: &Path) -> Result<PathBuf, InstallError> {
    let expanded = if let Some(raw) = path.to_str().and_then(|s| s.strip_prefix("~/")) {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(raw)
    } else {
        path.to_path_buf()
    };
    let resolved = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    };
    if resolved.exists() {
        Ok(resolved)
    } else {
        Err(InstallError::NotFound(resolved.display().to_string()))
    }
}

/// Copy a directory tree, creating `dest` if needed.
pub fn copy_dir_recursive(source: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in walkdir::WalkDir::new(source) {
        let entry = entry.map_err(io::Error::other)?;
        let relative = entry
            .path()
            .strip_prefix(source)
            .unwrap_or_else(|_| Path::new(""));
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = dest.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Discover extension files inside a package.
///
/// Mirrors the conventional-directory rule from `docs/packages.md`:
/// `extensions/` holds `.ts` / `.js` files (recursively, one level deep
/// to match the loader's `max_depth(2)`). A single-file package is its
/// own extension.
pub fn collect_extensions(package_root: &Path) -> Vec<PathBuf> {
    if package_root.is_file() {
        return if is_extension(package_root) {
            vec![package_root.to_path_buf()]
        } else {
            Vec::new()
        };
    }
    let extensions_dir = package_root.join("extensions");
    if !extensions_dir.is_dir() {
        return Vec::new();
    }
    let mut found = Vec::new();
    for entry in walkdir::WalkDir::new(&extensions_dir)
        .max_depth(2)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if entry.file_type().is_file() && is_extension(path) {
            found.push(path.to_path_buf());
        }
    }
    found.sort();
    found
}

/// Link (or copy) package extensions into the loader search path.
///
/// Symlinks keep relative imports inside the package working; platforms
/// without symlink support fall back to a copy.
fn publish_extensions(
    root: &Path,
    dir_name: &str,
    extensions: &[PathBuf],
) -> Result<Vec<PathBuf>, InstallError> {
    if extensions.is_empty() {
        return Ok(Vec::new());
    }
    let target_dir = root.join(EXTENSIONS_DIR).join(dir_name);
    if target_dir.exists() {
        fs::remove_dir_all(&target_dir)?;
    }
    fs::create_dir_all(&target_dir)?;

    let mut published = Vec::with_capacity(extensions.len());
    for (index, source) in extensions.iter().enumerate() {
        let file_name = source
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("extension.js");
        let file_name = if index == 0 {
            file_name.to_string()
        } else {
            format!("{index}-{file_name}")
        };
        let target = target_dir.join(file_name);
        link_or_copy(source, &target)?;
        published.push(target);
    }
    Ok(published)
}

fn is_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|ext| EXTENSION_EXTS.contains(&ext))
        .unwrap_or(false)
}

fn sanitize_dir_name(name: &str) -> String {
    super::spec::sanitize_segment(name)
}

#[cfg(unix)]
fn link_or_copy(source: &Path, target: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(source, target)
}

#[cfg(not(unix))]
fn link_or_copy(source: &Path, target: &Path) -> io::Result<()> {
    fs::copy(source, target).map(|_| ())
}

/// Real fetcher — shells out to `npm` and `git`.
///
/// No HTTP client is used: npm tarballs are installed through the `npm`
/// binary and git sources through `git clone`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFetcher;

impl PackageFetcher for RealFetcher {
    fn fetch(&self, spec: &PackageSpec, dest: &Path) -> Result<PathBuf, InstallError> {
        match spec {
            PackageSpec::Npm { spec, name, .. } => fetch_npm(spec, name, dest),
            PackageSpec::Git {
                url, reference, ..
            } => fetch_git(url, reference.as_deref(), dest),
            PackageSpec::Https {
                url, reference, ..
            } => {
                if looks_like_archive(url) {
                    return Err(InstallError::Unsupported(format!(
                        "https tarball installs are not supported yet ({url}); use `git:` or a local path"
                    )));
                }
                fetch_git(url, reference.as_deref(), dest)
            }
            PackageSpec::File { path } => Err(InstallError::Unsupported(format!(
                "local path {} does not go through the fetcher",
                path.display()
            ))),
        }
    }
}

fn fetch_npm(spec: &str, name: &str, dest: &Path) -> Result<PathBuf, InstallError> {
    let staging = dest.with_extension(format!("npm-staging-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;

    let result = run_command(
        "npm",
        &[
            "install",
            "--prefix",
            staging.to_string_lossy().as_ref(),
            "--no-save",
            "--no-audit",
            "--no-fund",
            "--loglevel=error",
            spec,
        ],
        None,
    );
    if let Err(err) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(err);
    }

    let package_dir = staging.join("node_modules").join(name);
    if !package_dir.is_dir() {
        let _ = fs::remove_dir_all(&staging);
        return Err(InstallError::Fetch(format!(
            "npm did not produce a package at {name}"
        )));
    }
    copy_dir_recursive(&package_dir, dest)?;
    let _ = fs::remove_dir_all(&staging);
    Ok(dest.to_path_buf())
}

fn fetch_git(url: &str, reference: Option<&str>, dest: &Path) -> Result<PathBuf, InstallError> {
    run_command("git", &["clone", url, dest.to_string_lossy().as_ref()], None)?;
    if let Some(reference) = reference {
        run_command("git", &["checkout", reference], Some(dest))?;
    }
    Ok(dest.to_path_buf())
}

fn run_command(command: &str, args: &[&str], cwd: Option<&Path>) -> Result<(), InstallError> {
    let mut cmd = Command::new(command);
    cmd.args(args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    // Fail fast rather than blocking on an interactive credential prompt.
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let output = match cmd.output() {
        Ok(output) => output,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(InstallError::Unavailable(format!(
                "`{command}` is not available on PATH (required to install `{command}:` packages)"
            )))
        }
        Err(err) => return Err(InstallError::Io(err)),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.lines().last().unwrap_or("").trim();
        let detail = if detail.is_empty() {
            format!("exit code {:?}", output.status.code())
        } else {
            detail.to_string()
        };
        return Err(InstallError::Unavailable(format!(
            "`{command} {}` failed: {detail}",
            args.join(" ")
        )));
    }
    Ok(())
}

fn looks_like_archive(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    [".tar.gz", ".tgz", ".tar", ".zip", ".tar.bz2", ".tar.xz"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}
