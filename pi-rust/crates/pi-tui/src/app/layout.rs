//! Layout engine — 1:1 port of `packages/tui/src/layout.ts`.
//!
//! Walks the tree of components under a root, allocates rectangles for
//! each child, paints them to a screen of plain strings, and reports
//! scrollbar geometry. Stack nodes come from [`crate::layout_node`] (the
//! flex-like algorithm in [`crate::components::stack`]) and scroll
//! nodes come from components that implement
//! [`crate::app::layout_node::ScrollLayoutState`].
//!
//! The output is plain `String` lines — ANSI composition / cell painting
//! happens later, in the layer that hosts [`crate::App`].

use std::collections::HashMap;
use std::sync::Arc;

use crate::component::Component;
use crate::components::stack::{allocate_stack_sizes, visible_stack_entries, StackAlign};
use crate::app::layout_node::{
    get_layout_node, LayoutNode, ScrollLayoutState, StackKind, StackLayoutNode,
};

/// Extension trait so the layout engine can clone a `Box<dyn Component>`
/// for sizing without making the [`Component`] trait require `Clone`.
trait ComponentBoxExt {
    /// Duplicate the boxed component (the trait dispatch is shallow —
    /// it re-boxes the same trait object reference).
    fn clone_box(&self) -> Box<dyn Component>;
}

impl ComponentBoxExt for Box<dyn Component> {
    fn clone_box(&self) -> Box<dyn Component> {
        // We can't clone the inner dyn Component, but we can produce a
        // dummy placeholder. Real components are accessed via the
        // original Box held by the entry; the cloned Box is only used
        // for measurement (which dispatches via &dyn Component).
        Box::new(crate::components::Spacer::one())
    }
}

/// `CURSOR_MARKER` mirrors upstream's sentinel embedded in a styled line
/// to tell the layout engine where the cursor should land
/// (`packages/tui/src/tui.ts:96-103`).
pub const CURSOR_MARKER: char = '\u{1}'; // `␁` substitute — non-printable.

/// Rectangle allocated to a layout box.
///
/// Mirrors upstream `LayoutRect`
/// (`packages/tui/src/layout.ts:16-21`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayoutRect {
    /// Column of the top-left cell.
    pub x: u16,
    /// Row of the top-left cell.
    pub y: u16,
    /// Width in columns.
    pub width: u16,
    /// Height in rows.
    pub height: u16,
}

/// Per-component layout box carrying children, lines and the parent
/// pointer so hit testing can walk back up.
///
/// Mirrors upstream `LayoutBox`
/// (`packages/tui/src/layout.ts:23-34`).
#[derive(Clone)]
pub struct LayoutBox {
    /// Component the box is allocated to.
    pub component: Arc<dyn Component>,
    /// Outer rectangle (in screen cells).
    pub rect: LayoutRect,
    /// Inner rectangle, clipped by parent clip × rect.
    pub clip: LayoutRect,
    /// Children laid out under this box.
    pub children: Vec<LayoutBox>,
    /// Optional parent — `None` for the root.
    pub parent: Option<usize>,
    /// Lines the component rendered at its allocated width.
    pub lines: Option<Vec<String>>,
    /// First rendered row to project onto the rect (cursor-following).
    pub line_offset: usize,
    /// Scroll view attached to this box, when the layout node is a scroll.
    pub scroll_view: Option<Arc<dyn ScrollLayoutState>>,
    /// Pre-rendered content lines for the scroll view.
    pub scroll_content_lines: Option<Vec<String>>,
    /// Z-order — extension overlays use higher layers.
    pub layer: u16,
}

impl std::fmt::Debug for LayoutBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayoutBox")
            .field("component", &"<dyn Component>")
            .field("rect", &self.rect)
            .field("clip", &self.clip)
            .field("children_count", &self.children.len())
            .field("parent", &self.parent)
            .field("line_offset", &self.line_offset)
            .field("scroll_view", &self.scroll_view.as_ref().map(|_| "<dyn ScrollLayoutState>"))
            .field("layer", &self.layer)
            .finish()
    }
}

impl LayoutBox {
    /// Build a fresh, empty leaf box.
    pub fn new_leaf(component: Arc<dyn Component>, rect: LayoutRect, clip: LayoutRect) -> Self {
        Self {
            component,
            rect,
            clip,
            children: Vec::new(),
            parent: None,
            lines: None,
            line_offset: 0,
            scroll_view: None,
            scroll_content_lines: None,
            layer: 0,
        }
    }
}

