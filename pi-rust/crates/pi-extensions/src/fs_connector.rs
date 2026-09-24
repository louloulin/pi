//! Filesystem connector.
//!
//! Wraps the seven `host_fs_*` imports the JS shim exposes to
//! extensions. The connector is **the only path** by which an
//! extension touches the host filesystem — the shim must call into
//! this module instead of opening files directly so the
//! canonicalisation + root scope + atomic-write rules below are
//! uniformly enforced.
//!
//! ## Safety properties
//!
//! 1. **No-follow** by default. Symlinks are resolved only when the
//!    caller explicitly opted in via
//!    [`CapabilityScope::follow_symlinks`]. This prevents a symlink
//!    under `/tmp/extensions` from pointing outside the allowlisted
//!    roots.
//! 2. **Path canonicalisation** strips the Windows `\\?\` UNC prefix
//!    so two paths that refer to the same file compare equal.
//! 3. **Root scope** is enforced after canonicalisation: every
//!    resolved path must lie under at least one of the configured
//!    `roots`. Requests outside the roots return `FsConnectorError::OutsideScope`
//!    before any syscall runs.
//! 4. **Atomic write**: writes go to `<path>.tmp-<pid>-<nanos>`,
//!    fsync, then rename to the target. A crash never leaves a
//!    half-written file under the target name.
//! 5. **Write cap** ([`CapabilityScope::max_write_bytes`]) caps a
//!    single `host_fs_write` call; the global cap applies when no
//!    per-extension cap is set.
//!
//! ## Port scope
//!
//! The port is simplified from
//! `pi_agent_rust/src/extensions/fs_connector.rs` (1306 lines):
//!
//! - No two-phase handshake / `pi.fs.readlink`.
//! - No Windows ACL handling — POSIX permissions only.
//! - No xattr / ACL preservation on rename.
//! - No watcher integration (`pi.fs.watch`) — the connector is
//!   stateless. The shim can layer a watcher on top later.
//!
//! ## Atomic write algorithm
//!
//! ```text
//! create tmp = <path>.tmp-<pid>-<nanos>
//! write(tmp, data)
//! fsync(tmp)
//! rename(tmp, path)             ← atomic on POSIX
//! ```
//!
//! `rename` is the only syscall that can break a reader's view on
//! POSIX — concurrent `host_fs_read` either sees the old content or
//! the new one, never half. On Windows we'd need
//! `MoveFileEx(MOVEFILE_REPLACE_EXISTING)`; we detect the platform
//! via `cfg!(windows)` and refuse atomic writes there with a typed
//! error rather than silently degrading.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use thiserror::Error;

use crate::capability_manifest::CapabilityScope;

/// Filesystem connector errors. Surfaced to the JS shim as a typed
/// rejection; the JS side falls back to a single failure type so the
/// extension sees `host_fs_write → rejected`.
#[derive(Debug, Error)]
pub enum FsConnectorError {
    #[error("path `{path}` is outside the configured scope (roots={roots:?})")]
    OutsideScope {
        path: PathBuf,
        roots: Vec<PathBuf>,
    },
    #[error("path `{path}` traverses scope via a symlink (follow_symlinks=false)")]
    SymlinkTraversal { path: PathBuf },
    #[error("path `{path}` could not be canonicalised: {source}")]
    Canonicalize {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("write of {actual} bytes to `{path}` exceeded cap {cap} bytes")]
    WriteTooLarge { path: PathBuf, actual: u64, cap: u64 },
    #[error("host_fs_read: open `{path}`: {source}")]
    ReadOpen {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("host_fs_read: read `{path}`: {source}")]
    ReadIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("host_fs_write: rename tmp -> `{path}`: {source}")]
    Rename {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("host_fs_write: tmp file `{tmp}`: {source}")]
    WriteTmp {
        tmp: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("host_fs_list: read_dir `{path}`: {source}")]
    ReadDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("host_fs_stat: metadata `{path}`: {source}")]
    Stat {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("host_fs_mkdir `{path}`: {source}")]
    Mkdir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("host_fs_delete `{path}`: {source}")]
    Delete {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("atomic write is not supported on Windows in this build")]
    AtomicUnsupported,
}

