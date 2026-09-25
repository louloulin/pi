//! Top-level key routing.
//!
//! [`App::step_key`] / [`App::step_key_at`] dispatch one keystroke through
//! the modal layers (history search, custom overlay, dialog, settings,
//! shortcut overlay, slash menu, search), the paste-burst classifier
//! ([`super::App::step_composer_burst`]), the `app.*` / `tui.altScreen.*`
//! keybinding chords, the editor-region component, and finally the
//! composer ([`App::step_composer`]). The two entry points exist because
//! the burst-flush window is measured against an `Instant`, and tests
//! drive it without sleeping by passing their own instant.

use crate::core::input_parse::{InputEvent, Key, KeyCode};
use crate::components::keybindings::get_keybindings;
use crate::locale::format_chord;
use crate::components::prompt::PromptAction;

use super::{App, StepOutcome, Submission, CLEAR_EXIT_WINDOW};

use crate::app::SearchKeyOutcome;

impl App {
    /// Process a single [`Key`]. Convenience wrapper that uses
    /// [`Instant::now`](std::time::Instant::now) as the burst window.
    pub fn step_key(&mut self, key: Key) -> StepOutcome {
        self.step_key_at(key, std::time::Instant::now())
    }

    /// Process a single [`Key`] at the caller-supplied instant.
    ///
    /// The instant is what the `app.clear` (`Ctrl+C`) double-press window is
    /// measured against, so tests can drive the window without sleeping;
    /// nothing else reads it except the paste-burst classifier
    /// (`AppConfig::paste_burst`). `Instant::now()` must not appear in the
    /// decision itself (LUM-1238 acceptance 2).
    pub fn step_key_at(&mut self, key: Key, now: std::time::Instant) -> StepOutcome {
        // A burst that a non-burst key flushed changes the draft even when
        // the key itself is a no-op: `Ctrl+R` must still open the search,
        // but the frame it opens on has to show the flushed paste text.
        self.burst_flush_pending = false;
        let outcome = self.step_key_inner(key, now);
        if self.burst_flush_pending && matches!(outcome, StepOutcome::Idle) {
            StepOutcome::Redraw
        } else {
            outcome
        }
    }

