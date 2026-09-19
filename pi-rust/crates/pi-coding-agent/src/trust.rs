//! Project trust store.
//!
//! Rust port of `packages/coding-agent/src/core/trust-manager.ts`. pi only
//! loads project-local `.pi` resources (`.pi/SYSTEM.md`, `.pi/skills`,
//! `.pi/prompts`, `.pi/extensions`, …) after the user has trusted the
//! project directory, so a cloned repository cannot inject a system
//! prompt or a skill by checking in a `.pi/` folder.
//!
//! Trust decisions live in `<agent_dir>/trust.json` — a flat
//! `{ "<canonical-path>": true | false }` map. A decision applies to its
//! directory *and every descendant*, which is why [`ProjectTrustStore::get`]
//! walks up the parent chain until it finds an entry.
//!
//! This module is filesystem-only (no UI, no extension hook). The CLI
//! resolves the final boolean with [`resolve_project_trusted`] using, in
//! order: the `--approve` / `--no-approve` override, the saved decision,
//! and finally [`DefaultProjectTrust::Ask`] (which is "untrusted" for the
//! non-interactive entry points, mirroring upstream's `hasUI: false`
//! fallback). Interactive `/trust yes|no` persists a decision.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::paths::{absolute, canonicalize, home_dir, strip_bom, CONFIG_DIR_NAME};

/// File name of the trust store inside the agent directory.
pub const TRUST_FILE_NAME: &str = "trust.json";

/// Project `.pi` entries that require trust before they are loaded.
///
/// Mirrors `TRUST_REQUIRING_PROJECT_CONFIG_RESOURCES` in
/// `core/trust-manager.ts`; `.agents/skills` in the project or an
/// ancestor is checked separately.
pub const TRUST_REQUIRING_PROJECT_CONFIG_RESOURCES: [&str; 7] = [
    "settings.json",
    "extensions",
    "skills",
    "prompts",
    "themes",
    "SYSTEM.md",
    "APPEND_SYSTEM.md",
];

/// A saved trust decision: `Some(true)` trusted, `Some(false)` untrusted,
/// `None` no decision for this directory or any ancestor.
pub type ProjectTrustDecision = Option<bool>;

/// One entry found while walking up from a working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTrustStoreEntry {
    /// Canonical directory the decision was saved for.
    pub path: PathBuf,
    /// `true` = trusted, `false` = untrusted.
    pub decision: bool,
}

/// A pending write to the trust store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTrustUpdate {
    /// Directory the decision applies to.
    pub path: PathBuf,
    /// New decision (`None` removes the entry).
    pub decision: ProjectTrustDecision,
}

/// What to do when a project needs a trust decision but none is saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DefaultProjectTrust {
    /// Trust without asking (`defaultProjectTrust: "always"`).
    Always,
    /// Never trust without a saved decision (`"never"`).
    Never,
    /// Ask the user; without a UI this resolves to untrusted.
    #[default]
    Ask,
}

