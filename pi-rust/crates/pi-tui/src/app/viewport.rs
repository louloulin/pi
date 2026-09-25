//! Viewport and scrollbar geometry for the TUI.
//!
//! This module extracts the per-frame geometry that the App needs to remember
//! between renders. The pointer arrives between frames, so the render path
//! writes the rectangles it painted into these atomics and the key/click paths
//! read them back.
//!
//! Upstream ports:
//! - `packages/tui/src/layout.ts` — scrollbar geometry
//! - `packages/tui/src/tui-alt-screen.ts` — viewport tracking

use std::sync::atomic::{AtomicU16, AtomicU8, AtomicUsize, Ordering};

/// Modal list kinds for hit testing.
pub const MODAL_LIST_NONE: u8 = 0;
/// Selector modal list kind.
pub const MODAL_LIST_SELECTOR: u8 = 1;
/// Dialog modal list kind.
pub const MODAL_LIST_DIALOG: u8 = 2;
/// Settings modal list kind.
pub const MODAL_LIST_SETTINGS: u8 = 3;

/// Scrollbar geometry — upstream's `getScrollbarGeometry`
/// (`packages/tui/src/layout.ts:280-326`).
///
/// The App has a single scroll view, so the convergence drops the view
/// handle and keeps only the geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbarGeometry {
    /// Absolute column the bar is painted in — the viewport's right edge.
    pub column: u16,
    /// Absolute row of the track's first cell.
    pub track_top: u16,
    /// Rows the track spans; the viewport height.
    pub track_height: u16,
    /// Absolute row of the thumb's first cell.
    pub thumb_top: u16,
    /// Rows the thumb spans — at least two, at most the whole track.
    pub thumb_height: u16,
    /// Largest valid top-relative scroll offset (`content - viewport`).
    pub max_scroll: usize,
}

/// In-flight scrollbar drag — upstream's `ScrollbarDrag`
/// (`packages/tui/src/tui-alt-screen.ts:129-138`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbarDrag {
    /// Rows between the pointer and the thumb's top when the press landed.
    pub grab_offset: u16,
}

/// Per-frame geometry the App needs to remember between renders.
///
/// Each field stays atomic (not `Cell`) because the App can be read by
/// the render task and the input task at the same time.
pub struct ViewportGeometry {
    /// Width of the message viewport as of the last render.
    pub viewport_width: AtomicU16,
    /// Columns the scrollbar reserved from the viewport width.
    pub viewport_reserved: AtomicU16,
    /// Height of the message viewport as of the last render.
    pub viewport_height: AtomicU16,
    /// Top-left cell of the message viewport.
    pub viewport_origin: (AtomicU16, AtomicU16),
    /// Width of the composer body as of the last render.
    pub composer_body_width: AtomicU16,
    /// First draft row the composer window showed as of the last render.
    pub composer_scroll: AtomicUsize,
    /// Rows the composer window could show as of the last render.
    pub composer_window: AtomicU16,
    /// Rectangle of the composer area as of the last render.
    pub composer_origin: (AtomicU16, AtomicU16),
    /// `(width, height)` of the composer area.
    pub composer_size: (AtomicU16, AtomicU16),
    /// Rectangle of the autocomplete dropdown as of the last render.
    pub autocomplete_origin: (AtomicU16, AtomicU16),
    /// `(width, height)` of the autocomplete dropdown.
    pub autocomplete_size: (AtomicU16, AtomicU16),
    /// Index of the first candidate in the dropdown.
    pub autocomplete_first_item: AtomicUsize,
    /// How many candidate rows the dropdown painted.
    pub autocomplete_item_rows: AtomicUsize,
    /// Row geometry of the modal list the last frame painted.
    pub modal_list_kind: AtomicU8,
    /// Absolute row the modal list's first item row landed on.
    pub modal_list_first_row: AtomicU16,
    /// Filtered index the first row carries.
    pub modal_list_first_item: AtomicUsize,
    /// Item rows that followed.
    pub modal_list_rows: AtomicUsize,
    /// Rectangle of the "jump to latest" pill.
    pub scroll_to_end: (AtomicU16, AtomicU16, AtomicU16),
    /// Rectangle of the "cut above" hint.
    pub truncated_above: (AtomicU16, AtomicU16, AtomicU16),
}

