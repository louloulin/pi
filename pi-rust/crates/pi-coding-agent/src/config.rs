//! Settings loader — the `settings.json` slice automatic compaction needs.
//!
//! Mirrors the merge rules in `packages/coding-agent/src/config.ts` and
//! `core/settings-manager.ts`: the user file (`~/.pi/agent/settings.json`)
//! is deep-merged with the project file (`.pi/settings.json`) and the
//! project wins key by key.
//!
//! Stage 27 only reads the `compaction` slice — `compaction.reserveTokens`,
//! `compaction.keepRecentTokens` and the auto-compaction toggle
//! (`compaction.enabled`, also accepted as a top-level `autoCompact`
//! boolean). Every other key stays untouched for a later stage.
//!
//! Malformed files and malformed values never abort a session: the loader
//! warns on stderr and falls back to the built-in default for the value it
//! could not read.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::compaction::{CompactionSettings, DEFAULT_COMPACTION_SETTINGS};
use crate::paths;

/// Settings file name inside `~/.pi/agent` and `.pi`.
pub const SETTINGS_FILE_NAME: &str = "settings.json";

/// Default for `compaction.reserveTokens`.
pub const DEFAULT_RESERVE_TOKENS: u32 = DEFAULT_COMPACTION_SETTINGS.reserve_tokens;

/// Default for `compaction.keepRecentTokens`.
pub const DEFAULT_KEEP_RECENT_TOKENS: u32 = DEFAULT_COMPACTION_SETTINGS.keep_recent_tokens;

/// Default for the auto-compaction toggle.
pub const DEFAULT_AUTO_COMPACT: bool = DEFAULT_COMPACTION_SETTINGS.enabled;

/// Locations the config loader reads from. Mirrors the precedence in the
/// TS implementation: CLI flags > project settings > user settings > defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigSources {
    /// User settings file (`~/.pi/agent/settings.json`).
    pub user: Option<PathBuf>,
    /// Project settings file (`.pi/settings.json`).
    pub project: Option<PathBuf>,
}

impl ConfigSources {
    /// Default locations for `cwd`: `~/.pi/agent/settings.json` and
    /// `<cwd>/.pi/settings.json`.
    ///
    /// The user file is skipped when no home directory is known.
    pub fn discover(cwd: &Path) -> Self {
        Self {
            user: paths::agent_dir().map(|dir| dir.join(SETTINGS_FILE_NAME)),
            project: Some(cwd.join(paths::CONFIG_DIR_NAME).join(SETTINGS_FILE_NAME)),
        }
    }
}

/// Load the compaction settings from the default locations under the
/// current working directory.
pub fn load_compaction_settings_default() -> CompactionSettings {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    load_compaction_settings(&ConfigSources::discover(&cwd))
}

/// Load the compaction settings from `sources`, project over user.
///
/// Every problem degrades to the default value for the affected key and
/// emits a warning on stderr; the loader itself never panics on user
/// input.
pub fn load_compaction_settings(sources: &ConfigSources) -> CompactionSettings {
    let user = sources.user.as_deref().and_then(read_object);
    let project = sources.project.as_deref().and_then(read_object);

    let mut merged = user.unwrap_or_default();
    if let Some(project) = project {
        deep_merge(&mut merged, &project);
    }

    CompactionSettings {
        enabled: read_auto_compact(&merged),
        reserve_tokens: read_token(
            &merged,
            "reserveTokens",
            DEFAULT_RESERVE_TOKENS,
        ),
        keep_recent_tokens: read_token(
            &merged,
            "keepRecentTokens",
            DEFAULT_KEEP_RECENT_TOKENS,
        ),
    }
}

/// Read one of the two `compaction` token settings, falling back to
/// `default` when it is missing or malformed.
fn read_token(merged: &Map<String, Value>, key: &str, default: u32) -> u32 {
    let Some(value) = merged.get("compaction").and_then(Value::as_object).and_then(|c| c.get(key))
    else {
        return default;
    };
    match value.as_u64().and_then(|raw| u32::try_from(raw).ok()) {
        Some(raw) => raw,
        None => {
            warn(&format!(
                "compaction.{key} must be a non-negative integer (got {value}); using {default}"
            ));
            default
        }
    }
}

/// The auto-compaction toggle. Upstream spells it `compaction.enabled`;
/// the Stage 27 task also accepts a top-level `autoCompact` boolean.
fn read_auto_compact(merged: &Map<String, Value>) -> bool {
    if let Some(value) = merged
        .get("compaction")
        .and_then(Value::as_object)
        .and_then(|c| c.get("enabled"))
    {
        return match value.as_bool() {
            Some(enabled) => enabled,
            None => {
                warn(&format!(
                    "compaction.enabled must be a boolean (got {value}); using {DEFAULT_AUTO_COMPACT}"
                ));
                DEFAULT_AUTO_COMPACT
            }
        };
    }
    match merged.get("autoCompact") {
        None => DEFAULT_AUTO_COMPACT,
        Some(value) => match value.as_bool() {
            Some(enabled) => enabled,
            None => {
                warn(&format!(
                    "autoCompact must be a boolean (got {value}); using {DEFAULT_AUTO_COMPACT}"
                ));
                DEFAULT_AUTO_COMPACT
            }
        },
    }
}

