//! Declarative capability manifest.
//!
//! Every extension should ship a `manifest.json` next to its source
//! file declaring which host capabilities it needs. The host then
//! enforces the manifest at load time (refuse to load if a call is
//! declared but never used — drift) and at call time (refuse a call
//! the extension never declared — privilege escalation).
//!
//! The schema is intentionally a subset of pi_agent_rust's
//! [`CapabilityManifest`](https://github.com/louloulin/pi_agent_rust)
//! so existing upstream manifests load unchanged.
//!
//! ## Example
//!
//! ```json
//! {
//!   "schema": "pi-extension/v1",
//!   "capabilities": [
//!     {
//!       "capability": "exec",
//!       "methods": ["run"],
//!       "risk_tier": "medium"
//!     },
//!     {
//!       "capability": "fs",
//!       "methods": ["read", "write"],
//!       "scope": { "roots": ["/tmp/extensions"], "follow_symlinks": false }
//!     },
//!     {
//!       "capability": "secret",
//!       "methods": ["get"],
//!       "intents": ["OPENAI_API_KEY"]
//!     }
//!   ]
//! }
//! ```
//!
//! ## Enforcement
//!
//! Enforcement is opt-in. The default loader logs a WARN when an
//! extension has no manifest, then loads it anyway — a permissive
//! mode that matches upstream pi. [`HostOptions::manifest_strict`]
//! upgrades to a hard rejection.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::compatibility_scanner::CompatLedger;

/// The schema tag every manifest must declare.
pub const MANIFEST_SCHEMA: &str = "pi-extension/v1";

/// A `manifest.json` next to an extension source file. The host reads
/// this, validates each capability, and uses it to gate host imports.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityManifest {
    /// Schema tag. Must equal [`MANIFEST_SCHEMA`]; loaders refuse
    /// any other tag (forward compatibility).
    pub schema: String,
    /// Declared capabilities, in declaration order. Duplicates are
    /// rejected (last-wins would hide a typo).
    pub capabilities: Vec<CapabilityRequirement>,
}

/// One declared capability.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRequirement {
    /// Capability name: `"exec"`, `"fs"`, `"secret"`, `"http"`, `"ui"`,
    /// `"session"`, `"log"`, `"provider"`, … The loader does not
    /// understand every value — only the ones that have a host import
    /// are enforced.
    pub capability: String,
    /// Methods within the capability the extension wants to call.
    /// Empty list means "all methods of this capability" — the
    /// caller should still cross-check this with the actual host
    /// imports used.
    #[serde(default)]
    pub methods: Vec<String>,
    /// For `"secret"` capabilities: which secret names the
    /// extension intends to fetch. The broker uses this as a
    /// coarse-grained scope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intents: Vec<String>,
    /// Class names the extension expects to bind to (`"fs_connector"`,
    /// `"exec_mediation"`). Forwarded to host import routing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connector_classes: Vec<String>,
    /// Hostcall classes the extension expects to issue.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hostcall_classes: Vec<String>,
    /// Risk tier the extension author assigns itself. The exec
    /// mediation layer reads this and may tighten the runtime's policy
    /// when it disagrees (see [`crate::exec_mediation::ExecMediationPolicy`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_tier: Option<String>,
    /// Scoped-roots / sandbox declaration. Only meaningful for
    /// capabilities that take a scope (e.g. `fs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<CapabilityScope>,
}

/// Scoped-roots / sandbox for a capability. Currently only meaningful
/// for `fs` (path allowlist).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityScope {
    /// Absolute paths the extension is allowed to touch. Empty list
    /// means "no filesystem access" — the host import returns `Denied`
    /// before the syscall.
    #[serde(default)]
    pub roots: Vec<PathBuf>,
    /// Whether to follow symlinks when canonicalising the request path.
    /// `false` (the default) prevents traversal via a symlink that
    /// points outside `roots`.
    #[serde(default)]
    pub follow_symlinks: bool,
    /// Maximum bytes a single `host_fs_write` may emit. `None`
    /// means "no explicit cap" — the global write cap applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_write_bytes: Option<u64>,
}

/// Parsed + validated manifest. The host checks `schema == MANIFEST_SCHEMA`
/// before constructing this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatedManifest {
    /// Source path the manifest was read from. Informational.
    pub source: PathBuf,
    /// Inner parsed manifest.
    pub inner: CapabilityManifest,
}