impl ViewportGeometry {
    /// Default-initialised geometry with every counter at zero.
    pub fn new() -> Self {
        Self {
            viewport_width: AtomicU16::new(0),
            viewport_reserved: AtomicU16::new(0),
            viewport_height: AtomicU16::new(0),
            viewport_origin: (AtomicU16::new(0), AtomicU16::new(0)),
            composer_body_width: AtomicU16::new(0),
            composer_scroll: AtomicUsize::new(0),
            composer_window: AtomicU16::new(0),
            composer_origin: (AtomicU16::new(0), AtomicU16::new(0)),
            composer_size: (AtomicU16::new(0), AtomicU16::new(0)),
            autocomplete_origin: (AtomicU16::new(0), AtomicU16::new(0)),
            autocomplete_size: (AtomicU16::new(0), AtomicU16::new(0)),
            autocomplete_first_item: AtomicUsize::new(0),
            autocomplete_item_rows: AtomicUsize::new(0),
            modal_list_kind: AtomicU8::new(MODAL_LIST_NONE),
            modal_list_first_row: AtomicU16::new(0),
            modal_list_first_item: AtomicUsize::new(0),
            modal_list_rows: AtomicUsize::new(0),
            scroll_to_end: (AtomicU16::new(0), AtomicU16::new(0), AtomicU16::new(0)),
            truncated_above: (AtomicU16::new(0), AtomicU16::new(0), AtomicU16::new(0)),
        }
    }

    /// Load the viewport width.
    pub fn viewport_width(&self) -> u16 {
        self.viewport_width.load(Ordering::Relaxed)
    }

    /// Load the viewport height.
    pub fn viewport_height(&self) -> u16 {
        self.viewport_height.load(Ordering::Relaxed)
    }

    /// Load the viewport origin as `(x, y)`.
    pub fn viewport_origin(&self) -> (u16, u16) {
        (
            self.viewport_origin.0.load(Ordering::Relaxed),
            self.viewport_origin.1.load(Ordering::Relaxed),
        )
    }

    /// Load the composer scroll offset.
    pub fn composer_scroll(&self) -> usize {
        self.composer_scroll.load(Ordering::Relaxed)
    }

    /// Load the composer window height.
    pub fn composer_window(&self) -> u16 {
        self.composer_window.load(Ordering::Relaxed)
    }

    /// Load the composer body width.
    pub fn composer_body_width(&self) -> u16 {
        self.composer_body_width.load(Ordering::Relaxed)
    }

    /// Load the modal list kind.
    pub fn modal_list_kind(&self) -> u8 {
        self.modal_list_kind.load(Ordering::Relaxed)
    }

    /// Check if a modal list is visible.
    pub fn has_modal_list(&self) -> bool {
        self.modal_list_kind.load(Ordering::Relaxed) != MODAL_LIST_NONE
    }
}

