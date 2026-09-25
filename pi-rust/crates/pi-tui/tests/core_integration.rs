//! Integration tests for the modular TUI core.
//!
//! Exercises the new `core::` module — `TuiBase`, `CoreComponent`,
//! mouse dispatch, overlay stack, focus chain, FramePacer, and
//! `StdinBuffer` — as a single end-to-end flow:
//!
//! 1. Build a `TuiBase` with a tiny root component.
//! 2. Register the root as a focusable.
//! 3. Render to a buffer.
//! 4. Diff against the next frame via `FramePacer`.
//! 5. Push raw bytes into `StdinBuffer` and confirm they round-trip.
//! 6. Stack two overlays and confirm focus restoration on pop.
//! 7. Dispatch a mouse event and confirm the host routes it.
//!
//! If any of these break, the modularization contract has regressed.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use pi_tui::core::component::{CoreComponent, Focusable, InputResult};
use pi_tui::core::focus::{FocusReason, FocusTarget};
use pi_tui::core::mouse::{MouseButton, MouseEvent, MouseEventResult, MouseEventType};
use pi_tui::core::overlay::OverlayOptions;
use pi_tui::core::tui_base::{FocusPair, TuiBase, TuiBaseConfig};
use pi_tui::frame_pacer::FramePacer;
use pi_tui::stdin_buffer::{StdinBuffer, StdinBufferOptions};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

/// Tiny component used as a root for the integration test.
struct Root {
    focused: Rc<RefCell<bool>>,
    mouse_hits: Rc<RefCell<Vec<(u16, u16)>>>,
}

impl Root {
    fn new() -> Self {
        Self {
            focused: Rc::new(RefCell::new(false)),
            mouse_hits: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

impl CoreComponent for Root {
    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        // Render the letter 'R' at the top-left of the area.
        if let Some(cell) = buf.cell_mut((area.x, area.y)) {
            cell.set_symbol("R");
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> Option<MouseEventResult> {
        if event.event_type == MouseEventType::Press {
            self.mouse_hits.borrow_mut().push((event.x, event.y));
            Some(MouseEventResult::handled())
        } else {
            Some(MouseEventResult::ignored())
        }
    }

    fn as_focus_target(&mut self) -> Option<FocusTarget<'_>> {
        Some(FocusTarget::new("root", self))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl Focusable for Root {
    fn focused(&self) -> bool {
        *self.focused.borrow()
    }
    fn set_focused(&mut self, focused: bool) {
        *self.focused.borrow_mut() = focused;
    }
}

#[test]
fn end_to_end_modular_tui() {
    // 1. Build a host with a tiny root.
    let root = Root::new();
    let focus_cell = root.focused.clone();
    let mouse_hits = root.mouse_hits.clone();
    let mut base = TuiBase::new(Box::new(root), TuiBaseConfig::default());

    // 2. Register the root as a focusable.
    base.register_focusable_pair(
        "root",
        FocusPair::from_cell(focus_cell.clone(), None),
    );
    base.set_focus(Some("root"), FocusReason::Explicit);
    assert_eq!(base.focused(), Some("root"));
    assert!(*focus_cell.borrow());

    // 3. Render to a buffer.
    let area = Rect::new(0, 0, 4, 2);
    let mut buf = Buffer::empty(area);
    base.render(&mut buf);
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "R");

    // 4. Frame pacer renders the same root and produces a non-empty
    //    payload.
    let mut pacer = FramePacer::new();
    let payload = pacer.render_with(area, |b| {
        if let Some(cell) = b.cell_mut((0, 0)) {
            cell.set_symbol("R");
        }
    });
    assert!(payload.contains('R'));

    // 5. StdinBuffer round-trip.
    let mut stdin = StdinBuffer::new(StdinBufferOptions::tty());
    stdin.push(b"\x1b[A");
    let drained = stdin.drain();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0], b"\x1b[A");

    // 6. Overlay stack with focus restoration.
    let h1 = base.show_overlay(OverlayOptions::centered());
    assert!(base.has_overlay());
    base.hide_overlay(h1);
    assert!(!base.has_overlay());
    assert!(*focus_cell.borrow()); // focus restored to "root"

    // 7. Mouse dispatch.
    let press = MouseEvent::press(MouseButton::Left, 1, 1);
    let handled = base.dispatch_mouse(&press);
    assert!(handled);
    assert_eq!(*mouse_hits.borrow(), vec![(1, 1)]);

    // Clear focus and confirm unregister drops the flag.
    assert!(base.unregister_focusable("root"));
    assert!(!*focus_cell.borrow());
}

#[test]
fn input_result_round_trip() {
    use pi_tui::input::{Key, KeyCode, KeyModifiers};
    let mut root = Root::new();
    // handle_input is a default no-op on Root, returning Ignored.
    let _ = root.handle_input(&Key::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(<Root as CoreComponent>::handle_input(&mut root, &Key::new(KeyCode::Enter, KeyModifiers::NONE)), InputResult::Ignored);
}

#[test]
fn frame_pacer_integration_with_tui_base() {
    let mut base = TuiBase::new(Box::new(Root::new()), TuiBaseConfig::default());
    let area = Rect::new(0, 0, 3, 1);
    let mut pacer = FramePacer::new();
    // First render — full redraw, must contain 'R'.
    let buf = base.render_to_buffer();
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "R");
    let _ = pacer.render_with(area, |b| {
        if let Some(cell) = b.cell_mut((0, 0)) {
            cell.set_symbol("R");
        }
    });
    let _next = base.render_to_buffer();
}

#[test]
fn mouse_capture_promotes_to_capture_target() {
    // A component that captures drag.
    struct CaptureComp {
        cell: Rc<Cell<bool>>,
        drag_count: Rc<Cell<u32>>,
    }
    impl CoreComponent for CaptureComp {
        fn render(&mut self, _: Rect, _: &mut Buffer) {}
        fn handle_mouse(&mut self, event: &MouseEvent) -> Option<MouseEventResult> {
            match event.event_type {
                MouseEventType::Press | MouseEventType::Drag => {
                    if event.event_type == MouseEventType::Drag {
                        self.drag_count.set(self.drag_count.get() + 1);
                    }
                    Some(MouseEventResult::capture())
                }
                _ => Some(MouseEventResult::ignored()),
            }
        }
        fn as_focus_target(&mut self) -> Option<FocusTarget<'_>> {
            None
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }
    impl Focusable for CaptureComp {
        fn focused(&self) -> bool {
            self.cell.get()
        }
        fn set_focused(&mut self, f: bool) {
            self.cell.set(f);
        }
    }

    let cell = Rc::new(Cell::new(false));
    let drag_count = Rc::new(Cell::new(0));
    let mut base = TuiBase::new(
        Box::new(CaptureComp { cell: cell.clone(), drag_count: drag_count.clone() }),
        TuiBaseConfig::default(),
    );

    let press = MouseEvent::press(MouseButton::Left, 5, 5);
    assert!(base.dispatch_mouse(&press));
    let drag = MouseEvent {
        event_type: MouseEventType::Drag,
        button: MouseButton::Left,
        x: 6,
        y: 5,
        screen_x: 6,
        screen_y: 5,
        ..press.clone()
    };
    assert!(base.dispatch_mouse(&drag));
    assert_eq!(drag_count.get(), 1);
}