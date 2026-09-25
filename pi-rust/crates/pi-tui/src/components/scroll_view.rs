//! `ScrollView` — a scrollable viewport around a child component.
//!
//! Mirrors upstream `ScrollView`
//! (`packages/tui/src/components/scroll-view.ts`).
//!
//! Phase 11.5 — the scrollbar now supports three modes:
//!
//! * `hidden` — no scrollbar (legacy default).
//! * `auto` — shown only when the content overflows the viewport, and only
//!   for `scrollbarHideDelayMs` after the last scroll input (upstream's
//!   1 s transient fade, `scroll-view.ts:55`).
//! * `always` — drawn every frame, no auto-hide.
//!
//! The bar lives in the last column of the viewport; the thumb is a single
//! `█` glyph whose row position is proportional to the scroll offset,
//! exactly the way upstream's `LayoutNode.getScrollThumb` paints it
//! (`packages/tui/src/layout-node.ts`). Track and thumb styles are
//! configurable so the embedder can match its theme — defaults are the
//! dim grey upstream uses for `scrollbarTrack` and the brighter grey for
//! `scrollbarThumb`.

use std::any::Any;

use crate::component::Component;
use crate::app::layout_node::{ScrollLayoutState, ScrollOverscroll};
use crate::utils::styled::{SpanStyle, StyledLine, StyledSpan};

/// Scrollbar visibility — upstream `ScrollViewScrollbar`
/// (`packages/tui/src/components/scroll-view.ts:4`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollbarMode {
    /// No scrollbar.
    #[default]
    Hidden,
    /// Shown only when content overflows the viewport; fades after the
    /// user stops scrolling for [`ScrollViewOptions::scrollbar_hide_delay_ms`].
    Auto,
    /// Always shown when the viewport is known.
    Always,
}

/// Upstream `ScrollViewOptions` — scroll options.
#[derive(Debug, Clone)]
pub struct ScrollViewOptions {
    /// Legacy boolean — when `true`, equivalent to [`ScrollbarMode::Always`].
    /// New code should prefer [`ScrollViewOptions::scrollbar`].
    pub show_scrollbar: bool,
    /// Scrollbar style.
    pub scrollbar_style: SpanStyle,
    /// Modern mode-based scrollbar configuration.
    pub scrollbar: ScrollbarMode,
    /// Track glyph style (the `│` column behind the thumb).
    pub scrollbar_track_style: SpanStyle,
    /// Thumb glyph style (the `█` block that shows the position).
    pub scrollbar_thumb_style: SpanStyle,
    /// Milliseconds the `auto` scrollbar stays visible after the last scroll
    /// before fading. Upstream's default is 1000 ms.
    pub scrollbar_hide_delay_ms: u64,
}

impl Default for ScrollViewOptions {
    fn default() -> Self {
        Self {
            show_scrollbar: false,
            scrollbar_style: SpanStyle::fg(crate::theme::ThemeColor::Dim),
            scrollbar: ScrollbarMode::Hidden,
            scrollbar_track_style: SpanStyle::fg(crate::theme::ThemeColor::ScrollbarTrack),
            scrollbar_thumb_style: SpanStyle::fg(crate::theme::ThemeColor::ScrollbarThumb),
            scrollbar_hide_delay_ms: 1000,
        }
    }
}

/// Upstream `ScrollViewScrollbar` — scrollbar rendering options.
///
/// New code should prefer the structured [`ScrollbarMode`]; this struct is
/// kept for backwards compatibility with the legacy
/// `ScrollViewOptions::show_scrollbar` boolean.
#[derive(Debug, Clone, Default)]
pub struct ScrollViewScrollbar {
    /// Whether the scrollbar is shown.
    pub show: bool,
    /// Span style.
    pub style: SpanStyle,
}

impl ScrollViewScrollbar {
    /// Convert to the new structured mode.
    pub fn to_mode(&self) -> ScrollbarMode {
        if self.show {
            ScrollbarMode::Always
        } else {
            ScrollbarMode::Hidden
        }
    }
}

