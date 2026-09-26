//! Narrow-terminal degradation rules.
//!
//! On a terminal narrower than [`NARROW_TERMINAL_WIDTH`] columns the
//! built-in chrome stops trying to render every affordance — the user is
//! out of horizontal space and every column counts. The port already has
//! per-zone sacrifice ordering for the status bar
//! ([`crate::components::status::NARROW_SACRIFICE_ORDER`]); this module
//! gives the *rest* of the chrome a single, observable answer to the
//! question "the user has `width` columns — what may I render?".
//!
//! Upstream (`packages/coding-agent/src/modes/interactive/interactive-mode.ts`
//! `visibleWidth`-based folding) cuts the header hint list down to the
//! fold line and freezes the spinner in place when the cell budget
//! cannot fit its animation. The Rust port keeps the same two thresholds
//! — [`NARROW_TERMINAL_WIDTH`] (the row hint list starts collapsing) and
//! [`EXTREME_NARROW_WIDTH`] (the spinner freezes, the composer is forced
//! to one row) — and exposes them as a single [`narrow_options`] value.
//!
//! # Why a dedicated module
//!
//! Multiple components ask the same question ("am I on a narrow
//! terminal?") and reach for different answers — the status bar wants
//! to drop segments, the header wants to drop hints, the composer wants
//! to cap its row count, the spinner wants to freeze. Centralising the
//! decision in [`narrow_options`] keeps the answers consistent: every
//! component sees the same threshold, every threshold has one owner.
//!
//! # What `narrow_options(width)` decides
//!
//! | Width band               | Width threshold | What happens                                                    |
//! |--------------------------|-----------------|------------------------------------------------------------------|
//! | Wide                     | `>= 30`         | Normal rendering; no constraint.                                |
//! | Narrow                   | `< 30`          | Secondary status segments dropped; header hints collapsed.       |
//! | Extreme narrow           | `< 20`          | Spinner frozen on its first frame; composer capped at one row.   |
//!
//! The cutoffs match the upstream `Container` folding path:
//!
//! - **30 columns** is the threshold upstream uses for the folded-header
//!   view (`interactive-mode.ts:938-944`): below it the title line is
//!   still readable but the hint list no longer fits.
//! - **20 columns** is where the spinner stops animating
//!   (`packages/tui/src/components/loader.ts:24-29`): a single-cell glyph
//!   flipping every 80 ms is the difference between "the TUI is alive"
//!   and "the cursor is wedged" on a tiny terminal.
//!
//! Both numbers are conservative — a 22-row x 30-col terminal still has
//! working width, and a 20-col terminal still fits a status row. The
//! thresholds err toward keeping the chrome rather than hiding it.

/// The width below which the port starts folding header hints and
/// dropping secondary status segments.
///
/// Upstream uses the same number to choose between the expanded and
/// folded header (`interactive-mode.ts:920-944`).
pub const NARROW_TERMINAL_WIDTH: u16 = 30;

/// The width below which the spinner freezes and the composer is
/// forced to a single row.
///
/// Below 20 columns a one-cell spinner animation flickers visibly enough
/// to confuse the reader; the cheapest fix is to pin it to its first
/// frame so the column budget is known and stable.
pub const EXTREME_NARROW_WIDTH: u16 = 20;

/// The maximum number of header hints to render in any width band.
///
/// Wide terminals show every hint; narrow terminals show the first
/// [`HEADER_HINT_COLLAPSE_THRESHOLD`]; extreme-narrow terminals show
/// none (the header line is reserved for the title alone).
pub const HEADER_HINT_COLLAPSE_THRESHOLD: usize = 2;

/// Width band + flags derived from a column count. Components consult
/// this struct rather than testing `width < ...` themselves so the
/// thresholds stay in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NarrowOptions {
    /// The "narrow" threshold result — `width < NARROW_TERMINAL_WIDTH`.
    /// `true` means the secondary status segments drop and the header
    /// hint list collapses to [`HEADER_HINT_COLLAPSE_THRESHOLD`].
    pub narrow: bool,
    /// The "extreme narrow" threshold result — `width < EXTREME_NARROW_WIDTH`.
    /// `true` means the spinner freezes and the composer is capped at
    /// one row.
    pub extreme_narrow: bool,
    /// The maximum number of header hints to render.
    pub header_hints: usize,
    /// Maximum composer rows; `1` in extreme-narrow, `u16::MAX` elsewhere.
    pub max_composer_rows: u16,
    /// Whether the spinner animation should advance. `false` in
    /// extreme-narrow.
    pub spinner_animated: bool,
    /// Whether secondary status segments (cache hit rate, cost, hint)
    /// should still render. `false` in narrow.
    pub show_secondary_segments: bool,
}