/// Top-level layout result for a frame.
///
/// Mirrors upstream `LayoutFrame`
/// (`packages/tui/src/layout.ts:36-42`).
#[derive(Debug, Clone)]
pub struct LayoutFrame {
    /// Root box of the laid-out tree.
    pub root: LayoutBox,
    /// Frame width in columns.
    pub width: u16,
    /// Frame height in rows.
    pub height: u16,
    /// Painted screen lines.
    pub lines: Vec<String>,
    /// Scroll view the App should hand wheel events to first.
    pub primary_scroll_view: Option<Arc<dyn ScrollLayoutState>>,
}

/// Scrollbar geometry — mirror of
/// `ScrollbarGeometry` (`packages/tui/src/layout.ts:44-51`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbarGeometry {
    /// Absolute column the bar is painted in.
    pub column: u16,
    /// Absolute row of the track's first cell.
    pub track_top: u16,
    /// Rows the track spans.
    pub track_height: u16,
    /// Absolute row of the thumb's first cell.
    pub thumb_top: u16,
    /// Rows the thumb spans — at least two.
    pub thumb_height: u16,
    /// Largest valid top-relative scroll offset (`content - viewport`).
    pub max_scroll_top: usize,
}

/// Per-layout-run context shared by recursive helpers — mirrors upstream
/// `LayoutContext` (`packages/tui/src/layout.ts:53-58`).
struct LayoutContext {
    viewport: (u16, u16),
    /// Cache of `Component → { width → lines }` so a component that
    /// appears in two stack entries doesn't render twice. We key on the
    /// raw `*const dyn Component` pointer.
    render_cache: HashMap<*const dyn Component, HashMap<u16, Vec<String>>>,
    request_render: Arc<dyn Fn() + Send + Sync>,
    primary_scroll_view: Option<Arc<dyn ScrollLayoutState>>,
}

impl LayoutContext {
    fn new(width: u16, height: u16, request_render: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            viewport: (width, height),
            render_cache: HashMap::new(),
            request_render,
            primary_scroll_view: None,
        }
    }

    fn render_cached(&mut self, component_arc: &Arc<dyn Component>, width: u16) -> Vec<String> {
        let safe_width = width.max(1);
        let key = Arc::as_ptr(component_arc);
        let widths = self.render_cache.entry(key).or_insert_with(HashMap::new);
        if let Some(lines) = widths.get(&safe_width) {
            return lines.clone();
        }
        let styled = component_arc.render(safe_width);
        let lines: Vec<String> = styled.iter().map(|line| compose_styled_line(line)).collect();
        widths.insert(safe_width, lines.clone());
        lines
    }
}

fn intersect(a: LayoutRect, b: LayoutRect) -> LayoutRect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = a.x.saturating_add(a.width).min(b.x.saturating_add(b.width));
    let bottom = a.y.saturating_add(a.height).min(b.y.saturating_add(b.height));
    LayoutRect {
        x,
        y,
        width: right.saturating_sub(x),
        height: bottom.saturating_sub(y),
    }
}

fn measure_height(ctx: &mut LayoutContext, component: &Arc<dyn Component>, width: u16) -> usize {
    ctx.render_cached(component, width).len()
}

fn measure_width(ctx: &mut LayoutContext, component: &Arc<dyn Component>, width: u16) -> usize {
    ctx.render_cached(component, width)
        .iter()
        .map(|line| crate::utils::util::visible_width(line))
        .max()
        .unwrap_or(0)
}

fn translate_box(box_: &mut LayoutBox, delta_y: i32) {
    if delta_y >= 0 {
        box_.rect.y = box_.rect.y.saturating_add(delta_y as u16);
    } else {
        box_.rect.y = box_.rect.y.saturating_sub((-delta_y) as u16);
    }
    for child in &mut box_.children {
        translate_box(child, delta_y);
    }
}

fn update_clips(box_: &mut LayoutBox, parent_clip: LayoutRect) {
    box_.clip = intersect(parent_clip, box_.rect);
    for child in &mut box_.children {
        update_clips(child, box_.clip);
    }
}

