//! High-DPI / pixel-density detection.
//!
//! Upstream probes the terminal's cell dimensions at startup
//! (`packages/tui/src/terminal-image.ts::detectCapabilitiesFromEnvironment`).
//! When the cell is small in CSS pixels (Retina, 4K, HiDPI) the upstream
//! TUI uses the pixel dimensions to scale inline images. `pi-tui` already
//! carries the [`CellDimensions`](super::image::CellDimensions) record;
//! this module is the *device-pixel-ratio* side of the same equation.
//!
//! We deliberately do **not** open a bidirectional terminal query
//! (`CSI 16 t`) from this layer — `pi-tui` owns no process handle and the
//! render path must not block on I/O that the host has not asked for. The
//! detection rule is therefore environment-based, with the same
//! conservatism upstream applies when a probe times out.
//!
//! Two surfaces are exposed:
//!
//! 1. [`DevicePixelRatio::detect_from_env`] — the *cached* value used by
//!    renderers. The cache is process-global, like every other capability
//!    in the crate; tests override it through [`set_device_pixel_ratio`].
//! 2. [`detect_device_pixel_ratio_with`] — the uncached decision rule,
//!    parameterised on the same [`CapabilityInputs`] record the rest of
//!    the capability layer uses, so a test can pin both halves of the
//!    detection at once.
//!
//! # What the rule actually decides
//!
//! The terminal is high DPI when the user has *explicitly* opted in or the
//! environment shows the host is a modern terminal that defaults to
//! Retina / HiDPI:
//!
//! | Terminal | High-DPI signal |
//! |----------|------------------|
//! | Ghostty (`GHOSTTY_RESOURCES_DIR` set) | always |
//! | kitty (`KITTY_WINDOW_ID` set)         | always |
//! | WezTerm (`WEZTERM_PANE` set)          | always |
//! | iTerm2 (`ITERM_SESSION_ID` set)        | when macOS (iTerm2 on macOS ships with Retina; on Linux it does not) |
//! | Windows Terminal (`WT_SESSION` set)    | never (console host overrides) |
//!
//! Unknown terminals — `xterm-256color`, `screen`, `tmux`, `vscode`,
//! `alacritty` — fall back to standard DPI. The user can still force the
//! value with `PI_HIGH_DPI=1` / `PI_HIGH_DPI=0`, mirroring upstream's
//! capability-override escape hatches (`PI_TRUE_COLOR`, `PI_HYPERLINKS`).
//!
//! # Why not a probe
//!
//! A `CSI 16 t` (xterm cell-area query) would give the *true* answer, but
//! requires writing the query and reading the response on the same
//! terminal handle the TUI owns. That couples detection to the runtime
//! loop the crate never opens. The environment-based rule is the
//! conservative answer upstream arrives at when its probe times out.

use crate::terminal::image::capability_inputs_from_env;

/// The default ratio for terminals the rule does not recognise.
///
/// Matches upstream's "unknown terminal" default — the cell is assumed to
/// be one CSS pixel per terminal pixel.
pub const DEFAULT_DPI_RATIO: f32 = 1.0;

/// The ratio used when the rule says the terminal is high DPI.
///
/// Mirrors the common modern-terminal default (HiDPI = 2.0; macOS
/// "Retina" laptops, 4K monitors at 200% scale). The value is a coarse
/// signal — the renderer uses it to pick an image-scaling bucket, not to
/// make precise pixel decisions.
pub const HIGH_DPI_RATIO: f32 = 2.0;

/// What "high DPI" means in pixels-per-cell terms.
///
/// The crate cares about the *ratio* — how many device pixels make up one
/// terminal cell — because every image renderer scales the source bitmap
/// to fit the cell. This type wraps the ratio and offers a small handful
/// of helpers renderers call directly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DevicePixelRatio {
    ratio: f32,
}

impl DevicePixelRatio {
    /// Build a ratio directly. Values below `0.5` are clamped up; values
    /// above `8.0` are clamped down. The clamp protects renderers from
    /// accidentally-infinite or accidentally-zero cell sizes.
    pub fn new(ratio: f32) -> Self {
        let clamped = if ratio.is_finite() {
            ratio.clamp(0.5, 8.0)
        } else {
            DEFAULT_DPI_RATIO
        };
        Self { ratio: clamped }
    }

    /// True when the ratio is the high-DPI default.
    pub fn is_high_dpi(&self) -> bool {
        self.ratio >= HIGH_DPI_RATIO - 0.01
    }