/// Upstream `ScrollViewScrollToOptions` — scroll-to options.
#[derive(Debug, Clone, Default)]
pub struct ScrollViewScrollToOptions {
    /// Vertical offset in rows.
    pub row: i32,
}

/// Upstream `ScrollView` — a scrollable viewport around a child.
pub struct ScrollView {
    child: Box<dyn Component>,
    scroll_row: i32,
    options: ScrollViewOptions,
    cached_lines: Vec<StyledLine>,
    dirty: bool,
    /// Last user-driven scroll event (system time ms). `None` until the
    /// user moves the viewport; used by the `auto` mode to fade the bar.
    last_scroll_at_ms: Option<u64>,
    /// Height of the viewport that was visible last frame. `0` = unknown.
    last_viewport_height: u16,
}

impl std::fmt::Debug for ScrollView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScrollView")
            .field("scroll_row", &self.scroll_row)
            .field("dirty", &self.dirty)
            .field("last_viewport_height", &self.last_viewport_height)
            .finish()
    }
}

impl ScrollView {
    /// Wrap a child component.
    pub fn new(child: Box<dyn Component>) -> Self {
        Self {
            child,
            scroll_row: 0,
            options: ScrollViewOptions::default(),
            cached_lines: Vec::new(),
            dirty: true,
            last_scroll_at_ms: None,
            last_viewport_height: 0,
        }
    }

    /// Wrap a child component with scroll options.
    pub fn with_options(child: Box<dyn Component>, options: ScrollViewOptions) -> Self {
        Self {
            child,
            scroll_row: 0,
            options,
            cached_lines: Vec::new(),
            dirty: true,
            last_scroll_at_ms: None,
            last_viewport_height: 0,
        }
    }

    /// Set the scrollbar mode after construction. Upstream
    /// `setScrollbar` (`scroll-view.ts:85-91`).
    pub fn set_scrollbar(&mut self, mode: ScrollbarMode) {
        self.options.scrollbar = mode;
        // Any non-`auto` mode clears the transient timer — the bar is
        // either always on or always off, so a leftover fade-out cannot
        // turn it off later.
        if mode != ScrollbarMode::Auto {
            self.last_scroll_at_ms = None;
        }
    }

    /// Scroll to a vertical offset (clamped to content length).
    pub fn scroll_to(&mut self, opts: ScrollViewScrollToOptions) {
        self.scroll_row = opts.row.max(0);
        self.dirty = true;
        self.last_scroll_at_ms = Some(current_time_ms());
    }

    /// Scroll down by `delta` rows.
    pub fn scroll_down(&mut self, delta: i32) {
        self.scroll_to(ScrollViewScrollToOptions { row: self.scroll_row + delta });
    }

    /// Scroll up by `delta` rows.
    pub fn scroll_up(&mut self, delta: i32) {
        self.scroll_to(ScrollViewScrollToOptions { row: self.scroll_row - delta });
    }

    /// Current vertical scroll offset.
    pub fn scroll_row(&self) -> i32 {
        self.scroll_row
    }

    /// Mutable access to the wrapped child.
    pub fn child_mut(&mut self) -> &mut Box<dyn Component> {
        self.dirty = true;
        &mut self.child
    }

    /// Update the viewport height known to the scrollbar. The App calls
    /// this after laying out so the auto-mode fade math has a real
    /// `viewport_height` to compare against `content_height`.
    pub fn set_viewport_height(&mut self, height: u16) {
        self.last_viewport_height = height;
    }