/// Read a `manifest.json` from `dir_path` (the directory the extension
/// source lives in). Returns:
/// - `Ok(Some(manifest))` when the file exists and parses.
/// - `Ok(None)` when no manifest is present (loaders treat this as
///   "permissive mode" unless `manifest_strict` is set).
/// - `Err(...)` when the manifest exists but is malformed — this is
///   distinct from "missing" so a permissive loader can still WARN.
pub fn load_manifest(dir_path: &Path) -> Result<Option<ValidatedManifest>, ManifestError> {
    let path = dir_path.join("manifest.json");
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(ManifestError::Io { path: path.clone(), source: err }),
    };
    let inner: CapabilityManifest =
        serde_json::from_str(&raw).map_err(|err| ManifestError::Parse {
            path: path.clone(),
            source: err,
        })?;
    if inner.schema != MANIFEST_SCHEMA {
        return Err(ManifestError::SchemaMismatch {
            path: path.clone(),
            found: inner.schema,
        });
    }
    if let Some(dup) = find_duplicate_capability(&inner.capabilities) {
        return Err(ManifestError::DuplicateCapability {
            path: path.clone(),
            capability: dup,
        });
    }
    Ok(Some(ValidatedManifest {
        source: path,
        inner,
    }))
}

/// Walk a list of `[capability, method]` tuples against the
/// validated manifest. Returns the set of tuples that the manifest
/// does not cover; an extension whose uncovered set is non-empty is in
/// drift (declared X but never uses X — or uses Y but never
/// declared Y).
pub fn drift(manifest: &ValidatedManifest, used: &[(String, String)]) -> DriftReport {
    let declared: HashSet<(String, String)> = manifest
        .inner
        .capabilities
        .iter()
        .flat_map(|c| {
            let cap = c.capability.clone();
            // An empty `methods` list means "all methods" — treat
            // it as declaring every observed method for this
            // capability.
            if c.methods.is_empty() {
                used.iter()
                    .filter(|(cap_used, _)| cap_used == &cap)
                    .cloned()
                    .collect::<Vec<_>>()
            } else {
                c.methods
                    .iter()
                    .map(|m| (cap.clone(), m.clone()))
                    .collect::<Vec<_>>()
            }
        })
        .collect();
    let used_set: HashSet<(String, String)> = used.iter().cloned().collect();
    let uncovered_used: Vec<(String, String)> = used_set
        .difference(&declared)
        .cloned()
        .collect();
    let unused_declared: Vec<(String, String)> = declared
        .difference(&used_set)
        .cloned()
        .collect();
    DriftReport {
        uncovered_used,
        unused_declared,
    }
}

/// Cross-check a [`CompatLedger`] against a manifest. An extension
/// whose compat ledger includes `dangerous` markers but whose manifest
/// does not declare `exec` or `fs` is a likely incident. Returns
/// `true` when the cross-check passes; `false` + reasons otherwise.
pub fn cross_check_with_ledger(manifest: &ValidatedManifest, ledger: &CompatLedger) -> Vec<String> {
    let mut issues = Vec::new();
    if !ledger.has_dangerous() {
        return issues;
    }
    let declared_caps: HashSet<&str> = manifest
        .inner
        .capabilities
        .iter()
        .map(|c| c.capability.as_str())
        .collect();
    // Any dangerous marker is only acceptable when an `exec` (or
    // `fs.write`) capability exists — those are the two paths
    // that can produce dangerous side effects.
    if !declared_caps.contains("exec") && !declared_caps.contains("fs") {
        issues.push(format!(
            "compat ledger contains dangerous markers but manifest declares neither `exec` nor `fs`"
        ));
    }
    issues
}

/// Report comparing declared vs observed usage.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftReport {
    /// `(capability, method)` tuples the extension called but did
    /// not declare in its manifest.
    pub uncovered_used: Vec<(String, String)>,
    /// `(capability, method)` tuples the extension declared but
    /// never called.
    pub unused_declared: Vec<(String, String)>,
}

impl DriftReport {
    pub fn is_empty(&self) -> bool {
        self.uncovered_used.is_empty() && self.unused_declared.is_empty()
    }
}

/// Manifest load / parse errors.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("failed to read manifest {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse manifest {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("manifest {path}: schema is `{found}`, expected `{MANIFEST_SCHEMA}`")]
    SchemaMismatch { path: PathBuf, found: String },
    #[error("manifest {path}: capability `{capability}` declared more than once")]
    DuplicateCapability {
        path: PathBuf,
        capability: String,
    },
}