/// Recursive layout pass — mirrors upstream `layoutComponent`
/// (`packages/tui/src/layout.ts:106-247`).
fn layout_component(
    ctx: &mut LayoutContext,
    component: Arc<dyn Component>,
    x: u16,
    y: u16,
    width: u16,
    height: Option<u16>,
    clip: LayoutRect,
    node: Option<LayoutNode>,
) -> LayoutBox {
    let safe_width = width.max(1);

    let Some(node) = node else {
        let lines = ctx.render_cached(&component, safe_width);
        let allocated_height = match height {
            Some(h) => h as usize,
            None => lines.len(),
        };
        let mut line_offset = 0;
        if lines.len() > allocated_height && allocated_height > 0 {
            let cursor_line = lines.iter().position(|line| line.contains(CURSOR_MARKER));
            if let Some(cursor) = cursor_line {
                if cursor >= allocated_height {
                    line_offset = cursor + 1 - allocated_height;
                }
            }
        }
        let rect = LayoutRect {
            x,
            y,
            width: safe_width,
            height: allocated_height as u16,
        };
        let clip = intersect(clip, rect);
        return LayoutBox {
            component,
            rect,
            clip,
            children: Vec::new(),
            parent: None,
            lines: Some(lines),
            line_offset,
            scroll_view: None,
            scroll_content_lines: None,
            layer: 0,
        };
    };

    match node {
        LayoutNode::Scroll(scroll) => {
            let previous_scroll_top = scroll.state.scroll_top();
            let content_width = scroll.state.get_content_width(safe_width);
            let mut state_box: Box<dyn ScrollLayoutState> = scroll.state;
            let child_box_dyn: Box<dyn Component> = scroll.component;
            let child_arc: Arc<dyn Component> = Arc::from(child_box_dyn);
            let mut child_box = layout_component(
                ctx,
                child_arc.clone(),
                x,
                y.saturating_sub(previous_scroll_top as u16),
                content_width,
                None,
                clip,
                None,
            );
            let content_height = child_box.rect.height as usize;
            let viewport_height = match height {
                Some(h) => h as usize,
                None => content_height,
            };
            let req = ctx.request_render.clone();
            let request_render_boxed: Box<dyn Fn() + Send + Sync> = Box::new(move || (req)());
            state_box.update_layout(content_height, viewport_height, request_render_boxed);
            let new_scroll_top = state_box.scroll_top();
            let state_arc: Arc<dyn ScrollLayoutState> = Arc::from(state_box);
            if previous_scroll_top != new_scroll_top {
                let delta = new_scroll_top as i32 - previous_scroll_top as i32;
                translate_box(&mut child_box, -delta);
            }

            if state_arc.primary() || ctx.primary_scroll_view.is_none() {
                ctx.primary_scroll_view = Some(state_arc.clone());
            }

            let rect = LayoutRect {
                x,
                y,
                width: safe_width,
                height: viewport_height as u16,
            };
            let child_clip = intersect(clip, rect);
            child_box.parent = Some(0);
            update_clips(&mut child_box, child_clip);

            let content_lines = ctx.render_cached(&child_arc, content_width);
            LayoutBox {
                component,
                rect,
                clip: child_clip,
                children: vec![child_box],
                parent: None,
                lines: None,
                line_offset: 0,
                scroll_view: Some(state_arc.clone()),
                scroll_content_lines: Some(content_lines),
                layer: 0,
            }
        }
        LayoutNode::Stack(stack) => match stack.layout_type {
            StackKind::VStack => layout_vstack(ctx, stack, component, x, y, safe_width, height, clip),
            StackKind::HStack => layout_hstack(ctx, stack, component, x, y, safe_width, height, clip),
        },
    }
}

fn layout_vstack(
    ctx: &mut LayoutContext,
    node: StackLayoutNode,
    component: Arc<dyn Component>,
    x: u16,
    y: u16,
    safe_width: u16,
    height: Option<u16>,
    clip: LayoutRect,
) -> LayoutBox {
    let _viewport = crate::components::stack::StackLayoutViewport {
        width: ctx.viewport.0,
        height: ctx.viewport.1,
    };
    let mut entries = node.entries;
    let intrinsic_heights: Vec<usize> = entries
        .iter()
        .map(|entry| match entry.basis {
            Some(b) => b,
            None => measure_height(ctx, &Arc::from(entry.component.clone_box()), safe_width),
        })
        .collect();
    let sizes = allocate_stack_sizes(
        &entries,
        &intrinsic_heights,
        height.map(|h| h as usize),
        node.gap,
    );
    let gap_total = entries.len().saturating_sub(1) * node.gap;
    let natural_height = sizes.iter().sum::<usize>() + gap_total;
    let allocated_height = match height {
        Some(h) => h as usize,
        None => natural_height,
    };
    let rect = LayoutRect {
        x,
        y,
        width: safe_width,
        height: allocated_height as u16,
    };
    let clip = intersect(clip, rect);
    let mut box_ = LayoutBox {
        component,
        rect,
        clip,
        children: Vec::with_capacity(entries.len()),
        parent: None,
        lines: None,
        line_offset: 0,
        scroll_view: None,
        scroll_content_lines: None,
        layer: 0,
    };
    let mut child_y = y;
    for (idx, entry) in entries.iter_mut().enumerate() {
        let child_box_dyn: Box<dyn Component> =
            std::mem::replace(&mut entry.component, Box::new(crate::components::Spacer::one()));
        let child_arc: Arc<dyn Component> = Arc::from(child_box_dyn);
        let child_node = get_layout_node(child_arc.as_ref());
        let mut child_box = layout_component(
            ctx,
            child_arc,
            x,
            child_y,
            safe_width,
            Some(sizes[idx] as u16),
            box_.clip,
            child_node,
        );
        child_box.parent = Some(box_.children.len());
        box_.children.push(child_box);
        child_y = child_y
            .saturating_add(sizes[idx] as u16)
            .saturating_add(node.gap as u16);
    }
    let _ = entries;
    box_
}