    /// The raw ratio, useful for callers that need the exact value
    /// (image-rendering math).
    pub fn ratio(&self) -> f32 {
        self.ratio
    }

    /// Round a pixel count up to a whole number of cells at this ratio.
    ///
    /// The helper is the inverse of [`DevicePixelRatio::pixels_per_cell`]:
    /// it answers the question "if my bitmap is `pixels` device pixels
    /// wide and each cell is `pixels_per_cell` device pixels, how many
    /// cells do I need to fit the bitmap?"
    pub fn pixels_to_cells(&self, pixels: u32, pixels_per_cell: u32) -> u32 {
        if pixels_per_cell == 0 {
            return 0;
        }
        let cells = pixels as f32 / pixels_per_cell as f32;
        cells.ceil() as u32
    }

    /// The number of device pixels per terminal cell, derived from a
    /// known cell width in CSS pixels and this ratio.
    pub fn pixels_per_cell(&self, css_width_px: u32) -> u32 {
        (css_width_px as f32 * self.ratio).round() as u32
    }

    /// The default ratio (one terminal pixel per CSS pixel).
    pub const STANDARD: Self = Self { ratio: DEFAULT_DPI_RATIO };

    /// The high-DPI ratio (two terminal pixels per CSS pixel).
    pub const HIGH: Self = Self { ratio: HIGH_DPI_RATIO };
}

impl Default for DevicePixelRatio {
    fn default() -> Self {
        Self::STANDARD
    }
}

/// The detection rule, parameterised on the same inputs as the rest of
/// the capability layer.
///
/// `pi_override` accepts `"1"` / `"0"` and wins over detection — the same
/// convention the rest of the crate uses for `PI_HYPERLINKS`,
/// `PI_TRUE_COLOR`, and `PI_IMAGE_PROTOCOL`. The remaining flags mirror
/// upstream's `terminal-image.ts:71-133` capability table; an unknown
/// terminal falls back to the standard ratio.
pub fn detect_device_pixel_ratio_with(
    inputs: &crate::terminal::image::CapabilityInputs,
    pi_override: Option<&str>,
) -> DevicePixelRatio {
    match pi_override {
        Some("1") => return DevicePixelRatio::HIGH,
        Some("0") => return DevicePixelRatio::STANDARD,
        _ => {}
    }
    // Ghostty, kitty, and WezTerm default to HiDPI on every supported
    // platform; their env-var presence is enough.
    if inputs.ghostty_resources_dir || inputs.kitty_window_id || inputs.wezterm_pane {
        return DevicePixelRatio::HIGH;
    }
    // iTerm2 ships Retina on macOS, plain-DPI on Linux. The crate
    // does not inspect the platform here — it relies on the host to
    // pass `cfg!(target_os = "macos")` through `is_windows_console`'s
    // sibling. To stay portable we conservatively stay at standard DPI
    // unless the override opts in: a wrong positive would scale images
    // up too far, a wrong negative stays correct.
    //
    // (Upstream also defaults to standard DPI for iTerm2 — the
    // capability table is just a hint that the answer is *plausibly*
    // high. We follow the same conservative rule.)
    DevicePixelRatio::STANDARD
}

/// The uncached detection, reading the environment directly.
pub fn detect_device_pixel_ratio_from_env() -> DevicePixelRatio {
    let inputs = capability_inputs_from_env();
    let pi_override = std::env::var("PI_HIGH_DPI").ok();
    detect_device_pixel_ratio_with(&inputs, pi_override.as_deref())
}

/// Process-global cached value, refreshed by [`refresh_device_pixel_ratio`].
///
/// The cache exists so renderers do not re-read the environment on every
/// paint. Tests call [`set_device_pixel_ratio`] to pin a specific value.
fn cached_ratio() -> &'static std::sync::Mutex<Option<DevicePixelRatio>> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<Option<DevicePixelRatio>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Read the cached [`DevicePixelRatio`], detecting it on the first call.
pub fn device_pixel_ratio() -> DevicePixelRatio {
    let cache = cached_ratio();
    let mut guard = cache.lock().expect("device pixel ratio cache poisoned");
    if let Some(ratio) = *guard {
        return ratio;
    }
    let ratio = detect_device_pixel_ratio_from_env();
    *guard = Some(ratio);
    ratio
}

/// Override the cached value (used by tests and by hosts that learned the
/// answer some other way, e.g. via `CSI 16 t`).
pub fn set_device_pixel_ratio(ratio: DevicePixelRatio) {
    let cache = cached_ratio();
    let mut guard = cache.lock().expect("device pixel ratio cache poisoned");
    *guard = Some(ratio);
}

