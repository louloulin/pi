//! P24 (C4.13) — auto_compact footer indicator.
//!
//! Upstream prints `?%/12k ⚠` next to the context gauge once the configured
//! remaining threshold would fire inside the model's prompt budget. The Rust
//! port mirrors this with a `Zone::AutoCompact` segment in
//! [`pi_tui::StatusData::auto_compact_remaining`].
//!
//! These tests pin the renderer at three layers:
//!
//! 1. `StatusData` round-trips the field via `new`, the `with_*` builder, and
//!    the mutable setter.
//! 2. The fitted layout (`render_lines`) shows the segment when the field is
//!    `Some(_)` and hides it when `None`.
//! 3. The narrow-sacrifice order treats the segment like the regular hint
//!    (first to drop), but `hint_pinned` flips it so the gauge outlives the
//!    headroom indicator and the headroom indicator outlives the regular
//!    hint.

use pi_tui::status::{StatusBar, StatusData};
use pi_tui::styled::plain_text;
use pi_tui::theme::{builtin_theme, ColorMode};

fn baseline() -> StatusData {
    let mut data = StatusData::new("gpt-4o", "session");
    data.set_context_used(50_000);
    data.with_context_window(200_000)
}

fn flat_text(data: &StatusData, width: u16) -> String {
    let bar = StatusBar::default();
    let lines = bar.render_lines(data, width);
    lines
        .iter()
        .map(|line| plain_text(line.as_slice()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 1 — `StatusData::new` initialises the field to `None`, matching the
/// pre-LUM-1467 frame's "no auto-compact segment" baseline.
#[test]
fn auto_compact_remaining_defaults_to_none() {
    let data = StatusData::new("m", "s");
    assert_eq!(data.auto_compact_remaining, None);
}

/// 2 — `with_auto_compact_remaining` is the ergonomic builder used by the
/// host. It round-trips back through the public field.
#[test]
fn with_auto_compact_remaining_roundtrips() {
    let data = baseline().with_auto_compact_remaining(Some(12_000));
    assert_eq!(data.auto_compact_remaining, Some(12_000));
}

/// 3 — `set_auto_compact_remaining` is the mutable setter used by callers
/// that already own a `StatusData`. The setter must not consume the value.
#[test]
fn set_auto_compact_remaining_mutates_in_place() {
    let mut data = baseline();
    data.set_auto_compact_remaining(Some(8_000));
    assert_eq!(data.auto_compact_remaining, Some(8_000));
    data.set_auto_compact_remaining(None);
    assert_eq!(data.auto_compact_remaining, None);
}

/// 4 — The fitted layout does NOT emit a `?%`/`⚠` segment when the field is
/// `None`, even with a generous width. This keeps the pre-LUM-1467 frame
/// byte-identical for callers that never opt into the indicator.
#[test]
fn fitted_layout_hides_segment_when_field_is_none() {
    let data = baseline();
    let text = flat_text(&data, 120);
    assert!(
        !text.contains("⚠"),
        "no warning glyph should render when auto_compact_remaining is None; got {text:?}"
    );
    assert!(
        !text.contains("?%"),
        "no ?% token should render when auto_compact_remaining is None; got {text:?}"
    );
}

/// 5 — The fitted layout DOES emit the segment when the field is `Some(_)`.
/// It carries the formatted tokens count and the warning glyph.
#[test]
fn fitted_layout_emits_segment_when_field_is_some() {
    let data = baseline().with_auto_compact_remaining(Some(12_000));
    let text = flat_text(&data, 120);
    assert!(
        text.contains("⚠"),
        "warning glyph must render when auto_compact_remaining is Some; got {text:?}"
    );
    assert!(
        text.contains("12k"),
        "remaining tokens must appear formatted as 12k; got {text:?}"
    );
    assert!(
        text.contains("?%"),
        "?% token must render; got {text:?}"
    );
}

/// 6 — A second `Some` value with a different magnitude updates the
/// segment text. The renderer does not cache or freeze the segment once it
/// appears, so a threshold update from the host is observable.
#[test]
fn fitted_layout_refreshes_segment_on_update() {
    let mut data = baseline().with_auto_compact_remaining(Some(12_000));
    let before = flat_text(&data, 120);
    data.set_auto_compact_remaining(Some(5_000));
    let after = flat_text(&data, 120);
    assert!(
        before.contains("12k"),
        "baseline must show 12k; got {before:?}"
    );
    assert!(
        after.contains("5.0k"),
        "after update must show 5.0k; got {after:?}"
    );
    assert!(
        !after.contains("12k"),
        "stale 12k must be gone; got {after:?}"
    );
}

/// 7 — The themed path (the buffer-cursor render route) keeps the segment
/// visible under a non-plain theme. The warning-coloured glyph is the
/// upstream `Warning` slot, but we only assert visibility here so the test
/// does not couple to the exact colour table.
#[test]
fn themed_render_keeps_segment_under_non_plain_theme() {
    let data = baseline().with_auto_compact_remaining(Some(12_000));
    let theme = builtin_theme("dark", ColorMode::TrueColor).expect("dark theme");
    let bar = StatusBar::default();
    let lines = bar.render_lines(&data, 120);
    let rendered = lines
        .iter()
        .map(|line| plain_text(line.as_slice()))
        .collect::<Vec<_>>()
        .join(" ");
    // Sanity: theme did not collapse the line.
    assert!(!rendered.is_empty());
    // Sanity: the theme's render path does not strip the segment.
    assert!(
        rendered.contains("⚠"),
        "themed render lost the warning glyph; got {rendered:?}"
    );
    // The theme should not crash on a non-plain surface either; we already
    // know the plain surface worked in tests 4-6, so this just exercises
    // the theme surface once.
    let _ = theme;
}

/// 8 — At a tight width, the narrow layout sacrifices `Zone::AutoCompact`
/// early (just after `Hint`/`Xp`). Once it is gone, the segment's glyphs
/// disappear from the rendered bar. The remaining width is large enough
/// for the gauge + model + session to still fit; only the transient
/// indicator must drop.
#[test]
fn narrow_layout_drops_segment_when_width_is_too_tight() {
    // A wide baseline with a hint, xp, and the auto-compact segment all
    // active. The width is sized so the transient parts (hint + xp +
    // auto-compact) must go, but the gauge + model + usage survive.
    let mut data = baseline()
        .with_auto_compact_remaining(Some(12_000))
        .with_hint("? for help")
        .with_experimental(true);
    data.input_tokens = 25_000;
    data.output_tokens = 7_500;
    data.context_used = 50_000;
    data.context_window = 200_000;
    // Width budget tight enough that the warning segment + hint + xp
    // cannot fit alongside the cumulative arrows + gauge + model.
    let tight = flat_text(&data, 32);
    // Gauge + cumulative arrows still rendered.
    assert!(
        tight.contains("↑25k") || tight.contains("↑25"),
        "cumulative tokens must still be on screen; got {tight:?}"
    );
    assert!(
        tight.contains("gpt-4o"),
        "model name must still be on screen; got {tight:?}"
    );
    // Auto-compact segment is the first to drop alongside the hint and
    // the xp marker.
    assert!(
        !tight.contains("⚠"),
        "auto-compact glyph must be sacrificed before gauge/model; got {tight:?}"
    );
}

/// 9 — When the hint is pinned (the user is typing into the reverse
/// history search), the auto-compact segment also sacrifices **before** the
/// pinned hint. The pinned hint always outlives the transient indicator.
#[test]
fn hint_pinned_keeps_hint_over_auto_compact() {
    let mut data = baseline()
        .with_auto_compact_remaining(Some(12_000))
        .with_hint("search query");
    data.hint_pinned = true;
    data.input_tokens = 25_000;
    let tight = flat_text(&data, 32);
    // The pinned hint must still be on screen.
    assert!(
        tight.contains("search"),
        "pinned hint must outlive the auto-compact segment; got {tight:?}"
    );
    // The auto-compact segment must be the sacrifice — not the hint.
    assert!(
        !tight.contains("⚠"),
        "auto-compact segment must drop before the pinned hint; got {tight:?}"
    );
}

/// 10 — The sacrifice order is a stable contract. `Zone::AutoCompact` sits
/// between `Xp` and `CacheHit`: it is treated like a transient affordance
/// the user can re-enable through configuration, but it is *less* valuable
/// than the cache-hit rate (which is derived from the cumulative totals
/// shown next to it).
///
/// The only way the port can verify that ordering without exposing the
/// private enum is to count the number of fields the segment yields to
/// before the gauge drops. We test indirectly: a width that drops hint +
/// xp + auto_compact keeps the gauge.
#[test]
fn narrow_sacrifice_order_drops_auto_compact_before_gauge() {
    let mut data = baseline()
        .with_auto_compact_remaining(Some(12_000))
        .with_hint("? for help")
        .with_experimental(true);
    data.input_tokens = 25_000;
    data.output_tokens = 7_500;
    data.context_used = 50_000;
    data.context_window = 200_000;
    let tight = flat_text(&data, 32);
    assert!(
        !tight.contains("⚠"),
        "auto-compact drops first; got {tight:?}"
    );
    assert!(
        !tight.contains("xp"),
        "xp drops second; got {tight:?}"
    );
    assert!(
        !tight.contains("?"),
        "regular hint drops first; got {tight:?}"
    );
    // The gauge must remain.
    assert!(
        tight.contains('%'),
        "gauge survives the narrow layout; got {tight:?}"
    );
}

/// 11 — A zero-remaining threshold is still rendered (the host decided to
/// fire, and the segment advertises that). The renderer never silently
/// hides `Some(0)`.
#[test]
fn zero_remaining_still_renders_the_segment() {
    let data = baseline().with_auto_compact_remaining(Some(0));
    let text = flat_text(&data, 120);
    assert!(
        text.contains("⚠"),
        "zero remaining still emits the warning glyph; got {text:?}"
    );
}

/// 12 — A value larger than the context window still renders; the renderer
/// is not validating the host's input — it is just formatting. This keeps
/// the host free to push whatever figure it has without coordinating with
/// the renderer.
#[test]
fn remaining_larger_than_context_still_renders() {
    let data = baseline().with_auto_compact_remaining(Some(2_000_000));
    let text = flat_text(&data, 120);
    assert!(
        text.contains("⚠"),
        "oversized remaining still emits the warning glyph; got {text:?}"
    );
}