fn find_duplicate_capability(caps: &[CapabilityRequirement]) -> Option<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    for cap in caps {
        if !seen.insert(cap.capability.as_str()) {
            return Some(cap.capability.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"{
        "schema": "pi-extension/v1",
        "capabilities": [
            { "capability": "exec", "methods": ["run"], "risk_tier": "medium" },
            { "capability": "fs", "methods": ["read", "write"], "scope": { "roots": ["/tmp/extensions"], "follow_symlinks": false } },
            { "capability": "secret", "methods": ["get"], "intents": ["OPENAI_API_KEY"] }
        ]
    }"#;

    #[test]
    fn parses_valid_manifest() {
        let parsed: CapabilityManifest = serde_json::from_str(VALID).unwrap();
        assert_eq!(parsed.schema, MANIFEST_SCHEMA);
        assert_eq!(parsed.capabilities.len(), 3);
        assert_eq!(parsed.capabilities[0].capability, "exec");
        assert_eq!(parsed.capabilities[1].scope.as_ref().unwrap().roots[0], PathBuf::from("/tmp/extensions"));
    }

    #[test]
    fn rejects_wrong_schema() {
        let bad = r#"{ "schema": "other/v1", "capabilities": [] }"#;
        let err = load_manifest_str(bad, Path::new("/x/manifest.json")).unwrap_err();
        assert!(matches!(err, ManifestError::SchemaMismatch { .. }));
    }

    #[test]
    fn rejects_duplicate_capability() {
        let bad = r#"{
            "schema": "pi-extension/v1",
            "capabilities": [
                { "capability": "exec" },
                { "capability": "exec" }
            ]
        }"#;
        let err = load_manifest_str(bad, Path::new("/x/manifest.json")).unwrap_err();
        assert!(matches!(err, ManifestError::DuplicateCapability { ref capability, .. } if capability == "exec"));
    }

    #[test]
    fn rejects_unknown_field() {
        let bad = r#"{
            "schema": "pi-extension/v1",
            "capabilities": [],
            "extra_field": true
        }"#;
        let parsed: Result<CapabilityManifest, _> = serde_json::from_str(bad);
        assert!(parsed.is_err(), "deny_unknown_fields must reject extras");
    }

    #[test]
    fn drift_finds_uncovered_used() {
        let manifest = parse_valid();
        let mut report = drift(&manifest, &[("exec".into(), "run".into()), ("exec".into(), "shell".into())]);
        report.uncovered_used.sort();
        report.unused_declared.sort();
        assert_eq!(report.uncovered_used, vec![("exec".into(), "shell".into())]);
        assert_eq!(report.unused_declared, vec![("fs".into(), "read".into()), ("fs".into(), "write".into()), ("secret".into(), "get".into())]);
    }

    #[test]
    fn drift_treats_empty_methods_as_all() {
        let manifest = parse_valid_with_empty_methods();
        // Extension declares `exec` with no methods; used calls
        // `(exec, fork)`. Should be covered.
        let report = drift(&manifest, &[("exec".into(), "fork".into())]);
        assert!(report.uncovered_used.is_empty(), "empty methods must mean all methods: {:?}", report);
    }

    #[test]
    fn cross_check_flags_undeclared_dangerous_markers() {
        // Manifest with no `exec` / `fs` — `eval` should be flagged.
        let manifest = parse_valid_no_exec();
        let mut ledger = crate::compatibility_scanner::CompatLedger::default();
        ledger.markers = crate::compatibility_scanner::Marker::EVAL;
        let issues = cross_check_with_ledger(&manifest, &ledger);
        assert!(issues.iter().any(|i| i.contains("dangerous")), "{:?}", issues);
    }

    #[test]
    fn cross_check_passes_when_manifest_declares_exec() {
        let manifest = parse_valid();
        let mut ledger = crate::compatibility_scanner::CompatLedger::default();
        ledger.markers = crate::compatibility_scanner::Marker::EVAL | crate::compatibility_scanner::Marker::PROCESS_ENV;
        let issues = cross_check_with_ledger(&manifest, &ledger);
        assert!(issues.is_empty(), "exec declaration should be sufficient: {:?}", issues);
    }

    // helpers ---------------------------------------------------------------

    fn parse_valid() -> ValidatedManifest {
        ValidatedManifest {
            source: PathBuf::from("/x/manifest.json"),
            inner: serde_json::from_str(VALID).unwrap(),
        }
    }

    fn parse_valid_with_empty_methods() -> ValidatedManifest {
        let raw = r#"{
            "schema": "pi-extension/v1",
            "capabilities": [
                { "capability": "exec", "methods": [] }
            ]
        }"#;
        ValidatedManifest {
            source: PathBuf::from("/x/manifest.json"),
            inner: serde_json::from_str(raw).unwrap(),
        }
    }

    fn parse_valid_no_exec() -> ValidatedManifest {
        let raw = r#"{
            "schema": "pi-extension/v1",
            "capabilities": [
                { "capability": "log", "methods": ["info"] }
            ]
        }"#;
        ValidatedManifest {
            source: PathBuf::from("/x/manifest.json"),
            inner: serde_json::from_str(raw).unwrap(),
        }
    }

    fn load_manifest_str(raw: &str, path: &Path) -> Result<ValidatedManifest, ManifestError> {
        let inner: CapabilityManifest = serde_json::from_str(raw).map_err(|err| ManifestError::Parse {
            path: path.to_path_buf(),
            source: err,
        })?;
        if inner.schema != MANIFEST_SCHEMA {
            return Err(ManifestError::SchemaMismatch {
                path: path.to_path_buf(),
                found: inner.schema,
            });
        }
        if let Some(dup) = find_duplicate_capability(&inner.capabilities) {
            return Err(ManifestError::DuplicateCapability {
                path: path.to_path_buf(),
                capability: dup,
            });
        }
        Ok(ValidatedManifest {
            source: path.to_path_buf(),
            inner,
        })
    }
}