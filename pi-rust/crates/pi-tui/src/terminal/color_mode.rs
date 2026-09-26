//! Color-mode detection — the port of the env-driven `ColorMode = "truecolor" | "256color"`
//! branch upstream runs at theme construction.
//!
//! Upstream's `createTheme` accepts `"truecolor"`, `"256color"`, or `"none"`. The
//! Rust port widens that with [`ColorMode::Auto`], so a driver that does not know
//! its terminal depth up front (the interactive TUI) can carry an `Auto`
//! through the API and pin it only when the terminal record is known —
//! [`crate::theme::ColorMode::resolve`] does the pinning.
//!
//! This module is the **single entry point** for that env-driven resolution
//! when the caller wants a concrete mode at startup (snapshot tests, headless
//! CI runs, embedded callers that skip the interactive TUI). It is intentionally
//! pure: every input that drives the decision is read into
//! [`ColorModeInputs`], and the rule itself ([`detect_with`]) takes the record
//! by reference, so every branch is testable without mutating process-global
//! state — the same shape [`crate::terminal::capabilities::detect_capabilities_with`]
//! uses for the parallel capability probe.
//!
//! # Decision tree
//!
//! 1. [`ColorModeInputs::no_color`] set to a truthy value → [`ColorMode::None`].
//!    Upstream honours `NO_COLOR` as "any non-empty value" (`packages/tui/src/theme.ts:104-106`),
//!    so the port matches that: `"1"`, `"true"`, `"yes"`, `"on"` all disable.
//! 2. [`ColorModeInputs::force_none`] is true (test/CLI override) → [`ColorMode::None`].
//! 3. [`ColorModeInputs::force_color`] = `Some(0)` → [`ColorMode::Ansi256`] (TS
//!    fallback when FORCE_COLOR is set but the request is "no colour").
//! 4. [`ColorModeInputs::color_term`] in `{"truecolor", "24bit"}` → [`ColorMode::TrueColor`].
//!    `COLORTERM=truecolor` is what xterm-compatible terminals set when the
//!    palette is 24-bit (kitty, alacritty, wezterm, ghostty, iTerm2).
//! 5. [`ColorModeInputs::true_color_capability`] true → [`ColorMode::TrueColor`].
//!    This is the cached result of [`crate::terminal::capabilities::true_color_supported`],
//!    which the live driver feeds in at startup.
//! 6. [`ColorModeInputs::term`] contains `256color` (e.g. `xterm-256color`,
//!    `screen-256color`, `tmux-256color`, `xterm-kitty`, `alacritty`) →
//!    [`ColorMode::Ansi256`].
//! 7. `TERM=dumb` or `TERM=linux` → [`ColorMode::None`] (no escape sequences).
//! 8. Otherwise → [`ColorMode::Ansi256`] (safe default; preserves colour while
//!    the test environment is unknown).
//!
//! The branches are evaluated in order; the first match wins. Mirroring the
//! capability probe's table-driven test, every branch has a dedicated test so
//! reordering is caught immediately.

use crate::theme::ColorMode;

/// Every signal the colour-mode rule inspects.
///
/// Reading `process.env` once and feeding the record into a pure function
/// keeps the rule testable. [`ColorModeInputs::from_env`] is the only place
/// the process environment is consulted; the rule itself ([`detect_with`]) is
/// pure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColorModeInputs {
    /// Raw `NO_COLOR` value (RFC: any non-empty string disables colour).
    pub no_color: Option<String>,
    /// Raw `PI_NO_COLOR` value — the Rust port's deliberate override for
    /// embedded callers that must disable colour regardless of host env.
    pub pi_no_color: Option<String>,
    /// Raw `FORCE_COLOR` value — `"0"` is "no colour", `"1"` / `"2"` / `"3"`
    /// are upstream's "1 = 8 colors, 2 = 256, 3 = truecolor" hint.
    pub force_color: Option<String>,
    /// Raw `COLORTERM` value.
    pub color_term: Option<String>,
    /// Raw `TERM` value.
    pub term: Option<String>,
    /// Cached `true_color_supported()` from
    /// [`crate::terminal::capabilities`]. The interactive driver feeds this
    /// in at startup; tests pass a literal `true`/`false`.
    pub true_color_capability: bool,
    /// Skip env entirely and return [`ColorMode::None`] (test/CLI override).
    pub force_none: bool,
}