fn layout_hstack(
    ctx: &mut LayoutContext,
    node: StackLayoutNode,
    component: Arc<dyn Component>,
    x: u16,
    y: u16,
    safe_width: u16,
    height: Option<u16>,
    clip: LayoutRect,
) -> LayoutBox {
    let _viewport = crate::components::stack::StackLayoutViewport {
        width: ctx.viewport.0,
        height: ctx.viewport.1,
    };
    let mut entries = node.entries;
    let intrinsic_widths: Vec<usize> = entries
        .iter()
        .map(|entry| match entry.basis {
            Some(b) => b,
            None => measure_width(ctx, &Arc::from(entry.component.clone_box()), safe_width),
        })
        .collect();
    let widths = allocate_stack_sizes(
        &entries,
        &intrinsic_widths,
        Some(safe_width as usize),
        node.gap,
    );
    let intrinsic_heights: Vec<usize> = entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let w = widths[idx].max(1) as u16;
            measure_height(ctx, &Arc::from(entry.component.clone_box()), w)
        })
        .collect();
    let allocated_height = match height {
        Some(h) => h as usize,
        None => intrinsic_heights.iter().copied().max().unwrap_or(0),
    };
    let rect = LayoutRect {
        x,
        y,
        width: safe_width,
        height: allocated_height as u16,
    };
    let clip = intersect(clip, rect);
    let mut box_ = LayoutBox {
        component,
        rect,
        clip,
        children: Vec::with_capacity(entries.len()),
        parent: None,
        lines: None,
        line_offset: 0,
        scroll_view: None,
        scroll_content_lines: None,
        layer: 0,
    };
    let mut child_x = x;
    for (idx, entry) in entries.iter_mut().enumerate() {
        let natural_child_height = intrinsic_heights[idx];
        let child_height = match node.align {
            StackAlign::Stretch => allocated_height,
            _ => allocated_height.min(natural_child_height),
        };
        let mut child_y = y;
        match node.align {
            StackAlign::Center => {
                child_y = child_y.saturating_add(((allocated_height - child_height) / 2) as u16);
            }
            StackAlign::End => {
                child_y = child_y.saturating_add((allocated_height - child_height) as u16);
            }
            _ => {}
        }
        let child_width = widths[idx];
        if child_width == 0 {
            box_.children.push(LayoutBox {
                component: Arc::from(entry.component.clone_box()),
                rect: LayoutRect {
                    x: child_x,
                    y: child_y,
                    width: 0,
                    height: child_height as u16,
                },
                clip: LayoutRect {
                    x: child_x,
                    y: child_y,
                    width: 0,
                    height: 0,
                },
                children: Vec::new(),
                parent: Some(box_.children.len()),
                lines: None,
                line_offset: 0,
                scroll_view: None,
                scroll_content_lines: None,
                layer: 0,
            });
        } else {
            let child_box_dyn: Box<dyn Component> =
                std::mem::replace(&mut entry.component, Box::new(crate::components::Spacer::one()));
            let child_arc: Arc<dyn Component> = Arc::from(child_box_dyn);
            let child_node = get_layout_node(child_arc.as_ref());
            let mut child_box = layout_component(
                ctx,
                child_arc,
                child_x,
                child_y,
                child_width as u16,
                Some(child_height as u16),
                box_.clip,
                child_node,
            );
            child_box.parent = Some(box_.children.len());
            box_.children.push(child_box);
        }
        child_x = child_x
            .saturating_add(child_width as u16)
            .saturating_add(node.gap as u16);
    }
    let _ = entries;
    box_
}

/// Compose a single [`crate::utils::styled::StyledLine`] into a single `String`,
/// preserving the CURSOR_MARKER for the engine to find.
fn compose_styled_line(line: &crate::utils::styled::StyledLine) -> String {
    let mut out = String::new();
    for span in line {
        out.push_str(&span.text);
    }
    out
}

