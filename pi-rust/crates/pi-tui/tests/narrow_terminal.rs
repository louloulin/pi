//! P29 (G1) — narrow-terminal degradation flags.
//!
//! Upstream (`packages/coding-agent/src/modes/interactive/interactive-mode.ts`)
//! folds the header hint list and freezes the spinner when the visible
//! width drops below 30 columns, and forces the composer to a single row
//! below 20 columns. The Rust port mirrors those cutoffs in
//! [`narrow_options`](pi_tui::narrow_options) and re-exports them through
//! [`pi_tui::ts_compat`]. The tests below pin the threshold semantics and
//! the surface area every component consumes.

use pi_tui::narrow_terminal::{
    is_extreme_narrow, is_narrow, narrow_options, NarrowOptions, EXTREME_NARROW_WIDTH,
    HEADER_HINT_COLLAPSE_THRESHOLD, NARROW_TERMINAL_WIDTH,
};

/// 1 — Wide terminal (>= 30): every flag set to its permissive value.
#[test]
fn wide_terminal_passes_every_flag_through() {
    let opts = narrow_options(120);
    assert!(!opts.narrow);
    assert!(!opts.extreme_narrow);
    assert_eq!(opts.header_hints, usize::MAX);
    assert_eq!(opts.max_composer_rows, u16::MAX);
    assert!(opts.spinner_animated);
    assert!(opts.show_secondary_segments);
}

/// 2 — Exactly at the NARROW threshold (30) counts as wide; the
/// comparison is strict `<`.
#[test]
fn exactly_at_narrow_threshold_is_wide() {
    let opts = narrow_options(NARROW_TERMINAL_WIDTH);
    assert!(!opts.narrow);
    assert!(opts.show_secondary_segments);
    assert!(is_narrow(NARROW_TERMINAL_WIDTH) == false);
}

/// 3 — One column below the NARROW threshold: secondary segments drop,
/// header hints collapse to HEADER_HINT_COLLAPSE_THRESHOLD, but the
/// composer and spinner are unaffected.
#[test]
fn one_column_below_narrow_threshold_drops_segments_only() {
    let opts = narrow_options(NARROW_TERMINAL_WIDTH - 1);
    assert!(opts.narrow);
    assert!(!opts.extreme_narrow);
    assert_eq!(opts.header_hints, HEADER_HINT_COLLAPSE_THRESHOLD);
    assert_eq!(opts.max_composer_rows, u16::MAX);
    assert!(opts.spinner_animated);
    assert!(!opts.show_secondary_segments);
}

/// 4 — At the EXTREME_NARROW threshold (20): narrow but not extreme;
/// spinner still animates.
#[test]
fn exactly_at_extreme_narrow_threshold_is_narrow_but_not_extreme() {
    let opts = narrow_options(EXTREME_NARROW_WIDTH);
    assert!(opts.narrow);
    assert!(!opts.extreme_narrow);
    assert!(opts.spinner_animated);
    assert_eq!(opts.max_composer_rows, u16::MAX);
}

/// 5 — One column below EXTREME_NARROW: spinner freezes, composer is
/// capped at one row, no header hints.
#[test]
fn one_column_below_extreme_narrow_freezes_and_caps() {
    let opts = narrow_options(EXTREME_NARROW_WIDTH - 1);
    assert!(opts.narrow);
    assert!(opts.extreme_narrow);
    assert_eq!(opts.header_hints, 0);
    assert_eq!(opts.max_composer_rows, 1);
    assert!(!opts.spinner_animated);
    assert!(!opts.show_secondary_segments);
}

/// 6 — Zero-width is the worst case: extreme_narrow + narrow + composer
/// capped + spinner frozen.
#[test]
fn zero_width_is_extreme_narrow() {
    let opts = narrow_options(0);
    assert!(opts.narrow);
    assert!(opts.extreme_narrow);
    assert_eq!(opts.max_composer_rows, 1);
    assert!(!opts.spinner_animated);
}

/// 7 — `is_narrow` and `is_extreme_narrow` agree with the NarrowOptions
/// fields for every width in [0, 200).
#[test]
fn is_helpers_match_options_everywhere_in_range() {
    for w in 0..200u16 {
        assert_eq!(is_narrow(w), narrow_options(w).narrow, "width={}", w);
        assert_eq!(
            is_extreme_narrow(w),
            narrow_options(w).extreme_narrow,
            "width={}",
            w,
        );
    }
}

/// 8 — Header hint thresholds: wide = usize::MAX, narrow = constant,
/// extreme = 0.
#[test]
fn header_hint_threshold_three_tiers() {
    assert_eq!(narrow_options(80).header_hints, usize::MAX);
    assert_eq!(
        narrow_options(NARROW_TERMINAL_WIDTH - 1).header_hints,
        HEADER_HINT_COLLAPSE_THRESHOLD,
    );
    assert_eq!(
        narrow_options(EXTREME_NARROW_WIDTH - 1).header_hints,
        0,
    );
}

/// 9 — `NarrowOptions` is Copy so it can be threaded through render
/// callbacks without clone().
#[test]
fn narrow_options_is_copy() {
    let opts = narrow_options(80);
    let copy = opts;
    assert_eq!(opts, copy);
    let _: NarrowOptions = opts;
}

/// 10 — `ts_compat` re-exports the helpers under their TS-spelling names.
#[test]
fn ts_compat_reexports_narrow_terminal_helpers() {
    use pi_tui::ts_compat::{
        is_extreme_narrow_terminal, is_narrow_terminal, narrow_options as ts_narrow_options,
        NarrowOptions as TsNarrowOptions, EXTREME_NARROW_WIDTH as TS_EXTREME,
        HEADER_HINT_COLLAPSE_THRESHOLD as TS_HINT, NARROW_TERMINAL_WIDTH as TS_NARROW,
    };
    assert_eq!(TS_NARROW, NARROW_TERMINAL_WIDTH);
    assert_eq!(TS_EXTREME, EXTREME_NARROW_WIDTH);
    assert_eq!(TS_HINT, HEADER_HINT_COLLAPSE_THRESHOLD);
    let _: TsNarrowOptions = ts_narrow_options(40);
    assert!(is_narrow_terminal(20));
    assert!(is_extreme_narrow_terminal(10));
    assert!(!is_narrow_terminal(80));
    assert!(!is_extreme_narrow_terminal(40));
}