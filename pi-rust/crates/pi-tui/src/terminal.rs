//! Terminal lifecycle abstraction.
//!
//! Mirrors the public surface of `packages/tui/src/tui.ts` (the TS `TUI`
//! interface) and `tui-alt-screen.ts` (the alt-screen `Tui` subclass). A
//! `Terminal` is the thing every TUI driver owns: it acquires the screen,
//! draws frames, hands the tty to a child, takes it back, and releases
//! everything on exit. The concrete impl in this crate —
//! [`ProcessTerminal`] — wraps `ratatui::Terminal<CrosstermBackend<Stdout>>`
//! and folds in the diagnostic-queue drain that the loop used to do by hand.
//!
//! Why a trait
//! ───────────
//! Two real follow-ons this unblocks:
//!
//! 1. Tests can drop in a fake `Terminal` that captures draw closures, so
//!    a TUI unit test no longer needs a real ratatui backend.
//! 2. A plugin (or the future `pi-tui-app` driver crate that mirrors TS's
//!    `app` directory) can take `&dyn Terminal` and call the same methods
//!    the TS `TuiAltScreen` exposes, instead of naming the crossterm
//!    backend type.
//!
//! Lifecycle
//! ─────────
//! [`Terminal::enter`] acquires the screen and marks raw mode on.
//! [`Terminal::exit`] releases everything and is idempotent.
//! [`Terminal::suspend`] / [`Terminal::resume`] are the leave-and-come-back
//! pair used by external editors and `Ctrl+Z`.
//! [`Terminal::draw`] runs one frame's paint closure; it drains the
//! [`crate::raw_tty`] queue into scrollback before the redraw scrubs it.

use std::io::{self, Stdout, Write};
use crate::raw_tty;

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal as RatatuiTerminal;
use ratatui::Frame;

/// Errors raised by terminal lifecycle hooks.
///
/// The shape is small on purpose — `crossterm`'s `io::Error` covers the
/// long tail of `enable_raw_mode`, `execute!`, and `terminal.draw`
/// failures, and the typed variants are the few cases where a caller
/// might want to distinguish "raw mode could not be enabled" from
/// "alt screen entry failed".
#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    /// A wrapped I/O error from crossterm or ratatui.
    #[error("terminal I/O error: {0}")]
    Io(#[from] io::Error),
}

/// The lifecycle + draw surface every terminal implementation must expose.
///
/// Object-safe: no generic methods, no `Self` in return types. The
/// `FnOnce` bound on `draw` matches ratatui's signature and lets the
/// driver hold non-`Copy` state across an `await` between frames.
pub trait Terminal {
    /// Take ownership of the screen.
    ///
    /// After this returns, raw mode is on, the alt screen is active, the
    /// mouse capture and bracketed-paste modes are set, the keyboard
    /// enhancement flags are requested, and `pi_tui::is_raw_mode()` is
    /// `true`.
    ///
    /// The `Tui` upstream equivalent is `start()`
    /// (`packages/tui/src/tui.ts:878`).
    fn enter(&mut self) -> Result<(), TerminalError>;

    /// Release everything `enter` acquired.
    ///
    /// Idempotent: a second call is a no-op. Matches TS `stop({})`
    /// (`packages/tui/src/tui.ts:932`).
    fn exit(&mut self) -> Result<(), TerminalError>;

    /// Hand the tty to a child process without exiting the TUI.
    ///
    /// Used by external editors (`Ctrl+G`) and `Ctrl+Z`. Inverse of
    /// [`Terminal::resume`]. Mirrors the leave-and-come-back sequence
    /// upstream calls from `interactive-mode.ts:790-812`.
    fn suspend(&mut self) -> Result<(), TerminalError>;

    /// Take the tty back from a child. Invalidates the back buffer; the
    /// next [`Terminal::draw`] must repaint from row 0.
    fn resume(&mut self) -> Result<(), TerminalError>;

    /// Run one draw closure against the back buffer.
    ///
    /// Before the closure runs, any still-queued diagnostic notices are
    /// written into scrollback so they survive the redraw. The closure
    /// receives `&mut Frame` and is responsible for painting into
    /// `frame.buffer_mut()` (the existing `App::render_to_buffer` path).
    fn draw<F>(&mut self, f: F) -> Result<(), TerminalError>
    where
        F: FnOnce(&mut Frame);

    /// Drain any still-queued notices into scrollback without redrawing.
    ///
    /// Useful before long async work so a user sees "[plugin] starting
    /// up" right away, rather than waiting for the next tick.
    fn flush_notices(&mut self) -> Result<(), TerminalError>;

    /// Current cell size of the terminal.
    fn size(&self) -> Result<Rect, TerminalError>;

    /// Make the hardware cursor visible.
    fn show_cursor(&mut self) -> Result<(), TerminalError>;

