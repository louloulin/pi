//! Extension loader — mirrors the resolution rules in
//! `packages/coding-agent/src/extensions/index.ts`:
//!
//! 1. `~/.pi/agent/extensions/` — global
//! 2. `.pi/extensions/` — project-local
//! 3. `-e <path>` — explicit single extension (handled by the binary, not the loader)

use std::path::{Path, PathBuf};

use crate::api::ExtensionEntry;

/// Resolve directories the loader searches for extensions.
#[derive(Debug, Clone, Default)]
pub struct ExtensionSearchPaths {
    /// Global extensions directory (`~/.pi/agent/extensions`).
    pub global: Option<PathBuf>,
    /// Project-local extensions directory (`.pi/extensions`).
    pub project: Option<PathBuf>,
}

impl ExtensionSearchPaths {
    /// Construct from defaults rooted at the given home / cwd.
    pub fn from_env(home: Option<&Path>, cwd: &Path) -> Self {
        Self {
            global: home.map(|h| h.join(".pi/agent/extensions")),
            project: Some(cwd.join(".pi/extensions")),
        }
    }

    /// List candidate extension files (`.ts`, `.js`, `.mjs`, `.wasm`).
    ///
    /// ESM vs CommonJS is decided per file inside the JS shim (see
    /// `runtime/pi-ext-shim.mjs` `_pi_load_extension`), so this resolver
    /// only has to hand every JS-family file through unchanged.
    pub fn candidates(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for dir in [&self.global, &self.project].into_iter().flatten() {
            if !dir.is_dir() {
                continue;
            }
            for entry in walkdir::WalkDir::new(dir)
                .max_depth(2)
                .into_iter()
                .filter_map(Result::ok)
            {
                let path = entry.path();
                let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
                    continue;
                };
                if matches!(ext, "ts" | "js" | "mjs" | "wasm") {
                    out.push(path.to_path_buf());
                }
            }
        }
        out
    }

    /// Build the [`ExtensionEntry`] for a given source path.
    pub fn entry_for(&self, source: &Path) -> ExtensionEntry {
        let id = source
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("extension")
            .to_string();
        ExtensionEntry {
            source: source.to_path_buf(),
            id,
            label: None,
        }
    }
}
