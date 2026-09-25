//! Focus management — the Rust shape of upstream's `setFocus` chain
//! (`packages/tui/src/tui.ts:554-621`).
//!
//! `TuiBase` owns a single `FocusTarget` at a time. When focus moves,
//! the previous target's `set_focused(false)` runs first, then the
//! next target's `set_focused(true)` runs. The `FocusReason` records
//! *why* focus moved, mirroring the bookkeeping the TS overlay stack
//! does when a focus change should restore the previously focused
//! overlay after an interleaving component releases it.

use crate::core::component::Focusable;

/// Why focus moved. Mirrors the cases `TuiBase.setFocusInternal` cares
/// about in `packages/tui/src/tui.ts:558-621`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusReason {
    /// A direct `set_focus(...)` call.
    Explicit,
    /// A mouse press routed by `TuiBase.dispatch_mouse`.
    Mouse,
    /// Focus returned to the previously focused component after a
    /// blocking component (custom overlay) released focus.
    Restored,
    /// Focus was cleared (`set_focus(None)`).
    Cleared,
}

/// Borrowed upcast into a focusable component.
///
/// The lifetime ties a [`FocusTarget`] to the component it borrows
/// from, which is why the [`crate::core::CoreComponent::as_focus_target`] method
/// returns it. `TuiBase` stores these in a way that lets the focus
/// bookkeeping read & write the underlying flag without taking
/// ownership of the component.
pub struct FocusTarget<'a> {
    /// The component the target belongs to (used for debug logging).
    pub name: &'a str,
    /// Mutable view used by `set_focused`.
    pub focusable: &'a mut dyn Focusable,
}

impl<'a> FocusTarget<'a> {
    /// Build a focus target from a name + a focusable view.
    pub fn new(name: &'a str, focusable: &'a mut dyn Focusable) -> Self {
        Self { name, focusable }
    }
}