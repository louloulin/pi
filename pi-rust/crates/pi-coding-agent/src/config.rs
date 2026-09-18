//! Settings loader — placeholder for Stage 4. Mirrors the merge rules in
//! `packages/coding-agent/src/config.ts`.

use std::path::PathBuf;

/// Locations the config loader reads from. Mirrors the precedence in the
/// TS implementation: CLI flags > project settings > user settings > defaults.
#[derive(Debug, Clone, Default)]
pub struct ConfigSources {
    /// User settings file (`~/.pi/agent/settings.json`).
    pub user: Option<PathBuf>,
    /// Project settings file (`.pi/settings.json`).
    pub project: Option<PathBuf>,
}
