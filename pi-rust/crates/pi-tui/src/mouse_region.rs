//! On-screen mouse regions — the Rust counterpart of upstream's
//! `packages/tui/src/components/mouse-region.ts`.
//!
//! Upstream wraps a component to add mouse handling without changing its
//! rendering: `handleMouse` first offers the event to the child, then to the
//! wrapper's own `onMouse` callback, and the event's coordinates are already
//! translated into the wrapper's cell space
//! (`packages/tui/src/components/mouse-region.ts:9-33`). The rectangle itself
//! lives in the surrounding layout — `Box::handleMouse` walks its children's
//! heights to find the one under the pointer
//! (`packages/tui/src/components/box.ts:75-95`) — and the alternate screen
//! hit-tests the rendered overlay layouts before anything else, mapping
//! screen coordinates to overlay-local ones
//! (`packages/tui/src/tui.ts:824-847`).
//!
//! `pi-tui` has no component tree and no `handleMouse` trait: components are
//! plain structs owned by the [`App`](crate::App). So this module keeps the
//! part that is load-bearing today — a rectangle that owns the gestures
//! landing inside it and hands its owner the pointer's cell in the
//! rectangle's own coordinates. The `onMouse` callback half is deliberately
//! not modelled: no component handles clicks yet (`select-list.ts:109` and
//! `settings-list.ts:179` are upstream's first mouse-handling components),
//! and the [`App`](crate::App) is the region owner that acts on a hit.

use ratatui::layout::Rect;

use crate::input::MouseGesture;

/// A pointer cell inside a [`MouseRegion`], in the region's own coordinates.
///
/// `(0, 0)` is the region's top-left cell. The values are 0-based terminal
/// cells, exactly what [`MouseGesture`] carries relative to the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MouseRegionPoint {
    /// Column, from the region's left edge.
    pub x: u16,
    /// Row, from the region's top edge.
    pub y: u16,
}

impl MouseRegionPoint {
    /// Construct a region-local point.
    pub const fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }
}

/// An on-screen rectangle that owns the mouse gestures landing inside it.
///
/// The right and bottom edges are exclusive, the same test upstream runs in
/// `dispatchMouseToOverlay` (`packages/tui/src/tui.ts:828-834`): a pointer on
/// `x + width` or `y + height` is *outside*. An empty rectangle never
/// matches, so an overlay that rendered no rows cannot swallow a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseRegion {
    rect: Rect,
}

impl MouseRegion {
    /// Wrap a screen rectangle, in absolute terminal cells.
    pub const fn new(rect: Rect) -> Self {
        Self { rect }
    }

    /// The rectangle this region covers.
    pub const fn rect(&self) -> Rect {
        self.rect
    }

    /// Whether the region covers no cells at all.
    pub const fn is_empty(&self) -> bool {
        self.rect.width == 0 || self.rect.height == 0
    }

    /// Whether the absolute cell `(x, y)` is inside the region.
    pub const fn contains(&self, x: u16, y: u16) -> bool {
        if x < self.rect.x || y < self.rect.y {
            return false;
        }
        x - self.rect.x < self.rect.width && y - self.rect.y < self.rect.height
    }

    /// Hit-test an absolute cell: `Some(point)` when it is inside, with the
    /// cell translated into the region's own coordinates.
    pub const fn hit(&self, x: u16, y: u16) -> Option<MouseRegionPoint> {
        if !self.contains(x, y) {
            return None;
        }
        Some(MouseRegionPoint::new(x - self.rect.x, y - self.rect.y))
    }

    /// Hit-test a gesture: `Some(point)` when the pointer is inside the
    /// region, with the cell the region's owner receives.
    pub const fn capture(&self, gesture: MouseGesture) -> Option<MouseRegionPoint> {
        self.hit(gesture.x, gesture.y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{MouseButton, MouseGestureKind};

    fn region() -> MouseRegion {
        MouseRegion::new(Rect::new(2, 3, 4, 5))
    }

    #[test]
    fn the_top_left_cell_is_the_origin() {
        assert_eq!(region().hit(2, 3), Some(MouseRegionPoint::new(0, 0)));
    }

    #[test]
    fn cells_map_into_region_local_coordinates() {
        let region = region();
        assert_eq!(region.hit(5, 7), Some(MouseRegionPoint::new(3, 4)));
        assert_eq!(region.hit(3, 4), Some(MouseRegionPoint::new(1, 1)));
    }

    #[test]
    fn the_right_and_bottom_edges_are_exclusive() {
        let region = region();
        // The last covered cell is (5, 7); one past either edge is outside.
        assert!(region.contains(5, 7));
        assert!(!region.contains(6, 7), "one past the right edge");
        assert!(!region.contains(5, 8), "one past the bottom edge");
        assert!(!region.contains(6, 8), "past the corner");
        assert_eq!(region.hit(6, 7), None);
        assert_eq!(region.hit(5, 8), None);
    }

    #[test]
    fn cells_before_the_origin_are_outside() {
        let region = region();
        assert!(!region.contains(1, 3));
        assert!(!region.contains(2, 2));
        assert_eq!(region.hit(0, 0), None);
    }

    #[test]
    fn an_empty_region_never_matches() {
        for rect in [
            Rect::new(2, 3, 0, 5),
            Rect::new(2, 3, 4, 0),
            Rect::new(0, 0, 0, 0),
        ] {
            let region = MouseRegion::new(rect);
            assert!(region.is_empty());
            assert!(!region.contains(2, 3));
            assert_eq!(region.hit(2, 3), None);
        }
    }

    #[test]
    fn capture_keeps_the_gesture_and_the_local_cell_together() {
        let gesture = MouseGesture::new(MouseGestureKind::Press(MouseButton::Left), 4, 5, false);
        assert_eq!(region().capture(gesture), Some(MouseRegionPoint::new(2, 2)));
        let outside = MouseGesture::new(MouseGestureKind::Move, 0, 0, false);
        assert_eq!(region().capture(outside), None);
    }
}
