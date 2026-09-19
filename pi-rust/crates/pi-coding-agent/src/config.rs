//! Settings loader — the `settings.json` slices the TUI needs.
//!
//! Mirrors the merge rules in `packages/coding-agent/src/config.ts` and
//! `core/settings-manager.ts`: the user file (`~/.pi/agent/settings.json`)
//! is deep-merged with the project file (`.pi/settings.json`) and the
//! project wins key by key.
//!
//! Two slices are read:
//!
//! * the compaction settings (`compaction.reserveTokens`,
//!   `compaction.keepRecentTokens`, and the toggle `compaction.enabled`,
//!   also accepted as a top-level `autoCompact` boolean);
//! * the `/settings` UI slice — `theme` and `fullscreenCopyOnSelect` —
//!   loaded by [`load_ui_settings`];
//! * the provider-request retry budget (`retry.provider.maxRetries`,
//!   `retry.provider.maxRetryDelayMs`) loaded by
//!   [`load_provider_retry_policy`].
//!
//! Writes go through [`save_user_setting`], which only ever touches the
//! **user** file: upstream's `setTheme` / `setAutoCompact` /
//! `setFullscreenCopyOnSelect` all write `globalSettings`
//! (`core/settings-manager.ts:790,849,1284`), never the project file.
//!
//! Malformed files and malformed values never abort a session: the loader
//! warns on stderr and falls back to the built-in default for the value it
//! could not read. The writer is stricter — it refuses to overwrite a file
//! it cannot parse, so a typo in `settings.json` costs the user a warning
//! instead of their configuration.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use pi_ai::ProviderRetryPolicy;

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

/// Default for `fullscreenCopyOnSelect` — upstream `?? true`
/// (`core/settings-manager.ts:1280`).
pub const DEFAULT_FULLSCREEN_COPY_ON_SELECT: bool = true;

/// Default for `retry.provider.maxRetryDelayMs` — upstream
/// `DEFAULT_MAX_RETRY_DELAY_MS`: a server-requested delay above this fails
/// the request instead of sleeping.
pub const DEFAULT_PROVIDER_MAX_RETRY_DELAY_MS: u64 = 60_000;

/// Default for `retry.provider.maxRetries` — upstream leaves it undefined,
/// and `retryProviderRequest` then retries nothing (the agent-level retry is
/// what retries by default in the TypeScript build).
pub const DEFAULT_PROVIDER_MAX_RETRIES: u32 = 0;

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

/// The `/settings` UI slice: the two keys upstream's settings selector
/// exposes that already have a live or next-launch effect in this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiSettings {
    /// `theme` — the explicit theme choice, `None` when the user never
    /// picked one (the App keeps its built-in default).
    pub theme: Option<String>,
    /// `fullscreenCopyOnSelect`.
    pub fullscreen_copy_on_select: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            theme: None,
            fullscreen_copy_on_select: DEFAULT_FULLSCREEN_COPY_ON_SELECT,
        }
    }
}

/// Load the UI slice from the default locations under the current working
/// directory.
pub fn load_ui_settings_default() -> UiSettings {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    load_ui_settings(&ConfigSources::discover(&cwd))
}

/// Load the UI slice from `sources`, project over user.
pub fn load_ui_settings(sources: &ConfigSources) -> UiSettings {
    let merged = merged_settings(sources);
    UiSettings {
        theme: read_theme(&merged),
        fullscreen_copy_on_select: read_bool(
            &merged,
            "fullscreenCopyOnSelect",
            DEFAULT_FULLSCREEN_COPY_ON_SELECT,
        ),
    }
}

/// Read the top-level `theme` string setting, warning on a malformed value.
fn read_theme(merged: &Map<String, Value>) -> Option<String> {
    match merged.get("theme") {
        None => None,
        Some(Value::String(theme)) if !theme.is_empty() => Some(theme.clone()),
        Some(other) => {
            warn(&format!(
                "theme must be a non-empty string (got {}); using the built-in default",
                json_kind(other)
            ));
            None
        }
    }
}

/// Read a top-level boolean setting, warning on a malformed value.
fn read_bool(merged: &Map<String, Value>, key: &str, default: bool) -> bool {
    match merged.get(key) {
        None => default,
        Some(Value::Bool(value)) => *value,
        Some(other) => {
            warn(&format!(
                "{key} must be a boolean (got {}); using {default}",
                json_kind(other)
            ));
            default
        }
    }
}

/// Load the provider-request retry budget from the default locations under
/// the current working directory.
pub fn load_provider_retry_policy_default() -> ProviderRetryPolicy {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    load_provider_retry_policy(&ConfigSources::discover(&cwd))
}

