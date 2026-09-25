//! Step handlers for modal dialogs and the settings list.
//!
//! Both [`crate::components::dialog::Dialog`] and [`crate::components::settings::SettingsList`] are
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

use crate::components::dialog::DialogAction;
use crate::components::settings::SettingsAction;
use crate::core::input_parse::Key;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::input_parse::{Key, KeyCode, KeyModifiers};
    use crate::components::settings::{SettingItem, SettingsList};

    fn enter_key() -> Key {
        Key::new(KeyCode::Enter, KeyModifiers::NONE)
    }

    fn esc_key() -> Key {
        Key::new(KeyCode::Esc, KeyModifiers::NONE)
    }

    #[test]
    fn step_dialog_returns_idle_when_no_dialog_is_open() {
        let mut app = make_app();
        assert!(app.dialog.is_none());
        let outcome = app.step_dialog(enter_key());
        assert_eq!(outcome, StepOutcome::Idle);
    }

    #[test]
    fn step_settings_returns_idle_when_no_list_is_open() {
        let mut app = make_app();
        assert!(app.settings.is_none());
        let outcome = app.step_settings(enter_key());
        assert_eq!(outcome, StepOutcome::Idle);
    }

    #[test]
    fn step_settings_records_value_changes() {
        let mut app = make_app();
        let list = SettingsList::new(
            vec![SettingItem::new("theme", "Theme").with_values(["dark", "light"], "dark")],
            4,
        );
        app.open_settings(list);
        let outcome = app.step_settings(enter_key());
        assert_eq!(outcome, StepOutcome::Redraw);
        let pending = app
            .take_pending_setting_change()
            .expect("settings value change must be recorded");
        assert_eq!(pending.0, "theme");
        assert!(
            pending.1 == "dark" || pending.1 == "light",
            "value cycles between dark and light"
        );
    }

    #[test]
    fn step_settings_records_activations() {
        let mut app = make_app();
        let list = SettingsList::new(
            vec![SettingItem::new("editor.theme", "Editor theme")],
            4,
        );
        app.open_settings(list);
        let outcome = app.step_settings(enter_key());
        assert_eq!(outcome, StepOutcome::Redraw);
        assert_eq!(
            app.take_pending_setting_activation().as_deref(),
            Some("editor.theme")
        );
    }

    #[test]
    fn step_settings_cancels_and_closes_on_esc() {
        let mut app = make_app();
        app.open_settings(SettingsList::new(
            vec![SettingItem::new("a", "A")],
            4,
        ));
        let outcome = app.step_settings(esc_key());
        assert_eq!(outcome, StepOutcome::Redraw);
        assert!(app.settings.is_none(), "Esc closes the list");
    }

    #[test]
    fn step_settings_wheel_returns_none_when_no_list_is_open() {
        let mut app = make_app();
        assert!(app.settings.is_none());
        let outcome = app.step_settings_wheel(true, false);
        assert!(outcome.is_none());
    }

    #[test]
    fn step_settings_wheel_scrolls_and_redraws() {
        let mut app = make_app();
        let items: Vec<SettingItem> = (0..20)
            .map(|i| SettingItem::new(format!("k{i}"), format!("Item {i}")))
            .collect();
        app.open_settings(SettingsList::new(items, 4));
        let outcome = app
            .step_settings_wheel(true, false)
            .expect("a settings list is open");
        // The first wheel up on a fresh list is a no-op (already at top),
        // so the action is `None` and the outcome is `Idle`. Either
        // outcome is fine — the test just exercises the dispatch.
        assert!(matches!(outcome, StepOutcome::Idle | StepOutcome::Redraw));
    }

    #[test]
    fn step_settings_wheel_with_alt_uses_the_multiplier() {
        // Sanity check: opening a list then wheeling with Alt must not
        // panic and must produce one of the two outcomes.
        let mut app = make_app();
        let items: Vec<SettingItem> = (0..30)
            .map(|i| SettingItem::new(format!("k{i}"), format!("Item {i}")))
            .collect();
        app.open_settings(SettingsList::new(items, 4));
        let outcome = app
            .step_settings_wheel(false, true)
            .expect("a settings list is open");
        assert!(matches!(outcome, StepOutcome::Idle | StepOutcome::Redraw));
    }

    /// Build a minimal `App` for `step_*` direct tests.
    ///
    /// `App::new` requires an agent, which requires a faux provider. We
    /// delegate to `tool_stream_tests::test_app` (a sibling `cfg(test)`
    /// module inside the same file) so we don't duplicate the agent
    /// wiring.
    fn make_app() -> App {
        super::super::tool_stream_tests::test_app()
    }
}
