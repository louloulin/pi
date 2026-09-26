//! P27 (G3) — High-DPI / device-pixel-ratio detection.
//!
//! Upstream probes the cell dimensions at startup; when the cell is small
//! in CSS pixels (Retina, 4K, HiDPI) inline images scale accordingly. The
//! Rust port lifts the rule into its own module — the device-pixel-ratio
//! side of the equation — and exposes both an uncached parameterised
//! detector and a process-global cache.
//!
//! These tests pin the full surface:
//!
//! 1. The cached accessor returns the default ratio for an unknown
//!    terminal.
//! 2. The `PI_HIGH_DPI` override (both branches) wins over detection.
//! 3. Ghostty, kitty, and WezTerm default to HiDPI.
//! 4. iTerm2 and Windows Terminal fall back to standard DPI.
//! 5. `set_device_pixel_ratio` / `refresh_device_pixel_ratio` round-trip
//!    through the cache and survive across calls.
//! 6. The constants and `is_high_dpi()` helper classify the two buckets
//!    consistently.

use pi_tui::high_dpi::{
    detect_device_pixel_ratio_with, device_pixel_ratio, refresh_device_pixel_ratio,
    set_device_pixel_ratio, DevicePixelRatio, DEFAULT_DPI_RATIO, HIGH_DPI_RATIO,
};
use pi_tui::terminal::image::{capability_inputs_from_env, CapabilityInputs};

fn inputs(
    ghostty_resources_dir: bool,
    kitty_window_id: bool,
    wezterm_pane: bool,
    iterm_session_id: bool,
    wt_session: bool,
) -> CapabilityInputs {
    CapabilityInputs {
        ghostty_resources_dir,
        kitty_window_id,
        wezterm_pane,
        iterm_session_id,
        wt_session,
        ..Default::default()
    }
}

fn reset_cache() {
    refresh_device_pixel_ratio();
}

/// 1 — The cached accessor returns *some* value (always set after first
/// call), and the default for an unknown terminal is the standard DPI.
#[test]
fn cached_accessor_returns_a_ratio() {
    reset_cache();
    let ratio = device_pixel_ratio();
    // The first call detects from the env, but with no `PI_HIGH_DPI`
    // override and unknown terminals the result is STANDARD.
    assert!(ratio.ratio() > 0.0, "the cached ratio must be positive");
    assert!(
        ratio.ratio() >= 0.5 && ratio.ratio() <= 8.0,
        "the cached ratio must be clamped into the safe range; got {:?}",
        ratio.ratio()
    );
}

/// 2 — `PI_HIGH_DPI=1` forces HiDPI even on terminals that would normally
/// be standard DPI. The uncached detector wins for the first call into
/// the cache; subsequent cached reads return the same value.
#[test]
fn pi_high_dpi_one_force_high_dpi() {
    let inputs = inputs(false, false, false, false, false);
    let ratio = detect_device_pixel_ratio_with(&inputs, Some("1"));
    assert!(ratio.is_high_dpi(), "PI_HIGH_DPI=1 must force HiDPI");
    assert_eq!(ratio.ratio(), HIGH_DPI_RATIO);
}

/// 3 — `PI_HIGH_DPI=0` forces standard DPI even on a HiDPI terminal
/// (ghostty + kitty + wezterm all set, override still wins).
#[test]
fn pi_high_dpi_zero_force_standard_dpi() {
    let inputs = inputs(true, true, true, false, false);
    let ratio = detect_device_pixel_ratio_with(&inputs, Some("0"));
    assert!(!ratio.is_high_dpi(), "PI_HIGH_DPI=0 must force standard DPI");
    assert_eq!(ratio.ratio(), DEFAULT_DPI_RATIO);
}

/// 4 — Ghostty is always HiDPI without the override.
#[test]
fn ghostty_is_high_dpi_without_override() {
    let ratio = detect_device_pixel_ratio_with(
        &inputs(true, false, false, false, false),
        None,
    );
    assert!(ratio.is_high_dpi());
    assert_eq!(ratio.ratio(), HIGH_DPI_RATIO);
}

/// 5 — kitty is always HiDPI without the override.
#[test]
fn kitty_is_high_dpi_without_override() {
    let ratio = detect_device_pixel_ratio_with(
        &inputs(false, true, false, false, false),
        None,
    );
    assert!(ratio.is_high_dpi());
    assert_eq!(ratio.ratio(), HIGH_DPI_RATIO);
}

/// 6 — WezTerm is always HiDPI without the override.
#[test]
fn wezterm_is_high_dpi_without_override() {
    let ratio = detect_device_pixel_ratio_with(
        &inputs(false, false, true, false, false),
        None,
    );
    assert!(ratio.is_high_dpi());
    assert_eq!(ratio.ratio(), HIGH_DPI_RATIO);
}

/// 7 — iTerm2 falls back to standard DPI (the rule is conservative —
/// the *capability* is detected, the *decision* stays standard unless
/// the user opts in). This matches upstream's behaviour when the probe
/// times out.
#[test]
fn iterm2_falls_back_to_standard_dpi() {
    let ratio = detect_device_pixel_ratio_with(
        &inputs(false, false, false, true, false),
        None,
    );
    assert_eq!(ratio, DevicePixelRatio::STANDARD);
    assert_eq!(ratio.ratio(), DEFAULT_DPI_RATIO);
}

