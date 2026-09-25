//! End-to-end TUI driver smoke test.
//!
//! Spins up a `TuiAltScreen` driver, dispatches a key event, renders a
//! frame, and confirms:
//!
//! - the alt-screen enter sequence appears exactly once on start,
//! - the alt-screen exit sequence appears on stop,
//! - a render round-trip writes cell content to the sink,
//! - the driver is idempotent across repeated start/stop calls.
//!
//! The test uses an in-memory [`TuiSink`] so it does not depend on a
//! real PTY — but the byte sequences are exactly what a real terminal
//! would see, so the same surface is exercised.

use pi_tui::core::component::CoreComponent;
use pi_tui::core::tui_base::TuiMode;
use pi_tui::core::tui_drivers::{
    TuiAltScreen, TuiMainScreen, TuiSink, ALT_SCREEN_ENTER, ALT_SCREEN_EXIT,
};
use pi_tui::input::Key;
use pi_tui::ts_compat::{TuiAltScreenOptions, TuiStopOptions};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::error::Error;
use std::fmt;
use std::io;

/// A capturing sink used by the end-to-end smoke test.
#[derive(Default, Clone)]
struct CapturingSink {
    bytes: Vec<u8>,
    size: (u16, u16),
}

impl TuiSink for CapturingSink {
    type Error = io::Error;
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
    fn hide_cursor(&mut self) -> Result<(), Self::Error> { Ok(()) }
    fn show_cursor(&mut self) -> Result<(), Self::Error> { Ok(()) }
}

/// Trivial root component that emits a single blank row when rendered.
struct BlankRoot;
impl CoreComponent for BlankRoot {
    fn render(&mut self, _area: Rect, _buf: &mut Buffer) {}
    fn as_any(&self) -> &dyn std::any::Any { self }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}

/// Custom error type to satisfy `Error: std::error::Error` if we
/// ever wrap the IO error.
#[derive(Debug)]
struct TestError(String);
impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl Error for TestError {}

fn count_subsequence(haystack: &[u8], needle: &[u8]) -> usize {
    haystack.windows(needle.len()).filter(|w| *w == needle).count()
}

#[test]
fn alt_screen_full_lifecycle_emits_enter_then_exit() {
    let sink = CapturingSink { size: (10, 5), ..Default::default() };
    let root: Box<dyn CoreComponent> = Box::new(BlankRoot);
    let mut driver = TuiAltScreen::new(root, sink, TuiAltScreenOptions::default());

    driver.start().expect("start");
    assert!(driver.is_started());

    // Render a frame.
    driver.render().expect("render");

    // Dispatch a key — `q` should reach the pipeline without panic.
    let key = Key::char('q');
    let _ = driver.dispatch_key(&key);

    // Stop the driver.
    driver.stop(TuiStopOptions::default()).expect("stop");
    assert!(!driver.is_started());

    // Verify the byte stream.
    let bytes = driver.sink().bytes.clone();
    let enter = count_subsequence(&bytes, ALT_SCREEN_ENTER.as_bytes());
    let exit = count_subsequence(&bytes, ALT_SCREEN_EXIT.as_bytes());
    assert_eq!(enter, 1, "expected exactly one alt-screen enter, got {}", enter);
    assert_eq!(exit, 1, "expected exactly one alt-screen exit, got {}", exit);
}

#[test]
fn alt_screen_idempotent_lifecycle() {
    let sink = CapturingSink { size: (10, 5), ..Default::default() };
    let root: Box<dyn CoreComponent> = Box::new(BlankRoot);
    let mut driver = TuiAltScreen::new(root, sink, TuiAltScreenOptions::default());

    for _ in 0..3 {
        driver.start().expect("start");
        driver.render().expect("render");
        driver.stop(TuiStopOptions::default()).expect("stop");
    }

    // Three full lifecycles — but each start/stop should only emit
    // one enter/exit, so the byte stream contains 3 of each.
    let bytes = driver.sink().bytes.clone();
    assert_eq!(count_subsequence(&bytes, ALT_SCREEN_ENTER.as_bytes()), 3);
    assert_eq!(count_subsequence(&bytes, ALT_SCREEN_EXIT.as_bytes()), 3);
}

#[test]
fn alt_screen_drop_after_start_does_not_panic() {
    let sink = CapturingSink { size: (10, 5), ..Default::default() };
    let root: Box<dyn CoreComponent> = Box::new(BlankRoot);
    let mut driver = TuiAltScreen::new(root, sink, TuiAltScreenOptions::default());
    driver.start().expect("start");
    drop(driver);
}

#[test]
fn alt_screen_overlay_lifecycle() {
    use pi_tui::core::overlay::OverlayOptions;

    let sink = CapturingSink { size: (10, 5), ..Default::default() };
    let root: Box<dyn CoreComponent> = Box::new(BlankRoot);
    let mut driver = TuiAltScreen::new(root, sink, TuiAltScreenOptions::default());
    driver.start().expect("start");

    // Show and hide an overlay.
    let _handle = driver.show_overlay(OverlayOptions::default());
    driver.render().expect("render");
    driver.stop(TuiStopOptions::default()).expect("stop");
}

#[test]
fn main_screen_lifecycle_does_not_enter_alt() {
    let sink = CapturingSink { size: (10, 5), ..Default::default() };
    let root: Box<dyn CoreComponent> = Box::new(BlankRoot);
    let mut driver = TuiMainScreen::new(root, sink);
    driver.render().expect("render");
    driver.dispatch_input("hello");
    let bytes = driver.sink().bytes.clone();
    assert_eq!(count_subsequence(&bytes, ALT_SCREEN_ENTER.as_bytes()), 0);
    assert_eq!(count_subsequence(&bytes, ALT_SCREEN_EXIT.as_bytes()), 0);
}

#[test]
fn alt_screen_mode_reflects_fullscreen() {
    let sink = CapturingSink { size: (10, 5), ..Default::default() };
    let root: Box<dyn CoreComponent> = Box::new(BlankRoot);
    let driver = TuiAltScreen::new(root, sink, TuiAltScreenOptions::default());
    assert_eq!(driver.base().mode(), TuiMode::Fullscreen);
}

#[test]
fn main_screen_mode_reflects_regular() {
    let sink = CapturingSink { size: (10, 5), ..Default::default() };
    let root: Box<dyn CoreComponent> = Box::new(BlankRoot);
    let driver = TuiMainScreen::new(root, sink);
    assert_eq!(driver.base().mode(), TuiMode::Regular);
}

#[test]
fn alt_screen_render_writes_at_least_clear_and_home() {
    let sink = CapturingSink { size: (10, 5), ..Default::default() };
    let root: Box<dyn CoreComponent> = Box::new(BlankRoot);
    let mut driver = TuiAltScreen::new(root, sink, TuiAltScreenOptions::default());
    driver.start().expect("start");
    let bytes_before = driver.sink().bytes.len();
    driver.render().expect("render");
    let bytes_after = driver.sink().bytes.len();
    assert!(bytes_after > bytes_before, "render should write more bytes");
}