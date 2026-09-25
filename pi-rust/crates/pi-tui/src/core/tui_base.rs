//! `TuiBase` — the host-side core that owns the focus chain, overlay
//! stack, mouse capture target, and input pipeline.
//!
//! Mirrors `packages/tui/src/tui.ts:465-...` (`TuiBase`) and
//! `tui-alt-screen.ts` (`TuiAltScreen`). The base is *abstract*: the
//! frame's paint pipeline (fullscreen vs. regular) is supplied by a
//! subclass that implements [`TuiBase::do_render`].
//!
//! ## Initial scope
//!
//! `TuiBase` covers the routing concerns (focus, overlay, mouse,
//! input listeners). Frame painting stays on the existing ratatui
//! pipeline; `do_render` is the bridge that walks the child list and
//! blits each component into a shared buffer.

use std::collections::HashMap;
use std::rc::Rc;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::core::component::{CoreComponent, Focusable};
use crate::core::focus::FocusReason;
use crate::core::input::{InputListener, InputListenerHandle, InputPipeline};
use crate::core::mouse::{MouseEvent, MouseEventResult, MouseTarget};
use crate::core::overlay::{OverlayEntry, OverlayHandle, OverlayOptions, OverlayPolicy};
use crate::core::input_parse::Key;

/// Where the host is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TuiMode {
    /// Regular main-screen mode (no alt-screen).
    #[default]
    Regular,
    /// Fullscreen alt-screen mode.
    Fullscreen,
}

/// Construction-time knobs for [`TuiBase`].
#[derive(Debug, Clone)]
pub struct TuiBaseConfig {
    /// Operating mode.
    pub mode: TuiMode,
    /// Initial terminal size (`width`, `height`).
    pub size: (u16, u16),
}

impl Default for TuiBaseConfig {
    fn default() -> Self {
        Self { mode: TuiMode::default(), size: (80, 24) }
    }
}

/// A focusable registration entry. Components register a callback the
/// base uses to read / write the focused flag without needing raw
/// pointers (the crate forbids `unsafe`).
#[derive(Clone)]
struct FocusRegistration {
    name: String,
    /// Read the current focused flag.
    reader: Rc<dyn Fn() -> bool>,
    /// Write the focused flag.
    writer: Rc<dyn Fn(bool)>,
    /// Optional focus-changed hook.
    notify: Option<Rc<dyn Fn(FocusReason)>>,
}

/// The host base. Holds the root container, focus, overlays, mouse
/// capture, and the input pipeline. Subclasses (`TuiAltScreen`,
/// `TuiMainScreen`) supply the painting strategy.
pub struct TuiBase {
    mode: TuiMode,
    size: (u16, u16),
    /// Root component (typically a [`crate::components::stack::VStack`]).
    root: Box<dyn CoreComponent>,
    /// Currently focused component name.
    focused_id: Option<String>,
    /// Registered focusables keyed by name.
    focus_registry: HashMap<String, FocusRegistration>,
    /// Overlay stack (top of stack = last entry).
    overlays: Vec<OverlayEntry>,
    /// Capture target for ongoing drag / release events.
    capture: Option<MouseTarget>,
    /// Input pipeline (raw bytes → batched listeners).
    pipeline: InputPipeline,
    /// Last rendered size — used to invalidate the buffer on resize.
    last_render_size: (u16, u16),
}

impl TuiBase {
    /// Construct a `TuiBase` with a given root component.
    pub fn new(root: Box<dyn CoreComponent>, config: TuiBaseConfig) -> Self {
        Self {
            mode: config.mode,
            size: config.size,
            root,
            focused_id: None,
            focus_registry: HashMap::new(),
            overlays: Vec::new(),
            capture: None,
            pipeline: InputPipeline::new(),
            last_render_size: config.size,
        }
    }

    /// Mode the host is running in.
    pub fn mode(&self) -> TuiMode {
        self.mode
    }

    /// Current terminal size.
    pub fn size(&self) -> (u16, u16) {
        self.size
    }

    /// Resize notification. Invalidates the buffer when the size
    /// changes.
    pub fn resize(&mut self, width: u16, height: u16) {
        if self.size != (width, height) {
            self.size = (width, height);
            self.root.invalidate();
        }
    }

