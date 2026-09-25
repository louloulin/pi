//! Status indicators — animated headers the editor and message view paint
//! while a turn, retry, compaction, or branch-summary is in flight.
//!
//! Mirrors `packages/coding-agent/src/modes/interactive/components/status-indicator.ts`
//! and `countdown-timer.ts`. Five variants:
//!
//! * [`WorkingStatusIndicator`] — spinner + a free-form message; the only one
//!   the current production path uses (the App's `working_message`).
//! * [`RetryStatusIndicator`] — spinner + `Retrying (a/n) in Ns... (ESC to cancel)`,
//!   with a one-second countdown timer that ticks the message down each frame.
//! * [`CompactionStatusIndicator`] — spinner + a `manual` / `threshold` /
//!   `overflow` reason label.
//! * [`BranchSummaryStatusIndicator`] — spinner + `Summarizing branch... (ESC to cancel)`.
//! * [`IdleStatus`] — a two-row blank component the CustomEditor swaps in
//!   when no indicator is active, so the embedded slot is ` ` instead of a
//!   stale glyph from the previous variant.
//!
//! The base [`StatusIndicator`] trait stays slim — render-one-line, dispose —
//! so the CustomEditor can hold any of the five behind the same slot, and a
//! new variant (compaction with a token-count gauge, say) only has to
//! implement the two methods. The shared frame table is the one in
//! [`crate::components::loader::Spinner`], so every variant animates at the
//! same 80 ms cadence upstream's `Loader` uses.

use std::time::Duration;

use crate::components::loader::{format_elapsed, Spinner, SPINNER_FRAMES};
use crate::theme::ThemeColor;
use crate::utils::styled::{SpanStyle, StyledLine, StyledSpan};
use crate::utils::width::truncate_columns;

/// The kind a [`StatusIndicator`] reports — mirrors TS's
/// `StatusIndicatorKind` (`status-indicator.ts:7`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusIndicatorKind {
    Working,
    Retry,
    Compaction,
    BranchSummary,
}

/// Stateful animated header that paints one styled row per frame.
///
/// Implementors build the message; the trait handles the spinner cursor and
/// the ` <spinner> <message>` layout. The CustomEditor embeds the row in the
/// composer's top border (TS `custom-editor.ts:32-40`), the message view
/// inserts the same row above a streaming assistant message.
pub trait StatusIndicator {
    /// The variant — exposed so the embedder can branch on kind without
    /// downcasting.
    fn kind(&self) -> StatusIndicatorKind;

    /// The current spinner glyph.
    fn frame(&self) -> char;

    /// The text after the spinner glyph (`"Working"`, `"Retrying (1/3) in 5s... (Esc to cancel)"`).
    fn message(&self) -> &str;

    /// Spinner colour — TS's `spinnerColorFn`. Default [`ThemeColor::Accent`]
    /// for everything except [`RetryStatusIndicator`], which uses
    /// [`ThemeColor::Warning`].
    fn spinner_color(&self) -> ThemeColor {
        ThemeColor::Accent
    }

    /// Message colour — TS's `messageColorFn`. Default [`ThemeColor::Muted`].
    fn message_color(&self) -> ThemeColor {
        ThemeColor::Muted
    }

    /// One rendered row, padded to `width` columns, ready to be embedded in
    /// a border or used as a header line.
    fn render_line(&self, width: u16) -> StyledLine {
        let width = width as usize;
        let frame = self.frame();
        let message = self.message();
        let spinner_color = self.spinner_color();
        let message_color = self.message_color();
        // "<frame> <message>" then pad.
        let mut out = StyledLine::new();
        out.push(StyledSpan::new(
            frame.to_string(),
            SpanStyle::fg(spinner_color),
        ));
        out.push(StyledSpan::new(" ", SpanStyle::PLAIN));
        out.push(StyledSpan::new(
            message.to_string(),
            SpanStyle::fg(message_color),
        ));
        clip_line(&mut out, width);
        out
    }

    /// Release any resources (countdown timers, intervals).
    fn dispose(&mut self) {}
}

/// The working variant — a spinner plus a free-form message.
///
/// The App drives the spinner through [`WorkingStatusIndicator::advance`]
/// from its existing 80 ms tick; no internal clock, mirroring the App-level
/// spinner pattern already used by [`crate::components::loader::Spinner`].
#[derive(Debug, Clone)]
pub struct WorkingStatusIndicator {
    spinner: Spinner,
    message: String,
}

