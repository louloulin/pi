//! Overlay stack — Rust port of `packages/tui/src/tui.ts:175-314`.
//!
//! `TuiBase` owns the overlay stack; every entry is a [`CoreComponent`]
//! rendered on top of the base content with a position derived from
//! [`OverlayOptions`]. The stack order is LIFO, focusable, and the
//! topmost visible entry holds keyboard focus unless it is
//! `non_capturing`.
//!
//! The shape matches upstream one-for-one:
//! [`OverlayAnchor`] (9 positions), [`OverlayMargin`] (top/right/
//! bottom/left), [`SizeValue`] (absolute cells or percent), and
//! [`OverlayHandle`] (id + visibility flag + focus control + last
//! rendered bounds).

use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::layout::Rect;

/// Anchor position for an overlay.
///
/// Matches upstream `OverlayAnchor`
/// (`packages/tui/src/tui.ts:175-184`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverlayAnchor {
    #[default]
    Center,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    TopCenter,
    BottomCenter,
    LeftCenter,
    RightCenter,
}

/// Margin configuration. Mirrors `OverlayMargin`
/// (`packages/tui/src/tui.ts:189-194`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OverlayMargin {
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
    pub left: u16,
}

impl OverlayMargin {
    /// Build a uniform margin.
    pub fn uniform(value: u16) -> Self {
        Self { top: value, right: value, bottom: value, left: value }
    }
}

/// Size value: either an absolute cell count or a percentage of the
/// reference dimension. Mirrors `SizeValue`
/// (`packages/tui/src/tui.ts:197`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeValue {
    Cells(u16),
    Percent(u8),
}

impl SizeValue {
    /// Resolve to a cell count given the reference dimension.
    pub fn resolve(self, reference: u16) -> u16 {
        match self {
            SizeValue::Cells(c) => c,
            SizeValue::Percent(p) => ((reference as u32 * p as u32) / 100) as u16,
        }
    }
}

/// Options for [`TuiBase::show_overlay`]. Mirrors `OverlayOptions`
/// (`packages/tui/src/tui.ts:215-251`).
#[derive(Debug, Clone, Copy, Default)]
pub struct OverlayOptions {
    /// Width in cells (or percentage).
    pub width: Option<SizeValue>,
    /// Minimum width in cells.
    pub min_width: Option<u16>,
    /// Maximum height in cells (or percentage).
    pub max_height: Option<SizeValue>,
    /// Anchor point.
    pub anchor: Option<OverlayAnchor>,
    /// Horizontal offset from the anchor.
    pub offset_x: Option<i16>,
    /// Vertical offset from the anchor.
    pub offset_y: Option<i16>,
    /// Row position (absolute or percentage of terminal height).
    pub row: Option<SizeValue>,
    /// Column position (absolute or percentage of terminal width).
    pub col: Option<SizeValue>,
    /// Margin from terminal edges.
    pub margin: Option<OverlayMargin>,
    /// Don't capture keyboard focus when shown.
    pub non_capturing: bool,
}

impl OverlayOptions {
    /// Common shape: centered, with a 1-cell margin.
    pub fn centered() -> Self {
        Self {
            anchor: Some(OverlayAnchor::Center),
            margin: Some(OverlayMargin::uniform(1)),
            ..Self::default()
        }
    }
}

/// Last rendered overlay rectangle in terminal-relative coordinates.
/// Mirrors `OverlayBounds` (`packages/tui/src/tui.ts:260-265`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayBounds {
    pub row: u16,
    pub col: u16,
    pub width: u16,
    pub height: u16,
}

impl From<Rect> for OverlayBounds {
    fn from(rect: Rect) -> Self {
        Self { row: rect.y, col: rect.x, width: rect.width, height: rect.height }
    }
}

/// Focus restoration policy when a focus change interacts with a
/// blocking overlay. Mirrors `OverlayFocusRestorePolicy`
/// (`packages/tui/src/tui.ts:314`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayPolicy {
    /// Clear the restoration state when focus moves elsewhere.
    Clear,
    /// Preserve it; the overlay will reclaim focus when the blocking
    /// component releases it.
    Preserve,
}

/// Internal entry in the overlay stack. Mirrors `OverlayStackEntry`
/// (`packages/tui/src/tui.ts:287-294`).
#[derive(Debug)]
pub struct OverlayEntry {
    pub id: u64,
    pub bounds: Option<OverlayBounds>,
    pub options: OverlayOptions,
    pub hidden: bool,
    pub focus_order: u64,
    pub pre_focus_id: Option<String>,
}

