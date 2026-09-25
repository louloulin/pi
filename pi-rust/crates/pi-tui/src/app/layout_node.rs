//! Layout node data structures — 1:1 port of
//! `packages/tui/src/layout-node.ts`.
//!
//! Components that want the layout engine to compute their children
//! sizes for them implement `LayoutComponent::layout_node()` (the
//! `Symbol.for("@earendil-works/pi-tui/layout-node")` marker upstream
//! uses). The [`get_layout_node`] helper pulls the node out of a
//! component via the Component trait's default method, mirroring
//! `getLayoutNode` (`packages/tui/src/layout-node.ts:48-51`).
//!
//! The actual layout engine lives in [`crate::layout`]; this module is
//! the data shapes it consumes.

use std::any::Any;
use std::sync::Arc;

use crate::component::Component;
use crate::components::stack::{StackAlign, StackLayoutEntry, StackLayoutViewport};

/// Well-known symbol the layout engine looks up on a component to ask
/// for its [`LayoutNode`].
///
/// Mirrors upstream's `LAYOUT_NODE = Symbol.for("@earendil-works/pi-tui/layout-node")`
/// (`packages/tui/src/layout-node.ts:3`). In Rust this is the
/// [`Component::layout_node`] method — same convention, type-safe.
pub const LAYOUT_NODE: &str = "earendil::pi-tui::layout-node";

/// Viewport size the layout engine is computing against.
///
/// Mirrors upstream `LayoutViewport`
/// (`packages/tui/src/layout-node.ts:5-8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LayoutViewport {
    /// Viewport width in columns.
    pub width: u16,
    /// Viewport height in rows.
    pub height: u16,
}

impl From<(u16, u16)> for LayoutViewport {
    fn from((width, height): (u16, u16)) -> Self {
        Self { width, height }
    }
}

impl From<StackLayoutViewport> for LayoutViewport {
    fn from(v: StackLayoutViewport) -> Self {
        Self { width: v.width, height: v.height }
    }
}

/// A vertical or horizontal stack layout node.
///
/// Mirrors upstream `StackLayoutNode`
/// (`packages/tui/src/layout-node.ts:20-25`).
pub struct StackLayoutNode {
    /// Stack orientation.
    pub layout_type: StackKind,
    /// Per-entry layout hints.
    pub entries: Vec<StackLayoutEntry>,
    /// Blank rows/cols between entries.
    pub gap: usize,
    /// Cross-axis alignment.
    pub align: StackAlign,
}

impl std::fmt::Debug for StackLayoutNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StackLayoutNode")
            .field("layout_type", &self.layout_type)
            .field("entries_count", &self.entries.len())
            .field("gap", &self.gap)
            .field("align", &self.align)
            .finish()
    }
}

/// Stack orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackKind {
    /// Vertical stack — children stack top-to-bottom.
    VStack,
    /// Horizontal stack — children stack left-to-right.
    HStack,
}

impl StackKind {
    /// Resolve from upstream's `"vstack" | "hstack"` string.
    pub fn from_upstream(value: &str) -> Self {
        match value {
            "vstack" => Self::VStack,
            "hstack" => Self::HStack,
            _ => Self::VStack,
        }
    }
}

/// Per-scroll-view layout state the engine drives.
pub trait ScrollLayoutState: std::fmt::Debug + Send + Sync {
    /// Current top scroll offset in rows.
    fn scroll_top(&self) -> usize;
    /// Whether this is the primary scroll view (used for key routing).
    fn primary(&self) -> bool;
    /// `"chain"` lets the scroll view hand off to a parent; `"contain"`
    /// traps the scroll inside.
    fn overscroll(&self) -> ScrollOverscroll;
    /// Current viewport height.
    fn viewport_height(&self) -> usize;
    /// Width the content should be rendered at, derived from the
    /// available width.
    fn get_content_width(&self, width: u16) -> u16;
    /// Called once per layout pass with the latest measurements.
    fn update_layout(&mut self, content_height: usize, viewport_height: usize, request_render: Box<dyn Fn() + Send + Sync>);
    /// Cast to `&dyn Any` so the layout engine can identify the same
    /// state across frames.
    fn as_any(&self) -> &dyn Any;
}

