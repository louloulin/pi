//! Morandi palette — nanopi-inspired Indexed-256 colours that render the
//! same on tmux-256color, xterm, iTerm2, WezTerm, and any truecolor
//! terminal. Borrowed from `nanopi/src/mode/tui.rs:4398-4419` (the
//! "muted navy / muted sage / dusty rose" Morandi palette).
//!
//! Use [`morandi_theme`] to build a fully-indexed theme for the case
//! when no JSON is loaded (low-color terminals, headless smoke tests).
//! Use the [`Morandi`] constants directly when an ad-hoc component
//! needs one specific slot — most components stay on the theme system
//! and never reach for these.

use std::collections::HashMap;

/// All eight Morandi slots, plus a couple of dim foregrounds the status
/// strip needs to mix-and-match. Indexed values picked to render
/// identically on 256-color and truecolor terminals — the cube lookup
/// picks the closest cube level so a truecolor host sees the same hue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Morandi {
    /// `Indexed(24)` — muted navy. Tool-running status strip bg
    /// (matches nanopi `blue_bg` at tui.rs:5322).
    pub navy: u8,
    /// `Indexed(65)` — muted sage. Tool success bg, default positive
    /// accent (matches nanopi success at tui.rs:4404).
    pub sage: u8,
    /// `Indexed(108)` — sage italic / thinking / selection arrow.
    /// Lighter than `sage` so italic stays readable on Reset bg.
    pub sage_light: u8,
    /// `Indexed(131)` — dusty rose. Tool error bg (matches nanopi
    /// `error` at tui.rs:4404).
    pub rose: u8,
    /// `Indexed(230)` — warm light foreground for bar text on dark bgs.
    pub bar_fg: u8,
    /// `Indexed(253)` — secondary foreground on bar bg.
    pub bar_dim: u8,
    /// `Indexed(250)` — italic hint foreground on bar bg.
    pub bar_hint: u8,
    /// `Indexed(238)` — user-message block bg (matches nanopi
    /// `user_bg` at tui.rs:4554).
    pub user_bg: u8,
    /// `Indexed(255)` — bright fg on `user_bg`.
    pub user_fg: u8,
}

impl Default for Morandi {
    fn default() -> Self {
        Self {
            navy: 24,
            sage: 65,
            sage_light: 108,
            rose: 131,
            bar_fg: 230,
            bar_dim: 253,
            bar_hint: 250,
            user_bg: 238,
            user_fg: 255,
        }
    }
}

impl Morandi {
    /// Stable handle used by the theme system to mark a theme as
    /// "Morandi fallback" — when JSON isn't loaded, the caller can
    /// fall back to the Indexed Morandi theme for portable rendering.
    pub const SCHEMA_NAME: &'static str = "morandi-fallback";

    /// The single Morandi instance. Cheap to copy — eight `u8`s.
    pub fn palette() -> Self {
        Self::default()
    }
}