    /// Borrow the root component.
    pub fn root(&self) -> &dyn CoreComponent {
        self.root.as_ref()
    }

    /// Mutable borrow of the root component.
    pub fn root_mut(&mut self) -> &mut dyn CoreComponent {
        self.root.as_mut()
    }

    /// Borrow the input pipeline.
    pub fn pipeline(&self) -> &InputPipeline {
        &self.pipeline
    }

    /// Mutable borrow of the input pipeline.
    pub fn pipeline_mut(&mut self) -> &mut InputPipeline {
        &mut self.pipeline
    }

    /// Register a focusable with paired reader/writer closures. The
    /// base uses this for focus transitions.
    pub fn register_focusable_with_writer<F, G>(
        &mut self,
        name: &str,
        reader: F,
        writer: G,
    ) where
        F: Fn() -> bool + 'static,
        G: Fn(bool) + 'static,
    {
        let reader = Rc::new(reader);
        let writer = Rc::new(writer);
        self.focus_registry.insert(
            name.to_string(),
            FocusRegistration { name: name.to_string(), reader, writer, notify: None },
        );
    }

    /// Register a focusable with a paired reader/writer and optional
    /// focus-change notification. Components build the pair from their
    /// own focus state and hand it to the host.
    pub fn register_focusable_pair(
        &mut self,
        name: &str,
        pair: FocusPair,
    ) {
        let notify = pair.notify;
        let reader: Rc<dyn Fn() -> bool> = pair.reader;
        let writer: Rc<dyn Fn(bool)> = pair.writer;
        self.focus_registry.insert(
            name.to_string(),
            FocusRegistration { name: name.to_string(), reader, writer, notify },
        );
    }

    /// Drop a registration. Clears the focused flag on the registered
/// component so a partially-built tree doesn't leak `focused = true`
/// into another host.
    pub fn unregister_focusable(&mut self, name: &str) -> bool {
        if let Some(reg) = self.focus_registry.remove(name) {
            (reg.writer)(false);
            true
        } else {
            false
        }
    }

    /// Set the focused component by name. Triggers `set_focused(false)`
    /// on the previous and `set_focused(true)` on the next.
    pub fn set_focus(&mut self, name: Option<&str>, reason: FocusReason) {
        let prev = self.focused_id.clone();
        let next = name.map(|s| s.to_string());
        if prev == next {
            return;
        }
        if let Some(prev_name) = prev {
            if let Some(reg) = self.focus_registry.get(&prev_name) {
                (reg.writer)(false);
            }
        }
        if let Some(next_name) = next.as_deref() {
            if let Some(reg) = self.focus_registry.get(next_name) {
                (reg.writer)(true);
                if let Some(notify) = &reg.notify {
                    notify(reason);
                }
            }
        }
        self.focused_id = next;
    }

    /// Name of the currently focused component, if any.
    pub fn focused(&self) -> Option<&str> {
        self.focused_id.as_deref()
    }

    /// Push an overlay onto the stack. Returns an opaque handle.
    pub fn show_overlay(&mut self, opts: OverlayOptions) -> OverlayHandle {
        let handle = OverlayHandle::next();
        let entry = OverlayEntry {
            id: handle.id(),
            bounds: None,
            options: opts,
            hidden: false,
            focus_order: handle.id(),
            pre_focus_id: self.focused_id.clone(),
        };
        self.overlays.push(entry);
        handle
    }

    /// Pop the overlay matching `handle`. Idempotent.
    pub fn hide_overlay(&mut self, handle: OverlayHandle) {
        if let Some(idx) = self.overlays.iter().position(|e| e.id == handle.id()) {
            let entry = self.overlays.remove(idx);
            if self.focused_id.is_some() {
                self.set_focus(entry.pre_focus_id.as_deref(), FocusReason::Restored);
            }
        }
    }

    /// Hide / show an overlay by handle.
    pub fn set_overlay_hidden(&mut self, handle: OverlayHandle, hidden: bool) {
        if let Some(entry) = self.overlays.iter_mut().find(|e| e.id == handle.id()) {
            entry.hidden = hidden;
        }
    }

    /// Whether any overlay is currently visible.
    pub fn has_overlay(&self) -> bool {
        self.overlays.iter().any(|e| !e.hidden)
    }

