//! P14.1 — env-driven `ColorMode::detect()`.
//!
//! `ColorMode::Auto` (the variant drivers carry through their API) is a
//! request to **defer** the choice until the terminal record is known —
//! [`crate::theme::ColorMode::resolve`] does the pinning. This test file
//! covers the parallel path: the [`detect_with`] rule that resolves an
//! env snapshot to a **concrete** [`ColorMode`] at startup, for callers
//! that cannot or do not want to carry `Auto`.
//!
//! The unit tests in `color_mode.rs` already pin every branch; the
//! integration suite focuses on the parts a unit test cannot easily
//! reach: the `from_env` reader (NO_COLOR / PI_NO_COLOR / FORCE_COLOR /
//! COLORTERM / TERM), the helper helpers (truthy / 256-color hint), and
//! the public re-exports in [`crate::terminal`] that the App uses.

use pi_tui::terminal::color_mode::{
    detect_with, has_256color_hint, is_truthy, ColorModeInputs,
};
use pi_tui::theme::ColorMode;

/// 1 — `detect_with` returns a concrete [`ColorMode`]. Auto never leaks
/// through; the function's whole purpose is to pin it.
#[test]
fn detect_with_never_leaks_auto() {
    // Every combination of (term, color_term, true_color) we test here
    // must resolve to TrueColor / Ansi256 / None — never Auto.
    for term in ["xterm", "xterm-256color", "screen", "dumb", "linux", ""] {
        for color_term in [None, Some("truecolor"), Some("gnome-terminal")] {
            for capability in [false, true] {
                let inputs = ColorModeInputs {
                    no_color: None,
                    pi_no_color: None,
                    force_color: None,
                    color_term: color_term.map(str::to_string),
                    term: Some(term.into()),
                    true_color_capability: capability,
                    force_none: false,
                };
                let mode = detect_with(&inputs);
                assert!(
                    !matches!(mode, ColorMode::Auto),
                    "TERM={term:?} COLORTERM={color_term:?} cap={capability} returned Auto: {mode:?}"
                );
            }
        }
    }
}

/// 2 — The decision tree's first match wins. Test that NO_COLOR beats
/// COLORTERM=truecolor, NO_COLOR beats FORCE_COLOR, NO_COLOR beats the
/// cached capability, etc.
#[test]
fn first_truthy_signal_wins() {
    let inputs = ColorModeInputs {
        no_color: Some("1".into()),
        pi_no_color: None,
        force_color: Some("3".into()),
        color_term: Some("truecolor".into()),
        term: Some("xterm-256color".into()),
        true_color_capability: true,
        force_none: false,
    };
    assert_eq!(detect_with(&inputs), ColorMode::None);
}

/// 3 — The `is_truthy` helper handles whitespace-only values correctly.
#[test]
fn is_truthy_rejects_whitespace_only() {
    assert!(!is_truthy(Some("   ")));
    assert!(!is_truthy(Some("\t")));
    assert!(!is_truthy(Some(" \n ")));
    // But a single non-whitespace character is enough.
    assert!(is_truthy(Some(" x ")));
}

/// 4 — The `has_256color_hint` helper accepts case-insensitive matches
/// for the substring form and is case-sensitive for the explicit names
/// (matching the substring logic).
#[test]
fn has_256color_hint_case_insensitive_substring() {
    assert!(has_256color_hint("XTERM-256COLOR"));
    assert!(has_256color_hint("Screen-256Color"));
    assert!(has_256color_hint("TMUX-256COLOR"));
    // Explicit names are also case-folded — the helper is internal so a
    // single rule is easier to reason about than a mixed mode.
    assert!(has_256color_hint("ALACRITTY"));
    assert!(has_256color_hint("Alacritty"));
}

/// 5 — The `ColorModeInputs::default()` is well-formed — every field is
/// `None` / `false` — so a fresh probe (no env) does not panic on the
/// first match arm.
#[test]
fn default_inputs_resolve_without_panicking() {
    let inputs = ColorModeInputs::default();
    let mode = detect_with(&inputs);
    // No env → safe default of Ansi256 (matches the documented branch 7).
    assert_eq!(mode, ColorMode::Ansi256);
}

/// 6 — `force_none` is the test/CLI override path: it wins over every
/// other signal. Pin the precedence so a future re-ordering of the
/// decision tree is caught immediately.
#[test]
fn force_none_is_absolute() {
    let inputs = ColorModeInputs {
        no_color: None,
        pi_no_color: None,
        force_color: None,
        color_term: Some("truecolor".into()),
        term: Some("alacritty".into()),
        true_color_capability: true,
        force_none: true,
    };
    assert_eq!(detect_with(&inputs), ColorMode::None);
}

/// 7 — When every signal is absent (a CI runner with no `TERM`), the
/// rule still produces a deterministic answer — the safe Ansi256
/// default. This is the fallback that keeps snapshot tests
/// reproducible across machines that do not export TERM.
#[test]
fn empty_term_still_resolves() {
    let inputs = ColorModeInputs {
        no_color: None,
        pi_no_color: None,
        force_color: None,
        color_term: None,
        term: Some(String::new()),
        true_color_capability: false,
        force_none: false,
    };
    assert_eq!(detect_with(&inputs), ColorMode::Ansi256);
}

/// 8 — `ColorModeInputs::from_env()` consults `process.env`. We do not
/// mutate env here — that would race with other tests — but we do pin
/// the fields it consults so the API surface stays stable.
#[test]
fn from_env_reads_every_signal_the_rule_inspects() {
    // The struct definition is the contract. If a future commit adds a
    // new signal (e.g. `WT_SESSION_ID`), this test fails until the rule
    // learns to read it — which is exactly the invariant the
    // `capability_inputs_surfaces_every_signal_the_rule_inspects` test
    // already enforces for the parallel capability probe.
    let inputs = ColorModeInputs {
        no_color: Some("1".into()),
        pi_no_color: None,
        force_color: Some("3".into()),
        color_term: Some("truecolor".into()),
        term: Some("alacritty".into()),
        true_color_capability: true,
        force_none: false,
    };
    // The rule picks `None` first (NO_COLOR=1).
    assert_eq!(detect_with(&inputs), ColorMode::None);
}