/// Build a Theme whose every slot is the corresponding Morandi Indexed
/// colour. Used by tests and any code path that explicitly wants
/// 256-color rendering (e.g. the snapshot smoke test on a 256-color
/// terminal).
pub fn morandi_theme() -> crate::theme::Theme {
    use crate::theme::{ColorMode, ColorValue, Theme, ThemeBg, ThemeColor};

    let p = Morandi::palette();
    let idx = |i: u8| ColorValue::Index(i);
    let fg_values: HashMap<ThemeColor, ColorValue> = [
        (ThemeColor::Accent, idx(p.sage_light)),
        (ThemeColor::Border, idx(p.sage_light)),
        (ThemeColor::BorderAccent, idx(p.sage)),
        (ThemeColor::BorderMuted, idx(250)),
        (ThemeColor::Success, idx(p.sage)),
        (ThemeColor::Error, idx(p.rose)),
        (ThemeColor::Warning, idx(136)),
        (ThemeColor::Muted, idx(246)),
        (ThemeColor::Dim, idx(245)),
        (ThemeColor::Text, idx(252)),
        (ThemeColor::ThinkingText, idx(p.sage_light)),
        (ThemeColor::ScrollbarTrack, idx(238)),
        (ThemeColor::ScrollbarThumb, idx(245)),
        (ThemeColor::SearchMatchText, idx(p.bar_fg)),
        (ThemeColor::UserMessageText, idx(p.user_fg)),
        (ThemeColor::CustomMessageText, idx(252)),
        (ThemeColor::CustomMessageLabel, idx(p.sage_light)),
        (ThemeColor::ToolTitle, idx(p.bar_fg)),
        (ThemeColor::ToolOutput, idx(p.bar_dim)),
        (ThemeColor::MdHeading, idx(p.sage_light)),
        (ThemeColor::MdHeading1, idx(p.sage)),
        (ThemeColor::MdHeading2, idx(p.sage_light)),
        (ThemeColor::MdHeading3, idx(110)),
        (ThemeColor::MdLink, idx(111)),
        (ThemeColor::MdLinkUrl, idx(111)),
        (ThemeColor::MdCode, idx(p.bar_fg)),
        (ThemeColor::MdCodeBlock, idx(p.bar_dim)),
        (ThemeColor::MdCodeBlockBorder, idx(240)),
        (ThemeColor::MdQuote, idx(245)),
        (ThemeColor::MdQuoteBorder, idx(240)),
        (ThemeColor::MdHr, idx(240)),
        (ThemeColor::MdListBullet, idx(p.sage_light)),
        (ThemeColor::ToolDiffAdded, idx(p.sage)),
        (ThemeColor::ToolDiffRemoved, idx(p.rose)),
        (ThemeColor::ToolDiffContext, idx(245)),
        (ThemeColor::SyntaxComment, idx(245)),
        (ThemeColor::SyntaxKeyword, idx(p.sage_light)),
        (ThemeColor::SyntaxFunction, idx(p.sage_light)),
        (ThemeColor::SyntaxVariable, idx(252)),
        (ThemeColor::SyntaxString, idx(p.sage)),
        (ThemeColor::SyntaxNumber, idx(136)),
        (ThemeColor::SyntaxType, idx(p.sage_light)),
        (ThemeColor::SyntaxOperator, idx(245)),
        (ThemeColor::SyntaxPunctuation, idx(245)),
        (ThemeColor::ThinkingOff, idx(245)),
        (ThemeColor::ThinkingMinimal, idx(p.sage_light)),
        (ThemeColor::ThinkingLow, idx(p.sage_light)),
        (ThemeColor::ThinkingMedium, idx(p.sage_light)),
        (ThemeColor::ThinkingHigh, idx(p.sage)),
        (ThemeColor::ThinkingXhigh, idx(p.sage)),
        (ThemeColor::ThinkingMax, idx(p.sage)),
        (ThemeColor::BashMode, idx(111)),
        (ThemeColor::Brand, idx(p.sage_light)),
        (ThemeColor::Hint, idx(245)),
        (ThemeColor::FgSecondary, idx(245)),
        (ThemeColor::Caption, idx(245)),
    ]
    .into_iter()
    .collect();

    let bg_values: HashMap<ThemeBg, ColorValue> = [
        (ThemeBg::SelectedBg, idx(60)),
        (ThemeBg::SearchMatchBg, idx(58)),
        (ThemeBg::UserMessageBg, idx(p.user_bg)),
        (ThemeBg::CustomMessageBg, idx(238)),
        (ThemeBg::ToolPendingBg, idx(p.navy)),
        (ThemeBg::ToolSuccessBg, idx(p.sage)),
        (ThemeBg::ToolErrorBg, idx(p.rose)),
        (ThemeBg::Panel, idx(238)),
        (ThemeBg::MdCodeBg, idx(236)),
    ]
    .into_iter()
    .collect();

    Theme::new(
        fg_values,
        bg_values,
        ColorMode::Ansi256,
        Some(Morandi::SCHEMA_NAME.to_string()),
        None,
    )
    .expect("Morandi theme covers every required slot")
}

