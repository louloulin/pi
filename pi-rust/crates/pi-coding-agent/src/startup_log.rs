//! Startup-time notice routing.
//!
//! Before the TUI takes over the terminal, the CLI prints three lines
//! that are noisy when interleaved with the first alt-screen frame:
//! extension load failures, the list of loaded extensions, and the
//! extension commands available under `/`. When pi runs interactively
//! those lines used to land on stderr and visibly race the renderer,
//! giving the impression that "pi has no return at all".
//!
//! The fix: route every startup notice to `~/.pi/agent/logs/pi.log` so
//! the file is always written, then mirror to stderr only when the user
//! asked for a non-interactive surface (print, RPC, package commands).
//! The interactive session can always consult the log via
//! `tail -F ~/.pi/agent/logs/pi.log` without paying for the noise.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

/// When `true` (default), [`log_message`] mirrors the line to stderr.
/// The CLI flips this to `false` once it determines the run target is
/// [`ModeTarget::Interactive`](crate::ModeTarget::Interactive).
static MIRROR_TO_STDERR: AtomicBool = AtomicBool::new(true);

/// Configure whether startup notices should also be written to stderr.
///
/// `true` keeps the historical behaviour (every `eprintln!` lands on
/// stderr) so package / print / RPC subcommands continue to surface
/// their notices on the user's terminal. `false` silences stderr and
/// leaves the log file as the only trace; the interactive driver picks
/// that mode so the first TUI frame paints onto a clean stream.
pub fn set_mirror_to_stderr(enabled: bool) {
    MIRROR_TO_STDERR.store(enabled, Ordering::SeqCst);
}

/// Returns the current mirror flag. Mostly for tests.
pub fn mirror_to_stderr() -> bool {
    MIRROR_TO_STDERR.load(Ordering::SeqCst)
}

/// Record one startup notice.
///
/// The line is always appended to `~/.pi/agent/logs/pi.log` (creating
/// the parent directory on first use) and additionally echoed to
/// stderr when [`set_mirror_to_stderr`] has been left at its default.
/// File write failures are silently swallowed: a non-writable log
/// directory must not abort the run before the user has even seen the
/// TUI.
pub fn log_message(line: &str) {
    if let Some(path) = crate::paths::startup_log_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = writeln!(file, "[{stamp}] {line}");
        }
    }
    if MIRROR_TO_STDERR.load(Ordering::SeqCst) {
        eprintln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_flag_toggles_independently_of_log_file() {
        let original = mirror_to_stderr();
        set_mirror_to_stderr(false);
        assert!(!mirror_to_stderr());
        set_mirror_to_stderr(true);
        assert!(mirror_to_stderr());
        set_mirror_to_stderr(original);
    }
}