impl Default for ViewportGeometry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn modal_list_kind_constants_are_distinct() {
        // The hit-test relies on every constant being a unique non-zero
        // value (zero is the "no modal" sentinel).
        assert_eq!(MODAL_LIST_NONE, 0);
        assert_ne!(MODAL_LIST_SELECTOR, MODAL_LIST_NONE);
        assert_ne!(MODAL_LIST_DIALOG, MODAL_LIST_NONE);
        assert_ne!(MODAL_LIST_SETTINGS, MODAL_LIST_NONE);
        assert_ne!(MODAL_LIST_SELECTOR, MODAL_LIST_DIALOG);
        assert_ne!(MODAL_LIST_SELECTOR, MODAL_LIST_SETTINGS);
        assert_ne!(MODAL_LIST_DIALOG, MODAL_LIST_SETTINGS);
    }

    #[test]
    fn new_starts_every_field_at_zero_or_modal_list_none() {
        let g = ViewportGeometry::new();
        assert_eq!(g.viewport_width(), 0);
        assert_eq!(g.viewport_height(), 0);
        assert_eq!(g.viewport_origin(), (0, 0));
        assert_eq!(g.composer_scroll(), 0);
        assert_eq!(g.composer_window(), 0);
        assert_eq!(g.composer_body_width(), 0);
        assert_eq!(g.modal_list_kind(), MODAL_LIST_NONE);
        assert!(!g.has_modal_list());
    }

    #[test]
    fn default_matches_new() {
        let a = ViewportGeometry::default();
        let b = ViewportGeometry::new();
        assert_eq!(a.viewport_width(), b.viewport_width());
        assert_eq!(a.viewport_height(), b.viewport_height());
        assert_eq!(a.viewport_origin(), b.viewport_origin());
        assert_eq!(a.composer_scroll(), b.composer_scroll());
        assert_eq!(a.modal_list_kind(), b.modal_list_kind());
        assert_eq!(a.has_modal_list(), b.has_modal_list());
    }

    #[test]
    fn viewport_width_round_trips_through_store() {
        let g = ViewportGeometry::new();
        g.viewport_width.store(120, Ordering::Relaxed);
        g.viewport_height.store(40, Ordering::Relaxed);
        assert_eq!(g.viewport_width(), 120);
        assert_eq!(g.viewport_height(), 40);
    }

    #[test]
    fn viewport_origin_round_trips() {
        let g = ViewportGeometry::new();
        g.viewport_origin.0.store(3, Ordering::Relaxed);
        g.viewport_origin.1.store(7, Ordering::Relaxed);
        assert_eq!(g.viewport_origin(), (3, 7));
    }

    #[test]
    fn composer_fields_round_trip() {
        let g = ViewportGeometry::new();
        g.composer_scroll.store(8, Ordering::Relaxed);
        g.composer_window.store(3, Ordering::Relaxed);
        g.composer_body_width.store(60, Ordering::Relaxed);
        assert_eq!(g.composer_scroll(), 8);
        assert_eq!(g.composer_window(), 3);
        assert_eq!(g.composer_body_width(), 60);
    }

    #[test]
    fn modal_list_kind_distinguishes_open_from_closed() {
        let g = ViewportGeometry::new();
        assert!(!g.has_modal_list());
        g.modal_list_kind.store(MODAL_LIST_SELECTOR, Ordering::Relaxed);
        assert!(g.has_modal_list());
        assert_eq!(g.modal_list_kind(), MODAL_LIST_SELECTOR);
        g.modal_list_kind.store(MODAL_LIST_DIALOG, Ordering::Relaxed);
        assert_eq!(g.modal_list_kind(), MODAL_LIST_DIALOG);
        g.modal_list_kind.store(MODAL_LIST_SETTINGS, Ordering::Relaxed);
        assert_eq!(g.modal_list_kind(), MODAL_LIST_SETTINGS);
        g.modal_list_kind.store(MODAL_LIST_NONE, Ordering::Relaxed);
        assert!(!g.has_modal_list());
    }

    #[test]
    fn scrollbar_geometry_holds_layout_rects() {
        // A pure-data struct; just exercise field access so the struct
        // doesn't silently drift away from its layout contract.
        let g = ScrollbarGeometry {
            column: 79,
            track_top: 0,
            track_height: 24,
            thumb_top: 6,
            thumb_height: 4,
            max_scroll: 100,
        };
        assert_eq!(g.column, 79);
        assert_eq!(g.track_height, 24);
        assert_eq!(g.thumb_height, 4);
        assert_eq!(g.max_scroll, 100);
    }

    #[test]
    fn scrollbar_drag_holds_grab_offset() {
        let d = ScrollbarDrag { grab_offset: 3 };
        assert_eq!(d.grab_offset, 3);
    }
}
