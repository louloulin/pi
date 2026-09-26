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

use crate::core::input_parse::{Key, KeyCode, KeyModifiers, MouseButton, MouseGesture, MouseGestureKind};
use crate::components::keybindings::get_keybindings;
use crate::locale::format_chord;
use crate::components::search::{apply_query_key, search_bar_rect, SearchSelectionMode};
use crate::utils::styled::{write_styled_line_ellipsized, SpanStyle, StyledLine};
use crate::utils::styled::StyledSpan;
use crate::theme::{ThemeBg, ThemeColor};

use super::{
    App, SearchKeyOutcome, StepOutcome, SCROLL_TO_END_LABEL, TRUNCATED_ABOVE_LEAD,
};

impl App {
    /// Route a key to the open search overlay.
    pub(super) fn step_search_key(&mut self, key: Key) -> SearchKeyOutcome {
        let bindings = get_keybindings();
        let event = crate::core::input_parse::InputEvent::Key(key);
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
        crate::utils::render_helpers::apply_search_highlight(
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
        crate::utils::render_helpers::apply_scrollbar(
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

    /// Composite a one-line welcome hint in the middle of the message area
    /// when the transcript is empty, no turn is running, and the editor has
    /// no draft — without it, an empty viewport is a wall of blank rows that
    /// reads as "the app is broken" instead of "type something".
    ///
    /// The hint stays out of the way the moment any of those three preconditions
    /// flips: as soon as a turn is running the user is watching the spinner,
    /// as soon as the transcript holds even one item the message renderer
    /// already fills the area, and as soon as the editor has text the
    /// composer grows to claim the rows the hint would have used.
    pub(super) fn paint_empty_hint(
        &self,
        message_area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        use std::sync::atomic::Ordering;
        if message_area.width == 0 || message_area.height < 3 {
            // A two-line editor on a 24-row terminal leaves only ~5 rows for
            // messages; below that height the hint would crowd the composer
            // and the user would never see it anyway.
            return;
        }
        if self.turn_busy.load(Ordering::SeqCst) {
            return;
        }
        if !self.messages.is_empty() {
            return;
        }
        if !self.prompt.is_empty() {
            return;
        }
        let label = StyledSpan {
            text: "Type a message and press Enter to chat — /help for commands".to_string(),
            style: SpanStyle::fg(ThemeColor::Muted),
            link: None,
        };
        let line: StyledLine = vec![label];
        let visible = crate::utils::hyperlink::visible_width(&line[0].text) as u16;
        if visible == 0 {
            return;
        }
        let width = visible.min(message_area.width);
        // Vertical anchor: middle of the viewport, biased slightly upward so
        // the hint sits at eye level on a tall window without crowding the
        // composer on a short one.
        let row = message_area.y + (message_area.height.saturating_sub(2) / 2);
        let column = message_area.x + (message_area.width.saturating_sub(width) / 2);
        // Blank the cells the hint covers so any leftover transcript glyphs
        // from a previous frame do not bleed through (ratatui only emits the
        // cells this buffer changed).
        for offset in 0..width {
            if let Some(cell) = buf.cell_mut((column + offset, row)) {
                cell.reset();
            }
        }
        write_styled_line_ellipsized(buf, column, row, width, &line, &self.theme);
    }

    /// Composite the pill onto the bottom row of the message viewport when
    /// the reader has scrolled away from the tail.
    pub(super) fn paint_scroll_to_end(
        &self,
        message_area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        crate::utils::render_helpers::paint_scroll_to_end(
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
        crate::utils::render_helpers::paint_truncated_above(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::input_parse::{Key, KeyCode, KeyModifiers};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn make_app() -> App {
        super::super::tool_stream_tests::test_app()
    }

    fn char_key(c: char) -> Key {
        Key::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn step_search_key_is_idle_when_overlay_is_closed() {
        let mut app = make_app();
        // No overlay open: any key is "handled" but produces no redraw.
        let outcome = app.step_search_key(char_key('a'));
        match outcome {
            SearchKeyOutcome::Handled(StepOutcome::Idle) => {}
            other => panic!("expected Handled(Idle), got {other:?}"),
        }
    }

    #[test]
    fn step_search_key_passes_through_unmodified_scroll_chords() {
        let mut app = make_app();
        // PageUp / PageDown / Home / End without modifiers must pass
        // through even when the overlay is closed — the test asserts the
        // overlay's filter logic does not swallow them.
        for code in [KeyCode::PageUp, KeyCode::PageDown, KeyCode::Home, KeyCode::End] {
            let outcome = app.step_search_key(Key::new(code, KeyModifiers::NONE));
            assert!(
                matches!(outcome, SearchKeyOutcome::PassThrough),
                "{code:?} must pass through, got {outcome:?}"
            );
        }
    }

    #[test]
    fn step_search_key_consumes_text_into_the_query_bar() {
        let mut app = make_app();
        assert!(app.open_search(), "open_search should succeed");
        let outcome = app.step_search_key(char_key('h'));
        assert!(
            matches!(outcome, SearchKeyOutcome::Handled(StepOutcome::Redraw)),
            "typing a character must be Handled(Redraw), got {outcome:?}"
        );
        // The query bar should now contain the typed character.
        let state = app.search.as_ref().expect("search state should be open");
        assert_eq!(state.bar.query(), "h");
    }

    #[test]
    fn apply_scrollbar_is_a_noop_when_geometry_is_zero() {
        let mut app = make_app();
        // No geometry has been painted yet; this must not panic.
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        app.apply_scrollbar(area, &mut buf);
    }

    #[test]
    fn apply_search_highlight_is_a_noop_when_overlay_is_closed() {
        let app = make_app();
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        // Closed overlay: must not panic, must not crash on `search.as_ref()`.
        app.apply_search_highlight(area, &mut buf);
    }

    #[test]
    fn scroll_to_end_label_has_a_fallback_when_no_binding_is_registered() {
        let app = make_app();
        let label = app.scroll_to_end_label();
        // The label must contain the canonical prefix.
        assert!(label.text.starts_with(SCROLL_TO_END_LABEL));
    }

    #[test]
    fn truncated_above_label_singular_for_one_line() {
        let app = make_app();
        let label = app.truncated_above_label(1);
        // The label uses the singular noun "line" for `hidden == 1`.
        assert!(label.text.contains("1 line above"));
    }

    #[test]
    fn truncated_above_label_plural_for_multiple_lines() {
        let app = make_app();
        let label = app.truncated_above_label(7);
        assert!(label.text.contains("7 lines above"));
    }

    #[test]
    fn paint_scroll_to_end_does_not_panic_on_an_empty_viewport() {
        let app = make_app();
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        app.paint_scroll_to_end(area, &mut buf);
    }

    #[test]
    fn paint_truncated_above_does_not_panic_on_an_empty_viewport() {
        let app = make_app();
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        app.paint_truncated_above(area, &mut buf);
    }

    #[test]
    fn paint_empty_hint_paints_centre_row_when_idle_and_empty() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut app = make_app();
        // Preconditions: not busy, no messages, no draft.
        assert!(!app.turn_busy.load(std::sync::atomic::Ordering::SeqCst));
        assert!(app.messages.is_empty());
        assert!(app.prompt.is_empty());

        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        app.paint_empty_hint(area, &mut buf);

        // The middle row (row 11 on a 24-row viewport) must have at least
        // one non-reset cell — the hint label.
        let row = area.y + (area.height.saturating_sub(2) / 2);
        let mut found = false;
        for x in area.x..(area.x + area.width) {
            if let Some(cell) = buf.cell((x, row)) {
                if cell.symbol() != " " || cell.style() != ratatui::style::Style::default() {
                    found = true;
                    break;
                }
            }
        }
        assert!(found, "paint_empty_hint must paint at least one cell on the centre row");
    }

    #[test]
    fn paint_empty_hint_is_quiet_when_composer_has_draft() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut app = make_app();
        app.prompt.editor_mut().set_text("hello world");
        assert!(!app.prompt.is_empty());

        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        app.paint_empty_hint(area, &mut buf);

        // No cell on the centre row should differ from a blank space.
        let row = area.y + (area.height.saturating_sub(2) / 2);
        for x in area.x..(area.x + area.width) {
            if let Some(cell) = buf.cell((x, row)) {
                assert_eq!(
                    cell.symbol().trim(),
                    "",
                    "paint_empty_hint must not paint when the composer has a draft"
                );
            }
        }
    }

    #[test]
    fn scroll_to_end_label_uses_a_non_empty_style() {
        let app = make_app();
        let label = app.scroll_to_end_label();
        // The pill must carry a non-empty fg/bg so it renders visibly.
        assert!(
            label.style.fg.is_some() || label.style.bg.is_some(),
            "the pill style must paint something, got {:?}",
            label.style
        );
    }
}