    /// Whether the scrollbar would be drawn at this frame given the cached
    /// viewport and last-scroll timestamp. Useful for tests and for hosts
    /// that want to know the bar's state without rendering.
    pub fn is_scrollbar_visible(&self, content_height: usize) -> bool {
        let mode = self.effective_mode();
        match mode {
            ScrollbarMode::Hidden => false,
            ScrollbarMode::Always => self.last_viewport_height > 0,
            ScrollbarMode::Auto => {
                if content_height <= self.last_viewport_height as usize {
                    return false;
                }
                match self.last_scroll_at_ms {
                    None => false,
                    Some(at) => {
                        let now = current_time_ms();
                        now.saturating_sub(at) <= self.options.scrollbar_hide_delay_ms
                    }
                }
            }
        }
    }

    /// The mode the legacy boolean upgrades to.
    fn effective_mode(&self) -> ScrollbarMode {
        if self.options.show_scrollbar {
            ScrollbarMode::Always
        } else {
            self.options.scrollbar
        }
    }

    /// Compute the row the thumb should sit on, plus whether to draw it.
    ///
    /// Upstream's `LayoutNode.getScrollThumb`
    /// (`packages/tui/src/layout-node.ts`): a single `█` block whose row is
    /// `floor((scrollTop / max(1, contentHeight - viewportHeight)) * (viewportHeight - 1))`.
    fn thumb_position(&self, content_height: usize) -> Option<usize> {
        let viewport = self.last_viewport_height as usize;
        if viewport == 0 || content_height <= viewport {
            return None;
        }
        let max_scroll = content_height.saturating_sub(viewport);
        let scroll = (self.scroll_row.max(0) as usize).min(max_scroll);
        let track_rows = viewport.saturating_sub(1);
        if track_rows == 0 {
            return None;
        }
        // Use u128 to avoid an overflow when content_height is huge.
        let pos = (scroll as u128 * track_rows as u128) / max_scroll.max(1) as u128;
        Some(pos.min(track_rows as u128) as usize)
    }
}

impl Component for ScrollView {
    fn render(&self, width: u16) -> Vec<StyledLine> {
        let all = if self.dirty {
            self.child.render(width)
        } else {
            self.cached_lines.clone()
        };
        let start = self.scroll_row as usize;
        let mut out: Vec<StyledLine> = all.iter().skip(start).cloned().collect();
        let mode = self.effective_mode();
        let visible = match mode {
            ScrollbarMode::Hidden => false,
            ScrollbarMode::Always => !all.is_empty() && self.last_viewport_height > 0,
            ScrollbarMode::Auto => self.is_scrollbar_visible(all.len()),
        };
        if visible {
            // Draw the thumb at its computed row, falling back to the
            // legacy single-bar-at-end when the viewport is unknown.
            let thumb = self.thumb_position(all.len());
            if let Some(row) = thumb {
                if row < out.len() {
                    // Track row carries the `│` so the column stays present
                    // even when the thumb moves past it.
                    if let Some(track_row) = out.get_mut(row) {
                        track_row.push(StyledSpan::new(
                            "│".to_string(),
                            self.options.scrollbar_track_style,
                        ));
                        track_row.push(StyledSpan::new(
                            "█".to_string(),
                            self.options.scrollbar_thumb_style,
                        ));
                    }
                }
            } else if let Some(last) = out.last_mut() {
                last.push(StyledSpan::new(
                    "┃".to_string(),
                    self.options.scrollbar_style,
                ));
            }
        }
        out
    }
}

impl ScrollLayoutState for ScrollView {
    fn scroll_top(&self) -> usize {
        self.scroll_row.max(0) as usize
    }

    fn primary(&self) -> bool {
        false
    }

    fn overscroll(&self) -> ScrollOverscroll {
        ScrollOverscroll::Chain
    }

    fn viewport_height(&self) -> usize {
        self.last_viewport_height as usize
    }

    fn get_content_width(&self, width: u16) -> u16 {
        width
    }

