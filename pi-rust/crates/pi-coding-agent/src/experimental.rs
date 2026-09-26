//! Experimental feature flag framework.
//!
//! Mirrors `core/experimental.ts` from the TS port: features that are
//! not part of the supported surface live behind a single
//! `PI_EXPERIMENTAL=1` environment variable. The framework is intentionally
//! minimal — a boolean gate plus a typed list of known flag names so a
//! feature can advertise itself in one place and the agent can refuse to
//! run when a flag a caller asked for is unknown.
//!
//! Tests inject the override through [`set_experimental_override`] the
//! same way [`crate::file_processor`] injects the TTY state; the env
//! path is only consulted when the override is `None`.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicI8, Ordering};

/// Environment variable consulted when no test override is set.
pub const PI_EXPERIMENTAL_ENV: &str = "PI_EXPERIMENTAL";

/// The set of well-known experimental flags. Adding a flag here is a
/// commitment to ship a working implementation before the next minor
/// release — an unknown flag must fail loudly so callers do not silently
/// drift onto a partial surface.
pub const KNOWN_FLAGS: &[&str] = &[
    "client-tui-chat",
    "radius-auth",
    "radius-relay",
    "mini-runtime",
];

/// True when experimental features are enabled.
///
/// Order of precedence (matching the TS port's `areExperimentalFeaturesEnabled`):
///  1. The test override (set with [`set_experimental_override`]).
///  2. The `PI_EXPERIMENTAL` environment variable, treated as `true` when
///     it equals `"1"`.
pub fn experimental_features_enabled() -> bool {
    if let Some(value) = experimental_override() {
        return value;
    }
    experimental_from_env(&read_env_var())
}

/// Read the env-var-driven branch in isolation so unit tests can drive
/// every case without touching the process environment.
fn experimental_from_env(value: &Option<String>) -> bool {
    value.as_deref().map(|raw| raw == "1").unwrap_or(false)
}

fn read_env_var() -> Option<String> {
    std::env::var(PI_EXPERIMENTAL_ENV).ok()
}

/// Resolve a single named flag against the [`KNOWN_FLAGS`] table.
///
/// Returns:
///
/// * `Some(true)` — the flag is known *and* experimental features are
///   enabled.
/// * `Some(false)` — the flag is known but experimental features are off.
/// * `None` — the flag is unknown; callers should refuse to run rather
///   than silently treat it as off.
pub fn resolve_flag(name: &str) -> Option<bool> {
    if !KNOWN_FLAGS.contains(&name) {
        return None;
    }
    Some(experimental_features_enabled())
}

// ---------------------------------------------------------------------------
// Test override plumbing — mirrors `set_stdin_tty_override` so the test
// suite does not have to mutate the process environment.
// ---------------------------------------------------------------------------

const STATE_UNSET: i8 = 0;
const STATE_FORCE_ON: i8 = 1;
const STATE_FORCE_OFF: i8 = 2;

static EXPERIMENTAL_OVERRIDE: AtomicI8 = AtomicI8::new(STATE_UNSET);

/// Override the value of [`experimental_features_enabled`] for tests.
/// Pass `Some(true)` to force the gate open, `Some(false)` to force it
/// closed, or `None` to clear the override and fall back to the env var.
pub fn set_experimental_override(value: Option<bool>) {
    let state = match value {
        None => STATE_UNSET,
        Some(true) => STATE_FORCE_ON,
        Some(false) => STATE_FORCE_OFF,
    };
    EXPERIMENTAL_OVERRIDE.store(state, Ordering::SeqCst);
}

fn experimental_override() -> Option<bool> {
    match EXPERIMENTAL_OVERRIDE.load(Ordering::SeqCst) {
        STATE_FORCE_ON => Some(true),
        STATE_FORCE_OFF => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clear_override() {
        set_experimental_override(None);
    }

    struct OverrideGuard(Option<bool>);

    impl OverrideGuard {
        fn new(value: Option<bool>) -> Self {
            set_experimental_override(value);
            Self(value)
        }
    }

    impl Drop for OverrideGuard {
        fn drop(&mut self) {
            clear_override();
        }
    }

    #[test]
    fn override_on_short_circuits_the_env() {
        let _guard = OverrideGuard::new(Some(true));
        // Even with PI_EXPERIMENTAL unset / "0" the override wins.
        assert!(experimental_features_enabled());
    }

    #[test]
    fn override_off_keeps_the_gate_closed() {
        let _guard = OverrideGuard::new(Some(false));
        assert!(!experimental_features_enabled());
    }

    #[test]
    fn env_branch_resolves_one_to_enabled() {
        // The env-reading helper takes the value as an argument so the
        // test does not have to mutate the process environment (the
        // crate forbids `unsafe`).
        assert!(experimental_from_env(&Some("1".to_string())));
        assert!(!experimental_from_env(&Some("0".to_string())));
        assert!(!experimental_from_env(&Some(String::new())));
        assert!(!experimental_from_env(&None));
    }

    #[test]
    fn clear_override_falls_back_to_env() {
        let _guard = OverrideGuard::new(None);
        // Without the env var set the gate stays closed.
        assert!(!experimental_features_enabled());
    }

    #[test]
    fn known_flag_resolves_to_gate_state() {
        let _guard = OverrideGuard::new(Some(true));
        assert_eq!(resolve_flag("client-tui-chat"), Some(true));
        let _guard = OverrideGuard::new(Some(false));
        assert_eq!(resolve_flag("client-tui-chat"), Some(false));
    }

    #[test]
    fn unknown_flag_returns_none() {
        // An unknown flag must surface as `None` so the caller can refuse
        // to run rather than silently treat it as off.
        let _guard = OverrideGuard::new(Some(true));
        assert_eq!(resolve_flag("not-a-real-flag"), None);
    }

    #[test]
    fn known_flags_table_is_stable() {
        // The list itself is part of the public contract: a downstream
        // manifest check enumerates it. Pin the order so an accidental
        // reordering opens a CI issue instead of breaking silently.
        assert_eq!(
            KNOWN_FLAGS,
            &[
                "client-tui-chat",
                "radius-auth",
                "radius-relay",
                "mini-runtime",
            ]
        );
    }
}