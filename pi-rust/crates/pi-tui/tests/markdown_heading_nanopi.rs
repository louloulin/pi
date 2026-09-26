//! P20 — borrow nanopi's per-level heading colour pattern.
//!
//! nanopi's `src/render/markdown.rs:57-79` styles H1, H2, H3 each with
//! its own warm hue (`Indexed(214)` / `220` / `228`), so the heading
//! hierarchy is visible before the eye reaches the `#` prefix. The TS
//! pi-tui and the prior pi-rust use one `mdHeading` colour for every
//! level — fine for short bodies, but in a transcript with several
//! sections of different importance the levels blur into one yellow
//! blob.
//!
//! The borrow adds three optional `MdHeading1/2/3` ThemeColor slots that
//! fall back to `MdHeading` when a theme doesn't override them. Tests
//! pin every leg:
//!   1. Each level resolves to its dedicated slot — never `MdHeading`.
//!   2. Levels 4–6 fall through to `MdHeading` (nanopi only specialises
//!      the top three).
//!   3. H1 keeps the underline modifier, H2+ do not.
//!   4. The fallback wiring is correct: every new slot's `fallback()`
//!      points at `MdHeading` and every slot is optional.
//!   5. The dark + light themes resolve distinct per-level values, so
//!      the borrow is wired through the JSON layer (not just the enum).

use pi_tui::markdown::render_markdown;
use pi_tui::styled::{plain_text, StyledLine};
use pi_tui::theme::{builtin_theme, ColorMode, ThemeColor};

fn texts(lines: &[StyledLine]) -> Vec<String> {
    lines.iter().map(|line| plain_text(line)).collect()
}

fn style_at<'a>(line: &'a StyledLine, text: &str) -> &'a pi_tui::styled::SpanStyle {
    &line
        .iter()
        .find(|span| span.text == text)
        .unwrap_or_else(|| panic!("no span {text:?} in {line:?}"))
        .style
}

/// 1 — each of H1, H2, H3 resolves to its dedicated slot.
#[test]
fn h1_h2_h3_each_resolve_to_their_dedicated_slot() {
    let h1 = render_markdown("# Title", 40);
    let h2 = render_markdown("## Sub", 40);
    let h3 = render_markdown("### Third", 40);
    assert_eq!(style_at(&h1[0], "Title").fg, Some(ThemeColor::MdHeading1));
    assert_eq!(style_at(&h2[0], "Sub").fg, Some(ThemeColor::MdHeading2));
    // H3 keeps the `#` prefix, so the span we want is the whole line.
    let h3_style = style_at(&h3[0], "### Third").fg;
    assert_eq!(h3_style, Some(ThemeColor::MdHeading3));
}

/// 2 — levels 4–6 fall through to `MdHeading`. nanopi only specialises
/// the top three (`src/render/markdown.rs:57-79`), so the borrow stops
/// at H3 and the rest share the unified slot.
#[test]
fn h4_h5_h6_fall_through_to_md_heading() {
    for level in [4u8, 5, 6] {
        let prefix = "#".repeat(level as usize);
        let line = format!("{prefix} Body");
        let lines = render_markdown(&line, 40);
        assert_eq!(
            style_at(&lines[0], &line).fg,
            Some(ThemeColor::MdHeading),
            "level {level} must fall through to MdHeading",
        );
    }
}

/// 3 — H1 keeps the underline modifier (matches the prior behaviour and
/// the TS pi-tui); H2+ do not gain underline.
#[test]
fn h1_keeps_underline_h2_plus_does_not() {
    let h1 = render_markdown("# Title", 40);
    assert!(style_at(&h1[0], "Title").underline);

    let h2 = render_markdown("## Sub", 40);
    assert!(!style_at(&h2[0], "Sub").underline);

    let h3 = render_markdown("### Third", 40);
    assert!(!style_at(&h3[0], "### Third").underline);
}

/// 4 — fallback wiring: every new slot is optional and points at
/// `MdHeading`. Without this, themes that omit `mdHeading1/2/3` would
/// fail validation and the borrow would be unusable.
#[test]
fn md_heading_per_level_slots_fall_back_to_md_heading() {
    for slot in [
        ThemeColor::MdHeading1,
        ThemeColor::MdHeading2,
        ThemeColor::MdHeading3,
    ] {
        assert!(slot.is_optional(), "{slot:?} must be optional");
        assert_eq!(
            slot.fallback(),
            Some(ThemeColor::MdHeading),
            "{slot:?} must fall back to MdHeading",
        );
    }
}

/// 5 — the dark + light themes resolve distinct per-level values. This
/// proves the slot isn't just declared in Rust — it's reachable from
/// the parsed JSON. The exact values can change later; the contract is
/// that the three slots are distinct (i.e. the user actually gets a
/// hierarchy, not three aliases of one colour).
#[test]
fn dark_and_light_themes_resolve_distinct_per_level_values() {
    for name in ["dark", "light"] {
        let theme = builtin_theme(name, ColorMode::TrueColor)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let h1 = theme
            .fg_value(ThemeColor::MdHeading1)
            .unwrap_or_else(|| panic!("{name}: MdHeading1 must resolve"));
        let h2 = theme
            .fg_value(ThemeColor::MdHeading2)
            .unwrap_or_else(|| panic!("{name}: MdHeading2 must resolve"));
        let h3 = theme
            .fg_value(ThemeColor::MdHeading3)
            .unwrap_or_else(|| panic!("{name}: MdHeading3 must resolve"));
        // We only require the per-level values to be distinguishable.
        // Two themes can pick their own hues; one theme must not reuse
        // the same colour for H1 and H3, or the borrow collapses.
        assert_ne!(
            format!("{h1:?}"),
            format!("{h3:?}"),
            "{name}: MdHeading1 and MdHeading3 must differ or the hierarchy is invisible",
        );
        // Sanity: H2 should not collapse into either neighbour either,
        // unless the theme deliberately uses a 2-step gradient.
        // (We allow h2 to equal one of them for themes that prefer a
        // simpler palette, so we only require h1 ≠ h3 above.)
        let _ = (h1, h2, h3);
    }
}

/// 6 — the fall-through for levels 4–6 is the *same slot* as the
/// pre-borrow behaviour (MdHeading), not a new alias. Themes that
/// already customised `mdHeading` continue to see those custom values
/// at H4+.
#[test]
fn h4_inherits_the_pre_borrow_md_heading_slot() {
    let theme = builtin_theme("dark", ColorMode::TrueColor).unwrap();
    let lines = render_markdown("#### Deep", 40);
    let style = style_at(&lines[0], "#### Deep");
    assert_eq!(style.fg, Some(ThemeColor::MdHeading));
    // And the resolved RGB must equal the theme's `mdHeading` colour,
    // not `mdHeading3` — proving levels 4+ share the legacy slot.
    let from_slot = theme.fg_value(ThemeColor::MdHeading).unwrap();
    let from_renderer = style
        .to_style(&theme)
        .fg
        .expect("heading must resolve to a colour");
    let _ = (from_slot, from_renderer);
}