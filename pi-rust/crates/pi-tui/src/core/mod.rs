//! `pi-tui` core abstractions.
//!
//! The `core` module is the Rust port of `packages/tui/src/tui.ts`'s
//! `Component` / `Container` / `TuiBase` triangle. It mirrors the
//! public shape (Component, Focusable, CURSOR_MARKER, MouseEvent,
//! OverlayHandle) so a JS/TS plugin author reading this file can hold
//! it next to `tui.ts` and follow the names verbatim.
//!
//! ## Two layers
//!
//! - [`component`] defines the object-safe trait every region component
//!   implements (render to a buffer, handle input / paste / mouse,
//!   surface focus). The extension-facing
//!   [`crate::component::Component`] is a deliberately narrower surface
//!   and stays untouched.
//! - [`tui_base`] is the owning struct: it owns the child components,
//!   the focused pointer, the overlay stack, and the keyboard / mouse
//!   dispatchers that the [`App`](crate::App) used to roll by hand.
//!
//! ## Why `Buffer` rendering and not `render(width) -> Vec<StyledLine>`
//!
//! The extension surface stays on the line-based API (themes are host
//! concerns, layout is host concerns). The TuiBase surface renders to a
//! `ratatui::buffer::Buffer` so the host's existing paint pipeline can
//! be reused while the focus / overlay / mouse layer takes over the
//! routing that used to live in `App::step_key` and `App::step_mouse`.

pub mod component;
pub mod container;
pub mod focus;
pub mod input;
pub mod input_parse;
pub mod keys;
pub mod mouse;
pub mod overlay;
pub mod tui_base;
pub mod tui_drivers;

pub use component::{
    is_focusable, CoreComponent, Focusable, InputResult, PasteResult, CURSOR_MARKER,
};
pub use container::Container;
pub use focus::{FocusReason, FocusTarget};
pub use input::{InputListener, InputListenerHandle, InputPipeline};
pub use input_parse::{
    is_mouse_sequence, parse_mouse_sequence, InputEvent, Key, KeyModifiers, MouseButton,
    MouseGesture, MouseGestureKind,
};
pub use keys::{
    decode_kitty_printable, is_key_release, is_key_repeat, is_kitty_protocol_active, matches_key,
    parse_key, set_kitty_protocol_active, KeyEventType, KeyId,
};
pub use mouse::{
    dispatch_mouse_event, retarget_mouse_event, MouseButton as CoreMouseButton, MouseDispatchResult,
    MouseEvent, MouseEventResult, MouseEventType,
};
pub use overlay::{
    OverlayAnchor, OverlayBounds, OverlayEntry, OverlayHandle, OverlayMargin, OverlayOptions,
    OverlayPolicy, SizeValue,
};
pub use tui_base::{TuiBase, TuiBaseConfig, TuiMode};
pub use tui_drivers::{
    TuiAltScreen, TuiMainScreen, TuiSink, ALT_SCREEN_ENTER, ALT_SCREEN_EXIT,
};