/// Opaque handle returned by [`TuiBase::show_overlay`].
///
/// Mirrors `OverlayHandle` (`packages/tui/src/tui.ts:270-285`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OverlayHandle {
    id: u64,
}

static OVERLAY_ID: AtomicU64 = AtomicU64::new(1);

impl OverlayHandle {
    pub(crate) fn next() -> Self {
        Self { id: OVERLAY_ID.fetch_add(1, Ordering::Relaxed) }
    }

    /// Internal id used by `TuiBase` to look the entry up.
    pub fn id(self) -> u64 {
        self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tui_base::TuiBase;
    use crate::core::component::{CoreComponent, Focusable};
    use crate::core::focus::{FocusReason, FocusTarget};
    use crate::core::tui_base::{FocusPair, TuiBaseConfig, TuiMode};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use std::rc::Rc;
    use std::cell::RefCell;

    struct Empty;
    impl CoreComponent for Empty {
        fn render(&mut self, _: Rect, _: &mut Buffer) {}
        fn as_focus_target(&mut self) -> Option<FocusTarget<'_>> { None }
        fn as_any(&self) -> &dyn std::any::Any { self }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    }
    impl crate::core::component::Focusable for Empty {
        fn focused(&self) -> bool { false }
        fn set_focused(&mut self, _: bool) {}
        fn focus_changed(&mut self, _: FocusReason) {}
    }

    fn fresh() -> (TuiBase, Rc<RefCell<bool>>) {
        let cell = Rc::new(RefCell::new(false));
        let mut base = TuiBase::new(Box::new(Empty), TuiBaseConfig::default());
        base.register_focusable_pair("root", FocusPair::from_cell(cell.clone(), None));
        base.set_focus(Some("root"), FocusReason::Explicit);
        (base, cell)
    }

    #[test]
    fn handle_ids_are_unique() {
        let h1 = OverlayHandle::next();
        let h2 = OverlayHandle::next();
        let h3 = OverlayHandle::next();
        assert_ne!(h1.id(), h2.id());
        assert_ne!(h2.id(), h3.id());
        assert_ne!(h1.id(), h3.id());
    }

    #[test]
    fn show_overlay_stores_entry() {
        let (mut base, _) = fresh();
        let h = base.show_overlay(OverlayOptions::centered());
        assert!(base.has_overlay());
        assert_eq!(base.topmost_overlay_bounds(), None);
        base.hide_overlay(h);
        assert!(!base.has_overlay());
    }

    #[test]
    fn hidden_overlay_does_not_block_topmost() {
        let (mut base, _) = fresh();
        let h1 = base.show_overlay(OverlayOptions::default());
        let h2 = base.show_overlay(OverlayOptions::default());
        base.set_overlay_hidden(h1, true);
        assert!(base.has_overlay());
        // h2 is topmost visible now
        base.hide_overlay(h2);
        // h1 is still hidden — has_overlay reports no visible overlay
        assert!(!base.has_overlay());
        // unhiding h1 brings it back
        base.set_overlay_hidden(h1, false);
        assert!(base.has_overlay());
        base.hide_overlay(h1);
        assert!(!base.has_overlay());
    }

    #[test]
    fn pre_focus_restored_on_pop() {
        let (mut base, cell) = fresh();
        assert!(*cell.borrow());
        let h = base.show_overlay(OverlayOptions::default());
        // Hide without changing focus; close it again
        base.hide_overlay(h);
        assert!(*cell.borrow());
    }

    #[test]
    fn size_value_percent_resolves() {
        assert_eq!(SizeValue::Percent(50).resolve(80), 40);
        assert_eq!(SizeValue::Percent(0).resolve(80), 0);
        assert_eq!(SizeValue::Cells(12).resolve(80), 12);
    }

    #[test]
    fn margin_uniform_sets_all_sides() {
        let m = OverlayMargin::uniform(4);
        assert_eq!(m.top, 4);
        assert_eq!(m.right, 4);
        assert_eq!(m.bottom, 4);
        assert_eq!(m.left, 4);
    }

    #[test]
    fn overlay_options_default_is_centered_anchor() {
        let opts = OverlayOptions::default();
        assert!(opts.anchor.is_none());
        assert!(!opts.non_capturing);
    }

    #[test]
    fn overlay_options_centered_helper() {
        let opts = OverlayOptions::centered();
        assert_eq!(opts.anchor, Some(OverlayAnchor::Center));
        assert_eq!(opts.margin, Some(OverlayMargin::uniform(1)));
    }

    #[test]
    fn overlay_anchor_default_is_center() {
        assert_eq!(OverlayAnchor::default(), OverlayAnchor::Center);
    }
}