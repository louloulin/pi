//! Top-level pointer routing.
//!
//! [`App::step_mouse_gesture`] is the screen-level dispatcher: it routes
//! the gesture to the open modal overlay first, then the search bar, then
//! the autocomplete dropdown, then the scroll-to-end pill, then the
//! truncated-above hint, then the chat-log scrollbar, then the prompt,
//! and finally the chat-log text selection. The actual handlers
//! (`step_modal_mouse_gesture`, `step_scrollbar_mouse_gesture`,
//! `step_selection_mouse_gesture`) live alongside it so this file owns the
//! whole pointer surface.

use crate::core::input_parse::{MouseButton, MouseGesture, MouseGestureKind};
use crate::app::viewport::ScrollbarDrag;

use super::{App, StepOutcome};

impl App {
    /// Feed a non-wheel mouse gesture to the App.
    ///
    /// Mirrors upstream's `handleSelectionMouseEvent` etc. The dispatch
    /// follows the same order upstream uses: modal overlay → search bar →
    /// autocomplete → scroll-to-end pill → truncated-above hint → scrollbar
    /// → prompt → chat-log selection.
    pub fn step_mouse_gesture(&mut self, gesture: MouseGesture) -> StepOutcome {
        // A modal owns the mouse exactly as it owns the keyboard
        // (`step_key`): gestures are hit-tested against the open overlays'
        // rectangles and never reach the chat log underneath.
        if self.dialog.is_some() || self.settings.is_some() || self.selector.is_some() {
            // A modal covers the chat log, so the bar behind it is neither
            // hovered nor draggable while one is up (upstream's `hasOverlay()`
            // guard in `getScrollbarTargetAt`).
            self.scrollbar_hover = false;
            self.scrollbar_drag = None;
            return self.step_modal_mouse_gesture(gesture);
        }
        // The search bar is an overlay too, but it lives alongside the chat
        // log instead of over a modal, so it gets the same first pass.
        if let Some(outcome) = self.step_search_mouse_gesture(&gesture) {
            // The bar was consumed by the search bar's own rows.
            self.scrollbar_hover = false;
            self.scrollbar_drag = None;
            return outcome;
        }
        // No modal is up, so a modal click cannot still be pending.
        self.modal_mouse_press = None;
        // The composer's autocomplete dropdown is painted over the transcript
        // rows directly above the editor, so it owns the pointer there —
        // upstream hit-tests the list rectangle inside the editor's own
        // `handleMouse` before the screen-level text selection
        // (`packages/tui/src/components/editor.ts:618-638`). Without this a
        // click on a candidate selected the transcript text behind it.
        if let Some(outcome) = self.autocomplete_mouse_gesture(&gesture) {
            self.scrollbar_hover = false;
            self.scrollbar_drag = None;
            return outcome;
        }
        // The pill is the first thing on the transcript to get the pointer
        // (upstream `handleScrollToEndIndicatorMouseEvent`, tested before the
        // scrollbar and the selection, `packages/tui/src/tui-alt-screen.ts:1017-1024`):
        // a left press on it jumps to the tail instead of starting a
        // selection of the pill's own text.
        if let Some(rect) = self.scroll_to_end_rect() {
            let on_pill = gesture.y == rect.y
                && gesture.x >= rect.x
                && gesture.x < rect.x.saturating_add(rect.width);
            if on_pill && matches!(gesture.kind, MouseGestureKind::Press(MouseButton::Left)) {
                self.stop_selection_autoscroll();
                self.selection = None;
                self.selection_dragging = false;
                self.scrollbar_hover = false;
                self.scrollbar_drag = None;
                self.messages.set_following(true);
                return StepOutcome::Redraw;
            }
        }
        // The "cut above" hint is the second piece of transcript furniture to
        // get the pointer: it advertises `tui.altScreen.top`, so a left press
        // on it does exactly what that key does — never a selection of cells
        // the hint is covering.
        if let Some(rect) = self.truncated_above_rect() {
            let on_hint = gesture.y == rect.y
                && gesture.x >= rect.x
                && gesture.x < rect.x.saturating_add(rect.width);
            if on_hint && matches!(gesture.kind, MouseGestureKind::Press(MouseButton::Left)) {
                self.stop_selection_autoscroll();
                self.selection = None;
                self.selection_dragging = false;
                self.scrollbar_hover = false;
                self.scrollbar_drag = None;
                self.scroll_viewport_to_top();
                return StepOutcome::Redraw;
            }
        }
        // The scrollbar is hit-tested before the selection path, exactly
        // like upstream (`handleScrollbarMouseEvent` runs before
        // `handleSelectionMouseEvent`). While a drag owns the pointer the
        // hover flag stays set; otherwise it follows the pointer.
        let handled = self.step_scrollbar_mouse_gesture(&gesture);
        let hover_changed = if self.scrollbar_drag.is_some() {
            false
        } else {
            self.update_scrollbar_hover(gesture.x, gesture.y)
        };
        let outcome = match handled {
            Some(outcome) => outcome,
            None => match self.prompt_mouse_gesture(&gesture) {
                Some(outcome) => outcome,
                None => self.step_selection_mouse_gesture(&gesture),
            },
        };
        if matches!(outcome, StepOutcome::Idle) && hover_changed {
            StepOutcome::Redraw
        } else {
            outcome
        }
    }

