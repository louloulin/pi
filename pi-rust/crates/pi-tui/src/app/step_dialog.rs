//! Step handlers for modal dialogs and the settings list.
//!
//! Both [`crate::dialog::Dialog`] and [`crate::settings::SettingsList`] are
//! modal layers that sit between the chat-log keyboard path and the
//! composer: while one is open, every key is routed to its `handle_key`
//! method (and the settings list also owns wheel events). The handlers
//! live here so [`super::App::step_key_inner`] stays focused on the
//! non-modal layer.
//!
//! The wheel entry points resolve the row that owns the notch through the
//! modal-list hit test ([`super::App::modal_list_hit`]); a notch that
//! misses is still the modal's — upstream's `shouldDeferViewportInputToOverlay`
//! returns it unconsumed so the transcript behind the modal never moves.

use crate::dialog::DialogAction;
use crate::settings::SettingsAction;
use crate::input::Key;

use super::{App, StepOutcome, ALT_WHEEL_SCROLL_MULTIPLIER, WHEEL_SCROLL_LINES};

impl App {
    /// Route a key to the open dialog.
    pub(super) fn step_dialog(&mut self, key: Key) -> StepOutcome {
        let Some(dialog) = self.dialog.as_mut() else {
            return StepOutcome::Idle;
        };
        match dialog.handle_key(key) {
            DialogAction::None => StepOutcome::Idle,
            DialogAction::Changed => StepOutcome::Redraw,
            // The answer already travelled to the host over the
            // dialog's reply channel; the modal just closes.
            DialogAction::Resolved(_) => {
                self.dialog = None;
                StepOutcome::Redraw
            }
        }
    }

    /// Feed a key to the open settings list.
    pub(super) fn step_settings(&mut self, key: Key) -> StepOutcome {
        let Some(list) = self.settings.as_mut() else {
            return StepOutcome::Idle;
        };
        match list.handle_key(key) {
            SettingsAction::None => StepOutcome::Idle,
            SettingsAction::Changed => StepOutcome::Redraw,
            SettingsAction::ValueChanged { id, value } => {
                self.pending_setting_change = Some((id, value));
                StepOutcome::Redraw
            }
            SettingsAction::Activated(id) => {
                self.pending_setting_activation = Some(id);
                StepOutcome::Redraw
            }
            SettingsAction::Cancelled => {
                self.settings = None;
                StepOutcome::Redraw
            }
        }
    }

    /// Route a wheel event to the open settings list. Returns `None` when
    /// no settings modal is open, so the caller can fall through to the
    /// transcript.
    pub(super) fn step_settings_wheel(&mut self, up: bool, alt: bool) -> Option<StepOutcome> {
        let list = self.settings.as_mut()?;
        let lines = WHEEL_SCROLL_LINES * if alt { ALT_WHEEL_SCROLL_MULTIPLIER } else { 1 };
        let delta = if up { -(lines as i32) } else { lines as i32 };
        Some(match list.scroll_by(delta) {
            SettingsAction::Changed => StepOutcome::Redraw,
            _ => StepOutcome::Idle,
        })
    }
}