/// Errors surfaced by [`ProjectTrustStore`].
#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    /// The trust file exists but could not be read.
    #[error("failed to read trust store {path}: {source}")]
    Read {
        /// Trust file path.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// The trust file is not a JSON object of `true` / `false` / `null`.
    #[error("invalid trust store {path}: {message}")]
    Invalid {
        /// Trust file path.
        path: PathBuf,
        /// Why the file was rejected.
        message: String,
    },
    /// The trust file could not be written.
    #[error("failed to write trust store {path}: {source}")]
    Write {
        /// Trust file path.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// Another process held the trust file lock for too long.
    #[error("failed to acquire trust store lock {path}")]
    Lock {
        /// Lock file path.
        path: PathBuf,
    },
}

/// Canonicalise a working directory the way upstream `normalizeCwd` does.
fn normalize_cwd(cwd: &Path) -> PathBuf {
    canonicalize(&absolute(cwd))
}

/// Nearest saved decision for `cwd`, walking up the parent chain.
///
/// A `null` entry is deliberately ignored: it marks "no decision", not
/// "untrusted" (upstream `findNearestTrustEntry`).
fn find_nearest_trust_entry(
    data: &BTreeMap<String, Option<bool>>,
    cwd: &Path,
) -> Option<ProjectTrustStoreEntry> {
    let mut current = normalize_cwd(cwd);
    loop {
        if let Some(Some(decision)) = data.get(current.to_string_lossy().as_ref()) {
            return Some(ProjectTrustStoreEntry {
                path: current,
                decision: *decision,
            });
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => return None,
        }
    }
}

/// Parent directory of the canonical `cwd`, for the "trust parent folder"
/// option. `None` at the filesystem root.
pub fn get_project_trust_parent_path(cwd: &Path) -> Option<PathBuf> {
    let trust_path = normalize_cwd(cwd);
    trust_path
        .parent()
        .filter(|parent| *parent != trust_path)
        .map(Path::to_path_buf)
}

/// Whether `cwd` has project resources that must be gated by trust:
/// trust-requiring entries under `cwd/.pi`, or `.agents/skills` in `cwd`
/// or an ancestor.
///
/// The user-level `~/.agents/skills` directory is always treated as a
/// trusted resource and is skipped even when `cwd` is `$HOME`.
pub fn has_trust_requiring_project_resources(cwd: &Path) -> bool {
    has_trust_requiring_project_resources_with_home(cwd, home_dir().as_deref())
}

/// [`has_trust_requiring_project_resources`] with an explicit home
/// directory, so tests do not have to mutate the process environment.
pub fn has_trust_requiring_project_resources_with_home(cwd: &Path, home: Option<&Path>) -> bool {
    let user_agents_skills = home.map(|home| canonicalize(home).join(".agents").join("skills"));
    let mut current = normalize_cwd(cwd);

    let config_dir = current.join(CONFIG_DIR_NAME);
    if TRUST_REQUIRING_PROJECT_CONFIG_RESOURCES
        .iter()
        .any(|entry| config_dir.join(entry).exists())
    {
        return true;
    }

    loop {
        let agents_skills = current.join(".agents").join("skills");
        if user_agents_skills.as_deref() != Some(agents_skills.as_path()) && agents_skills.exists()
        {
            return true;
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => return false,
        }
    }
}

/// Persistent `trust.json` store rooted at an agent directory.
#[derive(Debug, Clone)]
pub struct ProjectTrustStore {
    trust_path: PathBuf,
}

impl ProjectTrustStore {
    /// Open (or lazily create) the store at `<agent_dir>/trust.json`.
    pub fn new(agent_dir: &Path) -> Self {
        Self {
            trust_path: absolute(agent_dir).join(TRUST_FILE_NAME),
        }
    }

    /// Path of the backing file.
    pub fn path(&self) -> &Path {
        &self.trust_path
    }

    /// Saved decision applying to `cwd`, or `None` when nothing is saved.
    ///
    /// Read errors are surfaced as `Err`; a corrupt store is a real
    /// condition the caller may want to report, not silently "untrusted".
    pub fn get(&self, cwd: &Path) -> Result<ProjectTrustDecision, TrustError> {
        Ok(self.get_entry(cwd)?.map(|entry| entry.decision))
    }

    /// Nearest saved entry (path + decision) applying to `cwd`.
    pub fn get_entry(&self, cwd: &Path) -> Result<Option<ProjectTrustStoreEntry>, TrustError> {
        let _lock = TrustLock::acquire(&self.trust_path)?;
        let data = read_trust_file(&self.trust_path)?;
        Ok(find_nearest_trust_entry(&data, cwd))
    }

    /// Save a decision for `cwd` (`None` removes it).
    pub fn set(&self, cwd: &Path, decision: ProjectTrustDecision) -> Result<(), TrustError> {
        self.set_many(&[ProjectTrustUpdate {
            path: cwd.to_path_buf(),
            decision,
        }])
    }

    /// Save several decisions under one lock acquisition.
    pub fn set_many(&self, decisions: &[ProjectTrustUpdate]) -> Result<(), TrustError> {
        let _lock = TrustLock::acquire(&self.trust_path)?;
        let mut data = read_trust_file(&self.trust_path)?;
        for update in decisions {
            let key = normalize_cwd(&update.path).to_string_lossy().into_owned();
            match update.decision {
                Some(decision) => {
                    data.insert(key, Some(decision));
                }
                None => {
                    data.remove(&key);
                }
            }
        }
        write_trust_file(&self.trust_path, &data)
    }
}

/// Resolve the effective project-trust boolean.
///
/// Order: explicit override, "no trust-requiring resources" short-circuit,
/// saved decision, then [`DefaultProjectTrust`]. `Ask` resolves to
/// `false` here because this entry point has no UI; interactive mode
/// persists a decision through `/trust`.
pub fn resolve_project_trusted(
    cwd: &Path,
    store: &ProjectTrustStore,
    trust_override: Option<bool>,
    default_project_trust: DefaultProjectTrust,
) -> bool {
    if let Some(override_value) = trust_override {
        return override_value;
    }
    if !has_trust_requiring_project_resources(cwd) {
        return true;
    }
    match store.get(cwd) {
        Ok(Some(decision)) => return decision,
        Ok(None) => {}
        // A corrupt store must not crash the agent; treat it as "no
        // decision" so the user still has to opt in explicitly.
        Err(_) => {}
    }
    matches!(default_project_trust, DefaultProjectTrust::Always)
}

/// Read the trust file, returning an empty map when it does not exist.
fn read_trust_file(path: &Path) -> Result<BTreeMap<String, Option<bool>>, TrustError> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let raw = std::fs::read_to_string(path).map_err(|source| TrustError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let parsed: Value =
        serde_json::from_str(strip_bom(&raw)).map_err(|error| TrustError::Invalid {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    let Some(object) = parsed.as_object() else {
        return Err(TrustError::Invalid {
            path: path.to_path_buf(),
            message: "expected an object".to_string(),
        });
    };

    let mut data = BTreeMap::new();
    for (key, value) in object {
        match value {
            Value::Bool(decision) => {
                data.insert(key.clone(), Some(*decision));
            }
            Value::Null => {
                data.insert(key.clone(), None);
            }
            _ => {
                return Err(TrustError::Invalid {
                    path: path.to_path_buf(),
                    message: format!("value for {key:?} must be true, false, or null"),
                });
            }
        }
    }
    Ok(data)
}

/// Write the trust file with sorted keys, two-space indent, and a
/// trailing newline (byte-for-byte the upstream `writeTrustFile` shape).
fn write_trust_file(path: &Path, data: &BTreeMap<String, Option<bool>>) -> Result<(), TrustError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| TrustError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    }
    let mut text = serde_json::to_string_pretty(data).map_err(|error| TrustError::Invalid {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    text.push('\n');
    std::fs::write(path, text).map_err(|source| TrustError::Write {
        path: path.to_path_buf(),
        source,
    })
}

/// Create-exclusive lock file guard.
///
/// Upstream uses `proper-lockfile`; the Rust port only needs mutual
/// exclusion inside one machine, so a `<trust.json>.lock` file created
/// with `create_new` and retried for ~200 ms is enough. The guard removes
/// the lock on drop; a crashed process leaves it behind, which at worst
/// makes a later write wait and then fail.
struct TrustLock {
    lock_path: PathBuf,
}

impl TrustLock {
    fn acquire(trust_path: &Path) -> Result<Self, TrustError> {
        if let Some(parent) = trust_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let lock_path = PathBuf::from(format!("{}.lock", trust_path.display()));
        for attempt in 1..=10 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(_) => return Ok(Self { lock_path }),
                Err(error) if error.kind() == ErrorKind::AlreadyExists && attempt < 10 => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => {
                    return Err(TrustError::Lock { path: lock_path });
                }
            }
        }
        Err(TrustError::Lock { path: lock_path })
    }
}

impl Drop for TrustLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn stores_decisions_and_inherits_from_parent_directories() {
        let temp = tempfile::TempDir::with_prefix("pi-trust-").expect("tempdir");
        let agent_dir = temp.path().join("agent");
        let parent = temp.path().join("trusted-parent");
        let child = parent.join("project");
        fs::create_dir_all(&child).expect("mkdir");

        let store = ProjectTrustStore::new(&agent_dir);
        assert_eq!(store.get(&child).expect("read"), None);

        store.set(&parent, Some(true)).expect("set parent");
        assert_eq!(store.get(&child).expect("read"), Some(true));

        store.set(&child, Some(false)).expect("set child");
        assert_eq!(store.get(&child).expect("read"), Some(false));

        store.set(&child, None).expect("clear child");
        assert_eq!(store.get(&child).expect("read"), Some(true));
    }

    #[test]
    fn saves_one_update_per_path_and_ignores_null_in_lookups() {
        let temp = tempfile::TempDir::with_prefix("pi-trust-many-").expect("tempdir");
        let agent_dir = temp.path().join("agent");
        let project = temp.path().join("project");
        fs::create_dir_all(&project).expect("mkdir");

        let store = ProjectTrustStore::new(&agent_dir);
        store
            .set_many(&[
                ProjectTrustUpdate {
                    path: project.clone(),
                    decision: Some(false),
                },
                ProjectTrustUpdate {
                    path: project.clone(),
                    decision: Some(true),
                },
            ])
            .expect("set many");
        assert_eq!(store.get(&project).expect("read"), Some(true));

        // A file written with an explicit null decodes but does not match.
        fs::write(
            store.path(),
            format!(
                "{{\n  {:?}: null\n}}\n",
                project.canonicalize().unwrap().to_string_lossy()
            ),
        )
        .expect("write");
        assert_eq!(store.get(&project).expect("read"), None);
    }

    #[test]
    fn rejects_a_malformed_store() {
        let temp = tempfile::TempDir::with_prefix("pi-trust-bad-").expect("tempdir");
        let agent_dir = temp.path().join("agent");
        fs::create_dir_all(&agent_dir).expect("mkdir");
        let store = ProjectTrustStore::new(&agent_dir);
        fs::write(store.path(), "{\"/x\": 1}").expect("write");
        assert!(matches!(
            store.get(Path::new("/x")),
            Err(TrustError::Invalid { .. })
        ));
    }

    #[test]
    fn detects_trust_requiring_project_resources() {
        let temp = tempfile::TempDir::with_prefix("pi-trust-resources-").expect("tempdir");
        let home = temp.path().join("home");
        let cwd = temp.path().join("home").join("project");
        fs::create_dir_all(&cwd).expect("mkdir");

        // A user-level `.agents/skills` under $HOME is trusted, not gated.
        fs::create_dir_all(temp.path().join("home/.agents/skills")).expect("mkdir");
        assert!(!has_trust_requiring_project_resources_with_home(
            &cwd,
            Some(&home)
        ));

        fs::create_dir_all(cwd.join(".pi")).expect("mkdir");
        fs::write(cwd.join(".pi/settings.json"), "{}").expect("write");
        assert!(has_trust_requiring_project_resources_with_home(
            &cwd,
            Some(&home)
        ));

        fs::remove_dir_all(cwd.join(".pi")).expect("rm");
        fs::create_dir_all(cwd.join(".agents/skills")).expect("mkdir");
        assert!(has_trust_requiring_project_resources_with_home(
            &cwd,
            Some(&home)
        ));
    }

    #[test]
    fn has_no_trust_requiring_resources_in_an_empty_project() {
        let temp = tempfile::TempDir::with_prefix("pi-trust-empty-").expect("tempdir");
        let home = temp.path().join("home");
        let cwd = temp.path().join("home").join("empty");
        fs::create_dir_all(&cwd).expect("mkdir");
        assert!(!has_trust_requiring_project_resources_with_home(
            &cwd,
            Some(&home)
        ));
    }

    #[test]
    fn resolve_prefers_override_then_saved_decision() {
        let temp = tempfile::TempDir::with_prefix("pi-trust-resolve-").expect("tempdir");
        let agent_dir = temp.path().join("agent");
        let project = temp.path().join("project");
        fs::create_dir_all(project.join(".pi")).expect("mkdir");
        fs::write(project.join(".pi/settings.json"), "{}").expect("write");

        let store = ProjectTrustStore::new(&agent_dir);
        let cwd = project.as_path();

        // No decision, Ask without a UI -> untrusted.
        assert!(!resolve_project_trusted(
            cwd,
            &store,
            None,
            DefaultProjectTrust::Ask
        ));
        // Explicit override wins.
        assert!(resolve_project_trusted(
            cwd,
            &store,
            Some(true),
            DefaultProjectTrust::Never
        ));
        // Saved decision wins over the default.
        store.set(cwd, Some(true)).expect("set");
        assert!(resolve_project_trusted(
            cwd,
            &store,
            None,
            DefaultProjectTrust::Never
        ));
        store.set(cwd, Some(false)).expect("set");
        assert!(!resolve_project_trusted(
            cwd,
            &store,
            None,
            DefaultProjectTrust::Always
        ));
    }

    #[test]
    fn resolve_short_circuits_when_no_resources_need_trust() {
        let temp = tempfile::TempDir::with_prefix("pi-trust-noresources-").expect("tempdir");
        let agent_dir = temp.path().join("agent");
        let project = temp.path().join("project");
        fs::create_dir_all(&project).expect("mkdir");
        let store = ProjectTrustStore::new(&agent_dir);
        assert!(resolve_project_trusted(
            &project,
            &store,
            None,
            DefaultProjectTrust::Ask
        ));
    }
}