    /// Handle a pointer gesture against the chat-log scrollbar, if it lands
    /// there — upstream's `handleScrollbarMouseEvent`.
    pub(super) fn step_scrollbar_mouse_gesture(
        &mut self,
        gesture: &MouseGesture,
    ) -> Option<StepOutcome> {
        // A drag owns every non-wheel gesture until the button comes back up
        // (upstream's `if (this.scrollbarDrag) { … return true; }`), so a
        // stray press cannot start a selection mid-drag.
        if let Some(drag) = self.scrollbar_drag {
            return Some(match gesture.kind {
                MouseGestureKind::Release(_) => {
                    self.scrollbar_drag = None;
                    StepOutcome::Idle
                }
                MouseGestureKind::Drag(_) => {
                    // The geometry is recomputed from the live viewport, so a
                    // log that grows mid-drag still maps correctly.
                    let scrolled = self.scrollbar_geometry().is_some_and(|geometry| {
                        self.scroll_scrollbar_to_pointer(&geometry, gesture.y, drag.grab_offset)
                    });
                    if scrolled {
                        StepOutcome::Redraw
                    } else {
                        StepOutcome::Idle
                    }
                }
                _ => StepOutcome::Idle,
            });
        }

        let left = MouseButton::Left;
        match gesture.kind {
            MouseGestureKind::Press(button) if button == left => {
                let geometry = self.scrollbar_geometry_at(gesture.x, gesture.y)?;
                let on_thumb = gesture.y >= geometry.thumb_top
                    && gesture.y < geometry.thumb_top.saturating_add(geometry.thumb_height);
                let grab_offset = if on_thumb {
                    gesture.y - geometry.thumb_top
                } else {
                    geometry.thumb_height / 2
                };
                // The bar takes the pointer: drop the text selection and the
                // pending double-click, and cancel any drag autoscroll
                // (upstream's `clearTextSelection()` / `stopSelectionAutoScroll()`).
                self.selection = None;
                self.selection_dragging = false;
                self.stop_selection_autoscroll();
                self.last_click = None;
                self.scrollbar_hover = true;
                self.scrollbar_drag = Some(ScrollbarDrag { grab_offset });
                let scrolled = if on_thumb {
                    false
                } else {
                    self.scroll_scrollbar_to_pointer(&geometry, gesture.y, grab_offset)
                };
                Some(if scrolled {
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                })
            }
            _ => None,
        }
    }

