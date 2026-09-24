//! Stderr notices that survive raw mode.
//!
//! 1:1 mirror of nanopi's `src/render/raw_tty.rs` (335 L). The same
//! queue model is needed here because pi-rust's TUI also drives the
//! screen in raw mode, and any code path that wants to log a startup
//! notice / plugin warning / host-log entry while the TUI is up needs
//! to go through the same single sink that the TUI loop drains.
//!
//! Why a queue, not a direct write
//! ───────────────────────────────
//! While the TUI is up the terminal is in raw mode, where `\n` is a
//! LINE FEED and nothing else: the cursor drops one row and keeps its
//! column. A plain `eprintln!` from anywhere in the program therefore
//! staircases, each line starting where the previous one ended:
//!
//! ```text
//! [wasm:trace] events-plugin: observed input
//!                                           [wasm:trace] events-plugin: observed turn_start
//! ```
//!
//! Writing `\r\n` instead puts every line back at column 0 — that is
//! the half of this module every existing path gets for free.
//!
//! But where the line goes matters more than the line break (T4.7 in
//! nanopi's docs). Writing a legible line into the region ratatui
//! manages only buys the user a flash: the next redraw wipes it. So
//! while the TUI owns the screen, `note` does not write to stderr at
//! all — it pushes onto a process-wide queue that the TUI loop drains
//! into scrollback, which is the only way anything reaches scrollback
//! permanently.
//!
//! Two cases the queue has to survive, both handled rather than
//! assumed away:
//!
//! - notices written between `setup_terminal` and the first tick, or
//!   after the loop exits — `flush_pending_to_stderr` flushes whatever
//!   is still queued to stderr once raw mode is off, so nothing is lost
//!   to a crash on the way up or down;
//! - a loop that stops draining (a turn that blocks the runtime) —
//!   the queue is capped at [`MAX_PENDING`] and drops the OLDEST, so a
//!   plugin logging in a loop cannot grow it without bound and the
//!   newest line, the one describing what is happening now, is the one
//!   kept.
//!
//! nanopi tie-in
//! ─────────────
//! This is the Rust port's twin of nanopi's `raw_tty` module. Same
//! algorithm, same constants, same tests. If you change one, change
//! the other.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

/// True while the terminal is in raw mode (the TUI owns the screen).
static RAW: AtomicBool = AtomicBool::new(false);

/// Called by the TUI on setup and teardown. Anything else reading this
/// is asking "will a bare \n staircase right now?".
pub fn set_raw_mode(on: bool) {
    RAW.store(on, Ordering::Relaxed);
}

/// Returns whether raw mode is currently active. Cheap; just reads
/// the flag.
pub fn is_raw_mode() -> bool {
    RAW.load(Ordering::Relaxed)
}

/// Write one notice to stderr, line-terminated correctly for whichever
/// mode the terminal is in. Embedded newlines are translated too — a
/// multi-line hook warning staircases just as readily as two separate
/// ones.
///
/// Failures are ignored, deliberately: this is a diagnostic path, and a
/// closed stderr (a shell that closed the pipe, or `2>&-`) must not
/// take down the run that was working.
pub fn note(msg: &str) {
    match destination(is_raw_mode()) {
        Destination::Queue => push(msg),
        Destination::Stderr => write_stderr(msg),
    }
}

/// Where one notice goes. Two variants, not a pair of flags: a notice
/// written to BOTH would be visible twice for the ~120ms between the
/// stderr write and the redraw that wipes it, and an enum makes that
/// state unrepresentable rather than merely discouraged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Destination {
    /// The TUI owns the screen: queue for the loop to put in
    /// scrollback, which is the only way a line survives a redraw.
    Queue,
    /// Nobody is drawing; stderr is the user's screen.
    Stderr,
}

fn destination(raw: bool) -> Destination {
    if raw {
        Destination::Queue
    } else {
        Destination::Stderr
    }
}

