//! Integration tests for the experimental feature flag framework.
//!
//! Mirrors the surface contract from
//! `packages/coding-agent/src/core/experimental.ts`. The Rust module is
//! `pi_coding_agent::experimental`; this integration test pins the
//! public shape so a downstream manifest check can rely on it.
//!
//! Test override plumbing is identical to the unit tests inside the
//! module; we set the override, exercise the public function, and let
//! the `Drop` guard restore the default.

#![cfg(not(target_arch = "wasm32"))]

use pi_coding_agent::experimental::{
    experimental_features_enabled, resolve_flag, set_experimental_override, KNOWN_FLAGS,
    PI_EXPERIMENTAL_ENV,
};

struct OverrideGuard(Option<bool>);

impl OverrideGuard {
    fn new(value: Option<bool>) -> Self {
        set_experimental_override(value);
        Self(value)
    }
}

impl Drop for OverrideGuard {
    fn drop(&mut self) {
        set_experimental_override(None);
    }
}

#[test]
fn override_true_enables_the_gate() {
    let _guard = OverrideGuard::new(Some(true));
    assert!(experimental_features_enabled());
}

#[test]
fn override_false_keeps_the_gate_closed() {
    let _guard = OverrideGuard::new(Some(false));
    assert!(!experimental_features_enabled());
}

#[test]
fn override_clear_returns_to_default() {
    let _guard = OverrideGuard::new(None);
    // We deliberately do not mutate the env var (the crate forbids
    // `unsafe`); the default branch in production consults the env,
    // and the unit tests cover that path directly.
    assert!(!experimental_features_enabled() || std::env::var_os(PI_EXPERIMENTAL_ENV).is_some());
}

#[test]
fn resolve_known_flag_tracks_the_gate() {
    for override_value in [Some(true), Some(false)] {
        let _guard = OverrideGuard::new(override_value);
        let expected = override_value.unwrap_or(false);
        for name in KNOWN_FLAGS {
            assert_eq!(
                resolve_flag(name),
                Some(expected),
                "flag {name} must mirror the global gate"
            );
        }
    }
}

#[test]
fn resolve_unknown_flag_is_none() {
    let _guard = OverrideGuard::new(Some(true));
    assert_eq!(resolve_flag("not-a-real-flag"), None);
    assert_eq!(resolve_flag(""), None);
}

#[test]
fn known_flags_match_the_documented_surface() {
    // Pin the contract — the CI required_api check enumerates this list.
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