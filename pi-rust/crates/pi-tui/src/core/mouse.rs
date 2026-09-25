//! Mouse event types + dispatch helper.
//!
//! Port of upstream's mouse routing in
//! `packages/tui/src/tui.ts:21-109` and `tui-alt-screen.ts:115-118`.
//! The TS module dispatches to `Component.handleMouse`, which returns
//! a `TuiMouseEventResult` (handled / capture / focus / render flags).
//! This file is the Rust shape of that contract: a normalized
//! [`MouseEvent`], a [`MouseEventResult`] with the same four flags,
//! and [`dispatch_mouse_event`] / [`retarget_mouse_event`] helpers
//! that mirror `tui.ts:81-109`.

use ratatui::layout::Rect;

/// Kinds of mouse events the host understands.
///
/// Matches upstream `TuiMouseEventType`
/// (`packages/tui/src/tui.ts:21`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseEventType {
    /// A button was pressed.
    Press,
    /// A button was released.
    Release,
    /// The pointer moved without a button held down.
    Move,
    /// The pointer moved with a button held down (the host
    /// synthesizes this from `Press` + successive `Move` samples).
    Drag,
    /// A `Press` + `Release` pair without significant motion —
    /// synthesized by the host's click detector.
    Click,
    /// A wheel notch.
    Wheel,
}

/// Mouse button. `None` represents "no button held" (used for `Move`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MouseButton {
    #[default]
    None,
    Left,
    Middle,
    Right,
}

impl MouseButton {
    /// Convert from the SGR-encoded button number
    /// (`packages/tui/src/tui.ts` SGR decoder in `tui-alt-screen.ts`).
    pub fn from_sgr(code: u8) -> Self {
        match code {
            0 => MouseButton::Left,
            1 => MouseButton::Middle,
            2 => MouseButton::Right,
            _ => MouseButton::None,
        }
    }
}

/// Normalized mouse event — coordinates in **zero-based** cells,
/// local to the receiving component (`x`,`y`) and absolute
/// (`screen_x`,`screen_y`).
#[derive(Debug, Clone)]
pub struct MouseEvent {
    /// Event kind.
    pub event_type: MouseEventType,
    /// Button.
    pub button: MouseButton,
    /// Component-local column.
    pub x: u16,
    /// Component-local row.
    pub y: u16,
    /// Absolute column on the terminal.
    pub screen_x: u16,
    /// Absolute row on the terminal.
    pub screen_y: u16,
    /// Component width at the time of dispatch.
    pub width: u16,
    /// Component height at the time of dispatch.
    pub height: u16,
    /// Shift held.
    pub shift: bool,
    /// Alt held.
    pub alt: bool,
    /// Ctrl held.
    pub ctrl: bool,
    /// Logical wheel notches (negative = up, positive = down). Only
    /// populated for [`MouseEventType::Wheel`].
    pub wheel_delta: Option<i16>,
    /// Consecutive click count. Only populated for
    /// [`MouseEventType::Click`].
    pub click_count: Option<u16>,
}

impl MouseEvent {
    /// Build a press event with sensible defaults.
    pub fn press(button: MouseButton, x: u16, y: u16) -> Self {
        Self {
            event_type: MouseEventType::Press,
            button,
            x,
            y,
            screen_x: x,
            screen_y: y,
            width: 0,
            height: 0,
            shift: false,
            alt: false,
            ctrl: false,
            wheel_delta: None,
            click_count: None,
        }
    }
}

/// Result returned by [`CoreComponent::handle_mouse`](crate::core::CoreComponent::handle_mouse).
///
/// Mirrors upstream `TuiMouseEventResult`
/// (`packages/tui/src/tui.ts:46-58`): every field is optional.
#[derive(Debug, Clone, Copy, Default)]
pub struct MouseEventResult {
    /// Stop propagation; suppress the renderer-level fallback.
    pub handled: bool,
    /// Route subsequent drag / release events to this component.
    /// Implies `handled`.
    pub capture: bool,
    /// Give keyboard focus to this component. Implies `handled`.
    pub focus: bool,
    /// Force a redraw. `Press`, `Click`, `Drag`, and `Wheel` default
    /// to `true` upstream; `Move` and `Release` default to `false`.
    /// `TuiBase` honors the default when the field is `None`.
    pub render: Option<bool>,
}

impl MouseEventResult {
    /// Build a "consume and redraw" result.
    pub fn handled() -> Self {
        Self { handled: true, render: Some(true), ..Self::default() }
    }

    /// Build a "claim focus" result (implies handled).
    pub fn focus() -> Self {
        Self { handled: true, focus: true, render: Some(true), ..Self::default() }
    }

    /// Build a "capture drag" result (implies handled and focus).
    pub fn capture() -> Self {
        Self {
            handled: true,
            capture: true,
            focus: true,
            render: Some(true),
        }
    }

    /// Build an "ignored" result.
    pub fn ignored() -> Self {
        Self::default()
    }
}

/// Internal target metadata used by containers / alternate-screen
/// dispatch. Mirrors `TuiMouseDispatchTarget`
/// (`packages/tui/src/tui.ts:61-67`).
#[derive(Debug, Clone, Copy)]
pub struct MouseTarget {
    /// Component-local origin (for `retarget_mouse_event`).
    pub origin_x: u16,
    /// Component-local origin row.
    pub origin_y: u16,
    /// Component width at the time of dispatch.
    pub width: u16,
    /// Component height at the time of dispatch.
    pub height: u16,
}

