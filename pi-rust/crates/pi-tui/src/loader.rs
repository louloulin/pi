//! Busy / loading indicator — the frame table behind the footer spinner.
//!
//! Ported from upstream's `Loader` (`packages/tui/src/components/loader.ts`)
//! and Martty's App-level spinner (`src/app.rs` `SPINNER` / `spinner_idx` /
//! `spinner()` / `tick()`), which the Rust port follows: one shared frame
//! table, a `usize` cursor, and an `advance()` the render loop calls on its
//! existing tick. There is deliberately no timer, task or thread in here —
//! the caller already has a 50 ms poll beat (see [`crate::App`] and the
//! interactive driver), so the indicator reuses it instead of building a
//! second clock.
//!
//! The frame glyphs and the interval are byte-for-byte the upstream default
//! (`DEFAULT_FRAMES` / `DEFAULT_INTERVAL_MS`), so a user coming from the TS
//! CLI sees the same animation.

use std::time::Duration;

/// Braille spinner frames — upstream `DEFAULT_FRAMES`
/// (`packages/tui/src/components/loader.ts:6-16`), identical to Martty's
/// `pub const SPINNER: [char; 10]` (`src/app.rs`).
pub const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Milliseconds a frame stays on screen — upstream `DEFAULT_INTERVAL_MS`.
///
/// The App advances the cursor at most once per interval, so a driver that
/// polls faster than this (crossterm wakes on every key press) still animates
/// at the upstream rate.
pub const SPINNER_INTERVAL_MS: u64 = 80;

/// A cursor into [`SPINNER_FRAMES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Spinner {
    index: usize,
}

impl Spinner {
    /// A spinner sitting on the first frame.
    pub fn new() -> Self {
        Self { index: 0 }
    }

    /// The current cursor position (always `< SPINNER_FRAMES.len()`).
    pub fn index(&self) -> usize {
        self.index
    }

    /// The glyph for the current frame.
    pub fn frame(&self) -> char {
        SPINNER_FRAMES[self.index]
    }

    /// The current frame as a one-character string, ready to paint.
    pub fn frame_text(&self) -> &'static str {
        frame_text(self.index)
    }

    /// Step to the next frame, wrapping at the end, and return the new glyph.
    pub fn advance(&mut self) -> char {
        self.index = (self.index + 1) % SPINNER_FRAMES.len();
        self.frame()
    }

    /// Return to the first frame — called when a turn ends so the next one
    /// starts from a deterministic glyph instead of wherever it stopped.
    pub fn reset(&mut self) {
        self.index = 0;
    }
}

/// The glyph at `index`, wrapping out-of-range indices. Keeps the frame table
/// `&'static str` rather than allocating a `String` per frame.
fn frame_text(index: usize) -> &'static str {
    const FRAME_TEXT: [&str; SPINNER_FRAMES.len()] =
        ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    FRAME_TEXT[index % SPINNER_FRAMES.len()]
}

/// Format a turn's wall-clock duration for the busy indicator.
///
/// Seconds while the turn is young (`12s`), then minutes with zero-padded
/// seconds (`1m05s`), then hours with zero-padded minutes (`1h02m`). Once the
/// unit grows the lower unit is still shown, so the number never appears to
/// stall while a long answer streams.
pub fn format_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m{:02}s", seconds % 60);
    }
    format!("{}h{:02}m", minutes / 60, minutes % 60)
}

/// The single line an indicator renders: `"<frame> <label>"`.
///
/// A blank label renders as just the glyph, so the footer can carry the
/// spinner without a trailing space.
pub fn indicator_line(frame: char, label: &str) -> String {
    if label.is_empty() {
        frame.to_string()
    } else {
        format!("{frame} {label}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_match_the_upstream_default_table() {
        // Same glyphs, same order as `loader.ts` DEFAULT_FRAMES and Martty's
        // `SPINNER`.
        assert_eq!(SPINNER_FRAMES.len(), 10);
        assert_eq!(SPINNER_FRAMES[0], '⠋');
        assert_eq!(SPINNER_FRAMES[9], '⠏');
        assert_eq!(SPINNER_INTERVAL_MS, 80);
    }

    #[test]
    fn advance_walks_the_table_and_wraps() {
        let mut spinner = Spinner::new();
        assert_eq!(spinner.index(), 0);
        assert_eq!(spinner.frame(), '⠋');
        for expected in &SPINNER_FRAMES[1..] {
            assert_eq!(spinner.advance(), *expected);
        }
        // Wrapped back to the first frame.
        assert_eq!(spinner.advance(), SPINNER_FRAMES[0]);
        assert_eq!(spinner.index(), 0);
    }

    #[test]
    fn reset_returns_to_the_first_frame() {
        let mut spinner = Spinner::new();
        spinner.advance();
        spinner.advance();
        spinner.reset();
        assert_eq!(spinner.frame(), SPINNER_FRAMES[0]);
        assert_eq!(spinner.frame_text(), "⠋");
    }

    #[test]
    fn frame_text_matches_the_char_table() {
        for (index, frame) in SPINNER_FRAMES.iter().enumerate() {
            assert_eq!(frame_text(index), frame.to_string());
        }
    }

    #[test]
    fn formats_elapsed_in_the_largest_unit_with_one_sub_unit() {
        assert_eq!(format_elapsed(Duration::ZERO), "0s");
        assert_eq!(format_elapsed(Duration::from_secs(7)), "7s");
        assert_eq!(format_elapsed(Duration::from_secs(59)), "59s");
        assert_eq!(format_elapsed(Duration::from_secs(60)), "1m00s");
        assert_eq!(format_elapsed(Duration::from_secs(65)), "1m05s");
        assert_eq!(format_elapsed(Duration::from_secs(3599)), "59m59s");
        assert_eq!(format_elapsed(Duration::from_secs(3600)), "1h00m");
        assert_eq!(format_elapsed(Duration::from_secs(7260)), "2h01m");
    }

    #[test]
    fn indicator_line_renders_frame_and_label() {
        assert_eq!(indicator_line('⠋', "12s"), "⠋ 12s");
        assert_eq!(indicator_line('⠙', ""), "⠙");
    }
}