/// Compute [`NarrowOptions`] for a given width.
///
/// The single decision point every narrow-aware component consults.
pub fn narrow_options(width: u16) -> NarrowOptions {
    let narrow = width < NARROW_TERMINAL_WIDTH;
    let extreme_narrow = width < EXTREME_NARROW_WIDTH;
    NarrowOptions {
        narrow,
        extreme_narrow,
        header_hints: if extreme_narrow {
            0
        } else if narrow {
            HEADER_HINT_COLLAPSE_THRESHOLD
        } else {
            usize::MAX
        },
        max_composer_rows: if extreme_narrow { 1 } else { u16::MAX },
        spinner_animated: !extreme_narrow,
        show_secondary_segments: !narrow,
    }
}

/// `true` when `width` is below [`NARROW_TERMINAL_WIDTH`].
///
/// Shorthand for `narrow_options(width).narrow`. Callers that only
/// need the boolean flag use this; callers that need the rest of the
/// flags reach for [`narrow_options`].
pub fn is_narrow(width: u16) -> bool {
    width < NARROW_TERMINAL_WIDTH
}

/// `true` when `width` is below [`EXTREME_NARROW_WIDTH`].
///
/// Shorthand for `narrow_options(width).extreme_narrow`.
pub fn is_extreme_narrow(width: u16) -> bool {
    width < EXTREME_NARROW_WIDTH
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_terminal_renders_every_chrome() {
        let opts = narrow_options(120);
        assert!(!opts.narrow);
        assert!(!opts.extreme_narrow);
        assert_eq!(opts.header_hints, usize::MAX);
        assert_eq!(opts.max_composer_rows, u16::MAX);
        assert!(opts.spinner_animated);
        assert!(opts.show_secondary_segments);
    }

    #[test]
    fn at_narrow_width_status_segments_drop_and_hints_collapse() {
        let opts = narrow_options(NARROW_TERMINAL_WIDTH - 1);
        assert!(opts.narrow);
        assert!(!opts.extreme_narrow);
        assert_eq!(opts.header_hints, HEADER_HINT_COLLAPSE_THRESHOLD);
        assert_eq!(opts.max_composer_rows, u16::MAX);
        assert!(opts.spinner_animated);
        assert!(!opts.show_secondary_segments);
    }

    #[test]
    fn at_narrow_width_boundary_is_not_narrow() {
        // The threshold is `< NARROW_TERMINAL_WIDTH` — exactly at the
        // boundary counts as wide.
        let opts = narrow_options(NARROW_TERMINAL_WIDTH);
        assert!(!opts.narrow);
        assert!(opts.show_secondary_segments);
    }

    #[test]
    fn extreme_narrow_freezes_spinner_and_caps_composer() {
        let opts = narrow_options(EXTREME_NARROW_WIDTH - 1);
        assert!(opts.narrow);
        assert!(opts.extreme_narrow);
        assert_eq!(opts.header_hints, 0);
        assert_eq!(opts.max_composer_rows, 1);
        assert!(!opts.spinner_animated);
        assert!(!opts.show_secondary_segments);
    }

    #[test]
    fn extreme_narrow_boundary_is_narrow_but_not_extreme() {
        let opts = narrow_options(EXTREME_NARROW_WIDTH);
        assert!(opts.narrow);
        assert!(!opts.extreme_narrow);
        assert!(opts.spinner_animated);
        assert_eq!(opts.max_composer_rows, u16::MAX);
    }

    #[test]
    fn is_narrow_matches_narrow_options() {
        for width in 0..200u16 {
            assert_eq!(is_narrow(width), narrow_options(width).narrow);
            assert_eq!(
                is_extreme_narrow(width),
                narrow_options(width).extreme_narrow,
            );
        }
    }

    #[test]
    fn zero_width_is_extreme_narrow() {
        let opts = narrow_options(0);
        assert!(opts.extreme_narrow);
        assert!(opts.narrow);
        assert_eq!(opts.max_composer_rows, 1);
    }

    #[test]
    fn u16_max_width_is_wide() {
        let opts = narrow_options(u16::MAX);
        assert!(!opts.narrow);
        assert!(!opts.extreme_narrow);
    }
}