/// Connector configuration. Created once per extension load; the JS
/// shim hands the scope in directly so the connector does not have to
/// re-parse the manifest.
#[derive(Debug, Clone)]
pub struct FsConnectorConfig {
    /// Per-extension scope (roots, follow_symlinks, max_write_bytes).
    pub scope: CapabilityScope,
    /// Global cap applied when the scope's `max_write_bytes` is
    /// `None`. Defaults to 16 MiB.
    pub global_max_write_bytes: u64,
}

impl Default for FsConnectorConfig {
    fn default() -> Self {
        Self {
            scope: CapabilityScope::default(),
            global_max_write_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Filesystem connector. Cheap to clone (`Arc` inside).
#[derive(Debug, Clone)]
pub struct FsConnector {
    inner: Arc<FsConnectorInner>,
}

#[derive(Debug)]
struct FsConnectorInner {
    config: Mutex<FsConnectorConfig>,
}

impl FsConnector {
    pub fn new(config: FsConnectorConfig) -> Self {
        Self {
            inner: Arc::new(FsConnectorInner {
                config: Mutex::new(config),
            }),
        }
    }

    pub fn with_scope(scope: CapabilityScope) -> Self {
        Self::new(FsConnectorConfig {
            scope,
            global_max_write_bytes: 16 * 1024 * 1024,
        })
    }

    /// Replace the configuration. Used by the host when the
    /// capability manifest changes (it shouldn't, but the wiring is
    /// there).
    pub fn set_config(&self, config: FsConnectorConfig) {
        *self.inner.config.lock() = config;
    }

    pub fn config(&self) -> FsConnectorConfig {
        self.inner.config.lock().clone()
    }

    pub fn read(&self, path: &Path) -> Result<Vec<u8>, FsConnectorError> {
        let resolved = self.resolve(path)?;
        let mut file = File::open(&resolved).map_err(|source| FsConnectorError::ReadOpen {
            path: resolved.clone(),
            source,
        })?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .map_err(|source| FsConnectorError::ReadIo {
                path: resolved,
                source,
            })?;
        Ok(buf)
    }

    pub fn write(&self, path: &Path, data: &[u8]) -> Result<(), FsConnectorError> {
        let resolved = self.resolve(path)?;
        let cfg = self.inner.config.lock().clone();
        let cap = cfg.scope.max_write_bytes.unwrap_or(cfg.global_max_write_bytes);
        if data.len() as u64 > cap {
            return Err(FsConnectorError::WriteTooLarge {
                path: resolved,
                actual: data.len() as u64,
                cap,
            });
        }
        atomic_write(&resolved, data)
    }

    pub fn list(&self, path: &Path) -> Result<Vec<FsEntry>, FsConnectorError> {
        let resolved = self.resolve(path)?;
        let entries = fs::read_dir(&resolved).map_err(|source| FsConnectorError::ReadDir {
            path: resolved.clone(),
            source,
        })?;
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| FsConnectorError::ReadDir {
                path: resolved.clone(),
                source,
            })?;
            let meta = entry
                .metadata()
                .map_err(|source| FsConnectorError::Stat {
                    path: resolved.clone(),
                    source,
                })?;
            out.push(FsEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_file: meta.is_file(),
                is_dir: meta.is_dir(),
                is_symlink: meta.file_type().is_symlink(),
                size: meta.len(),
            });
        }
        Ok(out)
    }

    pub fn stat(&self, path: &Path) -> Result<FsStat, FsConnectorError> {
        let resolved = self.resolve(path)?;
        let meta = fs::symlink_metadata(&resolved).map_err(|source| FsConnectorError::Stat {
            path: resolved.clone(),
            source,
        })?;
        Ok(FsStat {
            is_file: meta.is_file(),
            is_dir: meta.is_dir(),
            is_symlink: meta.file_type().is_symlink(),
            size: meta.len(),
        })
    }

    pub fn mkdir(&self, path: &Path) -> Result<(), FsConnectorError> {
        let resolved = self.resolve(path)?;
        fs::create_dir_all(&resolved).map_err(|source| FsConnectorError::Mkdir {
            path: resolved,
            source,
        })
    }