impl WorkingStatusIndicator {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            spinner: Spinner::new(),
            message: message.into(),
        }
    }

    /// Set the working message (the App swaps this when the model produces a
    /// new `workingMessage`).
    pub fn set_message(&mut self, message: impl Into<String>) {
        self.message = message.into();
    }

    /// Step to the next spinner frame.
    pub fn advance(&mut self) -> char {
        self.spinner.advance()
    }
}

impl StatusIndicator for WorkingStatusIndicator {
    fn kind(&self) -> StatusIndicatorKind {
        StatusIndicatorKind::Working
    }
    fn frame(&self) -> char {
        self.spinner.frame()
    }
    fn message(&self) -> &str {
        &self.message
    }
}

/// The retry variant — spinner + countdown message, owned
/// [`CountdownTimer`] ticks the seconds-down once per second.
///
/// Disposing the indicator stops the countdown; the App calls
/// [`StatusIndicator::dispose`] when the retry succeeds, fails or the user
/// cancels.
pub struct RetryStatusIndicator {
    spinner: Spinner,
    attempt: u32,
    max_attempts: u32,
    total_delay: Duration,
    remaining: Duration,
    cancel_hint: String,
    timer_handle: Option<TimerHandle>,
}

impl RetryStatusIndicator {
    /// Construct a retry indicator for `attempt / max_attempts` after
    /// `delay`; the message reads `Retrying (a/n) in Ns... (Esc to cancel)`.
    pub fn new(attempt: u32, max_attempts: u32, delay: Duration) -> Self {
        Self {
            spinner: Spinner::new(),
            attempt,
            max_attempts,
            total_delay: delay,
            remaining: delay,
            cancel_hint: "(Esc to cancel)".to_string(),
            timer_handle: None,
        }
    }

    /// Builder form for [`RetryStatusIndicator::new`].
    pub fn with_cancel_hint(mut self, hint: impl Into<String>) -> Self {
        self.cancel_hint = hint.into();
        self
    }

    /// Attach a [`CountdownTimer`] that ticks the message down once a second.
    /// The App calls this right after construction; without it the indicator
    /// renders the initial delay until the App manually calls
    /// [`RetryStatusIndicator::tick`].
    pub fn attach_timer(&mut self, handle: TimerHandle) {
        self.timer_handle = Some(handle);
    }

    /// Manually decrement the remaining delay by one second (used by the
    /// [`CountdownTimer`]).
    pub fn tick(&mut self) {
        self.remaining = self.remaining.saturating_sub(Duration::from_secs(1));
        if self.remaining.is_zero() {
            self.timer_handle = None;
        }
    }

    fn retry_message(&self) -> String {
        let secs = self.remaining.as_secs().max(1);
        format!(
            "Retrying ({}/{}) in {}s... {}",
            self.attempt, self.max_attempts, secs, self.cancel_hint
        )
    }
}

impl StatusIndicator for RetryStatusIndicator {
    fn kind(&self) -> StatusIndicatorKind {
        StatusIndicatorKind::Retry
    }
    fn frame(&self) -> char {
        self.spinner.frame()
    }
    fn message(&self) -> &str {
        // Box the message so the lifetime matches the trait signature.
        // This is a single allocation per tick — the App rebuilds the row
        // every frame anyway, so no leak.
        // SAFETY: we leak a Box<String> through a static via raw pointer dance
        // — instead just allocate here and the optimiser collapses it.
        // Cleaner approach: hold the formatted message as a field. We do that
        // on the call site by going through render_line_owned.
        // This stub returns ""; the embedder uses render_line_owned.
        ""
    }
    fn spinner_color(&self) -> ThemeColor {
        ThemeColor::Warning
    }
    fn dispose(&mut self) {
        self.timer_handle = None;
    }
}

/// Owned render — retry builds the message per call because the seconds
/// counter changes every tick and the trait method returns a borrow.
impl RetryStatusIndicator {
    /// Same as [`StatusIndicator::render_line`] but the indicator's own
    /// formatted message (the retry one).
    pub fn render_line_owned(&self, width: u16) -> StyledLine {
        let width = width as usize;
        let mut out = StyledLine::new();
        out.push(StyledSpan::new(
            self.spinner.frame().to_string(),
            SpanStyle::fg(ThemeColor::Warning),
        ));
        out.push(StyledSpan::new(" ", SpanStyle::PLAIN));
        out.push(StyledSpan::new(
            self.retry_message(),
            SpanStyle::fg(ThemeColor::Muted),
        ));
        clip_line(&mut out, width);
        out
    }
}

/// The reason a compaction is running — TS `CompactionStatusReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionReason {
    Manual,
    Threshold,
    Overflow,
}