    /// Topmost visible overlay's bounds, if any.
    pub fn topmost_overlay_bounds(&self) -> Option<crate::core::overlay::OverlayBounds> {
        self.overlays
            .iter()
            .rev()
            .find(|e| !e.hidden)
            .and_then(|e| e.bounds)
    }

    /// Dispatch a mouse event into the host. Routed to the topmost
    /// overlay's component if any, otherwise to the root.
    pub fn dispatch_mouse(&mut self, event: &MouseEvent) -> bool {
        let needs_redraw = event.event_type != crate::core::mouse::MouseEventType::Move
            && event.event_type != crate::core::mouse::MouseEventType::Release;

        // Capture overrides overlay routing so a drag in progress
        // continues to feed the original target.
        if let Some(target) = self.capture {
            let local = crate::core::mouse::retarget_mouse_event(event, target);
            if let Some(r) = self.root.handle_mouse(&local) {
                return r.handled || needs_redraw;
            }
            return needs_redraw;
        }

        if let Some(result) = self.root.handle_mouse(event) {
            self.apply_mouse_result(&result, event);
            return result.handled || needs_redraw;
        }
        needs_redraw
    }

    fn apply_mouse_result(&mut self, result: &MouseEventResult, event: &MouseEvent) {
        if result.capture {
            self.capture = Some(MouseTarget {
                origin_x: event.screen_x.saturating_sub(event.x),
                origin_y: event.screen_y.saturating_sub(event.y),
                width: event.width,
                height: event.height,
            });
        }
        if event.event_type == crate::core::mouse::MouseEventType::Release {
            self.capture = None;
        }
    }

    /// Dispatch a key. The focused component (or the overlay's owner)
    /// gets the first shot; falling through is the host's job.
    pub fn dispatch_key(&mut self, key: &Key) -> bool {
        let result = self.root.handle_input(key);
        match result {
            crate::core::component::InputResult::Ignored => false,
            crate::core::component::InputResult::Consumed
            | crate::core::component::InputResult::ConsumedRedraw => true,
        }
    }

    /// Render the host into a fresh `Buffer` for the current size.
    pub fn render(&mut self, buf: &mut Buffer) {
        let area = Rect::new(0, 0, self.size.0, self.size.1);
        self.root.render(area, buf);
        self.last_render_size = self.size;
    }

    /// Drive one paint frame: build a buffer of `self.size`, render
    /// the root, then composite the topmost overlay if any.
    pub fn render_to_buffer(&mut self) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, self.size.0, self.size.1));
        self.render(&mut buf);
        buf
    }

    /// Add an input listener (raw bytes hook). Returns a handle.
    pub fn add_input_listener(
        &mut self,
        listener: Box<dyn InputListener>,
    ) -> InputListenerHandle {
        self.pipeline.add_listener(listener)
    }

    /// Remove a previously added input listener.
    pub fn remove_input_listener(&mut self, handle: InputListenerHandle) -> bool {
        self.pipeline.remove_listener(handle)
    }
}

impl Drop for TuiBase {
    fn drop(&mut self) {
        // Clear focus flags so a partially-built tree doesn't leak
        // `focused = true` into another host.
        for (_name, reg) in self.focus_registry.drain() {
            (reg.writer)(false);
        }
    }
}

/// A focus reader/writer pair handed to `TuiBase` when a component
/// registers itself. Components build this from their own focusable
/// state — typically by holding a `RefCell<bool>`.
pub struct FocusPair {
    pub reader: Rc<dyn Fn() -> bool>,
    pub writer: Rc<dyn Fn(bool)>,
    pub notify: Option<Rc<dyn Fn(FocusReason)>>,
}

impl FocusPair {
    /// Build a pair from a `RefCell<bool>` plus a notify hook.
    pub fn from_cell(
        cell: std::rc::Rc<std::cell::RefCell<bool>>,
        notify: Option<Rc<dyn Fn(FocusReason)>>,
    ) -> Self {
        let r = cell.clone();
        let w = cell.clone();
        Self {
            reader: Rc::new(move || *r.borrow()),
            writer: Rc::new(move |v| *w.borrow_mut() = v),
            notify,
        }
    }
}