/// The actual stderr write, still raw-mode aware because the teardown
/// flush runs with the flag already cleared while the terminal may not
/// have finished restoring.
fn write_stderr(msg: &str) {
    let mut err = std::io::stderr().lock();
    let _ = if is_raw_mode() {
        write!(err, "{}\r\n", crlf(msg))
    } else {
        writeln!(err, "{msg}")
    };
    let _ = err.flush();
}

/// Queued notices, oldest first, waiting for whoever owns the screen.
static PENDING: LazyLock<Mutex<VecDeque<String>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));

/// How many notices may wait for a drain.
///
/// Generous, because the normal depth is one or two and the cap only
/// matters when the loop has stopped ticking — at which point the
/// question is not "how much history" but "does an unattended plugin
/// eat memory". 512 short lines is tens of kilobytes.
pub const MAX_PENDING: usize = 512;

/// Internal: enqueue one notice, splitting multi-line messages into
/// rows. A poisoned lock is treated as "drop the line" rather than a
/// fatal error — same call `note` makes about a closed stderr.
fn push(msg: &str) {
    let Ok(mut q) = PENDING.lock() else {
        return;
    };
    for line in crlf(msg).replace("\r\n", "\n").split('\n') {
        if q.len() >= MAX_PENDING {
            q.pop_front();
        }
        q.push_back(line.to_string());
    }
}

/// Take every queued notice. Called by the TUI loop each tick; empty
/// is the overwhelmingly common answer.
pub fn drain() -> Vec<String> {
    let Ok(mut q) = PENDING.lock() else {
        return Vec::new();
    };
    q.drain(..).collect()
}

/// Flush anything still queued straight to stderr.
///
/// Called at teardown, after raw mode is off: notices written before
/// the first tick or after the loop exits have no drainer, and losing
/// the last diagnostic before an exit is exactly when it matters.
pub fn flush_pending_to_stderr() {
    for line in drain() {
        write_stderr(&line);
    }
}

/// How many notices are currently queued. Used by tests and by the
/// TUI loop to decide whether to bother draining this tick.
pub fn pending_len() -> usize {
    PENDING.lock().map(|q| q.len()).unwrap_or(0)
}

