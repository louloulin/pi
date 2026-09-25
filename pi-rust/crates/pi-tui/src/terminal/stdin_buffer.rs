//! Buffered stdin reader — Rust port of upstream's `StdinBuffer`
//! (`packages/tui/src/stdin-buffer.ts`).
//!
//! The reader accumulates input bytes until either:
//!
//! * a complete escape sequence / keypress is detected (so the
//!   `parseKey` parser can consume a single key unit), or
//! * the caller flushes via [`StdinBuffer::drain`].
//!
//! Upstream's StdinBuffer handles the pipe-mode vs tty-mode split:
//! when stdin is a TTY it expects raw keypresses; when stdin is a
//! pipe it expects whole lines. The Rust port surfaces the same
//! distinction through [`StdinBufferOptions::pipe_mode`].
//!
//! The current implementation is a pure, side-effect-free buffer:
//! production code reads from a `crossterm` event source and pushes
//! bytes in via [`StdinBuffer::push`]. Tests can drive it without
//! touching the terminal.

use std::collections::VecDeque;

/// Options that configure [`StdinBuffer`].
#[derive(Debug, Clone, Default)]
pub struct StdinBufferOptions {
    /// Whether stdin is a pipe (whole lines) or a TTY (raw bytes).
    /// The buffer splits on `\n` when `pipe_mode = true`.
    pub pipe_mode: bool,
    /// Maximum buffer size in bytes before the oldest bytes are
    /// dropped. Defaults to 64 KiB.
    pub max_buffer: usize,
}

impl StdinBufferOptions {
    /// Build options for a TTY (raw bytes, no `\n` splitting).
    pub fn tty() -> Self {
        Self { pipe_mode: false, max_buffer: 64 * 1024 }
    }

    /// Build options for a pipe (line splitting, larger buffer).
    pub fn pipe() -> Self {
        Self { pipe_mode: true, max_buffer: 1024 * 1024 }
    }
}

/// Event names emitted by `StdinBuffer`. Mirrors the TS string-literal
/// union upstream uses for `StdinBufferEventMap`.
pub trait StdinBufferEventMap {}

/// Buffered reader. Bytes pushed via [`push`](Self::push) are queued
/// until [`drain`](Self::drain) reads them out as complete units
/// (lines in pipe mode, single bytes / sequences in TTY mode).
#[derive(Debug, Clone)]
pub struct StdinBuffer {
    opts: StdinBufferOptions,
    /// Bytes that haven't been split into a unit yet.
    pending: Vec<u8>,
    /// Complete units ready to drain.
    ready: VecDeque<Vec<u8>>,
}

impl StdinBuffer {
    /// Build a buffer with the given options.
    pub fn new(opts: StdinBufferOptions) -> Self {
        Self { opts, pending: Vec::new(), ready: VecDeque::new() }
    }

    /// Whether the buffer is in pipe mode.
    pub fn is_pipe_mode(&self) -> bool {
        self.opts.pipe_mode
    }

    /// Number of bytes currently buffered (pending + ready).
    pub fn buffered(&self) -> usize {
        self.pending.len() + self.ready.iter().map(|v| v.len()).sum::<usize>()
    }

    /// Push bytes from the source. In TTY mode the bytes are queued
    /// as a single unit; in pipe mode the bytes are split on `\n`.
    pub fn push(&mut self, data: &[u8]) {
        if self.opts.pipe_mode {
            for &b in data {
                self.pending.push(b);
                if b == b'\n' {
                    let line: Vec<u8> = std::mem::take(&mut self.pending);
                    self.ready.push_back(line);
                }
            }
            self.enforce_limit();
        } else {
            if !data.is_empty() {
                self.ready.push_back(data.to_vec());
                self.enforce_limit();
            }
        }
    }

    /// Drain all ready units.
    pub fn drain(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.ready).into_iter().collect()
    }

    /// Pop the oldest ready unit.
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        self.ready.pop_front()
    }

    /// Clear pending and ready data.
    pub fn clear(&mut self) {
        self.pending.clear();
        self.ready.clear();
    }

    fn enforce_limit(&mut self) {
        let total = self.buffered();
        if total > self.opts.max_buffer {
            let drop = total - self.opts.max_buffer;
            let mut remaining = drop;
            // Drop from ready first, then from pending.
            while remaining > 0 {
                if let Some(front) = self.ready.front_mut() {
                    if front.len() <= remaining {
                        remaining -= front.len();
                        self.ready.pop_front();
                    } else {
                        front.drain(..remaining);
                        remaining = 0;
                    }
                } else if !self.pending.is_empty() {
                    let take = remaining.min(self.pending.len());
                    self.pending.drain(..take);
                    remaining -= take;
                } else {
                    break;
                }
            }
        }
    }
}

impl Default for StdinBuffer {
    fn default() -> Self {
        Self::new(StdinBufferOptions::tty())
    }
}

impl StdinBufferEventMap for StdinBuffer {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_mode_splits_on_newline() {
        let mut buf = StdinBuffer::new(StdinBufferOptions::pipe());
        buf.push(b"hello\nworld\n");
        let lines = buf.drain();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], b"hello\n");
        assert_eq!(lines[1], b"world\n");
    }

    #[test]
    fn tty_mode_keeps_bytes_verbatim() {
        let mut buf = StdinBuffer::new(StdinBufferOptions::tty());
        buf.push(b"\x1b[A");
        let units = buf.drain();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0], b"\x1b[A");
    }

    #[test]
    fn pending_carries_partial_line() {
        let mut buf = StdinBuffer::new(StdinBufferOptions::pipe());
        buf.push(b"hello");
        assert!(buf.drain().is_empty());
        assert_eq!(buf.pending, b"hello");
        buf.push(b"\n");
        let lines = buf.drain();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"hello\n");
    }

    #[test]
    fn max_buffer_drops_oldest_units() {
        // Capacity 5 bytes. Push three 4-byte lines.
        let mut buf = StdinBuffer::new(StdinBufferOptions { pipe_mode: true, max_buffer: 5 });
        buf.push(b"aaaa\n");
        buf.push(b"bbbb\n");
        // After the second line (10 bytes total) the cap kicks in:
        // the first line ("aaaa\n", 5 bytes) is dropped entirely.
        buf.push(b"cccc\n");
        assert_eq!(buf.buffered(), 5);
        let units = buf.drain();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0], b"cccc\n");
    }

    #[test]
    fn buffered_counts_both_queues() {
        let mut buf = StdinBuffer::new(StdinBufferOptions::tty());
        buf.push(b"abc");
        buf.push(b"defg");
        assert_eq!(buf.buffered(), 7);
        buf.drain();
        assert_eq!(buf.buffered(), 0);
    }

    #[test]
    fn pop_returns_oldest_unit() {
        let mut buf = StdinBuffer::new(StdinBufferOptions::tty());
        buf.push(b"first");
        buf.push(b"second");
        assert_eq!(buf.pop().unwrap(), b"first");
        assert_eq!(buf.pop().unwrap(), b"second");
        assert!(buf.pop().is_none());
    }
}