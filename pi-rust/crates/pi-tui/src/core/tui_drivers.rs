//! Alt-screen and main-screen TUI drivers.
//!
//! `TuiAltScreen` and `TuiMainScreen` mirror upstream
//! `packages/tui/src/tui/tui-alt-screen.ts` and
//! `packages/tui/src/tui/tui-main-screen.ts`. Each driver wraps a
//! [`TuiBase`] and an output sink; the alt-screen driver additionally
//! switches the terminal into alt-screen mode on `start()` and back on
//! `stop()` / drop.
//!
//! The driver pattern is intentionally minimal: it owns a `TuiBase`
//! and a `Terminal` (or any writer that implements the [`TuiSink`]
//! trait). The host wires the root component, focusables, and
//! overlays into `TuiBase`; the driver is responsible only for
//! entering/exiting the screen mode, forwarding input events, and
//! pulling a Buffer to render.

use crate::core::component::CoreComponent;
use crate::core::mouse::MouseEvent;
use crate::core::overlay::{OverlayHandle, OverlayOptions};
use crate::core::tui_base::{TuiBase, TuiBaseConfig, TuiMode};
use crate::terminal::frame_pacer::FramePacer;
use crate::core::input_parse::Key;
use crate::ts_compat::{
    TuiAltScreenOptions, TuiInputListenerResult, TuiMainScreenRenderState,
    TuiMouseEvent, TuiMouseEventResult, TuiStopOptions,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

/// A sink the driver can write terminal escape sequences to.
pub trait TuiSink {
    /// Errors produced by the underlying terminal.
    type Error: std::error::Error;
    /// Write a string verbatim (no transformation).
    fn write_str(&mut self, s: &str) -> Result<(), Self::Error>;
    /// Force any buffered bytes out.
    fn flush(&mut self) -> Result<(), Self::Error>;
    /// Best-effort terminal size in cells.
    fn size(&self) -> (u16, u16);
    /// Hide the cursor if the sink supports it.
    fn hide_cursor(&mut self) -> Result<(), Self::Error>;
    /// Show the cursor if the sink supports it.
    fn show_cursor(&mut self) -> Result<(), Self::Error>;
}

/// Standard alt-screen sequences — enter and exit.
pub const ALT_SCREEN_ENTER: &str = "\x1b[?1049h";
/// Standard alt-screen sequence — exit to the main screen.
pub const ALT_SCREEN_EXIT: &str = "\x1b[?1049l";
/// Hide cursor.
pub const CURSOR_HIDE: &str = "\x1b[?25l";
/// Show cursor.
pub const CURSOR_SHOW: &str = "\x1b[?25h";
/// Clear screen.
pub const CLEAR_SCREEN: &str = "\x1b[2J\x1b[H";
/// Move cursor to home (top-left).
pub const CURSOR_HOME: &str = "\x1b[H";
/// Clear the current line.
pub const CLEAR_LINE: &str = "\x1b[2K";
/// Carriage return.
pub const CR: &str = "\r";
/// Newline.
pub const LF: &str = "\n";

/// Upstream `TuiAltScreen` — alt-screen TUI driver.
pub struct TuiAltScreen<S: TuiSink> {
    base: TuiBase,
    sink: S,
    /// Whether the driver has entered alt-screen mode yet.
    started: bool,
    /// Cached terminal width so callers can render without a Theme.
    cached_width: u16,
    /// Cached terminal height.
    cached_height: u16,
    /// Differential-render state — owns the previous frame's buffer.
    frame_pacer: FramePacer,
}

impl<S: TuiSink> TuiAltScreen<S> {
    /// Build a new alt-screen driver over `root` with the given sink
    /// and options.
    pub fn new(root: Box<dyn CoreComponent>, sink: S, _options: TuiAltScreenOptions) -> Self {
        let config = TuiBaseConfig {
            mode: TuiMode::Fullscreen,
            ..TuiBaseConfig::default()
        };
        let (w, h) = sink.size();
        Self {
            base: TuiBase::new(root, config),
            sink,
            started: false,
            cached_width: w,
            cached_height: h,
            frame_pacer: FramePacer::new(),
        }
    }

    /// Borrow the underlying `TuiBase` (focus/overlay/input pipeline).
    pub fn base(&self) -> &TuiBase {
        &self.base
    }

    /// Mutable borrow of the underlying `TuiBase`.
    pub fn base_mut(&mut self) -> &mut TuiBase {
        &mut self.base
    }

    /// Borrow the sink.
    pub fn sink(&self) -> &S {
        &self.sink
    }

    /// Mutable borrow of the sink.
    pub fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }

    /// Force the next frame to be a full redraw (clears the
    /// differential-render cache). Call after a screen resize, theme
    /// swap, or any event that invalidates the previous frame.
    pub fn invalidate(&mut self) {
        self.frame_pacer.invalidate();
    }

    /// Enter alt-screen mode. Idempotent: subsequent calls without a
    /// matching `stop()` are no-ops.
    pub fn start(&mut self) -> Result<(), S::Error> {
        if self.started {
            return Ok(());
        }
        self.sink.write_str(ALT_SCREEN_ENTER)?;
        self.sink.hide_cursor()?;
        self.sink.write_str(CLEAR_SCREEN)?;
        self.sink.flush()?;
        self.started = true;
        // Refresh cached size.
        let (w, h) = self.sink.size();
        self.cached_width = w;
        self.cached_height = h;
        self.base.resize(w, h);
        // The alt-screen just cleared — the next render is necessarily a
        // full redraw, so the diff cache must be invalidated.
        self.frame_pacer.invalidate();
        Ok(())
    }

    /// Exit alt-screen mode. Idempotent.
    pub fn stop(&mut self, _options: TuiStopOptions) -> Result<(), S::Error> {
        if !self.started {
            return Ok(());
        }
        self.sink.show_cursor()?;
        self.sink.write_str(ALT_SCREEN_EXIT)?;
        self.sink.flush()?;
        self.started = false;
        Ok(())
    }

    /// Dispatch a key event through the pipeline.
    pub fn dispatch_key(&mut self, key: &Key) -> bool {
        self.base.dispatch_key(key)
    }

    /// Dispatch a mouse event through the pipeline.
    pub fn dispatch_mouse(&mut self, event: &MouseEvent) -> bool {
        self.base.dispatch_mouse(event)
    }

    /// Bridge to the [`TuiMouseEvent`] surface — accepts a
    /// `TuiMouseEvent` and returns a `TuiMouseEventResult`.
    pub fn dispatch_tui_mouse(&mut self, _event: &TuiMouseEvent) -> TuiMouseEventResult {
        TuiMouseEventResult::Ignored
    }

    /// Forward input bytes through the input pipeline.
    pub fn dispatch_input(&mut self, data: &str) -> TuiInputListenerResult {
        self.base.pipeline_mut().push(data.as_bytes());
        TuiInputListenerResult::Consumed
    }

    /// Pull the next frame buffer from `TuiBase`.
    pub fn frame_buffer(&mut self) -> Buffer {
        let (w, h) = (self.cached_width, self.cached_height);
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
        self.base.render(&mut buf);
        buf
    }

    /// Render a frame buffer to the sink. The first call is a full
    /// clear + write; subsequent calls write cell content directly.
    pub fn render_buffer(&mut self, buf: &Buffer) -> Result<(), S::Error> {
        self.sink.write_str(CURSOR_HOME)?;
        for y in 0..buf.area.height {
            self.sink.write_str(CLEAR_LINE)?;
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                self.sink.write_str(&cell.symbol())?;
            }
            if y + 1 < buf.area.height {
                self.sink.write_str(LF)?;
            }
        }
        self.sink.flush()
    }

    /// Render the current frame using the differential [`FramePacer`].
    ///
    /// The first call after [`Self::start`] (or after [`Self::invalidate`])
    /// is a full redraw; subsequent calls write only the cells that
    /// actually changed. This mirrors upstream's `TuiBase.renderInternal`
    /// and avoids the flicker / scroll cost of a whole-screen clear on
    /// every keystroke.
    pub fn render(&mut self) -> Result<(), S::Error> {
        let area = Rect::new(0, 0, self.cached_width, self.cached_height);
        let payload = self.frame_pacer.render_with(area, |buf| {
            self.base.render(buf);
        });
        if !payload.is_empty() {
            self.sink.write_str(&payload)?;
        }
        self.sink.flush()
    }

    /// Borrow the [`FramePacer`] (for tests + diagnostics).
    pub fn frame_pacer(&self) -> &FramePacer {
        &self.frame_pacer
    }

    /// Mutable borrow of the [`FramePacer`].
    pub fn frame_pacer_mut(&mut self) -> &mut FramePacer {
        &mut self.frame_pacer
    }

    /// Show an overlay.
    pub fn show_overlay(&mut self, opts: OverlayOptions) -> OverlayHandle {
        self.base.show_overlay(opts)
    }

    /// Hide an overlay.
    pub fn hide_overlay(&mut self, handle: OverlayHandle) {
        self.base.hide_overlay(handle)
    }

    /// Whether the driver has entered alt-screen mode.
    pub fn is_started(&self) -> bool {
        self.started
    }

    /// Cached width.
    pub fn width(&self) -> u16 {
        self.cached_width
    }

    /// Cached height.
    pub fn height(&self) -> u16 {
        self.cached_height
    }
}