    fn update_layout(&mut self, _content_height: usize, viewport_height: usize, _request_render: Box<dyn Fn() + Send + Sync>) {
        // Stash the viewport height so the next render has it for the
        // auto-mode fade math and the thumb row calculation.
        self.last_viewport_height = viewport_height.min(u16::MAX as usize) as u16;
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Wall-clock millisecond timestamp — a private indirection so tests can
/// freeze time through a single hook without each test reaching for
/// `Instant::now` directly.
fn current_time_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::text::Text;

    #[test]
    fn scroll_view_wraps_child() {
        let sv = ScrollView::new(Box::new(Text::from_lines(["a", "b", "c"])));
        let lines = sv.render(40);
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn scroll_view_scroll_to_offsets() {
        let mut sv = ScrollView::new(Box::new(Text::from_lines(["a", "b", "c"])));
        sv.scroll_to(ScrollViewScrollToOptions { row: 1 });
        let lines = sv.render(40);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn scroll_view_scroll_down_up() {
        let mut sv = ScrollView::new(Box::new(Text::from_lines(["a", "b", "c"])));
        sv.scroll_down(2);
        assert_eq!(sv.scroll_row(), 2);
        sv.scroll_up(1);
        assert_eq!(sv.scroll_row(), 1);
    }

    #[test]
    fn scrollbar_hidden_by_default() {
        let sv = ScrollView::new(Box::new(Text::from_lines(["a", "b", "c"])));
        let lines = sv.render(40);
        // No scrollbar glyph appended on the last line.
        let last = lines.last().unwrap();
        assert!(
            !last.iter().any(|s| s.text == "┃" || s.text == "│"),
            "{last:?}"
        );
    }

    #[test]
    fn scrollbar_always_mode_appends_thumb_when_overflow() {
        let mut sv = ScrollView::new(Box::new(Text::from_lines(
            (0..20).map(|i| format!("line {i}")).collect::<Vec<_>>(),
        )));
        sv.set_scrollbar(ScrollbarMode::Always);
        sv.set_viewport_height(5);
        sv.scroll_to(ScrollViewScrollToOptions { row: 5 });
        let lines = sv.render(20);
        // Some line carries the thumb.
        let has_thumb = lines
            .iter()
            .any(|l| l.iter().any(|s| s.text == "█"));
        assert!(has_thumb, "{lines:?}");
    }

    #[test]
    fn scrollbar_auto_mode_hides_when_content_fits() {
        let mut sv = ScrollView::new(Box::new(Text::from_lines(["a", "b"])));
        sv.set_scrollbar(ScrollbarMode::Auto);
        sv.set_viewport_height(10);
        let lines = sv.render(20);
        let has_thumb = lines
            .iter()
            .any(|l| l.iter().any(|s| s.text == "█"));
        assert!(!has_thumb, "{lines:?}");
    }

    #[test]
    fn scrollbar_auto_mode_shows_during_active_scroll() {
        let mut sv = ScrollView::new(Box::new(Text::from_lines(
            (0..30).map(|i| format!("line {i}")).collect::<Vec<_>>(),
        )));
        sv.set_scrollbar(ScrollbarMode::Auto);
        sv.set_viewport_height(5);
        sv.scroll_to(ScrollViewScrollToOptions { row: 1 });
        let lines = sv.render(20);
        let has_thumb = lines
            .iter()
            .any(|l| l.iter().any(|s| s.text == "█"));
        assert!(has_thumb, "{lines:?}");
    }

    #[test]
    fn scrollbar_thumb_position_tracks_scroll_offset() {
        let mut sv = ScrollView::new(Box::new(Text::from_lines(
            (0..20).map(|i| format!("line {i}")).collect::<Vec<_>>(),
        )));
        sv.set_scrollbar(ScrollbarMode::Always);
        sv.set_viewport_height(5);
        // content_height = 20, viewport = 5, max_scroll = 15
        // thumb at scroll=0 should land on row 0
        sv.scroll_to(ScrollViewScrollToOptions { row: 0 });
        assert_eq!(sv.thumb_position(20), Some(0));
        // scroll=15 (max) should land on the last row
        sv.scroll_to(ScrollViewScrollToOptions { row: 15 });
        assert_eq!(sv.thumb_position(20), Some(4));
    }
}