/// ScrollView overscroll behaviour. Mirrors upstream's
/// `"chain" | "contain"` union (`packages/tui/src/layout-node.ts:31`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollOverscroll {
    /// Propagate the scroll to a parent scroll view.
    Chain,
    /// Trap the scroll inside this view.
    Contain,
}

/// Scroll-view layout node — the layout engine keeps a child box at
/// `(x, y - scrollTop)` and translates it after `update_layout`.
pub struct ScrollLayoutNode {
    /// Inner component the scroll view renders.
    pub component: Box<dyn Component>,
    /// Layout state the engine drives.
    pub state: Box<dyn ScrollLayoutState>,
}

impl std::fmt::Debug for ScrollLayoutNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScrollLayoutNode")
            .field("component", &"<dyn Component>")
            .field("state", &"<dyn ScrollLayoutState>")
            .finish()
    }
}

impl ScrollLayoutNode {
    /// Build a new scroll layout node.
    pub fn new(component: Box<dyn Component>, state: Box<dyn ScrollLayoutState>) -> Self {
        Self { component, state }
    }
}

/// One layout node — either a stack or a scroll view.
pub enum LayoutNode {
    /// Vertical / horizontal stack.
    Stack(StackLayoutNode),
    /// Scroll view wrapping an inner component.
    Scroll(ScrollLayoutNode),
}

impl std::fmt::Debug for LayoutNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayoutNode::Stack(s) => f.debug_tuple("Stack").field(s).finish(),
            LayoutNode::Scroll(s) => f.debug_tuple("Scroll").field(s).finish(),
        }
    }
}

impl LayoutNode {
    /// True when this is a stack node.
    pub fn is_stack(&self) -> bool {
        matches!(self, Self::Stack(_))
    }

    /// True when this is a scroll-view node.
    pub fn is_scroll(&self) -> bool {
        matches!(self, Self::Scroll(_))
    }
}

/// Trait components implement to expose their layout node to the
/// engine.
///
/// Mirrors upstream `LayoutComponent`
/// (`packages/tui/src/layout-node.ts:44-46`). The default
/// implementation returns `None`, so a leaf component can ignore the
/// trait.
pub trait LayoutComponent: Component {
    /// Return the [`LayoutNode`] describing how to lay this component
    /// out, or `None` if the component has no children to layout.
    fn layout_node(&self) -> Option<LayoutNode> {
        None
    }
}

/// Look up the [`LayoutNode`] on `component`, returning `None` when the
/// component is not a [`LayoutComponent`].
///
/// Mirrors upstream `getLayoutNode`
/// (`packages/tui/src/layout-node.ts:48-51`). The implementation
/// dispatches through the [`Component::layout_node`] default method,
/// which every `LayoutComponent` impl overrides.
pub fn get_layout_node(component: &dyn Component) -> Option<LayoutNode> {
    component.layout_node()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::Spacer;

    #[test]
    fn layout_viewport_from_tuple() {
        let vp = LayoutViewport::from((40, 20));
        assert_eq!(vp.width, 40);
        assert_eq!(vp.height, 20);
    }

    #[test]
    fn stack_kind_resolves_upstream_strings() {
        assert_eq!(StackKind::from_upstream("vstack"), StackKind::VStack);
        assert_eq!(StackKind::from_upstream("hstack"), StackKind::HStack);
        assert_eq!(StackKind::from_upstream("garbage"), StackKind::VStack);
    }

    #[test]
    fn layout_node_is_stack_or_scroll() {
        let stack = LayoutNode::Stack(StackLayoutNode {
            layout_type: StackKind::VStack,
            entries: vec![],
            gap: 0,
            align: StackAlign::Stretch,
        });
        assert!(stack.is_stack());
        assert!(!stack.is_scroll());
    }

    #[test]
    fn layout_stack_entry_carries_a_component() {
        let entry = StackLayoutEntry {
            component: Box::new(Spacer::one()),
            basis: None,
            grow: 1,
            shrink: 1,
            min_size: 0,
            max_size: usize::MAX,
            visible: None,
        };
        assert_eq!(entry.grow, 1);
    }
}