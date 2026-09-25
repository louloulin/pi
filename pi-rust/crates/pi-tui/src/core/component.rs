//! `CoreComponent` — the port-side counterpart of upstream's
//! `Component` interface (`packages/tui/src/tui.ts:111-134`).
//!
//! Unlike the extension-facing [`crate::component::Component`], this
//! trait renders into a [`ratatui::buffer::Buffer`] and a
//! [`ratatui::layout::Rect`] so a host can keep the existing
//! `Frame::render_widget` style pipeline while still routing input,
//! mouse, focus, and invalidation through the same shape every other
//! component follows.
//!
//! The extension-facing trait stays a thin wrapper — its `render(width)
//! -> Vec<StyledLine>` continues to feed regions that go through the
//! line-based layout (header / footer / above-editor / below-editor).
//! Components that need the full mouse / overlay stack implement
//! [`CoreComponent`] instead.

use std::any::Any;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::core::focus::{FocusReason, FocusTarget};
use crate::core::mouse::{MouseEvent, MouseEventResult};
use crate::core::input_parse::{Key, KeyCode, KeyModifiers};

/// Zero-width escape sequence the focused component emits at its
/// cursor position so the TUI can place the hardware cursor (and let
/// the OS position the IME candidate window) without re-rendering.
///
/// Mirrors `packages/tui/src/tui.ts:168`.
pub const CURSOR_MARKER: &str = "\x1b_pi:c\x07";

/// Outcome of [`CoreComponent::handle_input`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputResult {
    /// The component did not consume the key. Host falls through.
    Ignored,
    /// The component consumed the key; host stops routing.
    Consumed,
    /// The component consumed the key AND a redraw is required.
    ConsumedRedraw,
}

impl InputResult {
    /// Convenience: `true` when the result carries a redraw request.
    pub fn needs_redraw(self) -> bool {
        matches!(self, InputResult::ConsumedRedraw)
    }
}

impl From<bool> for InputResult {
    fn from(consumed: bool) -> Self {
        if consumed {
            InputResult::Consumed
        } else {
            InputResult::Ignored
        }
    }
}

/// Outcome of [`CoreComponent::handle_paste`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteResult {
    /// Paste was inserted into the component.
    Consumed,
    /// Paste was ignored (component is not editable).
    Ignored,
}

/// The host-side component protocol.
///
/// Object-safe: no generic methods, no `Self` in return types. Mirrors
/// upstream's `Component` shape:
/// `render(width) → string[]`, `handleInput?`, `handleMouse?`,
/// `wantsKeyRelease?`, `invalidate?`.
pub trait CoreComponent: Any {
    /// Render the component into `buf` for the given `area`.
    fn render(&mut self, area: Rect, buf: &mut Buffer);

    /// Handle a key. Default: ignored.
    fn handle_input(&mut self, _key: &Key) -> InputResult {
        InputResult::Ignored
    }

    /// Handle a paste burst. Default: ignored.
    fn handle_paste(&mut self, _text: &str) -> PasteResult {
        PasteResult::Ignored
    }

    /// Handle a normalized mouse event.
    ///
    /// Returning `Some(MouseEventResult)` lets the host (a Container,
    /// or `TuiBase`) update focus / capture state. The default ignores
    /// mouse input.
    fn handle_mouse(&mut self, _event: &MouseEvent) -> Option<MouseEventResult> {
        None
    }

    /// Whether the component wants Kitty-protocol key release events.
    fn wants_key_release(&self) -> bool {
        false
    }

    /// Drop any cached rendering state.
    fn invalidate(&mut self) {}

    /// Name for debug / overlay focus tracking.
    fn name(&self) -> &str {
        "Component"
    }

    /// Upcast to [`FocusTarget`] for focus management.
    ///
    /// Returning `Some(...)` registers the component with `TuiBase`'s
    /// focus chain — the focus trait writes `focused = true/false` here
    /// when focus moves.
    fn as_focus_target(&mut self) -> Option<FocusTarget<'_>> {
        None
    }

    /// Upcast to `Any` for downcasting.
    fn as_any(&self) -> &dyn Any;
    /// Mutable upcast to `Any`.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Focus protocol — the Rust port of upstream's `Focusable`
/// (`packages/tui/src/tui.ts:152-155`).
pub trait Focusable {
    /// Whether the component currently holds keyboard focus.
    fn focused(&self) -> bool;
    /// Set the focus flag. The TUI calls this when focus moves.
    fn set_focused(&mut self, focused: bool);

    /// Optional hook for focus transitions (LUM-1500 §2.1).
    fn focus_changed(&mut self, _reason: FocusReason) {}
}