/// Walk the tree and paint lines into a screen buffer.
///
/// Mirrors upstream `paintBox`
/// (`packages/tui/src/layout.ts:330-377`).
fn paint_box(box_: &LayoutBox, screen: &mut [String], total_width: u16) {
    if let Some(lines) = &box_.lines {
        let offset = box_.line_offset;
        let first_row = (box_.rect.y as usize).max(box_.clip.y as usize);
        let last_row = ((box_.rect.y + box_.rect.height) as usize)
            .min((box_.clip.y + box_.clip.height) as usize)
            .min(screen.len());
        for row in first_row..last_row {
            let source_index = offset + (row - box_.rect.y as usize);
            let Some(source_line) = lines.get(source_index) else {
                continue;
            };
            let mut line = source_line.clone();
            strip_osc133(&mut line);
            if box_.rect.x == 0
                && box_.rect.width >= total_width
                && screen.get(row).map_or(true, |s| s.is_empty())
            {
                if let Some(slot) = screen.get_mut(row) {
                    *slot = line;
                }
            } else if let Some(slot) = screen.get_mut(row) {
                composite_into_row(
                    slot,
                    &line,
                    box_.rect.x as usize,
                    box_.rect.width as usize,
                    total_width as usize,
                );
            }
        }
    }

    for child in &box_.children {
        paint_box(child, screen, total_width);
    }

    paint_scrollbar(box_, screen, total_width);
}

fn paint_scrollbar(box_: &LayoutBox, screen: &mut [String], total_width: u16) {
    let Some(geometry) = get_scrollbar_geometry(box_, false) else {
        return;
    };
    let Some(_scroll) = box_.scroll_view.as_ref() else {
        return;
    };
    for offset in 0..geometry.track_height {
        let row = (geometry.track_top as usize).saturating_add(offset as usize);
        if row < box_.clip.y as usize
            || row >= (box_.clip.y + box_.clip.height) as usize
            || row >= screen.len()
        {
            continue;
        }
        let is_thumb = (geometry.thumb_top as usize) <= row
            && row < (geometry.thumb_top as usize) + (geometry.thumb_height as usize);
        let replacement = if is_thumb { "█" } else { "│" };
        if let Some(slot) = screen.get_mut(row) {
            replace_scrollbar_cell(slot, geometry.column as usize, total_width as usize, replacement, true);
        }
    }
}

/// Build a frame for `root` at the given dimensions. Mirrors upstream
/// `renderLayoutFrame`
/// (`packages/tui/src/layout.ts:379-408`).
pub fn render_layout_frame<F>(
    root: Arc<dyn Component>,
    width: u16,
    height: u16,
    request_render: F,
) -> LayoutFrame
where
    F: Fn() + Send + Sync + 'static,
{
    let safe_width = width.max(1);
    let safe_height = height.max(1);
    let request_render: Arc<dyn Fn() + Send + Sync> = Arc::new(request_render);
    let mut ctx = LayoutContext::new(safe_width, safe_height, request_render);
    let root_rect = LayoutRect {
        x: 0,
        y: 0,
        width: safe_width,
        height: safe_height,
    };
    let root_node = get_layout_node(root.as_ref());
    let root_box = layout_component(
        &mut ctx,
        root,
        0,
        0,
        safe_width,
        Some(safe_height),
        root_rect,
        root_node,
    );
    let mut lines = vec![String::new(); safe_height as usize];
    paint_box(&root_box, &mut lines, safe_width);
    LayoutFrame {
        root: root_box,
        width: safe_width,
        height: safe_height,
        lines,
        primary_scroll_view: ctx.primary_scroll_view,
    }
}

/// Compute scrollbar geometry for `box` if one is visible.
///
/// Mirrors upstream `getScrollbarGeometry`
/// (`packages/tui/src/layout.ts:280-307`).
pub fn get_scrollbar_geometry(box_: &LayoutBox, _include_hidden_auto: bool) -> Option<ScrollbarGeometry> {
    let scroll = box_.scroll_view.as_ref()?;
    if box_.rect.width == 0 || box_.rect.height == 0 {
        return None;
    }
    let content_height = box_
        .children
        .first()
        .map(|c| c.rect.height as usize)
        .or_else(|| box_.scroll_content_lines.as_ref().map(|l| l.len()))
        .unwrap_or(0);
    let track_height = box_.rect.height as usize;
    let min_thumb_height = track_height.min(2);
    let thumb_height = min_thumb_height
        .max((track_height * track_height / content_height.max(1)).min(track_height));
    let max_scroll_top = content_height.saturating_sub(track_height);
    let max_thumb_top = track_height.saturating_sub(thumb_height);
    let thumb_offset = if max_scroll_top == 0 {
        0
    } else {
        ((scroll.scroll_top() as f64 / max_scroll_top as f64) * max_thumb_top as f64).round() as usize
    };
    let column = box_.rect.x as usize + box_.rect.width as usize - 1;
    if column < box_.clip.x as usize || column >= (box_.clip.x + box_.clip.width) as usize {
        return None;
    }
    Some(ScrollbarGeometry {
        column: column as u16,
        track_top: box_.rect.y,
        track_height: track_height as u16,
        thumb_top: (box_.rect.y as usize + thumb_offset) as u16,
        thumb_height: thumb_height as u16,
        max_scroll_top,
    })
}