/// Every line break in `msg` as CRLF, whatever it started as. Normalize
/// existing CRLFs down to LF first — otherwise a caller that already
/// hand-wrote `\r\n` gets `\r\r\n`, which some terminals render as a
/// blank row.
fn crlf(msg: &str) -> String {
    msg.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// `eprintln!`, but raw-mode aware. Same formatting arguments.
///
/// Use this in place of `eprintln!` everywhere inside the TUI crate —
/// it costs nothing when the TUI is down, and it routes the line
/// through the queue when the TUI is up.
#[macro_export]
macro_rules! note {
    ($($arg:tt)*) => {
        $crate::raw_tty::note(&format!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// pi-rust doesn't have a shared test_lock yet, so each test guards
    /// the global state by hand. Tests run in parallel under cargo;
    /// touching the global state without a guard would flake.
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn reset_state() {
        let _ = drain();
        set_raw_mode(false);
    }

    /// The defect: while the TUI is up, a `note` went to stderr, which
    /// is inside the region ratatui manages, so the next redraw wiped
    /// it. It must be queued for the loop to put in scrollback instead
    /// — and NOT also written to stderr, or the user sees the same
    /// line twice for one frame.
    #[test]
    fn a_notice_in_raw_mode_is_queued_not_written() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_state();

        set_raw_mode(true);
        note("[wasm:trace] observed input");
        note("[wasm:trace] observed turn_start");
        let got = drain();
        set_raw_mode(false);

        assert_eq!(
            got,
            vec![
                "[wasm:trace] observed input".to_string(),
                "[wasm:trace] observed turn_start".to_string(),
            ],
            "raw-mode notices must be waiting for the TUI loop, in order"
        );
        assert!(drain().is_empty(), "a drain must take them");
    }

    /// One destination, never two. This asserts the decision rather
    /// than the write, because a stderr write from inside the process
    /// is not observable from a test.
    #[test]
    fn a_notice_has_exactly_one_destination() {
        assert_eq!(destination(true), Destination::Queue);
        assert_eq!(destination(false), Destination::Stderr);
    }

    /// Outside the TUI there is nobody to drain, so stderr stays the
    /// destination — print mode must not silently swallow diagnostics.
    #[test]
    fn a_notice_outside_raw_mode_is_not_queued() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_state();

        set_raw_mode(false);
        note("printed straight to stderr");
        let got = drain();
        assert!(got.is_empty(), "nothing should be queued: {got:?}");
    }

    /// One entry per scrollback row: the drainer inserts each entry as
    /// a row, so an embedded `\n` would land in ratatui's buffer as a
    /// control character rather than a line break.
    #[test]
    fn a_multiline_notice_is_split_into_rows() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_state();

        set_raw_mode(true);
        note("hook warning:\nsecond line\r\nthird");
        let got = drain();
        set_raw_mode(false);

        assert_eq!(got, vec!["hook warning:", "second line", "third"]);
    }

    /// A loop that stops ticking must not let a chatty plugin grow the
    /// queue without bound — and what survives is the NEWEST, since
    /// that is what describes what is happening now.
    #[test]
    fn the_queue_is_capped_and_drops_the_oldest() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_state();

        set_raw_mode(true);
        for i in 0..(MAX_PENDING + 10) {
            note(&format!("line {i}"));
        }
        let got = drain();
        set_raw_mode(false);

        assert_eq!(got.len(), MAX_PENDING);
        assert_eq!(got[0], format!("line {}", 10));
        assert_eq!(got[MAX_PENDING - 1], format!("line {}", MAX_PENDING + 9));
    }

    /// Notices written before the first tick or after the loop exits
    /// have no drainer. Teardown flushes them rather than dropping the
    /// last diagnostic before an exit.
    #[test]
    fn teardown_flushes_what_never_got_drained() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_state();

        set_raw_mode(true);
        note("the loop already stopped");
        set_raw_mode(false);
        flush_pending_to_stderr();

        assert!(drain().is_empty(), "teardown must not leave a line queued");
    }

    /// The state is global, so the round-trip runs in series with the
    /// other tests via the test lock rather than racing.
    #[test]
    fn raw_mode_flag_round_trips() {
        let _g = TEST_LOCK.lock().unwrap();
        set_raw_mode(true);
        assert!(is_raw_mode());
        set_raw_mode(false);
        assert!(!is_raw_mode());
    }

    /// pending_len mirrors drain's view of the queue.
    #[test]
    fn pending_len_reports_queue_depth() {
        let _g = TEST_LOCK.lock().unwrap();
        reset_state();

        assert_eq!(pending_len(), 0);
        set_raw_mode(true);
        note("one");
        note("two");
        assert_eq!(pending_len(), 2);
        let _ = drain();
        assert_eq!(pending_len(), 0);
        set_raw_mode(false);
    }

    /// The translation itself, tested on the string rather than on
    /// stderr — the point is that every line break carries a `\r`,
    /// including ones inside the message.
    #[test]
    fn every_newline_gains_a_carriage_return() {
        assert_eq!(crlf("first\nsecond\nthird"), "first\r\nsecond\r\nthird");
    }

    /// A caller that already hand-wrote CRLF must not end up with
    /// `\r\r\n`, which some terminals render as an extra blank row.
    #[test]
    fn an_existing_crlf_is_not_doubled() {
        assert_eq!(crlf("a\r\nb"), "a\r\nb");
        assert_eq!(crlf("a\r\nb\nc"), "a\r\nb\r\nc");
    }

    /// Nothing to translate is the common case — a single-line notice.
    #[test]
    fn a_single_line_message_is_untouched() {
        assert_eq!(crlf("plain"), "plain");
        assert_eq!(crlf(""), "");
    }
}