/// Result of dispatching to a concrete component.
///
/// Mirrors `TuiMouseDispatchResult`
/// (`packages/tui/src/tui.ts:70-75`).
#[derive(Debug, Clone, Copy)]
pub struct MouseDispatchResult {
    /// Mouse-handling flags.
    pub result: MouseEventResult,
    /// Where the event landed.
    pub target: MouseTarget,
    /// Optional keyboard focus target (defaults to the receiving
    /// component).
    pub focus_target: Option<()>,
}

/// Dispatch a mouse event to `component` and translate the result
/// into a [`MouseDispatchResult`] when the component consumed it.
///
/// Mirrors `packages/tui/src/tui.ts:81-98` (`dispatchMouseEvent`).
pub fn dispatch_mouse_event(
    component: &mut dyn crate::core::CoreComponent,
    event: &MouseEvent,
    origin_x: u16,
    origin_y: u16,
) -> Option<MouseDispatchResult> {
    let result = component.handle_mouse(event)?;
    if !result.handled && !result.capture && !result.focus {
        return None;
    }
    Some(MouseDispatchResult {
        result,
        target: MouseTarget {
            origin_x,
            origin_y,
            width: event.width,
            height: event.height,
        },
        focus_target: None,
    })
}

/// Recreate local coordinates for a previously dispatched mouse
/// target. Mirrors `packages/tui/src/tui.ts:101-109`
/// (`retargetMouseEvent`).
pub fn retarget_mouse_event(event: &MouseEvent, target: MouseTarget) -> MouseEvent {
    MouseEvent {
        x: event.screen_x.saturating_sub(target.origin_x),
        y: event.screen_y.saturating_sub(target.origin_y),
        ..event.clone()
    }
}

/// Bounds of a region inside the terminal — used by
/// [`MouseTarget`] and overlay bookkeeping.
pub fn point_inside(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x && x < area.x.saturating_add(area.width) && y >= area.y
        && y < area.y.saturating_add(area.height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::{CoreComponent, Focusable};
    use crate::core::focus::{FocusReason, FocusTarget};
    use crate::core::input_parse::Key;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    /// Component that records every mouse event it sees.
    struct Recorder {
        events: Vec<MouseEvent>,
        capture: bool,
        focus_on_press: bool,
    }

    impl Recorder {
        fn new() -> Self {
            Self { events: Vec::new(), capture: false, focus_on_press: false }
        }
    }

    impl CoreComponent for Recorder {
        fn render(&mut self, _area: Rect, _buf: &mut Buffer) {}
        fn handle_mouse(&mut self, event: &MouseEvent) -> Option<MouseEventResult> {
            self.events.push(event.clone());
            if self.capture
                && (event.event_type == MouseEventType::Press
                    || event.event_type == MouseEventType::Drag)
            {
                Some(MouseEventResult::capture())
            } else if self.focus_on_press && event.event_type == MouseEventType::Press {
                Some(MouseEventResult::focus())
            } else if event.event_type == MouseEventType::Press {
                Some(MouseEventResult::handled())
            } else {
                Some(MouseEventResult::ignored())
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

    impl Focusable for Recorder {
        fn focused(&self) -> bool {
            false
        }
        fn set_focused(&mut self, _focused: bool) {}
        fn focus_changed(&mut self, _reason: FocusReason) {}
    }

    #[test]
    fn press_event_is_handled() {
        let mut r = Recorder::new();
        let event = MouseEvent::press(MouseButton::Left, 5, 10);
        let result = dispatch_mouse_event(&mut r, &event, 0, 0);
        assert!(result.is_some());
        assert!(result.unwrap().result.handled);
        assert_eq!(r.events.len(), 1);
    }

    #[test]
    fn drag_capture_prompts_to_capture() {
        let mut r = Recorder { capture: true, ..Recorder::new() };
        let press = MouseEvent::press(MouseButton::Left, 1, 2);
        let drag = MouseEvent {
            event_type: MouseEventType::Drag,
            button: MouseButton::Left,
            x: 2,
            y: 3,
            screen_x: 2,
            screen_y: 3,
            ..press.clone()
        };
        let result = dispatch_mouse_event(&mut r, &drag, 0, 0).unwrap();
        assert!(result.result.capture);
        assert!(result.result.handled);
    }

    #[test]
    fn retarget_event_uses_origin() {
        let mut r = Recorder::new();
        let press = MouseEvent { screen_x: 50, screen_y: 30, x: 0, y: 0, ..MouseEvent::press(MouseButton::Left, 0, 0) };
        let target = MouseTarget { origin_x: 40, origin_y: 20, width: 10, height: 10 };
        let local = retarget_mouse_event(&press, target);
        assert_eq!(local.x, 10);
        assert_eq!(local.y, 10);
    }

    #[test]
    fn button_sgr_decode_round_trip() {
        assert_eq!(MouseButton::from_sgr(0), MouseButton::Left);
        assert_eq!(MouseButton::from_sgr(1), MouseButton::Middle);
        assert_eq!(MouseButton::from_sgr(2), MouseButton::Right);
        assert_eq!(MouseButton::from_sgr(99), MouseButton::None);
    }

    #[test]
    fn point_inside_rect() {
        let area = Rect::new(5, 5, 4, 2);
        assert!(point_inside(area, 5, 5));
        assert!(point_inside(area, 8, 6));
        assert!(!point_inside(area, 9, 5));
        assert!(!point_inside(area, 5, 7));
        assert!(!point_inside(area, 4, 5));
    }
}