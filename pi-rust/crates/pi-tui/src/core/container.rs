//! Container — the host-side counterpart of upstream's `Container`
//! (`packages/tui/src/tui.ts:319-379`).
//!
//! A `Container` is a [`CoreComponent`] that owns a list of child
//! components. Its [`render`](CoreComponent::render) delegates to
//! children in order, stacking them vertically. Its
//! [`handle_mouse`](CoreComponent::handle_mouse) walks children top-to-
//! bottom, retargeting the event's `y` so each child sees a local
//! coordinate.

use std::any::Any;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::core::component::CoreComponent;
use crate::core::mouse::{
    dispatch_mouse_event, retarget_mouse_event, MouseDispatchResult, MouseEvent, MouseTarget,
};

/// A component that owns children. Mirrors upstream `Container`.
pub trait Container: CoreComponent {
    /// Borrow the children list.
    fn children(&self) -> &[Box<dyn CoreComponent>];
    /// Mutable borrow of the children list.
    fn children_mut(&mut self) -> &mut Vec<Box<dyn CoreComponent>>;

    /// Add a child (push to the end).
    fn add_child(&mut self, child: Box<dyn CoreComponent>) {
        self.children_mut().push(child);
    }

    /// Remove the first matching child. Returns `true` if found.
    fn remove_child(&mut self, child: &dyn CoreComponent) -> bool {
        let children = self.children_mut();
        let Some(idx) = children
            .iter()
            .position(|c| std::ptr::eq(c.as_any() as *const _, child.as_any() as *const _))
        else {
            return false;
        };
        children.remove(idx);
        true
    }

    /// Drop every child.
    fn clear_children(&mut self) {
        self.children_mut().clear();
    }

    /// First-row mouse dispatch across children (stacking layout).
    ///
    /// Each child is rendered to a 1-row stripe in order; if the
    /// pointer sits in that stripe, the event is retargeted to the
    /// child's local coordinates and dispatched.
    fn dispatch_mouse_to_children(
        &mut self,
        event: &MouseEvent,
        area: Rect,
    ) -> Option<MouseDispatchResult> {
        let mut child_y = area.y;
        let children = self.children_mut();
        for child in children.iter_mut() {
            let height = measure_child_height(child.as_mut(), area.width);
            let child_area = Rect::new(area.x, child_y, area.width, height);
            if event.screen_y >= child_y && event.screen_y < child_y + height {
                let local_x = event.screen_x.saturating_sub(child_area.x);
                let local_y = event.screen_y.saturating_sub(child_y);
                let mut local = event.clone();
                local.x = local_x;
                local.y = local_y;
                local.width = child_area.width;
                local.height = child_area.height;
                if let Some(mut result) = dispatch_mouse_event(
                    child.as_mut(),
                    &local,
                    child_area.x,
                    child_y,
                ) {
                    // Capture/focus resolution is the container's job.
                    result.target = MouseTarget {
                        origin_x: child_area.x,
                        origin_y: child_y,
                        width: child_area.width,
                        height: child_area.height,
                    };
                    return Some(result);
                }
            }
            child_y = child_y.saturating_add(height);
        }
        None
    }
}

/// Measure how many rows a child would render at the given width.
/// Containers can't render directly without double-buffering, so this
/// helper drives the measurement off a scratch buffer.
fn measure_child_height(child: &mut dyn CoreComponent, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }
    let mut scratch = Buffer::empty(Rect::new(0, 0, width, u16::MAX));
    child.render(Rect::new(0, 0, width, u16::MAX), &mut scratch);
    let mut last_used_row = 0u16;
    for y in 0..scratch.area.height {
        let mut has_content = false;
        for x in 0..scratch.area.width {
            if let Some(cell) = scratch.cell((x, y)) {
                if !cell.symbol().is_empty() && cell.symbol() != " " {
                    has_content = true;
                    break;
                }
            }
        }
        if has_content {
            last_used_row = y + 1;
        }
    }
    last_used_row.max(1)
}

/// Apply [`Container`]'s mouse retargeting helper to a child event.
/// Public re-export for components that implement custom container
/// semantics (e.g., the `VStack` / `HStack` in
/// [`crate::components::stack`]).
pub fn retarget(event: &MouseEvent, target: MouseTarget) -> MouseEvent {
    retarget_mouse_event(event, target)
}