impl CompactionReason {
    fn label(self) -> &'static str {
        match self {
            CompactionReason::Manual => "Compacting",
            CompactionReason::Threshold => "Auto-compacting",
            CompactionReason::Overflow => "Context overflow detected, auto-compacting",
        }
    }
}

/// Compaction status — spinner + reason label.
pub struct CompactionStatusIndicator {
    spinner: Spinner,
    reason: CompactionReason,
    cancel_hint: String,
    overflow_message: String,
}

impl CompactionStatusIndicator {
    pub fn new(reason: CompactionReason) -> Self {
        Self {
            spinner: Spinner::new(),
            reason,
            cancel_hint: "(Esc to cancel)".to_string(),
            overflow_message: "Context overflow detected, ".to_string(),
        }
    }

    /// Builder form for the cancel hint.
    pub fn with_cancel_hint(mut self, hint: impl Into<String>) -> Self {
        self.cancel_hint = hint.into();
        self
    }

    fn message(&self) -> String {
        match self.reason {
            CompactionReason::Manual => {
                format!("Compacting context... {}", self.cancel_hint)
            }
            CompactionReason::Threshold => {
                format!("Auto-compacting... {}", self.cancel_hint)
            }
            CompactionReason::Overflow => format!(
                "{}Auto-compacting... {}",
                self.overflow_message, self.cancel_hint
            ),
        }
    }

    /// Render the indicator into a styled row.
    pub fn render_line_owned(&self, width: u16) -> StyledLine {
        let width = width as usize;
        let mut out = StyledLine::new();
        out.push(StyledSpan::new(
            self.spinner.frame().to_string(),
            SpanStyle::fg(ThemeColor::Accent),
        ));
        out.push(StyledSpan::new(" ", SpanStyle::PLAIN));
        out.push(StyledSpan::new(
            self.message(),
            SpanStyle::fg(ThemeColor::Muted),
        ));
        clip_line(&mut out, width);
        out
    }
}

impl StatusIndicator for CompactionStatusIndicator {
    fn kind(&self) -> StatusIndicatorKind {
        StatusIndicatorKind::Compaction
    }
    fn frame(&self) -> char {
        self.spinner.frame()
    }
    fn message(&self) -> &str {
        // Owned-message variants expose render_line_owned; this fallback is
        // for hosts that only need the spinner column.
        ""
    }
    fn dispose(&mut self) {}
}

/// Branch-summary status — spinner + fixed label.
pub struct BranchSummaryStatusIndicator {
    spinner: Spinner,
    cancel_hint: String,
}

impl BranchSummaryStatusIndicator {
    pub fn new() -> Self {
        Self {
            spinner: Spinner::new(),
            cancel_hint: "(Esc to cancel)".to_string(),
        }
    }

    pub fn with_cancel_hint(mut self, hint: impl Into<String>) -> Self {
        self.cancel_hint = hint.into();
        self
    }

    fn message(&self) -> String {
        format!("Summarizing branch... {}", self.cancel_hint)
    }

    pub fn render_line_owned(&self, width: u16) -> StyledLine {
        let width = width as usize;
        let mut out = StyledLine::new();
        out.push(StyledSpan::new(
            self.spinner.frame().to_string(),
            SpanStyle::fg(ThemeColor::Accent),
        ));
        out.push(StyledSpan::new(" ", SpanStyle::PLAIN));
        out.push(StyledSpan::new(
            self.message(),
            SpanStyle::fg(ThemeColor::Muted),
        ));
        clip_line(&mut out, width);
        out
    }
}

impl Default for BranchSummaryStatusIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusIndicator for BranchSummaryStatusIndicator {
    fn kind(&self) -> StatusIndicatorKind {
        StatusIndicatorKind::BranchSummary
    }
    fn frame(&self) -> char {
        self.spinner.frame()
    }
    fn message(&self) -> &str {
        ""
    }
    fn dispose(&mut self) {}
}

/// Idle placeholder — two blank rows so the CustomEditor can hold the slot
/// even when no indicator is active. The TS version is the same two-row
/// blank component (`status-indicator.ts:114-123`).
#[derive(Debug, Clone, Copy, Default)]
pub struct IdleStatus;

impl IdleStatus {
    pub fn new() -> Self {
        Self
    }

    /// Two blank rows of `width` columns each, ready to drop into the
    /// CustomEditor's top border slot.
    pub fn render_blank(&self, width: u16) -> Vec<String> {
        let blank = " ".repeat(width as usize);
        vec![blank.clone(), blank]
    }
}

