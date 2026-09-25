//! Paste handlers.
//!
//! Two paste paths reach the App: the bracketed-paste entry point
//! [`App::step_paste`], which the driver calls for `crossterm`'s
//! `CtEvent::Paste`, and the burst classifier
//! [`App::step_composer_burst`], which recognises a paste the terminal
//! delivered as a fast run of key events. Both paths share the same
//! compositor entry point ([`crate::Editor::insert_paste`]) so a
//! folded marker, its registry entry and its undo unit are the very
//! same ones a bracketed paste produces.

use std::time::Instant;

use crate::components::editor::PasteInsertOutcome;
use crate::core::input_parse::{BurstDecision, Key, KeyCode};

use super::{App, StepOutcome};

impl App {
    /// Route a bracketed paste into the composer.
    ///
    /// The driver calls this for `crossterm`'s [`crate::crossterm::event::Event::Paste`],
    /// which the terminal only ever sends while the driver has bracketed paste
    /// enabled (`pi-coding-agent`'s `setup_terminal`, upstream
    /// `packages/tui/src/terminal.ts:184`). The payload arrives as **one**
    /// event instead of a burst of keys, which is the whole point: a pasted
    /// block used to reach the composer as individual characters and every
    /// newline in it was an `Enter`, so pasting three lines submitted the
    /// draft three times.
    ///
    /// Paste is a separate entry point rather than an
    /// [`InputEvent`](crate::core::input_parse::InputEvent) variant because that enum is
    /// `Copy` by construction — [`App::step`] matches it by value and still
    /// uses it afterwards — and a heap payload cannot ride in a `Copy` type.
    ///
    /// The modal layers keep their priority: while a dialog, the settings
    /// modal or a selector owns the keyboard, a paste is dropped instead of
    /// editing the frozen composer underneath it.
    pub fn step_paste(&mut self, text: &str) -> StepOutcome {
        if self.exit_requested {
            return StepOutcome::Exit;
        }
        // A real bracketed paste is authoritative: any burst still being
        // classified is flushed first (so byte order is preserved) and the
        // classification window is dropped, so the keystrokes that follow the
        // paste cannot be grouped with it.
        self.flush_paste_burst_now();
        if self.dialog.is_some()
            || self.settings.is_some()
            || self.selector.is_some()
            || self.custom_open()
        {
            return StepOutcome::Idle;
        }
        match self.prompt.editor_mut().insert_paste(text) {
            PasteInsertOutcome::Ignored => StepOutcome::Idle,
            PasteInsertOutcome::Inserted | PasteInsertOutcome::Marker(_) => StepOutcome::Redraw,
        }
    }

    // -----------------------------------------------------------------
    // Paste burst (codex `paste_burst`)
    // -----------------------------------------------------------------

    /// Feed a key that has reached the composer into the paste-burst
    /// detector.
    ///
    /// `None` means "not consumed — handle the key normally"; `Some(outcome)`
    /// means the key joined a burst (or a due burst was flushed), so the
    /// ordinary composer path must not see it.
    ///
    /// Every character is inserted as ordinary typing until a run proves to
    /// be paste-like, so nothing is ever held back waiting for a tick; when
    /// the run is confirmed, the prefix is *retroactively* folded into the
    /// burst ([`Editor::cut_chars_before_cursor`](crate::Editor::cut_chars_before_cursor)).
    /// A misclassification therefore degrades to "a few characters inserted
    /// together" and never to a lost keystroke.
    pub(super) fn step_composer_burst(&mut self, key: Key, now: Instant) -> Option<StepOutcome> {
        // A reverse search owns the composer: its query is not a paste, and
        // its `Esc` must not be swallowed by a burst either.
        if !self.config.paste_burst || self.history_search_active() {
            return None;
        }
        // A burst that has gone quiet is flushed before this key is read, so
        // a chord never lands on top of half a paste.
        if self.flush_paste_burst_if_due(now) {
            self.burst_flush_pending = true;
        }

        if let Some(c) = Self::burst_plain_char(key) {
            return match self.paste_burst.on_plain_char(c, now) {
                // Not paste-like: let the ordinary path insert it. A due
                // flush, if any, is reported by the flag.
                BurstDecision::Typed => None,
                BurstDecision::BeginBurst { retro_chars } => {
                    let prefix = self
                        .prompt
                        .editor_mut()
                        .cut_chars_before_cursor(retro_chars);
                    if prefix.chars().count() == retro_chars {
                        self.paste_burst.absorb_retro(&prefix);
                    } else {
                        // The cursor had fewer characters than the run: the
                        // prefix cannot be absorbed, so fall back to plain
                        // insertion rather than dropping the buffered text.
                        let recovered = self.paste_burst.abort();
                        let text = format!("{prefix}{recovered}");
                        if !text.is_empty() {
                            self.prompt.editor_mut().insert_str(&text);
                        }
                    }
                    Some(StepOutcome::Redraw)
                }
                BurstDecision::Buffered => Some(StepOutcome::Idle),
            };
        }

        // `Enter` / `Tab` inside a burst stay inside the paste: the newline
        // of a pasted block must never submit the draft it was pasted into.
        if let Some(c) = Self::burst_control_char(key) {
            if self.paste_burst.append_control_if_active(c, now) {
                return Some(StepOutcome::Idle);
            }
        }

        // Any other key ends the burst context: flush whatever was buffered
        // and forget the window, so the next keystroke starts a fresh run
        // instead of being grouped with this one. The key itself is **not**
        // consumed — `Ctrl+R` must still open the search.
        if self.flush_paste_burst_now() {
            self.burst_flush_pending = true;
        }
        None
    }