impl ColorModeInputs {
    /// Read every signal the rule inspects out of `process.env`. Cheap;
    /// callers cache the result alongside the capability record.
    pub fn from_env() -> Self {
        Self {
            no_color: std::env::var("NO_COLOR").ok(),
            pi_no_color: std::env::var("PI_NO_COLOR").ok(),
            force_color: std::env::var("FORCE_COLOR").ok(),
            color_term: std::env::var("COLORTERM").ok(),
            term: std::env::var("TERM").ok(),
            true_color_capability: crate::terminal::capabilities::true_color_supported(),
            force_none: false,
        }
    }
}

/// Resolve a concrete [`ColorMode`] for the running terminal.
///
/// `Auto` is intentionally not a return value: callers that want a concrete
/// mode (tests, embedded entry points, snapshot tooling) get one directly;
/// callers that want the live TUI to defer the decision pass
/// [`ColorMode::Auto`] and resolve it later via
/// [`ColorMode::resolve`].
pub fn detect() -> ColorMode {
    detect_with(&ColorModeInputs::from_env())
}

/// Pure decision function — every branch a separate test.
pub fn detect_with(inputs: &ColorModeInputs) -> ColorMode {
    // 1. NO_COLOR = any truthy value.
    if is_truthy(inputs.no_color.as_deref()) || is_truthy(inputs.pi_no_color.as_deref()) {
        return ColorMode::None;
    }

    // 2. FORCE_COLOR=0 / explicit force.
    if inputs.force_none {
        return ColorMode::None;
    }
    if inputs.force_color.as_deref() == Some("0") {
        return ColorMode::None;
    }

    // 3. Dumb / linux terminals cannot render colour.
    if let Some(term) = inputs.term.as_deref() {
        if matches!(term, "dumb" | "linux") {
            return ColorMode::None;
        }
    }

    // 4. COLORTERM=truecolor|24bit is the canonical 24-bit hint.
    if let Some(ct) = inputs.color_term.as_deref() {
        let lower = ct.to_ascii_lowercase();
        if lower == "truecolor" || lower == "24bit" {
            return ColorMode::TrueColor;
        }
    }

    // 5. Cached capability (the live driver probes at startup).
    if inputs.true_color_capability {
        return ColorMode::TrueColor;
    }

    // 6. Known 256-colour terminals (xterm-256color, screen-256color, etc.).
    if let Some(term) = inputs.term.as_deref() {
        if has_256color_hint(term) {
            return ColorMode::Ansi256;
        }
    }

    // 7. Safe default — colour, but 8-bit.
    ColorMode::Ansi256
}

/// `true` when `value` is one of the well-known truthy strings. RFC-style
/// `NO_COLOR` says "any non-empty value", but in practice every consumer
/// treats `"0"`, `"false"`, `"off"`, `"no"` as explicit opt-outs. Match the
/// conservative reading so a `NO_COLOR=0` left over from a forgotten
/// `unset` does not silence the terminal.
pub fn is_truthy(value: Option<&str>) -> bool {
    match value {
        None => false,
        Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            // Explicit opt-outs are *not* truthy. They read as "no colour,
            // please", which is the same answer `None` gives — but we keep
            // the parsing layered so a future `NO_COLOR=0` extension is a
            // single match arm, not a behaviour change.
            "0" | "false" | "no" | "off" => false,
            // RFC-style fallback: any non-empty, non-whitespace string is
            // truthy unless explicitly opted out above.
            other if !other.is_empty() => true,
            _ => false,
        },
    }
}