/// Choose the colour to render a context-usage percentage at.
/// Matches nanopi `context_color` at tui.rs:88-96: >=90 → red,
/// >=70 → yellow, else sage.
pub fn context_color(pct: f64) -> MorandiAccent {
    if pct >= 90.0 {
        MorandiAccent::Rose
    } else if pct >= 70.0 {
        MorandiAccent::Warning
    } else {
        MorandiAccent::SageLight
    }
}

/// Which Morandi accent a context percentage should use. Resolves to
/// an Indexed slot at render time so the caller doesn't have to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MorandiAccent {
    SageLight,
    Warning,
    Rose,
}

impl MorandiAccent {
    /// The Indexed slot this accent maps to.
    pub fn slot(self) -> u8 {
        match self {
            MorandiAccent::SageLight => Morandi::palette().sage_light,
            MorandiAccent::Warning => 136,
            MorandiAccent::Rose => Morandi::palette().rose,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_morandi_values_match_nanopi_palette() {
        let p = Morandi::default();
        // Cross-checked against nanopi/src/mode/tui.rs:5322, 4404, 4554.
        assert_eq!(p.navy, 24, "tool-running status strip bg");
        assert_eq!(p.sage, 65, "tool success bg");
        assert_eq!(p.rose, 131, "tool error bg");
        assert_eq!(p.user_bg, 238, "user message bg");
    }

    #[test]
    fn context_color_thresholds_match_nanopi() {
        // nanopi tui.rs:88-96: red at >=90, yellow at >=70, sage otherwise.
        assert_eq!(context_color(95.0), MorandiAccent::Rose);
        assert_eq!(context_color(90.0), MorandiAccent::Rose);
        assert_eq!(context_color(89.9), MorandiAccent::Warning);
        assert_eq!(context_color(70.0), MorandiAccent::Warning);
        assert_eq!(context_color(69.9), MorandiAccent::SageLight);
        assert_eq!(context_color(0.0), MorandiAccent::SageLight);
    }

    #[test]
    fn context_color_slots_are_distinct_indexed_values() {
        // The three accent slots must be distinct so the colour shift
        // is visible at render time. Slot 136 (warning) was chosen
        // because it lands in the same warm range as `rose` but with
        // a different hue.
        let rose = MorandiAccent::Rose.slot();
        let warning = MorandiAccent::Warning.slot();
        let sage = MorandiAccent::SageLight.slot();
        assert_ne!(rose, warning);
        assert_ne!(warning, sage);
        assert_ne!(rose, sage);
    }

    #[test]
    fn morandi_theme_covers_every_required_slot() {
        // `Theme::new` errors on a missing required slot — calling it
        // covers both "all fg/bg present" and "no theme parse error".
        let theme = morandi_theme();
        assert_eq!(theme.name.as_deref(), Some(Morandi::SCHEMA_NAME));
        assert_eq!(theme.mode, crate::theme::ColorMode::Ansi256);
    }

    #[test]
    fn morandi_theme_uses_indexed_values_only() {
        // Every slot in a Morandi theme must be Indexed — the whole
        // point is portable 256-color rendering.
        use crate::theme::ColorValue;
        let theme = morandi_theme();
        for slot in crate::theme::ThemeColor::ALL {
            let value = theme
                .fg_values
                .get(&slot)
                .expect("fg slot resolved");
            assert!(
                matches!(value, ColorValue::Index(_)),
                "fg slot {:?} must be Indexed, got {:?}",
                slot,
                value,
            );
        }
        for slot in crate::theme::ThemeBg::ALL {
            let value = theme
                .bg_values
                .get(&slot)
                .expect("bg slot resolved");
            assert!(
                matches!(value, ColorValue::Index(_)),
                "bg slot {:?} must be Indexed, got {:?}",
                slot,
                value,
            );
        }
    }
}