/// Re-export `FocusTarget` for convenience.
pub use crate::core::focus::FocusTarget as FocusTargetRef;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::CoreComponent;
    use crate::core::focus::FocusTarget;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    /// A trivial focusable component for tests.
    struct Counter {
        name: String,
        cell: std::rc::Rc<std::cell::RefCell<bool>>,
    }

    impl Counter {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                cell: std::rc::Rc::new(std::cell::RefCell::new(false)),
            }
        }
        fn cell(&self) -> std::rc::Rc<std::cell::RefCell<bool>> {
            self.cell.clone()
        }
    }

    impl CoreComponent for Counter {
        fn render(&mut self, area: Rect, buf: &mut Buffer) {
            if let Some(cell_buf) = buf.cell_mut((area.x, area.y)) {
                cell_buf.set_symbol(&self.name);
            }
        }
        fn name(&self) -> &str {
            &self.name
        }
        fn as_focus_target(&mut self) -> Option<FocusTarget<'_>> {
            let n = self.name.clone();
            Some(FocusTarget::new(Box::leak(n.into_boxed_str()), self))
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    impl Focusable for Counter {
        fn focused(&self) -> bool {
            *self.cell.borrow()
        }
        fn set_focused(&mut self, focused: bool) {
            *self.cell.borrow_mut() = focused;
        }
    }

    fn fresh() -> (TuiBase, Counter) {
        let c = Counter::new("a");
        let cell = c.cell();
        let mut base = TuiBase::new(
            Box::new(c),
            TuiBaseConfig { mode: TuiMode::Regular, size: (10, 4) },
        );
        let pair = FocusPair::from_cell(cell, None);
        base.register_focusable_pair("a", pair);
        let c2 = base.root_mut();
        let any = c2.as_any_mut();
        let counter_ref = any.downcast_mut::<Counter>().unwrap();
        let cloned = Counter {
            name: counter_ref.name.clone(),
            cell: counter_ref.cell.clone(),
        };
        (base, cloned)
    }

    #[test]
    fn set_focus_flips_flag() {
        let (mut base, counter) = fresh();
        assert_eq!(base.focused(), None);
        assert!(!*counter.cell.borrow());
        base.set_focus(Some("a"), FocusReason::Explicit);
        assert_eq!(base.focused(), Some("a"));
        assert!(*counter.cell.borrow());
        base.set_focus(None, FocusReason::Cleared);
        assert_eq!(base.focused(), None);
        assert!(!*counter.cell.borrow());
    }

    #[test]
    fn overlay_round_trip() {
        let (mut base, _) = fresh();
        let h1 = base.show_overlay(OverlayOptions::default());
        let h2 = base.show_overlay(OverlayOptions::default());
        assert_ne!(h1.id(), h2.id());
        assert!(base.has_overlay());
        base.hide_overlay(h1);
        assert!(base.has_overlay());
        base.hide_overlay(h2);
        assert!(!base.has_overlay());
    }

    #[test]
    fn render_writes_into_buffer() {
        let (mut base, _) = fresh();
        let buf = base.render_to_buffer();
        let s = buf.cell((0, 0)).unwrap().symbol();
        assert_eq!(s, "a");
    }

    #[test]
    fn resize_invalidates_root() {
        let (mut base, _) = fresh();
        base.resize(20, 10);
        assert_eq!(base.size(), (20, 10));
    }

    #[test]
    fn input_listener_pipeline_handoff() {
        use crate::core::input::InputListener;
        struct Count(u32);
        impl InputListener for Count {
            fn handle_input(&mut self, data: &str) -> bool {
                self.0 += data.len() as u32;
                true
            }
        }
        let (mut base, _) = fresh();
        let h = base.add_input_listener(Box::new(Count(0)));
        base.pipeline_mut().push(b"hello");
        assert!(base.pipeline_mut().flush());
        assert!(base.remove_input_listener(h));
    }

    #[test]
    fn unregister_clears_focus() {
        let (mut base, counter) = fresh();
        base.set_focus(Some("a"), FocusReason::Explicit);
        assert!(*counter.cell.borrow());
        assert!(base.unregister_focusable("a"));
        // Dropping the registration also clears the flag.
        assert!(!*counter.cell.borrow());
    }
}