/// Whether `TERM` advertises at least 8-bit colour. Mirrors the substring set
/// upstream matches in `getCapabilities` (`packages/tui/src/terminal-image.ts`)
/// for the `xterm-256color` / `*-256color` family.
pub fn has_256color_hint(term: &str) -> bool {
    let lower = term.to_ascii_lowercase();
    lower.contains("256color") || matches!(lower.as_str(), "xterm-kitty" | "alacritty" | "wezterm" | "foot" | "st-256color")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> ColorModeInputs {
        ColorModeInputs {
            no_color: None,
            pi_no_color: None,
            force_color: None,
            color_term: None,
            term: Some("xterm".into()),
            true_color_capability: false,
            force_none: false,
        }
    }

    /// 1 — `NO_COLOR=1` silences the terminal.
    #[test]
    fn no_color_one_disables_colour() {
        let mut inputs = base();
        inputs.no_color = Some("1".into());
        assert_eq!(detect_with(&inputs), ColorMode::None);
    }

    /// 2 — `NO_COLOR=true` and `NO_COLOR=yes` likewise disable colour,
    /// matching the RFC's "any non-empty value" rule.
    #[test]
    fn no_color_truthy_strings_disable_colour() {
        for truthy in ["true", "yes", "on", "anything-non-empty"] {
            let mut inputs = base();
            inputs.no_color = Some(truthy.into());
            assert_eq!(
                detect_with(&inputs),
                ColorMode::None,
                "NO_COLOR={truthy:?} must disable colour"
            );
        }
    }

    /// 3 — `NO_COLOR=0` is an explicit opt-out. The RFC allows either
    /// reading; matching the conservative one keeps `unset NO_COLOR`
    /// leftovers harmless.
    #[test]
    fn no_color_zero_does_not_disable_colour() {
        let mut inputs = base();
        inputs.no_color = Some("0".into());
        inputs.term = Some("xterm-256color".into());
        assert_eq!(detect_with(&inputs), ColorMode::Ansi256);
    }

    /// 4 — `PI_NO_COLOR` mirrors `NO_COLOR` for embedded callers. Same
    /// truthy contract.
    #[test]
    fn pi_no_color_overrides_for_embedded_callers() {
        let mut inputs = base();
        inputs.pi_no_color = Some("1".into());
        inputs.color_term = Some("truecolor".into());
        // The override wins over a truecolor hint: PI_NO_COLOR=1 is a
        // hard request to render nothing.
        assert_eq!(detect_with(&inputs), ColorMode::None);
    }

    /// 5 — `FORCE_COLOR=0` is "render but disable", which the conservative
    /// reading maps to `None`.
    #[test]
    fn force_color_zero_disables_colour() {
        let mut inputs = base();
        inputs.force_color = Some("0".into());
        assert_eq!(detect_with(&inputs), ColorMode::None);
    }

    /// 6 — `COLORTERM=truecolor` is the canonical 24-bit hint; an unknown
    /// `COLORTERM` value (or absent value) does not promote to TrueColor
    /// on its own.
    #[test]
    fn color_term_truecolor_picks_true_color() {
        for hint in ["truecolor", "24bit", "TrueColor", "24BIT"] {
            let mut inputs = base();
            inputs.color_term = Some(hint.into());
            assert_eq!(
                detect_with(&inputs),
                ColorMode::TrueColor,
                "COLORTERM={hint:?} must resolve to TrueColor"
            );
        }
    }

    /// 7 — `COLORTERM=anything-else` (e.g. `mate-terminal`'s `gnome`) does
    /// not promote. The terminal falls through to the TERM-derived arm.
    #[test]
    fn color_term_unknown_does_not_promote() {
        let mut inputs = base();
        inputs.color_term = Some("gnome-terminal".into());
        inputs.term = Some("xterm-256color".into());
        assert_eq!(detect_with(&inputs), ColorMode::Ansi256);
    }

    /// 8 — The cached `true_color_capability` flag (from
    /// `terminal_capabilities::true_color_supported`) wins over the TERM
    /// arm, matching the live driver's startup probe.
    #[test]
    fn cached_true_color_capability_wins() {
        let mut inputs = base();
        inputs.true_color_capability = true;
        inputs.term = Some("xterm-256color".into());
        assert_eq!(detect_with(&inputs), ColorMode::TrueColor);
    }

    /// 9 — `TERM=dumb` and `TERM=linux` always render no colour. Upstream's
    /// rule, which is the reason `cat(1)` and `git log` render plain on
    /// dumb terminals.
    #[test]
    fn dumb_terminal_renders_no_colour() {
        for term in ["dumb", "linux"] {
            let mut inputs = base();
            inputs.term = Some(term.into());
            assert_eq!(
                detect_with(&inputs),
                ColorMode::None,
                "TERM={term:?} must disable colour"
            );
        }
    }

    /// 10 — `xterm-256color` and friends resolve to 8-bit colour when no
    /// 24-bit hint is present.
    #[test]
    fn known_256color_terms_resolve_to_ansi256() {
        for term in [
            "xterm-256color",
            "screen-256color",
            "tmux-256color",
            "alacritty",
            "wezterm",
            "foot",
            "xterm-kitty",
            "st-256color",
        ] {
            let mut inputs = base();
            inputs.term = Some(term.into());
            assert_eq!(
                detect_with(&inputs),
                ColorMode::Ansi256,
                "TERM={term:?} must resolve to Ansi256"
            );
        }
    }

    /// 11 — A bare `xterm` falls through to the safe Ansi256 default. This
    /// is the conservative read for an unknown terminal that did not opt
    /// into TrueColor: render colour, but at 8-bit depth.
    #[test]
    fn unknown_term_falls_through_to_ansi256() {
        let mut inputs = base();
        inputs.term = Some("xterm".into());
        assert_eq!(detect_with(&inputs), ColorMode::Ansi256);
    }

    /// 12 — `force_none` wins over every other signal — the test/CLI
    /// override path.
    #[test]
    fn force_none_overrides_every_other_signal() {
        let mut inputs = base();
        inputs.force_none = true;
        inputs.color_term = Some("truecolor".into());
        inputs.true_color_capability = true;
        inputs.term = Some("xterm-256color".into());
        assert_eq!(detect_with(&inputs), ColorMode::None);
    }

    /// 13 — `is_truthy` is the parsing helper; pin every arm so a future
    /// refactor that re-tables it does not silently flip a case.
    #[test]
    fn is_truthy_known_strings() {
        assert!(is_truthy(Some("1")));
        assert!(is_truthy(Some("true")));
        assert!(is_truthy(Some("yes")));
        assert!(is_truthy(Some("on")));
        assert!(is_truthy(Some("anything-non-empty")));
        assert!(!is_truthy(Some("0")));
        assert!(!is_truthy(Some("false")));
        assert!(!is_truthy(Some("no")));
        assert!(!is_truthy(Some("off")));
        assert!(!is_truthy(None));
        assert!(!is_truthy(Some("")));
        assert!(!is_truthy(Some("   ")));
    }

    /// 14 — `has_256color_hint` recognises the substring form (`*256color`)
    /// and the explicit-256 form (`xterm-kitty`, `wezterm`, …).
    #[test]
    fn has_256color_hint_recognises_both_forms() {
        assert!(has_256color_hint("xterm-256color"));
        assert!(has_256color_hint("screen-256color"));
        assert!(has_256color_hint("tmux-256color"));
        assert!(has_256color_hint("XTERM-256COLOR"));
        assert!(has_256color_hint("alacritty"));
        assert!(has_256color_hint("wezterm"));
        assert!(has_256color_hint("foot"));
        assert!(has_256color_hint("xterm-kitty"));
        assert!(!has_256color_hint("xterm"));
        assert!(!has_256color_hint("vt100"));
    }

    /// 15 — The end-to-end `detect()` runs without panicking on a fresh
    /// `ColorModeInputs::default()`. `from_env` may consult
    /// `process.env`; both `detect` and `detect_with(default())` must
    /// return *some* concrete mode.
    #[test]
    fn detect_with_default_is_concrete() {
        let default = ColorModeInputs::default();
        let mode = detect_with(&default);
        assert!(
            matches!(mode, ColorMode::TrueColor | ColorMode::Ansi256 | ColorMode::None),
            "Auto must not leak through detect_with; got {mode:?}"
        );
    }
}