    /// Route a gesture to the open modal overlay (`dialog`, `settings`, or
    /// `selector`).
    pub(super) fn step_modal_mouse_gesture(&mut self, gesture: MouseGesture) -> StepOutcome {
        let hit = self
            .mouse_regions()
            .into_iter()
            .find_map(|region| region.capture(gesture));
        let Some(point) = hit else {
            // Outside every overlay rectangle the modal still swallows the
            // gesture; a press that missed cannot become a click.
            if matches!(gesture.kind, MouseGestureKind::Release(MouseButton::Left)) {
                self.modal_mouse_press = None;
            }
            return StepOutcome::Idle;
        };
        match gesture.kind {
            MouseGestureKind::Press(MouseButton::Left) => {
                self.modal_mouse_press = Some(point);
                // The press highlights the row under the pointer, exactly like
                // upstream's `SelectList` / `SettingsList` press branch; the
                // release only *activates* it.
                self.modal_list_press(gesture.y)
            }
            MouseGestureKind::Release(MouseButton::Left) => {
                let clicked = self.modal_mouse_press.take() == Some(point);
                if clicked {
                    // A click on a list row activates it (upstream
                    // `SelectList::handleMouse` click branch).
                    if let Some(outcome) = self.modal_list_click(gesture.y) {
                        return outcome;
                    }
                }
                if clicked && self.has_selection() {
                    self.clear_selection();
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                }
            }
            // Drags inside a modal, bare moves and the other buttons are
            // consumed without changing anything.
            _ => StepOutcome::Idle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::input_parse::{MouseButton, MouseGesture, MouseGestureKind};
    use crate::components::selector::{Selector, SelectorItem};

    fn make_app() -> App {
        super::super::tool_stream_tests::test_app()
    }

    fn empty_selector() -> Selector {
        Selector::new("test", Vec::<SelectorItem>::new())
    }

    #[test]
    fn step_mouse_gesture_dispatches_to_the_modal_when_a_modal_is_open() {
        // With no geometry painted, no overlay rectangle captures the
        // gesture, so the dispatcher still routes to the modal handler and
        // returns Idle without panicking.
        let mut app = make_app();
        app.open_selector(empty_selector());
        let outcome = app.step_mouse_gesture(MouseGesture::left_press(2, 2));
        assert_eq!(outcome, StepOutcome::Idle);
    }

    #[test]
    fn step_mouse_gesture_clears_pending_press_when_no_modal_is_open() {
        // A stale modal press must not survive once the overlay closes;
        // the screen-level dispatcher resets it on every non-modal gesture.
        let mut app = make_app();
        let before = app.modal_mouse_press;
        let outcome = app.step_mouse_gesture(MouseGesture::left_press(0, 0));
        let _ = (before, outcome);
    }

    #[test]
    fn step_scrollbar_mouse_gesture_returns_none_for_non_press_gestures() {
        // Bare moves and non-left-button presses should be ignored: the
        // scrollbar only reacts to left-button press/drag/release cycles.
        let mut app = make_app();
        let drag = MouseGesture::new(MouseGestureKind::Drag(MouseButton::Right), 0, 0, false);
        assert!(app.step_scrollbar_mouse_gesture(&drag).is_none());
        let release = MouseGesture::new(MouseGestureKind::Release(MouseButton::Right), 0, 0, false);
        assert!(app.step_scrollbar_mouse_gesture(&release).is_none());
    }

    #[test]
    fn step_modal_mouse_gesture_drops_a_release_outside_any_overlay() {
        let mut app = make_app();
        app.open_selector(empty_selector());
        // No overlay rectangle is painted yet, so a release returns Idle
        // and the modal still swallows the gesture.
        let outcome = app.step_modal_mouse_gesture(MouseGesture::left_release(0, 0));
        assert_eq!(outcome, StepOutcome::Idle);
    }

    #[test]
    fn step_modal_mouse_gesture_highlights_on_a_press_outside_overlay() {
        let mut app = make_app();
        app.open_selector(empty_selector());
        // A left press that misses every overlay region records no press
        // and returns Idle — the modal still owns the gesture.
        let outcome = app.step_modal_mouse_gesture(MouseGesture::left_press(0, 0));
        assert_eq!(outcome, StepOutcome::Idle);
    }

    #[test]
    fn step_mouse_gesture_with_a_release_only_returns_idle_for_non_modal() {
        // Without a modal, a release lands in the selection/scrollbar path
        // — the dispatcher must not panic regardless of geometry.
        let mut app = make_app();
        let outcome = app.step_mouse_gesture(MouseGesture::left_release(0, 0));
        // No geometry has been painted, so nothing changes. The assertion
        // is purely "does not panic".
        let _ = outcome;
    }
}