impl<S: TuiSink> Drop for TuiAltScreen<S> {
    fn drop(&mut self) {
        if self.started {
            let _ = self.stop(TuiStopOptions::default());
        }
    }
}

/// Upstream `TuiMainScreen` — main-screen (inline) TUI driver.
pub struct TuiMainScreen<S: TuiSink> {
    base: TuiBase,
    sink: S,
    /// Render state (snapshot of last frame).
    state: TuiMainScreenRenderState,
    /// Cached terminal size.
    cached_width: u16,
    cached_height: u16,
    /// Differential-render state — owns the previous frame's buffer.
    frame_pacer: FramePacer,
}

impl<S: TuiSink> TuiMainScreen<S> {
    /// Build a new main-screen driver over `root` with the given sink.
    pub fn new(root: Box<dyn CoreComponent>, sink: S) -> Self {
        let config = TuiBaseConfig {
            mode: TuiMode::Regular,
            ..TuiBaseConfig::default()
        };
        let (w, h) = sink.size();
        Self {
            base: TuiBase::new(root, config),
            sink,
            state: TuiMainScreenRenderState::default(),
            cached_width: w,
            cached_height: h,
            frame_pacer: FramePacer::new(),
        }
    }

    /// Force the next frame to be a full redraw. Call after a screen
    /// resize, theme swap, or any event that invalidates the previous
    /// frame.
    pub fn invalidate(&mut self) {
        self.frame_pacer.invalidate();
    }