/// Load the provider-request retry budget from `sources`, project over user.
///
/// Mirrors upstream `SettingsManager.getProviderRetrySettings()`
/// (`core/settings-manager.ts:949`): only the `retry.provider` object reaches
/// the provider request layer. The agent-level keys (`retry.enabled`,
/// `retry.maxRetries`, `retry.baseDelayMs`, `retry.maxAgentDelayMs`) belong to
/// the agent retry loop and are deliberately not read here, so a user's
/// agent-level budget cannot silently start retrying raw HTTP calls.
///
/// Every problem degrades to the default for the affected key and warns on
/// stderr, like the other loaders in this module.
pub fn load_provider_retry_policy(sources: &ConfigSources) -> ProviderRetryPolicy {
    let merged = merged_settings(sources);
    let provider = merged
        .get("retry")
        .and_then(Value::as_object)
        .and_then(|retry| retry.get("provider"))
        .and_then(Value::as_object);

    let max_retries = match provider.and_then(|provider| provider.get("maxRetries")) {
        None => DEFAULT_PROVIDER_MAX_RETRIES,
        Some(value) => match value.as_u64().and_then(|raw| u32::try_from(raw).ok()) {
            Some(raw) => raw,
            None => {
                warn(&format!(
                    "retry.provider.maxRetries must be a non-negative integer (got {value}); \
                     provider retrying stays off"
                ));
                DEFAULT_PROVIDER_MAX_RETRIES
            }
        },
    };

    let max_retry_delay_ms = match provider.and_then(|provider| provider.get("maxRetryDelayMs")) {
        None => DEFAULT_PROVIDER_MAX_RETRY_DELAY_MS,
        Some(value) => match value.as_u64() {
            Some(raw) => raw,
            None => {
                warn(&format!(
                    "retry.provider.maxRetryDelayMs must be a non-negative integer (got {value}); \
                     using {DEFAULT_PROVIDER_MAX_RETRY_DELAY_MS}"
                ));
                DEFAULT_PROVIDER_MAX_RETRY_DELAY_MS
            }
        },
    };

    ProviderRetryPolicy::with_max_retry_delay_ms(max_retries, max_retry_delay_ms)
}

/// Persist one setting into the **user** settings file, creating it when
/// needed, and return the path that was written.
///
/// `key` is a dotted path — `"theme"`, `"fullscreenCopyOnSelect"`,
/// `"compaction.enabled"` — matching the nesting upstream's setters use
/// (`core/settings-manager.ts:790,849,1284`).
///
/// Every other key in the file is preserved verbatim, including comments-free
/// JSON the loader does not understand, so a `/settings` change never drops
/// configuration owned by a newer pi version. The write is atomic: the new
/// document is written to a sibling temporary file and renamed over the
/// target, so an interrupted write cannot truncate the user's settings.
///
/// # Errors
///
/// Fails without touching the file when no user path is known (no `$HOME`),
/// when the existing file cannot be parsed as a JSON object, or when an
/// intermediate segment of `key` is not an object.
pub fn save_user_setting(
    sources: &ConfigSources,
    key: &str,
    value: Value,
) -> anyhow::Result<PathBuf> {
    use anyhow::{bail, Context};

    let Some(path) = sources.user.clone() else {
        bail!("no user settings path is known (set $HOME, or $PI_CODING_AGENT_DIR)");
    };

    let mut root = match read_object_for_write(&path)? {
        Some(root) => root,
        None => Map::new(),
    };
    set_path(&mut root, key, value)?;

    let mut text = serde_json::to_string_pretty(&Value::Object(root))
        .context("serialize settings.json")?;
    // Two-space indent plus a trailing newline, the shape every other JSON
    // file this crate writes uses (`trust.rs:331`). Upstream's
    // `JSON.stringify(..., null, 2)` omits the newline.
    text.push('\n');

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let temp = temp_sibling(&path);
    std::fs::write(&temp, text).with_context(|| format!("write {}", temp.display()))?;
    std::fs::rename(&temp, &path)
        .with_context(|| format!("rename {} → {}", temp.display(), path.display()))?;
    Ok(path)
}

/// Read a settings file for a write: `None` when it does not exist.
///
/// Unlike [`read_object`], a file that exists but does not hold a JSON
/// object is an error — the writer must never replace data it cannot
/// understand.
fn read_object_for_write(path: &Path) -> anyhow::Result<Option<Map<String, Value>>> {
    use anyhow::{bail, Context};

    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(map)) => Ok(Some(map)),
        Ok(other) => bail!(
            "{}: expected a JSON object, got {} — refusing to overwrite it",
            path.display(),
            json_kind(&other)
        ),
        Err(err) => bail!("{}: invalid JSON ({err}) — refusing to overwrite it", path.display()),
    }
}

