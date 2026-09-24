//! Transcript search overlay.
//!
//! [`App::step_search_key`] routes keys while the overlay has focus;
//! [`App::refresh_search`] re-runs the index and selects a match using the
//! same anchor-row / retain-mode state machine upstream's `refreshSearch`
//! uses; [`App::apply_search_highlight`] / [`App::apply_scrollbar`] are the
//! render-side counterparts that paint matches and the chat-log scrollbar
//! onto the already-rendered message area. Mouse gestures on the bar are
//! routed by [`App::step_search_mouse_gesture`].
//!
//! The free-form hint affordances (`paint_scroll_to_end`,
//! `paint_truncated_above`) and their label helpers live here too — they
//! are part of the same transcript-navigation surface.

use crate::input::{Key, KeyCode, KeyModifiers, MouseButton, MouseGesture, MouseGestureKind};
use crate::keybindings::get_keybindings;
use crate::locale::format_chord;
use crate::search::{apply_query_key, search_bar_rect, SearchSelectionMode};
use crate::styled::SpanStyle;
use crate::styled::StyledSpan;
use crate::theme::{ThemeBg, ThemeColor};

use super::{
    App, SearchKeyOutcome, StepOutcome, SCROLL_TO_END_LABEL, TRUNCATED_ABOVE_LEAD,
};

impl App {
    /// Route a key to the open search overlay.
    pub(super) fn step_search_key(&mut self, key: Key) -> SearchKeyOutcome {
        let bindings = get_keybindings();
        let event = crate::input::InputEvent::Key(key);
        if bindings.matches(&event, "tui.altScreen.searchClose")
            || bindings.matches(&event, "tui.altScreen.search")
        {
            return SearchKeyOutcome::Handled(if self.close_search() {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            });
        }
        if bindings.matches(&event, "tui.altScreen.searchNext") {
            return SearchKeyOutcome::Handled(if self.navigate_search(1) {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            });
        }
        if bindings.matches(&event, "tui.altScreen.searchPrevious") {
            return SearchKeyOutcome::Handled(if self.navigate_search(-1) {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            });
        }

        // The viewport scroll chords and the process-global keys keep
        // working while the bar has focus.
        match key {
            Key {
                code: KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End,
                modifiers,
            } if modifiers.is_empty() => return SearchKeyOutcome::PassThrough,
            Key {
                code: KeyCode::Char('c' | 'l'),
                modifiers,
            } if modifiers == KeyModifiers::CONTROL => return SearchKeyOutcome::PassThrough,
            _ => {}
        }

        let Some(state) = self.search.as_mut() else {
            return SearchKeyOutcome::Handled(StepOutcome::Idle);
        };
        if apply_query_key(&mut state.bar, key) {
            self.search_query_changed();
            SearchKeyOutcome::Handled(StepOutcome::Redraw)
        } else {
            SearchKeyOutcome::Handled(StepOutcome::Idle)
        }
    }

    /// Paint the search matches into the already-rendered message area.
    pub(super) fn apply_search_highlight(
        &self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        crate::render_helpers::apply_search_highlight(
            &self.messages,
            self.search.as_ref(),
            area,
            buf,
        );
    }

    /// Paint the chat-log scrollbar into the message viewport's last column.
    pub(super) fn apply_scrollbar(
        &self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        crate::render_helpers::apply_scrollbar(
            self.scrollbar_hover,
            self.scrollbar_drag,
            self.scrollbar_geometry(),
            &self.theme,
            area,
            buf,
        );
    }

    /// Give an open search overlay the mouse first, exactly like the modal
    /// overlays.
    pub(super) fn step_search_mouse_gesture(
        &mut self,
        gesture: &MouseGesture,
    ) -> Option<StepOutcome> {
        self.search.as_ref()?;
        let (width, height) = self.viewport();
        let (origin_x, origin_y) = self.viewport_origin();
        if width == 0 || height == 0 {
            return None;
        }
        let rect = search_bar_rect(ratatui::layout::Rect::new(origin_x, origin_y, width, height))?;
        let inside = gesture.x >= rect.x
            && gesture.x < rect.x + rect.width
            && gesture.y >= rect.y
            && gesture.y < rect.y + rect.height;
        let (row, col) = (
            gesture.y as i64 - rect.y as i64,
            gesture.x as i64 - rect.x as i64,
        );
        let direction = if inside {
            self.search
                .as_ref()
                .and_then(|state| state.bar.navigation_direction_at(rect.width, row, col))
        } else {
            None
        };
        let changed = self
            .search
            .as_mut()
            .map(|state| state.bar.set_hovered(direction))
            .unwrap_or(false);
        if !inside {
            return if changed {
                Some(StepOutcome::Redraw)
            } else {
                None
            };
        }
        if let (Some(direction), true) = (
            direction,
            matches!(gesture.kind, MouseGestureKind::Press(MouseButton::Left)),
        ) {
            if self.navigate_search(direction) {
                return Some(StepOutcome::Redraw);
            }
        }
        Some(if changed {
            StepOutcome::Redraw
        } else {
            StepOutcome::Idle
        })
    }

    /// The pill's label: ` ↓ Jump to latest message · <shortcut> `, exactly
    /// upstream's string with the shortcut resolved from `tui.altScreen.bottom`.
    pub(super) fn scroll_to_end_label(&self) -> StyledSpan {
        let shortcut = get_keybindings()
            .get_keys("tui.altScreen.bottom")
            .iter()
            .map(|chord| format_chord(chord))
            .collect::<Vec<_>>()
            .join("/");
        let text = if shortcut.is_empty() {
            SCROLL_TO_END_LABEL.to_string()
        } else {
            format!("{SCROLL_TO_END_LABEL}· {shortcut} ")
        };
        StyledSpan {
            text,
            style: SpanStyle::fg_bg(ThemeColor::Text, ThemeBg::SelectedBg),
            link: None,
        }
    }

    /// Composite the pill onto the bottom row of the message viewport when
    /// the reader has scrolled away from the tail.
    pub(super) fn paint_scroll_to_end(
        &self,
        message_area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        crate::render_helpers::paint_scroll_to_end(
            &self.viewport,
            self.messages.is_following(),
            self.max_scroll(),
            self.scrollbar_geometry(),
            message_area,
            &self.theme,
            buf,
        );
    }

    /// The hint's label: ` ⋯ <n> line(s) above · <shortcut> ` — the prompt
    /// half of the affordance, resolved from `tui.altScreen.top`.
    pub(super) fn truncated_above_label(&self, hidden: usize) -> StyledSpan {
        let shortcut = get_keybindings()
            .get_keys("tui.altScreen.top")
            .iter()
            .map(|chord| format_chord(chord))
            .collect::<Vec<_>>()
            .join("/");
        let noun = if hidden == 1 { "line" } else { "lines" };
        let base = format!("{TRUNCATED_ABOVE_LEAD}{hidden} {noun} above ");
        let text = if shortcut.is_empty() {
            base
        } else {
            format!("{base}· {shortcut} ")
        };
        StyledSpan {
            text,
            style: SpanStyle::fg(ThemeColor::Muted),
            link: None,
        }
    }

    /// Disclose that the block at the top edge of the viewport continues
    /// above it, on the viewport's first row.
    pub(super) fn paint_truncated_above(
        &self,
        message_area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        let (width, height) = self.viewport();
        crate::render_helpers::paint_truncated_above(
            &self.viewport,
            &self.messages,
            self.scrollbar_geometry(),
            width,
            height,
            self.messages.is_following(),
            self.resolved_scroll(),
            message_area,
            &self.theme,
            buf,
        );
    }
}