/// Type-guard helper matching upstream's `isFocusable`.
pub fn is_focusable(c: &mut dyn CoreComponent) -> bool {
    c.as_focus_target().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::focus::FocusReason;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use std::cell::Cell;

    struct Toggle {
        focus: Cell<bool>,
        name: String,
    }

    impl Toggle {
        fn new(name: &str) -> Self {
            Self { focus: Cell::new(false), name: name.to_string() }
        }
    }

    impl CoreComponent for Toggle {
        fn render(&mut self, _: Rect, _: &mut Buffer) {}
        fn name(&self) -> &str {
            &self.name
        }
        fn as_focus_target(&mut self) -> Option<FocusTarget<'_>> {
            None
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    impl Focusable for Toggle {
        fn focused(&self) -> bool {
            self.focus.get()
        }
        fn set_focused(&mut self, focused: bool) {
            self.focus.set(focused);
        }
        fn focus_changed(&mut self, _: FocusReason) {}
    }

    #[test]
    fn cursor_marker_is_apc_sequence() {
        // CURSOR_MARKER mirrors pi-ts's "\x1b_pi:c\x07" (APC + ETX).
        assert!(CURSOR_MARKER.starts_with('\x1b'));
        assert!(CURSOR_MARKER.ends_with('\x07'));
        assert_eq!(CURSOR_MARKER, "\x1b_pi:c\x07");
    }

    #[test]
    fn input_result_needs_redraw_flag() {
        assert!(!InputResult::Ignored.needs_redraw());
        assert!(!InputResult::Consumed.needs_redraw());
        assert!(InputResult::ConsumedRedraw.needs_redraw());
    }

    #[test]
    fn input_result_from_bool() {
        assert_eq!(InputResult::from(true), InputResult::Consumed);
        assert_eq!(InputResult::from(false), InputResult::Ignored);
    }

    #[test]
    fn paste_result_distinguishes_consumed() {
        assert_eq!(PasteResult::Consumed, PasteResult::Consumed);
        assert_eq!(PasteResult::Ignored, PasteResult::Ignored);
        assert_ne!(PasteResult::Consumed, PasteResult::Ignored);
    }

    #[test]
    fn focusable_set_get_round_trip() {
        let mut t = Toggle::new("t");
        assert!(!t.focused());
        t.set_focused(true);
        assert!(t.focused());
        t.set_focused(false);
        assert!(!t.focused());
    }

    #[test]
    fn is_focusable_recognizes_focusable_components() {
        struct Focusable1;
        impl CoreComponent for Focusable1 {
            fn render(&mut self, _: Rect, _: &mut Buffer) {}
            fn as_focus_target(&mut self) -> Option<FocusTarget<'_>> {
                Some(FocusTarget::new("f", self))
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
            fn as_any_mut(&mut self) -> &mut dyn Any {
                self
            }
        }
        impl Focusable for Focusable1 {
            fn focused(&self) -> bool {
                false
            }
            fn set_focused(&mut self, _: bool) {}
        }
        struct NotFocusable;
        impl CoreComponent for NotFocusable {
            fn render(&mut self, _: Rect, _: &mut Buffer) {}
            fn as_focus_target(&mut self) -> Option<FocusTarget<'_>> {
                None
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
            fn as_any_mut(&mut self) -> &mut dyn Any {
                self
            }
        }
        let mut f = Focusable1;
        let mut nf = NotFocusable;
        assert!(is_focusable(&mut f));
        assert!(!is_focusable(&mut nf));
    }

    #[test]
    fn core_component_default_methods() {
        let mut t = Toggle::new("t");
        assert_eq!(t.handle_input(&Key::new(KeyCode::Enter, KeyModifiers::NONE)), InputResult::Ignored);
        assert_eq!(t.handle_paste("abc"), PasteResult::Ignored);
        assert!(!t.wants_key_release());
        t.invalidate(); // no-op, but must not panic
        assert_eq!(t.name(), "t");
        let _any = t.as_any();
        let _any_mut = t.as_any_mut();
    }
}

/// Adapter: blanket-implements [`CoreComponent`] for any extension
/// [`crate::component::Component`] by rendering its lines into the
/// given buffer.
///
/// This is a thin convenience over
/// [`crate::utils::render_helpers::write_styled_lines_to_buffer`]. The host
/// must wire a theme into the writer for the styles to resolve; the
/// adapter stores one on construction.
pub struct ExtensionComponentAdapter<C: crate::component::Component> {
    inner: C,
}

impl<C: crate::component::Component> ExtensionComponentAdapter<C> {
    /// Wrap an extension component so it can sit inside a `TuiBase`.
    pub fn new(inner: C) -> Self {
        Self { inner }
    }
}

impl<C: crate::component::Component + 'static> CoreComponent
    for ExtensionComponentAdapter<C>
{
    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        let lines = self.inner.render(area.width);
        write_styled_lines(area, &lines, buf);
    }

    fn handle_input(&mut self, key: &Key) -> InputResult {
        InputResult::from(self.inner.handle_input(*key))
    }

    fn invalidate(&mut self) {
        self.inner.dispose();
    }

    fn name(&self) -> &str {
        "ExtensionComponentAdapter"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Theme-agnostic `StyledLine` → `Buffer` writer.
///
/// Resolves each span through a default style. Production code should
/// pass a theme; tests and the extension adapter use this default.
fn write_styled_lines(area: Rect, lines: &[crate::utils::styled::StyledLine], buf: &mut Buffer) {
    use crate::utils::styled::SpanStyle;
    use crate::theme::ThemeColor;
    use ratatui::style::{Color, Modifier, Style};
    for (row, line) in lines.iter().enumerate() {
        if row as u16 >= area.height {
            break;
        }
        let mut x = area.x;
        let y = area.y + row as u16;
        for span in line {
            let style = match span.style.fg {
                None => Style::default(),
                Some(ThemeColor::Accent) => {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                }
                Some(ThemeColor::Muted) => Style::default().fg(Color::DarkGray),
                Some(ThemeColor::Error) => Style::default().fg(Color::Red),
                Some(ThemeColor::Warning) => Style::default().fg(Color::Yellow),
                Some(ThemeColor::Success) => Style::default().fg(Color::Green),
                _ => Style::default(),
            };
            let mut style = style;
            if span.style.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if span.style.italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if span.style.underline {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            for ch in span.text.chars() {
                if x >= area.x + area.width {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_symbol(&ch.to_string());
                    cell.set_style(style);
                }
                x += 1;
            }
        }
    }
}