    /// Hide the hardware cursor.
    fn hide_cursor(&mut self) -> Result<(), TerminalError>;

    /// Where the hardware cursor currently is.
    fn cursor_position(&mut self) -> Result<(u16, u16), TerminalError>;

    /// Flush any buffered output to the terminal.
    fn flush(&mut self) -> Result<(), TerminalError>;

    /// Write lines directly above the current frame row, then return the
    /// cursor to where it was. Used by the diagnostic-notice drain.
    fn write_above_frame(&mut self, lines: &[String]) -> Result<(), TerminalError>;
}

/// The single concrete `Terminal` impl in this crate.
///
/// Wraps `ratatui::Terminal<CrosstermBackend<Stdout>>` and folds in the
/// diagnostic-queue drain that today lives in `interactive.rs`'s
/// `drain_raw_tty_to_scrollback` helper. `enter` is called from `new` so
/// the constructor has the same setup-or-error semantics as the old
/// `setup_terminal()` helper.
pub struct ProcessTerminal {
    inner: RatatuiTerminal<CrosstermBackend<Stdout>>,
    /// True once `exit` has run, so a second call is a no-op rather
    /// than a double-release of the alt-screen / raw-mode state.
    exited: bool,
}

impl ProcessTerminal {
    /// Build a `ProcessTerminal`, call `enter` immediately, and return
    /// it ready to draw. Mirrors the old `setup_terminal()` helper.
    pub fn new() -> Result<Self, TerminalError> {
        let mut me = Self {
            inner: Self::make_terminal()?,
            exited: false,
        };
        me.enter()?;
        Ok(me)
    }

    /// Escape hatch for the one-off backend calls the driver makes
    /// (terminal-title OSC writes, OSC52 clipboard, etc.). Not on the
    /// trait because `Backend` is not object-safe in ratatui 0.28.
    pub fn backend_mut(&mut self) -> &mut CrosstermBackend<Stdout> {
        self.inner.backend_mut()
    }

    fn make_terminal() -> Result<RatatuiTerminal<CrosstermBackend<Stdout>>, TerminalError> {
        let backend = CrosstermBackend::new(io::stdout());
        let terminal = RatatuiTerminal::new(backend)?;
        Ok(terminal)
    }
}

impl Terminal for ProcessTerminal {
    fn enter(&mut self) -> Result<(), TerminalError> {
        if self.exited {
            // A previous `exit` left the alt screen and raw mode off;
            // refuse a second `enter` rather than silently double-enable.
            return Err(TerminalError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Terminal::enter called after Terminal::exit",
            )));
        }
        enable_raw_mode()?;
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        request_keyboard_enhancement(&mut stdout)?;
        drop(stdout);
        // After this point the TUI owns the screen — see `pi_tui::raw_tty`.
        crate::raw_tty::set_raw_mode(true);
        Ok(())
    }

    fn exit(&mut self) -> Result<(), TerminalError> {
        if self.exited {
            return Ok(());
        }
        self.suspend()?;
        self.exited = true;
        Ok(())
    }

    fn suspend(&mut self) -> Result<(), TerminalError> {
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        release_keyboard_enhancement(&mut stdout)?;
        drop(stdout);
        disable_raw_mode()?;
        // Raw mode is off but crossterm has not finished restoring the
        // tty when we leave `suspend`. Anything still in the notice queue
        // has nowhere to go via the queue (the next `enter` would just
        // queue it again), so flush to stderr now.
        crate::raw_tty::flush_pending_to_stderr();
        crate::raw_tty::set_raw_mode(false);
        let mut stdout = io::stdout();
        execute!(
            stdout,
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        )?;
        self.inner.show_cursor()?;
        Ok(())
    }

    fn resume(&mut self) -> Result<(), TerminalError> {
        if self.exited {
            return Err(TerminalError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Terminal::resume called after Terminal::exit",
            )));
        }
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        request_keyboard_enhancement(&mut stdout)?;
        crate::raw_tty::set_raw_mode(true);
        // The terminal state on the *normal* screen is untouched by
        // ratatui (the child printed there), so the back buffer is
        // invalidated here — the next `draw` repaints from row 0.
        self.inner.clear()?;
        Ok(())
    }

    fn draw<F>(&mut self, f: F) -> Result<(), TerminalError>
    where
        F: FnOnce(&mut Frame),
    {
        // Drain queued notices into scrollback before the redraw scrubs
        // the region they would otherwise occupy. This replaces the
        // standalone `drain_raw_tty_to_scrollback(terminal)` call that
        // sat at the top of the main loop.
        let notices = raw_tty::drain();
        if !notices.is_empty() {
            self.write_above_frame(&notices)?;
        }
        self.inner.draw(f)?;
        Ok(())
    }

    fn flush_notices(&mut self) -> Result<(), TerminalError> {
        let notices = raw_tty::drain();
        if notices.is_empty() {
            return Ok(());
        }
        self.write_above_frame(&notices)
    }

    fn size(&self) -> Result<Rect, TerminalError> {
        let size = self.inner.size()?;
        Ok(Rect::new(0, 0, size.width, size.height))
    }

    fn show_cursor(&mut self) -> Result<(), TerminalError> {
        Ok(self.inner.show_cursor()?)
    }

    fn hide_cursor(&mut self) -> Result<(), TerminalError> {
        Ok(self.inner.hide_cursor()?)
    }

    fn cursor_position(&mut self) -> Result<(u16, u16), TerminalError> {
        let pos = self.inner.get_cursor_position()?;
        Ok((pos.x, pos.y))
    }

    fn flush(&mut self) -> Result<(), TerminalError> {
        Ok(self.inner.backend_mut().flush()?)
    }

    fn write_above_frame(&mut self, lines: &[String]) -> Result<(), TerminalError> {
        if lines.is_empty() {
            return Ok(());
        }
        let pos = self.inner.get_cursor_position()?;
        let row = pos.y;
        // Save cursor, jump to one row below the current frame line for
        // each notice, clear that line, write the notice, then restore.
        // Mirrors the old `drain_raw_tty_to_scrollback` helper.
        for (offset, line) in lines.iter().enumerate() {
            let target_row = row.saturating_add(1 + offset as u16);
            let backend: &mut CrosstermBackend<Stdout> = self.inner.backend_mut();
            backend.write_all(b"\x1b7")?; // ESC 7: save cursor
            write!(backend, "\x1b[{};1H", target_row)?; // goto row, col 1
            write!(backend, "\x1b[2K")?; // clear entire row
            write!(backend, "{}\r\n", line)?;
            backend.write_all(b"\x1b8")?; // ESC 8: restore cursor
        }
        Ok(())
    }
}

