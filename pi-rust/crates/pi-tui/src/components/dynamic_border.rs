//! `DynamicBorder` — a single horizontal divider that fills the given width.
//!
//! Mirrors upstream's `DynamicBorder`
//! (`packages/coding-agent/src/modes/interactive/components/dynamic-border.ts`).
//! Upstream renders `─`.repeat(width)` with a configurable color; we do the
//! same and accept a [`ThemeColor`] slot so the divider picks up theme
//! changes for free.

use crate::component::Component;
use crate::theme::ThemeColor;
use crate::utils::styled::{SpanStyle, StyledLine, StyledSpan};

/// A horizontal divider that stretches to the rendered width.
///
/// `DynamicBorder::new()` defaults to [`ThemeColor::Border`]. Use
/// [`DynamicBorder::with_color`] to override (upstream uses
/// `theme.fg("warning", …)` for the "Update Available" / "Package
/// Updates Available" notices).
pub struct DynamicBorder {
    color: ThemeColor,
}

impl DynamicBorder {
    /// Build a divider coloured by [`ThemeColor::Border`].
    pub fn new() -> Self {
        Self {
            color: ThemeColor::Border,
        }
    }

    /// Build a divider that paints its glyphs with `color`.
    pub fn with_color(color: ThemeColor) -> Self {
        Self { color }
    }
}

impl Default for DynamicBorder {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for DynamicBorder {
    fn render(&self, width: u16) -> Vec<StyledLine> {
        let w = width.max(1) as usize;
        let text = "─".repeat(w);
        vec![vec![StyledSpan::new(text, SpanStyle::fg(self.color))]]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::styled::plain_text;

    #[test]
    fn dynamic_border_fills_width() {
        let b = DynamicBorder::new();
        let lines = b.render(10);
        assert_eq!(lines.len(), 1);
        assert_eq!(plain_text(&lines[0]), "─".repeat(10));
    }

    #[test]
    fn dynamic_border_min_width_one() {
        let b = DynamicBorder::new();
        let lines = b.render(0);
        assert_eq!(plain_text(&lines[0]), "─");
    }
}