/// Read and parse one settings file.
///
/// Missing files are silent (the default locations routinely do not
/// exist); unreadable or malformed files warn and are ignored.
fn read_object(path: &Path) -> Option<Map<String, Value>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            warn(&format!("cannot read {}: {err}", path.display()));
            return None;
        }
    };
    // Editors on Windows routinely leave a BOM behind.
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(map)) => Some(map),
        Ok(other) => {
            warn(&format!(
                "{}: expected a JSON object, got {}",
                path.display(),
                json_kind(&other)
            ));
            None
        }
        Err(err) => {
            warn(&format!("{}: invalid JSON ({err})", path.display()));
            None
        }
    }
}

/// Recursively merge `overrides` into `base` — objects merge key by key,
/// every other value replaces the base value (upstream
/// `deepMergeObjects`).
fn deep_merge(base: &mut Map<String, Value>, overrides: &Map<String, Value>) {
    for (key, value) in overrides {
        match (base.get_mut(key), value) {
            (Some(Value::Object(base_nested)), Value::Object(override_nested)) => {
                deep_merge(base_nested, override_nested);
            }
            _ => {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Human-readable JSON type for warning messages.
fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn warn(message: &str) {
    eprintln!("pi: settings: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).expect("write settings");
        path
    }

    fn settings(user: Option<&Path>, project: Option<&Path>) -> CompactionSettings {
        load_compaction_settings(&ConfigSources {
            user: user.map(Path::to_path_buf),
            project: project.map(Path::to_path_buf),
        })
    }

    #[test]
    fn missing_files_yield_defaults() {
        assert_eq!(settings(None, None), DEFAULT_COMPACTION_SETTINGS);
    }

    #[test]
    fn project_overrides_user_key_by_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = write(
            dir.path(),
            "user.json",
            r#"{"compaction":{"reserveTokens": 4096, "keepRecentTokens": 1234}}"#,
        );
        let project = write(
            dir.path(),
            "project.json",
            r#"{"compaction":{"reserveTokens": 2048}}"#,
        );
        let resolved = settings(Some(&user), Some(&project));
        // Project wins for reserveTokens; the untouched key falls through.
        assert_eq!(
            resolved,
            CompactionSettings {
                enabled: true,
                reserve_tokens: 2048,
                keep_recent_tokens: 1234,
            }
        );
    }

    #[test]
    fn missing_compaction_object_keeps_other_keys_at_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = write(dir.path(), "user.json", r#"{"defaultModel":"faux"}"#);
        assert_eq!(settings(Some(&user), None), DEFAULT_COMPACTION_SETTINGS);
    }

    #[test]
    fn malformed_json_degrades_to_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = write(dir.path(), "user.json", "{ not json");
        let project = write(dir.path(), "project.json", "42");
        assert_eq!(
            settings(Some(&user), Some(&project)),
            DEFAULT_COMPACTION_SETTINGS
        );
    }

    #[test]
    fn malformed_values_fall_back_per_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = write(
            dir.path(),
            "user.json",
            r#"{"compaction":{"reserveTokens": -5, "keepRecentTokens": "lots"}}"#,
        );
        let resolved = settings(Some(&user), None);
        assert_eq!(resolved.reserve_tokens, DEFAULT_RESERVE_TOKENS);
        assert_eq!(resolved.keep_recent_tokens, DEFAULT_KEEP_RECENT_TOKENS);
        assert!(resolved.enabled);
    }

    #[test]
    fn zero_is_a_valid_token_setting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = write(
            dir.path(),
            "user.json",
            r#"{"compaction":{"reserveTokens": 0, "keepRecentTokens": 0}}"#,
        );
        let resolved = settings(Some(&user), None);
        assert_eq!(resolved.reserve_tokens, 0);
        assert_eq!(resolved.keep_recent_tokens, 0);
    }

    #[test]
    fn compaction_enabled_and_auto_compact_control_the_toggle() {
        let dir = tempfile::tempdir().expect("tempdir");

        let disabled = write(dir.path(), "a.json", r#"{"compaction":{"enabled":false}}"#);
        assert!(!settings(Some(&disabled), None).enabled);

        let alias = write(dir.path(), "b.json", r#"{"autoCompact":false}"#);
        assert!(!settings(Some(&alias), None).enabled);

        // The upstream spelling wins when both are present.
        let both = write(
            dir.path(),
            "c.json",
            r#"{"autoCompact":false,"compaction":{"enabled":true}}"#,
        );
        assert!(settings(Some(&both), None).enabled);

        let bad = write(dir.path(), "d.json", r#"{"autoCompact":"no"}"#);
        assert!(settings(Some(&bad), None).enabled);
    }

    #[test]
    fn project_toggle_overrides_user_toggle() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = write(dir.path(), "user.json", r#"{"compaction":{"enabled":false}}"#);
        let project = write(dir.path(), "project.json", r#"{"compaction":{"enabled":true}}"#);
        assert!(settings(Some(&user), Some(&project)).enabled);
    }

    #[test]
    fn discover_points_at_agent_and_project_files() {
        let sources = ConfigSources::discover(Path::new("/work"));
        assert_eq!(
            sources.project,
            Some(PathBuf::from("/work/.pi/settings.json"))
        );
        if let Some(user) = sources.user {
            assert!(user.ends_with(".pi/agent/settings.json"), "{user:?}");
        }
    }
}