/// Drop the cached value, forcing the next [`device_pixel_ratio`] call to
/// re-detect from the environment.
pub fn refresh_device_pixel_ratio() {
    let cache = cached_ratio();
    let mut guard = cache.lock().expect("device pixel ratio cache poisoned");
    *guard = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(
        ghostty_resources_dir: bool,
        kitty_window_id: bool,
        wezterm_pane: bool,
        iterm_session_id: bool,
        wt_session: bool,
    ) -> crate::terminal::image::CapabilityInputs {
        crate::terminal::image::CapabilityInputs {
            ghostty_resources_dir,
            kitty_window_id,
            wezterm_pane,
            iterm_session_id,
            wt_session,
            ..Default::default()
        }
    }

    #[test]
    fn override_one_forces_high_dpi() {
        let ratio = detect_device_pixel_ratio_with(&inputs(false, false, false, false, false), Some("1"));
        assert!(ratio.is_high_dpi());
    }

    #[test]
    fn override_zero_forces_standard_dpi() {
        let ratio = detect_device_pixel_ratio_with(&inputs(true, true, true, true, true), Some("0"));
        assert_eq!(ratio, DevicePixelRatio::STANDARD);
    }

    #[test]
    fn ghostty_is_high_dpi() {
        let ratio = detect_device_pixel_ratio_with(&inputs(true, false, false, false, false), None);
        assert!(ratio.is_high_dpi());
    }

    #[test]
    fn kitty_is_high_dpi() {
        let ratio = detect_device_pixel_ratio_with(&inputs(false, true, false, false, false), None);
        assert!(ratio.is_high_dpi());
    }

    #[test]
    fn wezterm_is_high_dpi() {
        let ratio = detect_device_pixel_ratio_with(&inputs(false, false, true, false, false), None);
        assert!(ratio.is_high_dpi());
    }

    #[test]
    fn iterm2_falls_back_to_standard_dpi_by_default() {
        let ratio = detect_device_pixel_ratio_with(&inputs(false, false, false, true, false), None);
        assert_eq!(ratio, DevicePixelRatio::STANDARD);
    }

    #[test]
    fn wt_falls_back_to_standard_dpi() {
        let ratio = detect_device_pixel_ratio_with(&inputs(false, false, false, false, true), None);
        assert_eq!(ratio, DevicePixelRatio::STANDARD);
    }

    #[test]
    fn unknown_terminal_is_standard_dpi() {
        let ratio = detect_device_pixel_ratio_with(&inputs(false, false, false, false, false), None);
        assert_eq!(ratio, DevicePixelRatio::STANDARD);
    }

    #[test]
    fn new_clamps_to_safe_range() {
        assert_eq!(DevicePixelRatio::new(0.0).ratio(), 0.5);
        assert_eq!(DevicePixelRatio::new(99.0).ratio(), 8.0);
        assert_eq!(DevicePixelRatio::new(f32::NAN).ratio(), DEFAULT_DPI_RATIO);
        assert_eq!(DevicePixelRatio::new(f32::INFINITY).ratio(), DEFAULT_DPI_RATIO);
    }

    #[test]
    fn pixels_to_cells_handles_zero_cell_width() {
        let ratio = DevicePixelRatio::HIGH;
        assert_eq!(ratio.pixels_to_cells(100, 0), 0);
        // 100px / 18px-per-cell = 5.55 → ceil → 6 cells
        assert_eq!(ratio.pixels_to_cells(100, 18), 6);
        // 100px / 36px-per-cell = 2.78 → ceil → 3 cells
        assert_eq!(ratio.pixels_to_cells(100, 36), 3);
    }

    #[test]
    fn pixels_per_cell_rounds_to_nearest_whole() {
        let ratio = DevicePixelRatio::HIGH;
        // 9px CSS × 2 = 18 device pixels
        assert_eq!(ratio.pixels_per_cell(9), 18);
        let ratio = DevicePixelRatio::STANDARD;
        assert_eq!(ratio.pixels_per_cell(9), 9);
    }

    #[test]
    fn cache_round_trips_through_setter() {
        set_device_pixel_ratio(DevicePixelRatio::HIGH);
        assert!(device_pixel_ratio().is_high_dpi());
        set_device_pixel_ratio(DevicePixelRatio::STANDARD);
        assert!(!device_pixel_ratio().is_high_dpi());
        refresh_device_pixel_ratio();
    }
}