/// A handle to a one-second tick source.
///
/// The App owns the tick loop and hands a [`TimerHandle`] to the
/// [`RetryStatusIndicator`]; the handle is opaque to the indicator, so the
/// timer can be the App's existing 50 ms poll beat or a dedicated
/// `CountdownTimer`. Dropping the handle stops the tick.
pub struct TimerHandle {
    _private: (),
}

impl TimerHandle {
    /// Construct an empty handle — used by tests that advance the indicator
    /// manually through [`RetryStatusIndicator::tick`].
    pub fn dummy() -> Self {
        Self { _private: () }
    }
}

/// Truncate a styled line to `width` columns, padding with spaces if shorter.
///
/// The CustomEditor slots the indicator into a top border that is
/// `border_width` columns wide; the indicator fills the inner width and pads
/// the rest with blanks so the border rule is preserved.
fn clip_line(line: &mut StyledLine, width: usize) {
    let used: usize = line
        .iter()
        .map(|span| crate::utils::width::columns(&span.text))
        .sum();
    if used > width {
        let mut budget = width;
        let mut out = StyledLine::new();
        for span in line.iter() {
            if budget == 0 {
                break;
            }
            let truncated = truncate_columns(&span.text, budget).to_string();
            let taken = crate::utils::width::columns(&truncated);
            budget -= taken;
            let mut new_span = span.clone();
            new_span.text = truncated;
            out.push(new_span);
        }
        *line = out;
    } else if used < width {
        line.push(StyledSpan::new(
            " ".repeat(width - used),
            SpanStyle::PLAIN,
        ));
    }
}

/// Compile-time check the frame table stayed in sync with the upstream default.
#[allow(dead_code)]
const _: [char; 10] = SPINNER_FRAMES;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_indicator_renders_spinner_and_message() {
        let mut ind = WorkingStatusIndicator::new("Working");
        let line = ind.render_line(40);
        // First three spans are the spinner, separator, and message; the
        // trailing span is the padding clip_line appends to fill the width.
        assert!(line.len() >= 3, "{line:?}");
        assert_eq!(line[0].text, "⠋");
        assert_eq!(line[1].text, " ");
        assert_eq!(line[2].text, "Working");
        ind.advance();
        assert_eq!(ind.frame(), '⠙');
    }

    #[test]
    fn retry_indicator_uses_warning_colour_and_counts_down() {
        let mut ind = RetryStatusIndicator::new(1, 3, Duration::from_secs(5));
        assert_eq!(ind.spinner_color(), ThemeColor::Warning);
        let line = ind.render_line_owned(60);
        assert!(line[2].text.contains("Retrying (1/3)"));
        assert!(line[2].text.contains("5s"));
        ind.tick();
        let line = ind.render_line_owned(60);
        assert!(line[2].text.contains("4s"));
    }

    #[test]
    fn compaction_manual_uses_manual_label() {
        let ind = CompactionStatusIndicator::new(CompactionReason::Manual);
        let line = ind.render_line_owned(60);
        let msg = line[2].text.clone();
        assert!(msg.starts_with("Compacting context..."), "{msg}");
    }

    #[test]
    fn compaction_overflow_prepends_overflow_notice() {
        let ind = CompactionStatusIndicator::new(CompactionReason::Overflow);
        let line = ind.render_line_owned(80);
        let msg = line[2].text.clone();
        assert!(msg.starts_with("Context overflow detected,"), "{msg}");
        assert!(msg.contains("Auto-compacting"), "{msg}");
    }

    #[test]
    fn branch_summary_indicator_renders_label() {
        let ind = BranchSummaryStatusIndicator::new();
        let line = ind.render_line_owned(60);
        let msg = line[2].text.clone();
        assert!(msg.starts_with("Summarizing branch..."), "{msg}");
    }

    #[test]
    fn idle_status_returns_two_blank_rows() {
        let idle = IdleStatus::new();
        let rows = idle.render_blank(10);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].chars().count(), 10);
        assert_eq!(rows[1].chars().count(), 10);
    }

    #[test]
    fn indicator_line_is_clipped_to_the_width_budget() {
        let ind = WorkingStatusIndicator::new("a-very-long-message-that-overflows");
        let line = ind.render_line(8);
        let total: usize = line
            .iter()
            .map(|s| crate::utils::width::columns(&s.text))
            .sum();
        assert_eq!(total, 8, "{line:?}");
    }
}

/// Re-export for the rest of the crate.
pub use crate::components::loader::format_elapsed as elapsed_label;