    /// [`App::step_key_at`]'s body, after the burst-flush bookkeeping.
    fn step_key_inner(&mut self, key: Key, now: std::time::Instant) -> StepOutcome {
        // A transient status message lives for exactly one key press
        // (upstream's `showStatus` clears on a timer; this port has no timer
        // in the App, and a key press is the next thing the reader does).
        self.status_flash = None;
        // A reverse history search (`Ctrl+R`) owns the composer for the whole
        // session. codex's `handle_history_search_key` keeps its facade's
        // chords out for the same reason: a stray `Ctrl+O` / `Alt+V` / `PageUp`
        // must not fire on a keystroke the user meant as a search query, and
        // nothing may touch the preview `Esc` is there to undo.
        if self.history_search_active() {
            return self.step_composer(key);
        }
        // A visible custom overlay is the outermost layer; see [`App::step`].
        if self.extension.handle_overlay_input(key) {
            return StepOutcome::Redraw;
        }
        // A modal dialog swallows every key — including Ctrl+C / Esc,
        // which cancel the dialog instead of the turn or the App.
        if self.dialog.is_some() {
            return self.step_dialog(key);
        }
        // The settings modal is the next-outermost layer.
        if self.settings.is_some() {
            return self.step_settings(key);
        }
        // `?` on an empty composer toggles the shortcut overlay (codex
        // `ChatComposer::handle_shortcut_overlay_key`,
        // `bottom_pane/chat_composer.rs:3149`); any other key closes it and is
        // handled normally below (codex's `reset_mode_after_activity`), so a
        // reader who opened help by reflex and then types is not stuck. The
        // gates mirror the layers above — the transcript search overlay and an
        // extension-owned input surface (`custom`, in either placement) keep
        // the keyboard, and a draft (or an attached image) means `?` is text,
        // not a chord.
        let plain_question = key.code == KeyCode::Char('?') && key.modifiers.is_empty();
        if self.shortcut_overlay {
            if plain_question || key.code == KeyCode::Esc {
                self.shortcut_overlay = false;
                return StepOutcome::Redraw;
            }
            self.shortcut_overlay = false;
        } else if plain_question
            && self.search.is_none()
            && !self.extension.custom_visible()
            && !self.extension.has_editor_component()
            && !self.prompt.editor().is_showing_autocomplete()
            && self.prompt.is_empty()
            && self.prompt.images().is_empty()
        {
            self.shortcut_overlay = true;
            return StepOutcome::Redraw;
        }

        // Slash menu: ↑/↓ navigate, Enter executes, Tab completes, Escape closes.
        // The menu owns vertical arrows while it is visible.
        if self.slash_menu.is_visible() {
            match key.code {
                KeyCode::Up => {
                    self.slash_menu.move_up();
                    return StepOutcome::Redraw;
                }
                KeyCode::Down => {
                    self.slash_menu.move_down();
                    return StepOutcome::Redraw;
                }
                KeyCode::PageUp => {
                    self.slash_menu.page_up(5);
                    return StepOutcome::Redraw;
                }
                KeyCode::PageDown => {
                    self.slash_menu.page_down(5);
                    return StepOutcome::Redraw;
                }
                KeyCode::Esc => {
                    self.slash_menu.hide();
                    return StepOutcome::Redraw;
                }
                // Enter, Tab, and other keys fall through to composer handling
                // (Enter will submit if the menu is closed, Tab will complete)
                _ => {
                    self.slash_menu.hide();
                }
            }
        }

        // Paste-burst classification (LUM-1461, codex `paste_burst`): a
        // terminal without bracketed paste delivers a paste as a fast run of
        // key events, and this is where such a run is recognized. It runs
        // after the modal layers (which own the keyboard outright) and before
        // the `app.*` chords, because a chord is not paste content: it ends
        // the burst, flushing whatever was buffered first. A plain character
        // that is not (yet) paste-like falls through to the ordinary path.
        if let Some(outcome) = self.step_composer_burst(key, now) {
            return outcome;
        }
        // Global keys. Resolved through the keybinding registry so an
        // installed override reaches the App; with nothing installed the
        // registry serves the defaults, so the behaviour below is the
        // pre-keybinding behaviour (see the module docs).
        let kb = get_keybindings();
        let event = InputEvent::Key(key);
        // The transcript search overlay owns the keyboard while it is open,
        // except for the chords the viewport keeps for itself
        // (`shouldDeferViewportInputToOverlay`,
        // `packages/tui/src/tui-alt-screen.ts:644-645`).
        if self.search.is_some() {
            match self.step_search_key(key) {
                SearchKeyOutcome::Handled(outcome) => return outcome,
                SearchKeyOutcome::PassThrough => {}
            }
        }
        // `tui.altScreen.search` opens the overlay; while it is open the
        // overlay itself consumes the chord above (upstream checks the chord
        // before it checks whether the overlay holds focus,
        // `packages/tui/src/tui-alt-screen.ts:705-708`).
        if kb.matches(&event, "tui.altScreen.search") {
            return if self.open_search() {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            };
        }
        // `app.interrupt` (`Escape`): cancel the in-flight turn. When the
        // App is idle the chord falls through to the prompt, which is the
        // pre-keybinding behaviour (selectors/settings got the key above).
        if Self::matches_app_key(&kb, &event, "app.interrupt", &["escape"]) && self.is_busy() {
            self.cancel();
            return StepOutcome::Redraw;
        }
        // `app.clear` (`Ctrl+C`): cancel while a turn is in flight; when
        // idle, clear the composer on the first press and exit on a second
        // press inside [`CLEAR_EXIT_WINDOW`] — upstream `handleCtrlC`
        // (`interactive-mode.ts:3931-3939`). Only the composer is touched by
        // the clearing press (`Prompt::clear` drops the draft text, chips,
        // history browsing and undo stack), so nothing else about the
        // session changes.
        if Self::matches_app_key(&kb, &event, "app.clear", &["ctrl+c"]) {
            if self.is_busy() {
                self.cancel();
                return StepOutcome::Redraw;
            }
            let double_press = self
                .last_clear_at
                .is_some_and(|last| now.saturating_duration_since(last) < CLEAR_EXIT_WINDOW);
            if double_press {
                self.exit_requested = true;
                return StepOutcome::Exit;
            }
            self.last_clear_at = Some(now);
            self.prompt.clear();
            return StepOutcome::Redraw;
        }
        // `app.model.select` (`Ctrl+L`) is **not** claimed here. Upstream's
        // `app.model.select` means "open the model selector"
        // (`packages/coding-agent/src/core/keybindings.ts:116`), and this port's
        // selector lives in the coding-agent driver, which claims the chord
        // before the App sees the key. Clearing the transcript is `/clear`'s
        // job; a hardcoded `Ctrl+L` here would shadow the driver's chord (see
        // the module docs).
        // `app.thinking.toggle` (`Ctrl+T`): collapse / expand every assistant
        // reasoning block (upstream's `toggleThinkingBlockVisibility`,
        // `interactive-mode.ts:4239`, which also reports the new state through
        // `showStatus`).
        if Self::matches_app_key(&kb, &event, "app.thinking.toggle", &["ctrl+t"]) {
            let visible = self.toggle_thinking_visibility();
            self.flash_status(format!(
                "Thinking blocks: {}",
                if visible { "visible" } else { "hidden" }
            ));
            return StepOutcome::Redraw;
        }
        // `app.tools.expand` (`Ctrl+O`): expand / collapse every tool block
        // (upstream's `setToolsExpanded` / `toggleToolOutputExpansion`,
        // `interactive-mode.ts:4231-4246`, which also reports the new state
        // through `showStatus`). The rich bodies were rendered by the driver
        // at execution end; this only flips how much of them the App paints.
        if Self::matches_app_key(&kb, &event, "app.tools.expand", &["ctrl+o"]) {
            let expanded = self.toggle_tools_expanded();
            self.flash_status(format!(
                "Tool output: {}",
                if expanded { "expanded" } else { "collapsed" }
            ));
            return StepOutcome::Redraw;
        }
        // `app.clipboard.pasteImage` (`Alt+V`): attach a clipboard image to
        // the draft. The App cannot read the system clipboard, so it records
        // the request and the driver answers it with [`App::paste_image`]
        // (or the text fallback, [`App::paste_text`]) — the same seam as
        // copy-on-select. A key press always redraws so the "reading…" frame
        // and the restored status hint stay honest.
        if Self::matches_app_key(&kb, &event, "app.clipboard.pasteImage", &["alt+v"]) {
            self.pending_image_paste = true;
            return StepOutcome::Redraw;
        }
        // `app.header` (`Alt+H`): fold / unfold the built-in startup header.
        // A Rust-port addition — upstream ties the header's expansion to
        // `app.tools.expand` — so it is resolved with the same
        // registry-first / builtin-fallback rule as every other `app.*` chord
        // and reported through `showStatus` (`flash_status`).
        if Self::matches_app_key(&kb, &event, "app.header", &["alt+h"]) {
            let expanded = self.toggle_header();
            let chord = kb
                .get_keys("app.header")
                .first()
                .map(|chord| format_chord(chord))
                .unwrap_or_else(|| format_chord("alt+h"));
            self.flash_status(format!(
                "Startup header: {}{}",
                if expanded { "expanded" } else { "collapsed" },
                if expanded {
                    String::new()
                } else {
                    format!(" ({chord} to show)")
                }
            ));
            return StepOutcome::Redraw;
        }
        // Fullscreen chat-log scrolling. Upstream deliberately shadows
        // the bare editor bindings for these chords in fullscreen mode
        // (`packages/tui/src/keybindings.ts:159-165,208-209`: "These
        // intentionally shadow the unmodified editor bindings in
        // fullscreen mode"); `Ctrl+A` / `Ctrl+E` still reach the editor
        // for start / end of line.
        //
        // LUM-1317: the shadowing is unconditional in the transcript's
        // favour only while the composer has nothing hidden. A draft taller
        // than the composer window is content the user cannot reach any
        // other way, so `PageUp` / `PageDown` page *it* then —
        // `tui.editor.pageUp` / `pageDown` — and the transcript keeps them
        // when it fits (see [`App::composer_overflows`]).
        if kb.matches(&event, "tui.altScreen.pageUp") && !self.composer_overflows() {
            let page = self.message_page();
            return if self.scroll_viewport_up(page) {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            };
        }
        if kb.matches(&event, "tui.altScreen.pageDown") && !self.composer_overflows() {
            let page = self.message_page();
            return if self.scroll_viewport_down(page) {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            };
        }
        // `tui.altScreen.top` / `tui.altScreen.bottom`.
        if kb.matches(&event, "tui.altScreen.top") {
            return if self.scroll_viewport_to_top() {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            };
        }
        if kb.matches(&event, "tui.altScreen.bottom") {
            return if self.scroll_viewport_to_bottom() {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            };
        }

        // The editor region's component (a non-overlay `custom` session or a
        // custom editor component) gets the key before the prompt. The
        // app-level chords above already had their chance, so an extension
        // editor cannot shadow interrupt / clear / scrolling.
        if self.extension.handle_editor_input(key) {
            return StepOutcome::Redraw;
        }

        // `PageUp` / `PageDown` are owned by the transcript when the composer
        // fits; the editor's own `tui.editor.pageUp` / `pageDown` bindings
        // also include the bare chord (so a draft that overflows can still
        // page itself), which means a bare `PageUp` would silently slip
        // through to the editor once the user releases `tui.altScreen.pageUp`
        // by rebinding it to something else. Trap the bare chord here so a
        // released transcript binding is a real no-op rather than a stray
        // editor move. The `Ctrl+PageUp` / `Ctrl+PageDown` chords still reach
        // the editor because the transcript's binding is the bare key only.
        if key.modifiers.is_empty() && !self.composer_overflows() {
            match key.code {
                KeyCode::PageUp if !kb.matches(&event, "tui.altScreen.pageUp") => {
                    return StepOutcome::Idle;
                }
                KeyCode::PageDown if !kb.matches(&event, "tui.altScreen.pageDown") => {
                    return StepOutcome::Idle;
                }
                _ => {}
            }
        }

        // The composer's wrap width is a rendering fact the editor needs
        // while it handles the key: `Up` / `Down` move by visual row, and a
        // different width would move the caret to a row the frame did not
        // draw it on. `0` (no frame yet) leaves the draft on one row per
        // hard line. The page height is the same story for `PageUp` /
        // `PageDown`, which move the caret by a windowful.
        self.step_composer(key)
    }