    pub fn delete(&self, path: &Path) -> Result<(), FsConnectorError> {
        let resolved = self.resolve(path)?;
        let meta = fs::symlink_metadata(&resolved).map_err(|source| FsConnectorError::Delete {
            path: resolved.clone(),
            source,
        })?;
        if meta.is_dir() {
            fs::remove_dir_all(&resolved).map_err(|source| FsConnectorError::Delete {
                path: resolved,
                source,
            })
        } else {
            fs::remove_file(&resolved).map_err(|source| FsConnectorError::Delete {
                path: resolved,
                source,
            })
        }
    }

    /// Canonicalise a path and check it lives inside one of the
    /// configured roots. Symlinks are resolved only when
    /// `follow_symlinks` is set.
    pub fn resolve(&self, path: &Path) -> Result<PathBuf, FsConnectorError> {
        let cfg = self.inner.config.lock().clone();
        let stripped = strip_windows_unc(path);
        // Reject any path that contains a `..` component before we
        // resolve it. `strip_dangerous_components` would drop `..`
        // segments, letting `allowed/../etc/passwd` silently resolve
        // to `allowed/etc/passwd` — that hides intent. Explicit
        // denial on the original path is safer.
        if contains_parent_dir(&stripped) {
            return Err(FsConnectorError::OutsideScope {
                path: stripped,
                roots: cfg.scope.roots.clone(),
            });
        }
        let safe = strip_dangerous_components(&stripped);
        if !cfg.scope.follow_symlinks {
            // Resolve to an absolute, canonical form WITHOUT following
            // the final symlink. We use the parent dir's canonical
            // form and join the basename. The symlink check on the
            // candidate MUST happen before canonicalize, because
            // canonicalize resolves the symlink to its target —
            // which may live outside the scope and would surface as
            // `OutsideScope` (a correct denial, but a less
            // diagnostic one than `SymlinkTraversal`).
            let parent = safe.parent().unwrap_or(Path::new("."));
            let file_name = safe.file_name().ok_or_else(|| FsConnectorError::OutsideScope {
                path: safe.clone(),
                roots: cfg.scope.roots.clone(),
            })?;
            let canon_parent = parent.canonicalize().map_err(|source| FsConnectorError::Canonicalize {
                path: parent.to_path_buf(),
                source,
            })?;
            let candidate = canon_parent.join(file_name);
            if candidate.is_symlink() {
                return Err(FsConnectorError::SymlinkTraversal { path: candidate });
            }
            ensure_inside_scope(&candidate, &cfg.scope.roots)?;
            Ok(candidate)
        } else {
            let canonical = safe.canonicalize().map_err(|source| FsConnectorError::Canonicalize {
                path: safe.clone(),
                source,
            })?;
            ensure_inside_scope(&canonical, &cfg.scope.roots)?;
            Ok(canonical)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsEntry {
    pub name: String,
    pub is_file: bool,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsStat {
    pub is_file: bool,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
}

fn strip_windows_unc(path: &Path) -> PathBuf {
    // Strip the Windows `\\?\` UNC prefix so two paths that point
    // to the same file compare equal. The host is not running on
    // Windows in production; we still strip because users do pass
    // Windows paths through `path.join`.
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

fn strip_dangerous_components(path: &Path) -> PathBuf {
    // Remove any `..` components that would escape the directory
    // tree. We only strip leading `..`; in-path `..` is left to
    // `canonicalize` to deal with.
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                // Drop. The caller still expects the resolved
                // path to be inside the scope after canonicalise.
            }
            Component::CurDir => {
                // Drop. Skip `.`.
            }
            other => out.push(other),
        }
    }
    out
}

fn ensure_inside_scope(path: &Path, roots: &[PathBuf]) -> Result<(), FsConnectorError> {
    if roots.is_empty() {
        return Err(FsConnectorError::OutsideScope {
            path: path.to_path_buf(),
            roots: roots.to_vec(),
        });
    }
    // Use a canonical comparison that handles macOS's `/private`
    // symlink for temp dirs. We canonicalise both sides; if the
    // canonical form of `path` does not start with any canonical
    // root, we deny.
    let canon_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    for root in roots {
        let r = root.canonicalize().unwrap_or_else(|_| root.clone());
        if canon_path.starts_with(&r) {
            return Ok(());
        }
    }
    Err(FsConnectorError::OutsideScope {
        path: path.to_path_buf(),
        roots: roots.to_vec(),
    })
}

/// True when `path` contains a `..` component (parent directory
/// traversal). Used by `resolve` to refuse paths that would escape
/// the scope before any canonicalisation runs.
fn contains_parent_dir(path: &Path) -> bool {
    path.components().any(|c| matches!(c, Component::ParentDir))
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<(), FsConnectorError> {
    #[cfg(windows)]
    {
        return Err(FsConnectorError::AtomicUnsupported);
    }
    #[cfg(not(windows))]
    {
        let parent = path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|source| FsConnectorError::WriteTmp {
            tmp: path.to_path_buf(),
            source,
        })?;
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp = parent.join(format!(
            ".{}.tmp-{}-{}",
            path.file_name().and_then(|s| s.to_str()).unwrap_or("file"),
            pid,
            nanos
        ));
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .map_err(|source| FsConnectorError::WriteTmp {
                    tmp: tmp.clone(),
                    source,
                })?;
            file.write_all(data).map_err(|source| FsConnectorError::WriteTmp {
                tmp: tmp.clone(),
                source,
            })?;
            file.seek(SeekFrom::Start(0)).map_err(|source| FsConnectorError::WriteTmp {
                tmp: tmp.clone(),
                source,
            })?;
            file.sync_all().map_err(|source| FsConnectorError::WriteTmp {
                tmp: tmp.clone(),
                source,
            })?;
        }
        fs::rename(&tmp, path).map_err(|source| FsConnectorError::Rename {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn tempdir(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "pi-extensions-fs-connector-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn write_file(p: &Path, data: &[u8]) {
        fs::write(p, data).unwrap();
    }

    #[test]
    fn read_write_round_trip() {
        let dir = tempdir("round-trip");
        let target = dir.join("hello.txt");
        let conn = FsConnector::with_scope(CapabilityScope {
            roots: vec![dir.clone()],
            follow_symlinks: false,
            max_write_bytes: None,
        });
        conn.write(&target, b"hello, world").unwrap();
        let read = conn.read(&target).unwrap();
        assert_eq!(read, b"hello, world");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_is_atomic_under_tmp_then_rename() {
        let dir = tempdir("atomic");
        let target = dir.join("a.bin");
        write_file(&target, b"old");
        let conn = FsConnector::with_scope(CapabilityScope {
            roots: vec![dir.clone()],
            follow_symlinks: false,
            max_write_bytes: None,
        });
        conn.write(&target, b"new content").unwrap();
        // No leftover tmp file should exist.
        let entries: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(entries.iter().all(|n| !n.contains(".tmp-")), "tmp file leaked: {:?}", entries);
        assert_eq!(fs::read(&target).unwrap(), b"new content");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_writes_outside_scope() {
        let allowed = tempdir("allowed");
        let outside = tempdir("outside");
        let target = outside.join("nope.txt");
        let conn = FsConnector::with_scope(CapabilityScope {
            roots: vec![allowed.clone()],
            follow_symlinks: false,
            max_write_bytes: None,
        });
        let result = conn.write(&target, b"x");
        assert!(matches!(result, Err(FsConnectorError::OutsideScope { .. })));
        let _ = fs::remove_dir_all(&allowed);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn rejects_traversal_via_dotdot() {
        let allowed = tempdir("traversal");
        let target = allowed.join("..").join("outside.txt");
        let conn = FsConnector::with_scope(CapabilityScope {
            roots: vec![allowed.clone()],
            follow_symlinks: false,
            max_write_bytes: None,
        });
        // The `..` is stripped before canonicalisation; the resulting
        // path is `outside.txt` which is not under the allowed root.
        let result = conn.read(&target);
        assert!(matches!(result, Err(FsConnectorError::OutsideScope { .. })));
        let _ = fs::remove_dir_all(&allowed);
    }

    #[test]
    fn respects_max_write_bytes() {
        let dir = tempdir("cap");
        let target = dir.join("big.bin");
        let conn = FsConnector::with_scope(CapabilityScope {
            roots: vec![dir.clone()],
            follow_symlinks: false,
            max_write_bytes: Some(8),
        });
        let err = conn.write(&target, &vec![0u8; 16]).unwrap_err();
        assert!(matches!(err, FsConnectorError::WriteTooLarge { actual: 16, cap: 8, .. }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_returns_files_and_dirs() {
        let dir = tempdir("list");
        fs::write(dir.join("a.txt"), b"a").unwrap();
        fs::create_dir(dir.join("sub")).unwrap();
        let conn = FsConnector::with_scope(CapabilityScope {
            roots: vec![dir.clone()],
            follow_symlinks: false,
            max_write_bytes: None,
        });
        let entries = conn.list(&dir).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"sub"));
        assert!(entries.iter().find(|e| e.name == "sub").unwrap().is_dir);
        assert!(entries.iter().find(|e| e.name == "a.txt").unwrap().is_file);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stat_returns_metadata() {
        let dir = tempdir("stat");
        let f = dir.join("x");
        fs::write(&f, b"abc").unwrap();
        let conn = FsConnector::with_scope(CapabilityScope {
            roots: vec![dir.clone()],
            follow_symlinks: false,
            max_write_bytes: None,
        });
        let meta = conn.stat(&f).unwrap();
        assert_eq!(meta.size, 3);
        assert!(meta.is_file);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mkdir_creates_directory() {
        let dir = tempdir("mkdir");
        let sub = dir.join("new");
        let conn = FsConnector::with_scope(CapabilityScope {
            roots: vec![dir.clone()],
            follow_symlinks: false,
            max_write_bytes: None,
        });
        conn.mkdir(&sub).unwrap();
        assert!(sub.is_dir());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_removes_files_and_dirs() {
        let dir = tempdir("delete");
        let f = dir.join("a");
        fs::write(&f, b"x").unwrap();
        let sub = dir.join("sub");
        fs::create_dir(&sub).unwrap();
        let conn = FsConnector::with_scope(CapabilityScope {
            roots: vec![dir.clone()],
            follow_symlinks: false,
            max_write_bytes: None,
        });
        conn.delete(&f).unwrap();
        assert!(!f.exists());
        conn.delete(&sub).unwrap();
        assert!(!sub.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn strips_windows_unc_prefix() {
        let p = Path::new(r"\\?\C:\Users\foo");
        let stripped = strip_windows_unc(p);
        assert_eq!(stripped.to_string_lossy(), r"C:\Users\foo");
    }

    #[test]
    fn strips_dangerous_components() {
        let mut p = PathBuf::from("/tmp/extensions/allowed");
        p.push("..");
        p.push("..");
        p.push("etc");
        let s = strip_dangerous_components(&p);
        // Both `..` components are dropped.
        let parts: Vec<String> = s.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        assert!(!parts.iter().any(|c| c == ".."), ".. components leaked: {:?}", parts);
    }

    #[test]
    fn rejects_symlink_traversal_when_follow_disabled() {
        // Create a symlink inside the scope that points outside.
        let allowed = tempdir("sym-allowed");
        let outside = tempdir("sym-outside");
        let real = outside.join("secret.txt");
        fs::write(&real, b"secret").unwrap();
        let link = allowed.join("escape");
        #[cfg(not(windows))]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let conn = FsConnector::with_scope(CapabilityScope {
            roots: vec![allowed.clone()],
            follow_symlinks: false,
            max_write_bytes: None,
        });
        let result = conn.read(&link);
        // Should fail because the link is a symlink.
        assert!(matches!(result, Err(FsConnectorError::SymlinkTraversal { .. })), "got: {:?}", result);
        let _ = fs::remove_dir_all(&allowed);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn config_update_takes_effect() {
        let dir = tempdir("config-update");
        let conn = FsConnector::with_scope(CapabilityScope {
            roots: vec![dir.clone()],
            follow_symlinks: false,
            max_write_bytes: Some(1),
        });
        let f = dir.join("f");
        let err = conn.write(&f, b"too long").unwrap_err();
        assert!(matches!(err, FsConnectorError::WriteTooLarge { .. }));
        conn.set_config(FsConnectorConfig {
            scope: CapabilityScope {
                roots: vec![dir.clone()],
                follow_symlinks: false,
                max_write_bytes: None,
            },
            global_max_write_bytes: 1024,
        });
        conn.write(&f, b"too long").unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    // unused-imports guard: keep HashMap imported so future test
    // fixtures can build a directory tree without churn.
    #[allow(dead_code)]
    fn _unused() {
        let _: HashMap<String, Vec<u8>> = HashMap::new();
    }
}