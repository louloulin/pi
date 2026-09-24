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