/// Set a dotted `key` in `root`, creating intermediate objects.
fn set_path(root: &mut Map<String, Value>, key: &str, value: Value) -> anyhow::Result<()> {
    use anyhow::bail;

    let mut segments = key.split('.').peekable();
    let mut cursor = root;
    while let Some(segment) = segments.next() {
        if segment.is_empty() {
            bail!("invalid settings key {key:?}");
        }
        if segments.peek().is_none() {
            cursor.insert(segment.to_string(), value);
            return Ok(());
        }
        let entry = cursor
            .entry(segment.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        cursor = match entry {
            Value::Object(map) => map,
            other => bail!(
                "settings key {key:?}: {segment} is {}, not an object",
                json_kind(other)
            ),
        };
    }
    // `key` was empty, so no segment was consumed.
    bail!("invalid settings key {key:?}")
}

/// Sibling temporary path for `path`, used for the atomic replace.
fn temp_sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| SETTINGS_FILE_NAME.to_string());
    path.with_file_name(format!(".{name}.tmp"))
}

/// Read the two settings files and merge them, project over user.
///
/// Missing files are silent; unreadable or malformed files warn and are
/// ignored.
fn merged_settings(sources: &ConfigSources) -> Map<String, Value> {
    let user = sources.user.as_deref().and_then(read_object);
    let project = sources.project.as_deref().and_then(read_object);

    let mut merged = user.unwrap_or_default();
    if let Some(project) = project {
        deep_merge(&mut merged, &project);
    }
    merged
}