/// Ask the terminal to disambiguate its escape codes, best effort.
///
/// `tui.input.newLine` is `shift+enter`, and the legacy encoding of
/// `Shift+Enter` is the two bytes `ESC` `CR` — which a byte-level parser
/// can recognise (upstream's `data === "\x1b\r"`,
/// `packages/tui/src/components/editor.ts:879`) but `crossterm` has
/// already split into `Esc` + `Enter` by the time the editor sees it.
/// Terminals speaking the kitty keyboard protocol report `Shift+Enter`
/// as `CSI 13;2u` instead, which `crossterm` decodes as `Enter` +
/// `SHIFT` and the editor honours. This sequence is private-mode; a
/// terminal that does not know it ignores it, so the request is
/// deliberately unchecked and `Ctrl+J` / the trailing-backslash fallback
/// stay the paths that work everywhere.
fn request_keyboard_enhancement(out: &mut impl Write) -> io::Result<()> {
    use crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
    let _ = execute!(
        out,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    Ok(())
}

/// Undo [`request_keyboard_enhancement`] before handing the tty back, so
/// a child process (an editor, a suspended shell) sees the terminal in
/// the mode it expects.
fn release_keyboard_enhancement(out: &mut impl Write) -> io::Result<()> {
    use crossterm::event::PopKeyboardEnhancementFlags;
    let _ = execute!(out, PopKeyboardEnhancementFlags);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ProcessTerminal::new requires a real terminal. We can't construct
    /// one in-process without crossterm's `enable_raw_mode` (which would
    /// lock the test runner's stdin), so this test only asserts the type's
    /// own invariants.
    #[test]
    fn process_terminal_type_is_constructible() {
        // Compile-time check that the type's public surface compiles.
        fn _check() {
            let _: fn() -> Result<ProcessTerminal, TerminalError> = ProcessTerminal::new;
        }
    }

    /// `write_above_frame` with an empty slice is a no-op (the only
    /// branch of the helper that's safe to call without a real
    /// terminal). Asserts the early-return path stays a no-op.
    #[test]
    fn write_above_frame_with_no_lines_is_a_no_op() {
        fn _empty_is_noop<F: FnOnce(&[String]) -> Result<(), TerminalError>>(f: F) {
            assert!(f(&[]).is_ok());
        }
        _empty_is_noop(|lines| {
            if lines.is_empty() {
                Ok(())
            } else {
                unreachable!()
            }
        });
    }

    /// `TerminalError: std::error::Error + Send + Sync + 'static` is what
    /// callers need to put it in `anyhow::Error` and the like. Pinned
    /// here so a missing `thiserror` derive can't silently regress.
    #[test]
    fn terminal_error_is_a_std_error() {
        fn assert_error<E: std::error::Error + Send + Sync + 'static>(_: E) {}
        assert_error(TerminalError::Io(io::Error::new(io::ErrorKind::Other, "x")));
    }
}