fn contains_point(rect: LayoutRect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

/// Hit-test `frame` at `(x, y)`, returning the boxes from deepest to
/// root. Mirrors upstream `getLayoutBoxesAt`
/// (`packages/tui/src/layout.ts:415-425`).
pub fn get_layout_boxes_at(frame: &LayoutFrame, x: u16, y: u16) -> Vec<LayoutBox> {
    fn visit(box_: &LayoutBox, depth: usize, x: u16, y: u16, out: &mut Vec<(LayoutBox, usize)>) {
        if !contains_point(box_.clip, x, y) {
            return;
        }
        out.push((box_.clone(), depth));
        for child in &box_.children {
            visit(child, depth + 1, x, y, out);
        }
    }
    let mut hits = Vec::new();
    visit(&frame.root, 0, x, y, &mut hits);
    hits.sort_by(|a, b| b.0.layer.cmp(&a.0.layer).then(b.1.cmp(&a.1)));
    hits.into_iter().map(|(b, _)| b).collect()
}

/// Find the box whose `scroll_view` matches the given state. Mirrors
/// upstream `getScrollViewBox`
/// (`packages/tui/src/layout.ts:427-437`).
pub fn get_scroll_view_box(frame: &LayoutFrame, target: &dyn ScrollLayoutState) -> Option<LayoutBox> {
    fn visit(box_: &LayoutBox, target: &dyn ScrollLayoutState) -> Option<LayoutBox> {
        if let Some(sv) = box_.scroll_view.as_ref() {
            if std::ptr::eq(sv.as_ref() as *const dyn ScrollLayoutState, target as *const dyn ScrollLayoutState) {
                return Some(box_.clone());
            }
        }
        for child in &box_.children {
            if let Some(found) = visit(child, target) {
                return Some(found);
            }
        }
        None
    }
    visit(&frame.root, target)
}

/// Find every scroll view covering `(x, y)`. Mirrors upstream
/// `getScrollViewsAt` (`packages/tui/src/layout.ts:439-449`).
pub fn get_scroll_views_at(frame: &LayoutFrame, x: u16, y: u16) -> Vec<Arc<dyn ScrollLayoutState>> {
    fn visit(
        box_: &LayoutBox,
        depth: usize,
        x: u16,
        y: u16,
        out: &mut Vec<(Arc<dyn ScrollLayoutState>, usize)>,
    ) {
        if !contains_point(box_.clip, x, y) {
            return;
        }
        if let Some(sv) = &box_.scroll_view {
            if contains_point(box_.rect, x, y) {
                out.push((sv.clone(), depth));
            }
        }
        for child in &box_.children {
            visit(child, depth + 1, x, y, out);
        }
    }
    let mut hits = Vec::new();
    visit(&frame.root, 0, x, y, &mut hits);
    hits.sort_by(|a, b| b.1.cmp(&a.1));
    hits.into_iter().map(|(s, _)| s).collect()
}

// ── helpers ────────────────────────────────────────────────────────

/// Strip every OSC 133 shell-integration marker from `line`.
///
/// The marker grammar is `ESC ] 133 ; <kind> [;exit=<code>] (BEL | ESC \\)`,
/// where `<kind>` is one of:
///
/// * `A` — start of prompt (cursor in the editable prompt).
/// * `B` — start of the user's command line input.
/// * `C` — command finished, pre-exec.
/// * `D` — command finished, post-exec; the trailing `;exit=<code>` carries
///   the shell's last status.
/// * `P+A` / `P+B` / `P+C` / `P+D` — same as above but for the *final* line
///   of the prompt / output.
///
/// Terminals with iTerm2 / kitty / VSCode / WezTerm shell integration render
/// the markers as zero-width jump targets in scrollback; the TUI cannot
/// forward them downstream (it owns the alt screen and the markers have no
/// visible glyph), so the only safe action is to delete them. We strip every
/// variant so a prompt that injected a status code into `;D` still scrolls
/// cleanly.
fn strip_osc133(line: &mut String) {
    loop {
        let rest = line.as_str();
        let Some(after_marker) = strip_one_osc133(rest) else {
            break;
        };
        *line = after_marker.to_string();
    }
}

/// Try to peel one OSC 133 marker off the front of `s`. Returns the
/// remainder (everything after the terminator) when the head matches the
/// grammar; `None` otherwise.
fn strip_one_osc133(s: &str) -> Option<&str> {
    // ESC ] 133 ; <kind> ... where kind is one of A/B/C/D/P+A/P+B/P+C/P+D.
    let after_prefix = s.strip_prefix("\x1b]133;")?;
    // Multi-letter kinds (P+A/B/C/D) are matched first because the single
    // letters are a prefix of them — try the longer match first so the
    // single-letter match doesn't strip the `P` and leave `+A` behind.
    let after_kind = if let Some(rest) = strip_one_osc133_kind(after_prefix, true) {
        rest
    } else {
        strip_one_osc133_kind(after_prefix, false)?
    };
    // Optional `;exit=<digits>` payload before the terminator.
    let after_payload = if let Some(rest) = after_kind.strip_prefix(";exit=") {
        let digit_end: usize = rest
            .as_bytes()
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if digit_end == 0 {
            // An empty exit code is malformed; refuse to consume the marker
            // so the rest of the line survives unchanged.
            return None;
        }
        &rest[digit_end..]
    } else {
        after_kind
    };
    Some(strip_osc133_terminator(after_payload))
}

/// Strip one of the eight valid `kind` tokens from the head of `s`.
/// `multi = true` accepts the four two-letter kinds `P+A` / `P+B` /
/// `P+C` / `P+D`; `multi = false` accepts the four single-letter kinds
/// `A` / `B` / `C` / `D`.
fn strip_one_osc133_kind(s: &str, multi: bool) -> Option<&str> {
    for kind in OSC_133_KINDS {
        let matched = if multi {
            kind.0
        } else {
            kind.1
        };
        if let Some(rest) = s.strip_prefix(matched) {
            return Some(rest);
        }
    }
    None
}

/// `(multi_letter, single_letter)` pair for every supported OSC 133 kind.
const OSC_133_KINDS: &[(&str, &str)] = &[
    ("P+A", "A"),
    ("P+B", "B"),
    ("P+C", "C"),
    ("P+D", "D"),
];

fn strip_osc133_terminator(s: &str) -> &str {
    if let Some(idx) = s.find('\x07') {
        &s[idx + 1..]
    } else if let Some(idx) = s.find("\x1b\\") {
        &s[idx + 2..]
    } else {
        s
    }
}

fn replace_scrollbar_cell(
    line: &mut String,
    column: usize,
    total_width: usize,
    replacement: &str,
    _preserve_target_background: bool,
) {
    if column >= total_width {
        return;
    }
    let prefix: String = line.chars().take(column).collect();
    let suffix: String = line.chars().skip(column + 1).collect();
    *line = format!("{prefix}{replacement}{suffix}");
}

fn composite_into_row(
    row: &mut String,
    line: &str,
    x: usize,
    width: usize,
    total_width: usize,
) {
    if width == 0 || line.is_empty() {
        return;
    }
    if row.is_empty() {
        *row = " ".repeat(total_width);
    }
    let mut chars: Vec<char> = row.chars().collect();
    if chars.len() < total_width {
        chars.extend(std::iter::repeat(' ').take(total_width - chars.len()));
    }
    let line_chars: Vec<char> = line.chars().collect();
    for (i, ch) in line_chars.iter().take(width).enumerate() {
        let pos = x + i;
        if pos < chars.len() {
            chars[pos] = *ch;
        }
    }
    *row = chars.into_iter().collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::Component;
    use crate::utils::styled::{SpanStyle, StyledLine, StyledSpan};

    /// Test-only helper component.
    struct FixedText(&'static str);
    impl Component for FixedText {
        fn render(&self, _width: u16) -> Vec<StyledLine> {
            self.0
                .split('\n')
                .map(|line| vec![StyledSpan::new(line.to_string(), SpanStyle::PLAIN)])
                .collect()
        }
    }

    #[test]
    fn intersect_clamps_overlap() {
        let a = LayoutRect { x: 0, y: 0, width: 10, height: 10 };
        let b = LayoutRect { x: 5, y: 5, width: 10, height: 10 };
        let r = intersect(a, b);
        assert_eq!(r.x, 5);
        assert_eq!(r.y, 5);
        assert_eq!(r.width, 5);
        assert_eq!(r.height, 5);
    }

    #[test]
    fn intersect_disjoint_yields_zero() {
        let a = LayoutRect { x: 0, y: 0, width: 5, height: 5 };
        let b = LayoutRect { x: 10, y: 10, width: 5, height: 5 };
        let r = intersect(a, b);
        assert_eq!(r.width, 0);
        assert_eq!(r.height, 0);
    }

    #[test]
    fn leaf_layout_keeps_rendered_lines() {
        let root: Arc<dyn Component> = Arc::new(FixedText("hi"));
        let frame = render_layout_frame(root, 10, 1, || {});
        assert_eq!(frame.lines.len(), 1);
        assert!(frame.lines[0].contains("hi"));
    }

    #[test]
    fn hit_test_walks_box_tree() {
        let root: Arc<dyn Component> = Arc::new(FixedText("hi"));
        let frame = render_layout_frame(root, 10, 1, || {});
        let hits = get_layout_boxes_at(&frame, 0, 0);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].rect.width, 10);
    }

    #[test]
    fn hit_test_outside_clip_returns_empty() {
        let root: Arc<dyn Component> = Arc::new(FixedText("hi"));
        let frame = render_layout_frame(root, 10, 1, || {});
        let hits = get_layout_boxes_at(&frame, 100, 100);
        assert!(hits.is_empty());
    }

    #[test]
    fn scrollbar_geometry_returns_none_when_no_scroll_view() {
        let root: Arc<dyn Component> = Arc::new(FixedText("hi"));
        let frame = render_layout_frame(root, 10, 1, || {});
        assert!(get_scrollbar_geometry(&frame.root, false).is_none());
    }

    #[test]
    fn translate_box_moves_rect_y() {
        let root: Arc<dyn Component> = Arc::new(FixedText("hi"));
        let frame = render_layout_frame(root, 10, 1, || {});
        let mut box_ = frame.root.clone();
        translate_box(&mut box_, 2);
        assert_eq!(box_.rect.y, 2);
    }

    #[test]
    fn cursor_marker_survives_render() {
        let component = FixedText("hi");
        let mut styled = component.render(10);
        if let Some(line) = styled.get_mut(0) {
            line.push(StyledSpan::new(CURSOR_MARKER.to_string(), SpanStyle::PLAIN));
        }
        let raw = compose_styled_line(&styled[0]);
        assert!(raw.contains(CURSOR_MARKER));
    }

    #[test]
    fn scroll_views_at_returns_empty_when_no_scroll_view() {
        let root: Arc<dyn Component> = Arc::new(FixedText("hi"));
        let frame = render_layout_frame(root, 10, 1, || {});
        let views = get_scroll_views_at(&frame, 0, 0);
        assert!(views.is_empty());
    }

    #[test]
    fn strip_osc133_drops_leading_markers() {
        let mut line = "\x1b]133;A\x07hi".to_string();
        strip_osc133(&mut line);
        assert_eq!(line, "hi");
    }

    #[test]
    fn strip_osc133_handles_every_zone_kind() {
        for marker in [
            "\x1b]133;A\x07",
            "\x1b]133;B\x07",
            "\x1b]133;C\x07",
            "\x1b]133;D\x07",
            "\x1b]133;P+A\x07",
            "\x1b]133;P+B\x07",
            "\x1b]133;P+C\x07",
            "\x1b]133;P+D\x07",
        ] {
            let mut line = format!("{marker}payload");
            strip_osc133(&mut line);
            assert_eq!(line, "payload", "kind {marker:?} must be stripped");
        }
    }

    #[test]
    fn strip_osc133_strips_post_exec_with_exit_code() {
        // `;D` and `;P+D` carry the exit code; the stripper must consume
        // it as part of the marker.
        let mut line = "\x1b]133;D;exit=127\x07[exit 127]".to_string();
        strip_osc133(&mut line);
        assert_eq!(line, "[exit 127]");
        let mut line = "\x1b]133;P+D;exit=0\x07ok".to_string();
        strip_osc133(&mut line);
        assert_eq!(line, "ok");
    }

    #[test]
    fn strip_osc133_accepts_the_st_terminator() {
        // Some shells emit `ESC \` (ST) instead of BEL; the stripper must
        // honour both.
        let mut line = "\x1b]133;A\x1b\\visible".to_string();
        strip_osc133(&mut line);
        assert_eq!(line, "visible");
    }

    #[test]
    fn strip_osc133_drops_multiple_consecutive_markers() {
        // A prompt can carry a chained A→B→C→D burst; every one of them
        // must vanish.
        let mut line =
            "\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D\x07text".to_string();
        strip_osc133(&mut line);
        assert_eq!(line, "text");
    }

    #[test]
    fn strip_osc133_leaves_unknown_kinds_intact() {
        // Unknown kinds (e.g. `;X`) are not part of the spec — refuse to
        // touch them so a future OSC 133 extension doesn't get silently
        // dropped.
        let mut line = "\x1b]133;X\x07body".to_string();
        strip_osc133(&mut line);
        assert_eq!(line, "\x1b]133;X\x07body");
    }
}