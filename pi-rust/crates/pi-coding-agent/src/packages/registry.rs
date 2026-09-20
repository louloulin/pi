//! Installed-package registry — reads and writes `<root>/packages.json`.
//!
//! The on-disk schema mirrors the shape described in the Stage 11 issue
//! (and the upstream settings entry):
//!
//! ```json
//! {
//!   "packages": [
//!     {
//!       "spec": "file:./fixture",
//!       "name": "fixture",
//!       "resolved": "/home/me/.pi/packages/fixture",
//!       "installedAt": "2026-09-19T01:59:10Z",
//!       "extensions": ["/home/me/.pi/agent/extensions/fixture/hello.js"]
//!     }
//!   ]
//! }
//! ```
//!
//! Writes go through a sibling temporary file followed by `rename`, so a
//! crashed process never leaves a half-written registry behind.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// File name of the registry inside the pi root (`~/.pi/packages.json`).
pub const REGISTRY_FILE: &str = "packages.json";

/// One installed package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageEntry {
    /// Source spec exactly as passed to `pi install`.
    pub spec: String,
    /// Package name (`@scope/pkg`, `github.com/user/repo`, or a local
    /// directory name).
    pub name: String,
    /// Absolute path to the installed package root.
    pub resolved: String,
    /// RFC 3339 timestamp of the install / reinstall.
    #[serde(rename = "installedAt")]
    pub installed_at: String,
    /// Absolute paths of the extensions exposed to the loader.
    #[serde(default)]
    pub extensions: Vec<String>,
}

/// Serialized registry payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryData {
    /// Installed packages, insertion-ordered.
    #[serde(default)]
    pub packages: Vec<PackageEntry>,
}

/// Errors raised while loading or saving the registry.
#[derive(Debug, Error)]
pub enum RegistryError {
    /// The registry could not be read.
    #[error("failed to read registry {path}: {source}")]
    Read {
        /// Registry path.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// The registry contained invalid JSON.
    #[error("failed to parse registry {path}: {source}")]
    Parse {
        /// Registry path.
        path: PathBuf,
        /// Underlying serde error.
        source: serde_json::Error,
    },
    /// The registry could not be written.
    #[error("failed to write registry {path}: {source}")]
    Write {
        /// Registry path.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
}

/// In-memory view of `<root>/packages.json`.
#[derive(Debug, Clone)]
pub struct Registry {
    path: PathBuf,
    data: RegistryData,
}

impl Registry {
    /// Load the registry rooted at `root` (`~/.pi`). A missing file is
    /// treated as an empty registry.
    pub fn load(root: &Path) -> Result<Self, RegistryError> {
        let path = root.join(REGISTRY_FILE);
        let data = match fs::read_to_string(&path) {
            Ok(contents) if contents.trim().is_empty() => RegistryData::default(),
            Ok(contents) => {
                serde_json::from_str(&contents).map_err(|source| RegistryError::Parse {
                    path: path.clone(),
                    source,
                })?
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => RegistryData::default(),
            Err(source) => {
                return Err(RegistryError::Read {
                    path: path.clone(),
                    source,
                })
            }
        };
        Ok(Self { path, data })
    }

    /// Path of the backing JSON file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Installed packages.
    pub fn packages(&self) -> &[PackageEntry] {
        &self.data.packages
    }

    /// Whether no package is registered.
    pub fn is_empty(&self) -> bool {
        self.data.packages.is_empty()
    }

    /// Find an entry by exact spec, exact name, or install directory
    /// name.
    pub fn find(&self, query: &str) -> Option<&PackageEntry> {
        self.data.packages.iter().find(|entry| {
            entry.spec == query
                || entry.name == query
                || Path::new(&entry.resolved)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|dir| dir == query)
                    .unwrap_or(false)
        })
    }

    /// Insert or replace (by spec/name identity) an entry.
    pub fn upsert(&mut self, entry: PackageEntry) {
        if let Some(existing) = self
            .data
            .packages
            .iter_mut()
            .find(|candidate| candidate.name == entry.name || candidate.spec == entry.spec)
        {
            *existing = entry;
        } else {
            self.data.packages.push(entry);
        }
    }

    /// Remove an entry matching `query`, returning it when present.
    pub fn remove(&mut self, query: &str) -> Option<PackageEntry> {
        let index = self.data.packages.iter().position(|entry| {
            entry.spec == query
                || entry.name == query
                || Path::new(&entry.resolved)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|dir| dir == query)
                    .unwrap_or(false)
        })?;
        Some(self.data.packages.remove(index))
    }

    /// Persist the registry atomically (temp file + rename).
    pub fn save(&self) -> Result<(), RegistryError> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|source| RegistryError::Write {
            path: self.path.clone(),
            source,
        })?;
        let json =
            serde_json::to_string_pretty(&self.data).map_err(|source| RegistryError::Write {
                path: self.path.clone(),
                source: io::Error::new(io::ErrorKind::InvalidData, source),
            })?;
        let tmp = self
            .path
            .with_extension(format!("json.tmp.{}", std::process::id()));
        fs::write(&tmp, format!("{json}\n")).map_err(|source| RegistryError::Write {
            path: tmp.clone(),
            source,
        })?;
        match fs::rename(&tmp, &self.path) {
            Ok(()) => Ok(()),
            Err(source) => {
                let _ = fs::remove_file(&tmp);
                Err(RegistryError::Write {
                    path: self.path.clone(),
                    source,
                })
            }
        }
    }
}