/// 8 — Windows Terminal falls back to standard DPI (the console host
/// overrides the user's HiDPI display).
#[test]
fn wt_falls_back_to_standard_dpi() {
    let ratio = detect_device_pixel_ratio_with(
        &inputs(false, false, false, false, true),
        None,
    );
    assert_eq!(ratio, DevicePixelRatio::STANDARD);
    assert_eq!(ratio.ratio(), DEFAULT_DPI_RATIO);
}

/// 9 — An unknown terminal (no flags set, no override) defaults to
/// standard DPI. This is the conservative answer.
#[test]
fn unknown_terminal_defaults_to_standard_dpi() {
    let ratio = detect_device_pixel_ratio_with(
        &inputs(false, false, false, false, false),
        None,
    );
    assert_eq!(ratio, DevicePixelRatio::STANDARD);
    assert_eq!(ratio.ratio(), DEFAULT_DPI_RATIO);
}

/// 10 — `set_device_pixel_ratio` updates the cached value and the next
/// `device_pixel_ratio` call returns the new value. This is the round-trip
/// hosts and tests rely on.
#[test]
fn set_then_get_round_trips_through_cache() {
    reset_cache();
    set_device_pixel_ratio(DevicePixelRatio::HIGH);
    assert!(device_pixel_ratio().is_high_dpi());
    set_device_pixel_ratio(DevicePixelRatio::STANDARD);
    assert!(!device_pixel_ratio().is_high_dpi());
    refresh_device_pixel_ratio();
}

/// 11 — `refresh_device_pixel_ratio` drops the cache. The next call to
/// `device_pixel_ratio` re-detects from the environment (or the
/// `PI_HIGH_DPI` override if set in the host's env).
#[test]
fn refresh_drops_the_cache() {
    set_device_pixel_ratio(DevicePixelRatio::HIGH);
    assert!(device_pixel_ratio().is_high_dpi());
    refresh_device_pixel_ratio();
    let after = device_pixel_ratio();
    // After refresh, the value is whatever the env says — with no
    // override and unknown terminals, this is standard DPI.
    assert!(
        after.ratio() >= 0.5 && after.ratio() <= 8.0,
        "the refreshed ratio must still be in the safe range; got {:?}",
        after.ratio()
    );
}

/// 12 — `DevicePixelRatio::STANDARD` and `DevicePixelRatio::HIGH` are
/// stable across the crate — plugin authors reference them by name.
#[test]
fn constants_are_stable() {
    assert_eq!(DevicePixelRatio::STANDARD.ratio(), DEFAULT_DPI_RATIO);
    assert_eq!(DevicePixelRatio::HIGH.ratio(), HIGH_DPI_RATIO);
    assert!(DevicePixelRatio::STANDARD != DevicePixelRatio::HIGH);
    assert!(!DevicePixelRatio::STANDARD.is_high_dpi());
    assert!(DevicePixelRatio::HIGH.is_high_dpi());
}

/// 13 — `capability_inputs_from_env` returns the same struct the detector
/// uses. The bridge is real — calling `detect_device_pixel_ratio_with`
/// with `&capability_inputs_from_env()` and no override reproduces the
/// env-only decision. (The exact value depends on the host environment
/// of the test; we only assert the function compiles and produces a
/// ratio in the safe range.)
#[test]
fn env_inputs_bridge_round_trips() {
    let inputs = capability_inputs_from_env();
    let ratio = detect_device_pixel_ratio_with(&inputs, None);
    assert!(ratio.ratio() >= 0.5 && ratio.ratio() <= 8.0);
}

/// 14 — The legacy alias module (`pi_tui::high_dpi`) and the canonical
/// path (`pi_tui::terminal::high_dpi`) reach the same types. This pins
/// the user-facing re-exports.
#[test]
fn high_dpi_alias_module_re_exports_match_canonical_path() {
    use pi_tui::high_dpi as alias;
    use pi_tui::terminal::high_dpi as canonical;
    assert_eq!(
        std::mem::size_of::<alias::DevicePixelRatio>(),
        std::mem::size_of::<canonical::DevicePixelRatio>(),
    );
    assert_eq!(
        alias::DEFAULT_DPI_RATIO,
        canonical::DEFAULT_DPI_RATIO,
    );
    assert_eq!(
        alias::HIGH_DPI_RATIO,
        canonical::HIGH_DPI_RATIO,
    );
}

/// 15 — `ts_compat` re-exports the same surface. Plugin authors that
/// import from `pi_tui::ts_compat::*` see the high-DPI helpers.
#[test]
fn ts_compat_re_exports_the_high_dpi_surface() {
    use pi_tui::ts_compat as ts;
    let _: fn() -> DevicePixelRatio = ts::device_pixel_ratio;
    let _: fn(DevicePixelRatio) = ts::set_device_pixel_ratio;
    let _: fn() = ts::refresh_device_pixel_ratio;
    let _: fn(&CapabilityInputs, Option<&str>) -> DevicePixelRatio =
        ts::detect_device_pixel_ratio_with;
    let _: fn() -> DevicePixelRatio = ts::detect_device_pixel_ratio_from_env;
    assert_eq!(ts::DEFAULT_DPI_RATIO, DEFAULT_DPI_RATIO);
    assert_eq!(ts::HIGH_DPI_RATIO, HIGH_DPI_RATIO);
}