/// Load the compaction settings from `sources`, project over user.
///
/// Every problem degrades to the default value for the affected key and
/// emits a warning on stderr; the loader itself never panics on user
/// input.
pub fn load_compaction_settings(sources: &ConfigSources) -> CompactionSettings {
    let merged = merged_settings(sources);

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

    fn retry_policy(user: Option<&Path>, project: Option<&Path>) -> ProviderRetryPolicy {
        load_provider_retry_policy(&ConfigSources {
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

    #[test]
    fn ui_settings_default_to_no_theme_and_copy_on_select() {
        assert_eq!(
            load_ui_settings(&ConfigSources::default()),
            UiSettings::default()
        );
        assert!(load_ui_settings(&ConfigSources::default()).fullscreen_copy_on_select);
    }

    #[test]
    fn ui_settings_read_both_keys_project_over_user() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = write(
            dir.path(),
            "user.json",
            r#"{"theme":"light","fullscreenCopyOnSelect":false}"#,
        );
        let project = write(dir.path(), "project.json", r#"{"theme":"dark"}"#);

        let settings = load_ui_settings(&ConfigSources {
            user: Some(user.clone()),
            project: Some(project),
        });
        assert_eq!(settings.theme.as_deref(), Some("dark"));
        assert!(!settings.fullscreen_copy_on_select, "the user value survives");

        let user_only = load_ui_settings(&ConfigSources {
            user: Some(user),
            project: None,
        });
        assert_eq!(user_only.theme.as_deref(), Some("light"));
    }

    #[test]
    fn ui_settings_fall_back_per_field_on_malformed_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = write(
            dir.path(),
            "user.json",
            r#"{"theme":42,"fullscreenCopyOnSelect":"yes"}"#,
        );
        let settings = load_ui_settings(&ConfigSources {
            user: Some(user),
            project: None,
        });
        assert_eq!(settings, UiSettings::default());
    }

    #[test]
    fn provider_retry_defaults_to_no_retries_with_a_sixty_second_cap() {
        assert_eq!(
            retry_policy(None, None),
            ProviderRetryPolicy::default(),
            "missing files leave provider retrying off, like upstream"
        );
        assert!(!retry_policy(None, None).is_enabled());
        assert_eq!(
            retry_policy(None, None).max_retry_delay_ms,
            DEFAULT_PROVIDER_MAX_RETRY_DELAY_MS
        );
    }

    #[test]
    fn provider_retry_slice_is_read_project_over_user() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = write(
            dir.path(),
            "user.json",
            r#"{"retry":{"provider":{"maxRetries":2,"maxRetryDelayMs":1000}}}"#,
        );
        let project = write(
            dir.path(),
            "project.json",
            r#"{"retry":{"provider":{"maxRetries":5}}}"#,
        );

        let resolved = retry_policy(Some(&user), Some(&project));
        assert_eq!(resolved.max_retries, 5, "project wins for maxRetries");
        assert_eq!(
            resolved.max_retry_delay_ms, 1_000,
            "the untouched user key falls through"
        );
        assert!(resolved.is_enabled());

        let user_only = retry_policy(Some(&user), None);
        assert_eq!(user_only.max_retries, 2);
    }

    #[test]
    fn agent_level_retry_keys_do_not_turn_on_provider_retrying() {
        // `retry.maxRetries` is the agent-level budget; the provider layer
        // reads `retry.provider.maxRetries` and nothing else.
        let dir = tempfile::tempdir().expect("tempdir");
        let user = write(
            dir.path(),
            "user.json",
            r#"{"retry":{"enabled":true,"maxRetries":3,"baseDelayMs":2000}}"#,
        );
        assert!(!retry_policy(Some(&user), None).is_enabled());
    }

    #[test]
    fn malformed_provider_retry_values_fall_back_per_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = write(
            dir.path(),
            "user.json",
            r#"{"retry":{"provider":{"maxRetries":-1,"maxRetryDelayMs":"soon"}}}"#,
        );
        let resolved = retry_policy(Some(&user), None);
        assert_eq!(resolved.max_retries, DEFAULT_PROVIDER_MAX_RETRIES);
        assert_eq!(
            resolved.max_retry_delay_ms,
            DEFAULT_PROVIDER_MAX_RETRY_DELAY_MS
        );

        // A zero cap is valid and means "no limit" (upstream).
        let uncapped = write(
            dir.path(),
            "uncapped.json",
            r#"{"retry":{"provider":{"maxRetries":1,"maxRetryDelayMs":0}}}"#,
        );
        let resolved = retry_policy(Some(&uncapped), None);
        assert_eq!(resolved.max_retries, 1);
        assert_eq!(resolved.max_retry_delay_ms, 0);
    }

    #[test]
    fn save_user_setting_creates_the_file_and_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = dir.path().join("agent").join(SETTINGS_FILE_NAME);
        let sources = ConfigSources {
            user: Some(user.clone()),
            project: None,
        };

        let written = save_user_setting(
            &sources,
            "theme",
            serde_json::Value::String("light".to_string()),
        )
        .expect("save");
        assert_eq!(written, user);
        let text = std::fs::read_to_string(&user).expect("read back");
        assert_eq!(text, "{\n  \"theme\": \"light\"\n}\n");
        // The temporary sibling is renamed away, never left behind.
        assert!(!user.with_file_name(".settings.json.tmp").exists());
        assert_eq!(
            load_ui_settings(&sources).theme.as_deref(),
            Some("light"),
            "the writer produces a file the loader reads back"
        );
    }

    #[test]
    fn save_user_setting_preserves_unknown_keys_and_nests_dotted_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = write(
            dir.path(),
            SETTINGS_FILE_NAME,
            r#"{"defaultModel":"faux","compaction":{"reserveTokens":4096}}"#,
        );
        let sources = ConfigSources {
            user: Some(user.clone()),
            project: None,
        };

        save_user_setting(
            &sources,
            "compaction.enabled",
            serde_json::Value::Bool(false),
        )
        .expect("save");

        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&user).expect("read")).expect("json");
        // The unknown top-level key is untouched...
        assert_eq!(parsed["defaultModel"], serde_json::json!("faux"));
        // ...and the untouched sibling inside the nested object too.
        assert_eq!(parsed["compaction"]["reserveTokens"], serde_json::json!(4096));
        assert_eq!(parsed["compaction"]["enabled"], serde_json::json!(false));
        assert!(!load_compaction_settings(&sources).enabled);
    }

    #[test]
    fn save_user_setting_refuses_to_clobber_an_unparsable_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = write(dir.path(), SETTINGS_FILE_NAME, "{ not json");
        let sources = ConfigSources {
            user: Some(user.clone()),
            project: None,
        };

        let err = save_user_setting(
            &sources,
            "theme",
            serde_json::Value::String("light".to_string()),
        )
        .expect_err("must refuse");
        assert!(err.to_string().contains("invalid JSON"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&user).expect("read"),
            "{ not json",
            "the user's file is byte-for-byte intact"
        );
    }

    #[test]
    fn save_user_setting_rejects_a_scalar_intermediate_segment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let user = write(dir.path(), SETTINGS_FILE_NAME, r#"{"compaction":true}"#);
        let sources = ConfigSources {
            user: Some(user.clone()),
            project: None,
        };

        let err = save_user_setting(
            &sources,
            "compaction.enabled",
            serde_json::Value::Bool(false),
        )
        .expect_err("must refuse");
        assert!(err.to_string().contains("not an object"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&user).expect("read"),
            r#"{"compaction":true}"#
        );
    }

    #[test]
    fn save_user_setting_without_a_path_fails() {
        let sources = ConfigSources {
            user: None,
            project: None,
        };
        assert!(save_user_setting(
            &sources,
            "theme",
            serde_json::Value::String("light".to_string())
        )
        .is_err());
    }
}