    /// The character a plain text-producing key carries, if any.
    ///
    /// Shifted characters count — a capital letter is ordinary text — while
    /// Control / Alt / Meta combinations are chords, never paste content.
    fn burst_plain_char(key: Key) -> Option<char> {
        if key.modifiers.control || key.modifiers.alt || key.modifiers.meta {
            return None;
        }
        match key.code {
            KeyCode::Char(c) => Some(c),
            _ => None,
        }
    }

    /// The control character a paste can carry, if `key` is one.
    fn burst_control_char(key: Key) -> Option<char> {
        if key.modifiers.control || key.modifiers.alt || key.modifiers.meta {
            return None;
        }
        match key.code {
            KeyCode::Enter => Some('\n'),
            KeyCode::Tab => Some('\t'),
            _ => None,
        }
    }

    /// Flush a burst that has gone quiet, returning whether the draft
    /// changed.
    pub(super) fn flush_paste_burst_if_due(&mut self, now: Instant) -> bool {
        match self.paste_burst.flush_if_due(now) {
            Some(text) => self.insert_paste_burst_text(&text),
            None => false,
        }
    }

    /// Flush an active burst immediately (a key that cannot belong to the
    /// paste is about to be handled).
    pub(super) fn flush_paste_burst_now(&mut self) -> bool {
        match self.paste_burst.flush_now_and_clear() {
            Some(text) => self.insert_paste_burst_text(&text),
            None => false,
        }
    }

    /// Route burst content through the composer's paste entry point, so a
    /// folded marker, its registry entry and its undo unit are the very same
    /// ones a bracketed paste produces.
    fn insert_paste_burst_text(&mut self, text: &str) -> bool {
        match self.prompt.editor_mut().insert_paste(text) {
            PasteInsertOutcome::Ignored => false,
            PasteInsertOutcome::Inserted | PasteInsertOutcome::Marker(_) => true,
        }
    }

    /// Flush a paste burst that has gone quiet, driven by the render loop's
    /// beat.
    ///
    /// A burst becomes visible only when it is flushed, and the flush is
    /// time-based, so a driver must call this on every loop iteration;
    /// `pi-coding-agent`'s `run_loop` does, and uses
    /// [`App::paste_burst_deadline`] to shorten its poll timeout while a
    /// burst is accumulating. Returns whether the draft changed.
    pub fn tick_paste_burst(&mut self, now: Instant) -> bool {
        if !self.config.paste_burst {
            return false;
        }
        self.flush_paste_burst_if_due(now)
    }

    /// When [`App::tick_paste_burst`] next has something to flush, if a burst
    /// is accumulating. `None` when the classifier is off or idle.
    pub fn paste_burst_deadline(&self) -> Option<Instant> {
        if !self.config.paste_burst {
            return None;
        }
        self.paste_burst.flush_deadline()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::input_parse::{Key, KeyCode, KeyModifiers};

    fn make_app() -> App {
        super::super::tool_stream_tests::test_app()
    }

    fn char_key(c: char) -> Key {
        Key::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn step_paste_inserts_text_when_no_modal_is_open() {
        let mut app = make_app();
        let before = app.prompt.editor().text().to_string();
        let outcome = app.step_paste("hello");
        assert_eq!(outcome, StepOutcome::Redraw);
        let after = app.prompt.editor().text().to_string();
        assert!(after.starts_with(&before));
        assert!(after.contains("hello"));
    }

    #[test]
    fn step_paste_returns_exit_when_exit_requested() {
        let mut app = make_app();
        app.request_exit();
        let outcome = app.step_paste("anything");
        assert_eq!(outcome, StepOutcome::Exit);
    }

    #[test]
    fn burst_plain_char_recognises_unshifted_text() {
        assert_eq!(App::burst_plain_char(char_key('a')), Some('a'));
        assert_eq!(
            App::burst_plain_char(Key::new(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            Some('A')
        );
    }

    #[test]
    fn burst_plain_char_rejects_chords_and_non_chars() {
        // Ctrl+letter: modifiers prevent it from counting as paste content.
        assert!(
            App::burst_plain_char(Key::new(KeyCode::Char('a'), KeyModifiers::CONTROL)).is_none()
        );
        // Enter, Esc, Tab, etc. are not plain chars.
        assert!(App::burst_plain_char(Key::new(KeyCode::Enter, KeyModifiers::NONE)).is_none());
        assert!(App::burst_plain_char(Key::new(KeyCode::Esc, KeyModifiers::NONE)).is_none());
        assert!(App::burst_plain_char(Key::new(KeyCode::Tab, KeyModifiers::NONE)).is_none());
    }

    #[test]
    fn burst_control_char_recognises_enter_and_tab() {
        assert_eq!(
            App::burst_control_char(Key::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some('\n')
        );
        assert_eq!(
            App::burst_control_char(Key::new(KeyCode::Tab, KeyModifiers::NONE)),
            Some('\t')
        );
        assert!(App::burst_control_char(char_key('a')).is_none());
        assert!(App::burst_control_char(Key::new(KeyCode::Esc, KeyModifiers::NONE)).is_none());
    }

    #[test]
    fn tick_paste_burst_is_a_noop_for_an_empty_draft() {
        let mut app = make_app();
        // No active burst, so ticking is a no-op and reports no change.
        let changed = app.tick_paste_burst(std::time::Instant::now());
        assert!(!changed);
    }
}