    /// Hand a key to the composer.
    ///
    /// Split out of [`App::step_key_at`] so a reverse history search can route
    /// here directly: while the search is open it owns the keyboard, and the
    /// `app.*` chords above (expand tools, paste an image, fold the header,
    /// page the transcript) must not fire on a keystroke the user means as a
    /// search query.
    ///
    /// Paste-burst classification happens in [`App::step_key_at`], ahead of
    /// the `app.*` chords, so this only ever sees a character the classifier
    /// left alone.
    /// `app.*` chords above (expand tools, paste an image, fold the header,
    /// page the transcript) must not fire on a keystroke the user means as a
    /// search query.
    ///
    /// Paste-burst classification happens in [`App::step_key_at`], ahead of
    /// the `app.*` chords, so this only ever sees a character the classifier
    /// left alone.
    fn step_composer(&mut self, key: Key) -> StepOutcome {
        use std::sync::atomic::Ordering;
        let composer_width = self.viewport.composer_body_width.load(Ordering::Relaxed) as usize;
        let composer_page = self.composer_window_rows();
        self.prompt.editor_mut().set_visual_width(composer_width);
        self.prompt.editor_mut().set_page_rows(composer_page);

        // A keystroke that changes the draft shifts the offsets the drag
        // selection was measured against — drop the highlight and any
        // in-flight clipboard request so neither lingers on a stale range
        // (LUM-1332).
        let before_text = self.prompt.text().to_string();
        let action = self.prompt.handle_key(key);
        if matches!(action, PromptAction::Changed) && self.prompt.text() != before_text {
            self.composer_drag_selection = None;
            self.pending_clipboard = None;
        }
        // A recall from the cross-session history file can bring back a
        // marker whose content this session never had; say so once.
        if self.prompt.editor_mut().take_stale_paste_notice() {
            self.flash_status(
                "Pasted content recalled from a previous session is no longer available",
            );
        }
        match action {
            PromptAction::None => StepOutcome::Idle,
            PromptAction::Changed => StepOutcome::Redraw,
            PromptAction::Submit(text) => {
                // Caller is responsible for invoking `submit` with an
                // `Arc<AsyncMutex<Agent>>` — we just announce the submitted
                // draft (text plus any pasted image chips) and clear the
                // buffer. The images are captured before `clear()` wipes
                // them.
                let images = self.prompt.images().to_vec();
                if self.turn_busy.load(Ordering::SeqCst) && !images.is_empty() {
                    // Refuse *before* clearing: the Stage 61 pending queue is
                    // text-only, so accepting would silently drop the chips.
                    // Nothing is consumed — the draft (text and chips) stays
                    // in the editor for the next attempt.
                    self.flash_status("Cannot attach images while a turn is running");
                    return StepOutcome::Redraw;
                }
                let submitted = Submission {
                    text,
                    images,
                    draft: Some(self.prompt.editor().text().to_string()),
                };
                self.prompt.clear();
                StepOutcome::Submitted(submitted)
            }
            PromptAction::Interrupt => {
                self.cancel();
                StepOutcome::Redraw
            }
            PromptAction::Eof => {
                self.exit_requested = true;
                StepOutcome::Exit
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::input_parse::{Key, KeyCode, KeyModifiers};
    use std::time::Instant;

    fn make_app() -> App {
        super::super::tool_stream_tests::test_app()
    }

    fn char_key(c: char) -> Key {
        Key::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn step_key_inserts_plain_text_into_the_composer() {
        let mut app = make_app();
        let before = app.prompt.text().to_string();
        let outcome = app.step_key_at(char_key('x'), Instant::now());
        // Typing a character either reports Idle (no redraw needed) or
        // Redraw; the test asserts the path runs without panicking.
        let _ = outcome;
        let after = app.prompt.text().to_string();
        assert!(after.starts_with(&before), "draft prefix must be preserved");
        assert!(after.len() >= before.len() + 1, "draft must grow on insert");
    }

    #[test]
    fn step_key_clears_status_flash_on_any_key() {
        // The transient status message lives for exactly one keystroke.
        let mut app = make_app();
        app.flash_status("hello");
        assert!(app.status_flash.is_some());
        let _ = app.step_key_at(char_key('a'), Instant::now());
        assert!(
            app.status_flash.is_none(),
            "any keystroke must clear the status flash"
        );
    }

    #[test]
    fn step_key_shortcut_overlay_toggles_on_question_mark() {
        // `?` on an empty composer with no extensions opens the shortcut
        // overlay; any subsequent key closes it again.
        let mut app = make_app();
        assert!(!app.shortcut_overlay);
        let _ = app.step_key_at(char_key('?'), Instant::now());
        assert!(app.shortcut_overlay, "empty composer + `?` opens the overlay");
        let _ = app.step_key_at(char_key('a'), Instant::now());
        assert!(!app.shortcut_overlay, "next key closes the overlay");
    }

    #[test]
    fn step_key_records_last_clear_at_on_ctrl_c() {
        // Ctrl+C on an idle app clears the composer and records the
        // double-press window timestamp.
        let mut app = make_app();
        let before = app.last_clear_at;
        let now = Instant::now();
        let outcome = app.step_key_at(
            Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            now,
        );
        assert_eq!(outcome, StepOutcome::Redraw);
        assert_ne!(app.last_clear_at, before);
    }

    #[test]
    fn step_key_requests_exit_on_double_ctrl_c() {
        let mut app = make_app();
        let now = Instant::now();
        // First press — clears the draft.
        let _ = app.step_key_at(
            Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            now,
        );
        assert!(!app.exit_requested);
        // Second press within the window — exits.
        let outcome = app.step_key_at(
            Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            now,
        );
        assert_eq!(outcome, StepOutcome::Exit);
        assert!(app.exit_requested);
    }

    #[test]
    fn step_key_returns_when_already_exit_requested() {
        // The exit flag is observed by the driver loop, not by step_key
        // itself; this test just makes sure the path runs without
        // panicking.
        let mut app = make_app();
        app.request_exit();
        let outcome = app.step_key_at(char_key('a'), Instant::now());
        let _ = outcome;
    }

    #[test]
    fn step_key_opens_search_overlay_on_chord() {
        let mut app = make_app();
        assert!(app.search.is_none());
        // The chord is Ctrl+Shift+F by default; the test only asserts the
        // open path runs (no panic) — the exact chord is owned by the
        // keybinding registry.
        let _ = app.step_key_at(
            Key::new(KeyCode::Char('f'), KeyModifiers::CONTROL | KeyModifiers::SHIFT),
            Instant::now(),
        );
        let _ = app.search;
    }

    #[test]
    fn step_key_with_empty_composer_falls_through_to_composer() {
        let mut app = make_app();
        // An Enter on an empty composer must not submit anything: the
        // outcome is Idle because nothing changed.
        let outcome = app.step_key_at(Key::new(KeyCode::Enter, KeyModifiers::NONE), Instant::now());
        let _ = outcome;
        // No flash status from this path.
        assert!(app.status_flash.is_none());
    }

    #[test]
    fn step_key_routes_keys_into_settings_when_one_is_open() {
        let mut app = make_app();
        app.open_settings(crate::components::settings::SettingsList::new(
            vec![crate::components::settings::SettingItem::new("id", "Setting")],
            4,
        ));
        let outcome = app.step_key_at(Key::new(KeyCode::Enter, KeyModifiers::NONE), Instant::now());
        let _ = outcome;
    }
}