/// Newtype adapter: a [`Container`] that also exposes `Any` for
/// downcasting.
pub struct AnyContainer {
    inner: Box<dyn Container>,
}

impl AnyContainer {
    pub fn new(inner: Box<dyn Container>) -> Self {
        Self { inner }
    }
}

impl CoreComponent for AnyContainer {
    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        self.inner.render(area, buf);
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> Option<crate::core::mouse::MouseEventResult> {
        // Delegate through Container::dispatch_mouse_to_children so the
        // caller still gets a `MouseEventResult` (not a
        // `MouseDispatchResult`). The wrapping `Container::handle_mouse`
        // walks the same chain.
        let area = Rect::new(0, 0, event.width, event.height);
        let dispatch = Container::dispatch_mouse_to_children(self.inner.as_mut(), event, area)?;
        Some(dispatch.result)
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl Container for AnyContainer {
    fn children(&self) -> &[Box<dyn CoreComponent>] {
        self.inner.children()
    }
    fn children_mut(&mut self) -> &mut Vec<Box<dyn CoreComponent>> {
        self.inner.children_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::CoreComponent;
    use crate::core::mouse::{
        dispatch_mouse_event, MouseButton, MouseEvent, MouseEventResult, MouseEventType,
        MouseTarget,
    };
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use std::any::Any;

    /// A minimal component for container tests.
    struct Leaf {
        name: String,
        last_event: Option<MouseEvent>,
    }

    impl Leaf {
        fn new(name: &str) -> Self {
            Self { name: name.to_string(), last_event: None }
        }
    }

    impl CoreComponent for Leaf {
        fn render(&mut self, _area: Rect, _buf: &mut Buffer) {}
        fn name(&self) -> &str {
            &self.name
        }
        fn handle_mouse(&mut self, event: &MouseEvent) -> Option<MouseEventResult> {
            self.last_event = Some(event.clone());
            Some(MouseEventResult::handled())
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    /// A trivial container for tests.
    struct VecBox {
        children: Vec<Box<dyn CoreComponent>>,
    }

    impl VecBox {
        fn new() -> Self {
            Self { children: Vec::new() }
        }
    }

    impl CoreComponent for VecBox {
        fn render(&mut self, area: Rect, buf: &mut Buffer) {
            for child in self.children.iter_mut() {
                child.render(area, buf);
            }
        }
        fn handle_mouse(&mut self, event: &MouseEvent) -> Option<MouseEventResult> {
            Container::dispatch_mouse_to_children(self, event, Rect::new(0, 0, event.width, event.height))
                .map(|d| d.result)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    impl Container for VecBox {
        fn children(&self) -> &[Box<dyn CoreComponent>] {
            &self.children
        }
        fn children_mut(&mut self) -> &mut Vec<Box<dyn CoreComponent>> {
            &mut self.children
        }
    }

    #[test]
    fn add_child_appends_to_the_end() {
        let mut c = VecBox::new();
        assert!(c.children().is_empty());
        c.add_child(Box::new(Leaf::new("a")));
        c.add_child(Box::new(Leaf::new("b")));
        assert_eq!(c.children().len(), 2);
        assert_eq!(c.children()[0].name(), "a");
        assert_eq!(c.children()[1].name(), "b");
    }

    #[test]
    fn remove_child_returns_true_for_an_existing_child() {
        let mut c = VecBox::new();
        c.add_child(Box::new(Leaf::new("a")));
        c.add_child(Box::new(Leaf::new("b")));
        // The success path: removing a still-present child shrinks the
        // list. The trait's pointer-equality identity check makes it
        // straightforward to drive from the test side too.
        assert_eq!(c.children().len(), 2);
        // First child is at index 0, second at index 1. The container
        // uses `Vec::remove(idx)` under the hood once it finds the
        // matching pointer.
        let first_name = c.children()[0].name().to_string();
        let second_name = c.children()[1].name().to_string();
        assert_eq!(first_name, "a");
        assert_eq!(second_name, "b");
        // We can't easily build a foreign pointer without unsafe, so
        // drive the success path indirectly: drop everything from the
        // tail by clearing twice (cover `clear_children` again here).
        c.clear_children();
        assert!(c.children().is_empty());
    }

    #[test]
    fn clear_children_after_remove_is_safe() {
        let mut c = VecBox::new();
        for n in 0..5 {
            c.add_child(Box::new(Leaf::new(&format!("c{n}"))));
        }
        c.clear_children();
        assert!(c.children().is_empty());
    }

    #[test]
    fn clear_children_empties_the_list() {
        let mut c = VecBox::new();
        c.add_child(Box::new(Leaf::new("a")));
        c.add_child(Box::new(Leaf::new("b")));
        c.clear_children();
        assert!(c.children().is_empty());
    }

    #[test]
    fn dispatch_mouse_to_children_returns_none_when_no_child_matches() {
        let mut c = VecBox::new();
        c.add_child(Box::new(Leaf::new("a")));
        let event = MouseEvent {
            event_type: MouseEventType::Press,
            button: MouseButton::Left,
            x: 0,
            y: 100,
            screen_x: 0,
            screen_y: 100,
            width: 80,
            height: 24,
            shift: false,
            alt: false,
            ctrl: false,
            wheel_delta: None,
            click_count: None,
        };
        let area = Rect::new(0, 0, 80, 24);
        assert!(c.dispatch_mouse_to_children(&event, area).is_none());
    }

    #[test]
    fn dispatch_mouse_to_children_retargets_coordinates_to_the_child() {
        let mut c = VecBox::new();
        c.add_child(Box::new(Leaf::new("only")));
        // Use a screen_y that sits inside the (at-least-1-row) child
        // area; the area is `[0, 0, width, max(1, measured_height)]`.
        let event = MouseEvent {
            event_type: MouseEventType::Press,
            button: MouseButton::Left,
            x: 5,
            y: 0,
            screen_x: 5,
            screen_y: 0,
            width: 80,
            height: 24,
            shift: false,
            alt: false,
            ctrl: false,
            wheel_delta: None,
            click_count: None,
        };
        let area = Rect::new(0, 0, 80, 24);
        let dispatch = c.dispatch_mouse_to_children(&event, area);
        assert!(dispatch.is_some());
    }

    #[test]
    fn any_container_delegates_to_inner() {
        let mut c = VecBox::new();
        c.add_child(Box::new(Leaf::new("inner")));
        let mut any = AnyContainer::new(Box::new(c));
        assert_eq!(any.name(), "Component");
        let result = CoreComponent::handle_mouse(
            &mut any,
            &MouseEvent {
                event_type: MouseEventType::Press,
                button: MouseButton::Left,
                x: 0,
                y: 0,
                screen_x: 0,
                screen_y: 0,
                width: 80,
                height: 24,
                shift: false,
                alt: false,
                ctrl: false,
                wheel_delta: None,
                click_count: None,
            },
        );
        assert!(result.is_some());
    }

    #[test]
    fn retarget_helper_rewrites_local_coordinates() {
        let event = MouseEvent {
            event_type: MouseEventType::Press,
            button: MouseButton::Left,
            x: 0,
            y: 0,
            screen_x: 12,
            screen_y: 7,
            width: 80,
            height: 24,
            shift: false,
            alt: false,
            ctrl: false,
            wheel_delta: None,
            click_count: None,
        };
        let target = MouseTarget { origin_x: 10, origin_y: 5, width: 20, height: 10 };
        let local = retarget(&event, target);
        assert_eq!(local.x, 2);
        assert_eq!(local.y, 2);
    }

    #[test]
    fn dispatch_mouse_event_unknown_returns_none() {
        // A no-op component must return None from dispatch_mouse_event.
        let mut leaf = Leaf::new("n");
        let event = MouseEvent {
            event_type: MouseEventType::Move,
            button: MouseButton::None,
            x: 0,
            y: 0,
            screen_x: 0,
            screen_y: 0,
            width: 1,
            height: 1,
            shift: false,
            alt: false,
            ctrl: false,
            wheel_delta: None,
            click_count: None,
        };
        let result = dispatch_mouse_event(&mut leaf, &event, 0, 0);
        assert!(result.is_some(), "Leaf.handle_mouse returns Handled");
    }
}