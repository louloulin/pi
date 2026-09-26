//! Manifest-level `required_api` validator.
//!
//! Extensions declare their dependencies in the manifest:
//!
//! ```json
//! {
//!     "name": "my-extension",
//!     "required_api": ["ui.setWidget", "registerShortcut", "turn_start"]
//! }
//! ```
//!
//! At load time [`validate_required_api`] compares the declared list
//! against [`STABLE_API`](crate::extensions::api_surface::STABLE_API) and
//! returns the names that are not (yet) implemented on this build. The
//! caller is expected to fail-fast and surface a clear error, so a
//! plugin author immediately knows whether their target API exists in
//! the version they are loading against — instead of discovering it
//! at runtime when a call lands on a `None` / unimplemented method.
//!
//! The check is intentionally cheap (`Vec<String>` comparison); a
//! 100-entry manifest runs in microseconds. No glob / prefix matching
//! is supported today; exact strings are unambiguous and match the way
//! upstream enumerates them.

use crate::extensions::api_surface::missing;

/// Result of a [`validate_required_api`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredApiReport {
    /// Every entry declared in the manifest, preserved verbatim so the
    /// error message can show what the author actually wrote.
    pub declared: Vec<String>,
    /// Subset of `declared` that is not part of
    /// [`STABLE_API`](crate::extensions::api_surface::STABLE_API).
    /// Empty when the manifest is satisfiable on this build.
    pub missing: Vec<String>,
}

impl RequiredApiReport {
    /// True when every declared name is implemented.
    pub fn is_satisfied(&self) -> bool {
        self.missing.is_empty()
    }
}

impl std::fmt::Display for RequiredApiReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_satisfied() {
            return write!(f, "required_api satisfied ({} entries)", self.declared.len());
        }
        write!(
            f,
            "required_api missing {} of {} declared entries: {:?}",
            self.missing.len(),
            self.declared.len(),
            self.missing
        )
    }
}

impl std::error::Error for RequiredApiReport {}

/// Validate an extension manifest's `required_api` list.
///
/// `declared` is the raw value from the manifest. Duplicates are
/// de-duplicated before the comparison so a typo + a spelling of the
/// same name does not show up twice in the error.
///
/// Returns a [`RequiredApiReport`] whose `is_satisfied()` answers the
/// manifest's central question. The caller is free to either ignore
/// the missing entries (extension is allowed to load, individual
/// methods will fail later) or to fail-fast (the recommended path —
/// see `ExtensionLoader`).
pub fn validate_required_api(declared: &[String]) -> RequiredApiReport {
    let mut deduped: Vec<String> = declared.to_vec();
    deduped.sort();
    deduped.dedup();
    let missing = missing(&deduped);
    RequiredApiReport {
        declared: deduped,
        missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_manifest_is_satisfied() {
        let report = validate_required_api(&[]);
        assert!(report.is_satisfied());
        assert!(report.missing.is_empty());
    }

    #[test]
    fn fully_supported_manifest_is_satisfied() {
        let report = validate_required_api(&[
            "turn_start".to_string(),
            "ui.setWidget".to_string(),
            "ui.registerShortcut".to_string(),
        ]);
        assert!(report.is_satisfied(), "{report}");
        assert!(report.missing.is_empty());
    }

    #[test]
    fn unknown_name_is_reported_as_missing() {
        let report = validate_required_api(&["ui.setWidget".into(), "ui.notARealMethod".into()]);
        assert!(!report.is_satisfied());
        assert_eq!(report.missing, vec!["ui.notARealMethod".to_string()]);
    }

    #[test]
    fn duplicates_are_deduped() {
        let report = validate_required_api(&[
            "turn_start".into(),
            "turn_start".into(),
            "turn_start".into(),
        ]);
        assert_eq!(report.declared, vec!["turn_start".to_string()]);
        assert!(report.is_satisfied());
    }

    #[test]
    fn declared_keeps_only_one_copy_per_missing_name() {
        let report = validate_required_api(&[
            "ui.noSuchMethod".into(),
            "ui.noSuchMethod".into(),
            "ui.alsoMissing".into(),
        ]);
        assert_eq!(
            report.missing,
            vec!["ui.alsoMissing".to_string(), "ui.noSuchMethod".to_string()]
        );
        assert_eq!(
            report.declared,
            vec![
                "ui.alsoMissing".to_string(),
                "ui.noSuchMethod".to_string(),
            ]
        );
    }

    #[test]
    fn display_form_includes_missing_count() {
        let report = validate_required_api(&["ui.noSuchMethod".into()]);
        let display = format!("{report}");
        assert!(display.contains("missing"), "{display}");
        assert!(display.contains("ui.noSuchMethod"), "{display}");
    }

    #[test]
    fn display_form_for_satisfied_report() {
        let report = validate_required_api(&["turn_start".into()]);
        assert!(format!("{report}").contains("satisfied"));
    }
}