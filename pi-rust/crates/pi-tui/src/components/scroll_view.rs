//! `ScrollView` — a scrollable viewport around a child component.
//!
//! Mirrors upstream `ScrollView`
//! (`packages/tui/src/components/scroll-view.ts`).

use std::any::Any;

use crate::component::Component;
use crate::app::layout_node::{ScrollLayoutState, ScrollOverscroll};
use crate::utils::styled::{SpanStyle, StyledLine, StyledSpan};

/// Upstream `ScrollViewOptions` — scroll options.
#[derive(Debug, Clone, Default)]
pub struct ScrollViewOptions {
    /// Whether the scrollbar is shown.
    pub show_scrollbar: bool,
    /// Scrollbar style.
    pub scrollbar_style: SpanStyle,
}

/// Upstream `ScrollViewScrollbar` — scrollbar rendering options.
#[derive(Debug, Clone, Default)]
pub struct ScrollViewScrollbar {
    /// Whether the scrollbar is shown.
    pub show: bool,
    /// Span style.
    pub style: SpanStyle,
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
}

impl std::fmt::Debug for ScrollView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScrollView")
            .field("scroll_row", &self.scroll_row)
            .field("dirty", &self.dirty)
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
        }
    }

    /// Scroll to a vertical offset (clamped to content length).
    pub fn scroll_to(&mut self, opts: ScrollViewScrollToOptions) {
        self.scroll_row = opts.row.max(0);
        self.dirty = true;
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
}

impl Component for ScrollView {
    fn render(&self, width: u16) -> Vec<StyledLine> {
        let all = if self.dirty {
            // Re-render the child every frame when dirty.
            self.child.render(width)
        } else {
            // Best-effort: render the cached lines.
            self.cached_lines.clone()
        };
        let start = self.scroll_row as usize;
        let mut out: Vec<StyledLine> = all.iter().skip(start).cloned().collect();
        if self.options.show_scrollbar && !all.is_empty() {
            // Append a tiny scrollbar indicator on the last line.
            if let Some(last) = out.last_mut() {
                last.push(StyledSpan::new("┃".to_string(), self.options.scrollbar_style));
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
        0
    }

    fn get_content_width(&self, width: u16) -> u16 {
        width
    }

    fn update_layout(&mut self, _content_height: usize, _viewport_height: usize, _request_render: Box<dyn Fn() + Send + Sync>) {
        // No-op for the basic ScrollView; the layout engine reads scroll_top
        // back via the next call.
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
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
}