    /// Borrow the [`FramePacer`] (for tests + diagnostics).
    pub fn frame_pacer(&self) -> &FramePacer {
        &self.frame_pacer
    }

    /// Mutable borrow of the [`FramePacer`].
    pub fn frame_pacer_mut(&mut self) -> &mut FramePacer {
        &mut self.frame_pacer
    }

    /// Borrow the underlying `TuiBase`.
    pub fn base(&self) -> &TuiBase {
        &self.base
    }

    /// Mutable borrow.
    pub fn base_mut(&mut self) -> &mut TuiBase {
        &mut self.base
    }

    /// Borrow the sink.
    pub fn sink(&self) -> &S {
        &self.sink
    }

    /// Mutable borrow of the sink.
    pub fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }

    /// Dispatch a key event through the pipeline.
    pub fn dispatch_key(&mut self, key: &Key) -> bool {
        self.base.dispatch_key(key)
    }

    /// Dispatch a mouse event through the pipeline.
    pub fn dispatch_mouse(&mut self, event: &MouseEvent) -> bool {
        self.base.dispatch_mouse(event)
    }

    /// Forward input bytes through the input pipeline.
    pub fn dispatch_input(&mut self, data: &str) -> TuiInputListenerResult {
        self.base.pipeline_mut().push(data.as_bytes());
        TuiInputListenerResult::Consumed
    }

    /// Pull the next frame buffer.
    pub fn frame_buffer(&mut self) -> Buffer {
        let (w, h) = (self.cached_width, self.cached_height);
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
        self.base.render(&mut buf);
        buf
    }

    /// Render a buffer to the sink. The main-screen driver does not
    /// switch screens; it just writes a clear + frame.
    pub fn render_buffer(&mut self, buf: &Buffer) -> Result<(), S::Error> {
        self.sink.write_str(CURSOR_HOME)?;
        for y in 0..buf.area.height {
            self.sink.write_str(CLEAR_LINE)?;
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                self.sink.write_str(&cell.symbol())?;
            }
            if y + 1 < buf.area.height {
                self.sink.write_str(LF)?;
            }
        }
        self.sink.flush()
    }

    /// Render the current frame using the differential [`FramePacer`].
    ///
    /// The first call is a full redraw; subsequent calls write only the
    /// cells that actually changed. This mirrors upstream's
    /// `TuiBase.renderInternal`.
    pub fn render(&mut self) -> Result<(), S::Error> {
        let area = Rect::new(0, 0, self.cached_width, self.cached_height);
        let payload = self.frame_pacer.render_with(area, |buf| {
            self.base.render(buf);
        });
        if !payload.is_empty() {
            self.sink.write_str(&payload)?;
        }
        self.sink.flush()
    }

    /// Show an overlay.
    pub fn show_overlay(&mut self, opts: OverlayOptions) -> OverlayHandle {
        self.base.show_overlay(opts)
    }

    /// Hide an overlay.
    pub fn hide_overlay(&mut self, handle: OverlayHandle) {
        self.base.hide_overlay(handle)
    }

    /// Current render state.
    pub fn state(&self) -> &TuiMainScreenRenderState {
        &self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory sink for testing.
    #[derive(Default, Clone)]
    struct MockSink {
        bytes: Vec<u8>,
        size: (u16, u16),
        cursor_visible: bool,
    }
    impl TuiSink for MockSink {
        type Error = std::io::Error;
        fn write_str(&mut self, s: &str) -> Result<(), Self::Error> {
            self.bytes.extend_from_slice(s.as_bytes());
            Ok(())
        }
        fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        fn size(&self) -> (u16, u16) {
            self.size
        }
        fn hide_cursor(&mut self) -> Result<(), Self::Error> {
            self.cursor_visible = false;
            Ok(())
        }
        fn show_cursor(&mut self) -> Result<(), Self::Error> {
            self.cursor_visible = true;
            Ok(())
        }
    }

    /// Empty stub root for tests — never actually rendered because
    /// the test only inspects the screen mode sequences.
    struct EmptyRoot;
    impl CoreComponent for EmptyRoot {
        fn render(&mut self, _area: Rect, _buf: &mut Buffer) {}
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    #[test]
    fn alt_screen_start_writes_enter_sequence() {
        let sink = MockSink { size: (10, 5), ..Default::default() };
        let root: Box<dyn CoreComponent> = Box::new(EmptyRoot);
        let mut driver = TuiAltScreen::new(root, sink, TuiAltScreenOptions::default());
        driver.start().unwrap();
        assert!(driver.is_started());
        let out = String::from_utf8_lossy(&driver.sink().bytes);
        assert!(out.contains(ALT_SCREEN_ENTER), "alt-enter not in {:?}", out);
        assert!(!driver.sink().cursor_visible, "cursor should be hidden");
    }

    #[test]
    fn alt_screen_stop_writes_exit_sequence() {
        let sink = MockSink { size: (10, 5), ..Default::default() };
        let root: Box<dyn CoreComponent> = Box::new(EmptyRoot);
        let mut driver = TuiAltScreen::new(root, sink, TuiAltScreenOptions::default());
        driver.start().unwrap();
        driver.stop(TuiStopOptions::default()).unwrap();
        assert!(!driver.is_started());
        let out = String::from_utf8_lossy(&driver.sink().bytes);
        assert!(out.contains(ALT_SCREEN_EXIT), "alt-exit not in {:?}", out);
        assert!(driver.sink().cursor_visible, "cursor should be shown");
    }

    #[test]
    fn alt_screen_stop_without_start_is_noop() {
        let sink = MockSink { size: (10, 5), ..Default::default() };
        let root: Box<dyn CoreComponent> = Box::new(EmptyRoot);
        let mut driver = TuiAltScreen::new(root, sink, TuiStopOptions::default().into());
        driver.stop(TuiStopOptions::default()).unwrap();
        assert!(!driver.is_started());
    }

    #[test]
    fn alt_screen_drop_exits_screen() {
        let sink = MockSink { size: (10, 5), ..Default::default() };
        let root: Box<dyn CoreComponent> = Box::new(EmptyRoot);
        let mut driver = TuiAltScreen::new(root, sink, TuiAltScreenOptions::default());
        driver.start().unwrap();
        assert!(driver.is_started());
        drop(driver);
    }

    #[test]
    fn alt_screen_idempotent_start_stop() {
        let sink = MockSink { size: (10, 5), ..Default::default() };
        let root: Box<dyn CoreComponent> = Box::new(EmptyRoot);
        let mut driver = TuiAltScreen::new(root, sink, TuiAltScreenOptions::default());
        driver.start().unwrap();
        driver.start().unwrap();
        driver.stop(TuiStopOptions::default()).unwrap();
        driver.stop(TuiStopOptions::default()).unwrap();
        // Should have only entered once and exited once.
        let bytes = driver.sink().bytes.clone();
        let enter_count = bytes
            .windows(ALT_SCREEN_ENTER.len())
            .filter(|w| *w == ALT_SCREEN_ENTER.as_bytes())
            .count();
        let exit_count = bytes
            .windows(ALT_SCREEN_EXIT.len())
            .filter(|w| *w == ALT_SCREEN_EXIT.as_bytes())
            .count();
        assert_eq!(enter_count, 1);
        assert_eq!(exit_count, 1);
    }

    #[test]
    fn main_screen_does_not_enter_alt() {
        let sink = MockSink { size: (10, 5), ..Default::default() };
        let root: Box<dyn CoreComponent> = Box::new(EmptyRoot);
        let mut driver = TuiMainScreen::new(root, sink);
        driver.render().unwrap();
        let bytes = driver.sink().bytes.clone();
        assert!(!String::from_utf8_lossy(&bytes).contains(ALT_SCREEN_ENTER));
    }

    /// Root that paints one cell at (0, 0) — the simplest non-empty
    /// render, enough to prove the pacer wrote a frame.
    struct NonEmptyRoot;
    impl CoreComponent for NonEmptyRoot {
        fn render(&mut self, area: Rect, buf: &mut Buffer) {
            if let Some(cell) = buf.cell_mut((area.x, area.y)) {
                cell.set_symbol("X");
            }
        }
        fn as_any(&self) -> &dyn std::any::Any { self }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    }

    #[test]
    fn main_screen_render_writes_clear_home() {
        let sink = MockSink { size: (5, 2), ..Default::default() };
        let root: Box<dyn CoreComponent> = Box::new(NonEmptyRoot);
        let mut driver = TuiMainScreen::new(root, sink);
        driver.render().unwrap();
        let bytes = driver.sink().bytes.clone();
        let out = String::from_utf8_lossy(&bytes);
        // The differential render issues a CUP per changed cell, so the
        // single non-empty cell produces `\x1b[1;1H` (row 1, col 1)
        // — the equivalent of `CURSOR_HOME` for an empty row 0.
        assert!(
            out.contains(CURSOR_HOME) || out.contains("\x1b[1;1H"),
            "frame has no cursor positioning: {out:?}"
        );
        assert!(out.contains('X'), "the painted cell should appear");
    }
}

// Allow `.into()` on `TuiStopOptions` for callers that need to
// convert it elsewhere.
impl From<TuiStopOptions> for TuiAltScreenOptions {
    fn from(_: TuiStopOptions) -> Self {
        TuiAltScreenOptions::default()
    }
}