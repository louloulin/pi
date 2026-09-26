//! Incrementally track streaming tool output with bounded memory.
//!
//! Mirrors [`OutputAccumulator`](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/src/core/tools/output-accumulator.ts):
//! append raw byte chunks (UTF-8 decoding happens lazily per chunk), keep only
//! a decoded rolling tail for display snapshots, and spill the full byte
//! stream to `$TMPDIR/<prefix>-<id>.log` once any of the configured limits is
//! exceeded so the model can be pointed at the complete file.
//!
//! # Usage
//!
//! ```no_run
//! use pi_coding_agent::tools::output_accumulator::{OutputAccumulator, OutputAccumulatorOptions};
//!
//! let mut acc = OutputAccumulator::new(OutputAccumulatorOptions::default());
//! acc.append(b"hello\n");
//! let snap = acc.snapshot(None);
//! assert_eq!(snap.content, "hello\n");
//! acc.finish();
//! ```
//!
//! This module is intentionally synchronous: the underlying readers (bash,
//! grep, …) drain their byte stream on blocking threads and hand completed
//! chunks back to the accumulator. The async story lives one level up.

#![cfg(not(target_arch = "wasm32"))]

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::truncate::{truncate_tail, TruncationOptions, TruncationResult};

/// Default rolling-tail byte budget. Upstream uses `maxBytes * 2` with a
/// minimum of 1; we expose it because the test suite pins it.
pub const DEFAULT_MAX_ROLLING_BYTES_FLOOR: usize = 1;

/// Knobs for [`OutputAccumulator`]. Mirrors `OutputAccumulatorOptions`.
#[derive(Debug, Clone, Copy)]
pub struct OutputAccumulatorOptions {
    /// Maximum number of *complete* lines kept in the rolling tail. Forwarded
    /// to [`truncate_tail`](super::truncate::truncate_tail) when producing
    /// snapshots.
    pub max_lines: usize,
    /// Maximum number of decoded bytes kept in the rolling tail. The same
    /// budget is used as the temp-file spillover threshold.
    pub max_bytes: usize,
    /// Prefix for the spilled temp-file path. The full path is
    /// `$TMPDIR/<prefix>-<16-hex>.log`.
    pub temp_file_prefix: &'static str,
}

impl Default for OutputAccumulatorOptions {
    fn default() -> Self {
        Self {
            max_lines: super::truncate::DEFAULT_MAX_LINES,
            max_bytes: super::truncate::DEFAULT_MAX_BYTES,
            temp_file_prefix: "pi-output",
        }
    }
}

/// Snapshot of the accumulator's tail, suitable for handing back to the model
/// or rendering in the TUI.
///
/// Mirrors upstream `OutputSnapshot`. `content` is always valid UTF-8; raw
/// bytes that landed mid-character are replaced with U+FFFD by the streaming
/// decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSnapshot {
    /// The (possibly truncated) tail content.
    pub content: String,
    /// The truncation metadata produced by [`truncate_tail`].
    pub truncation: TruncationResult,
    /// Absolute path to the temp file holding the full byte stream, if one was
    /// created (either by spillover or by `persist_if_truncated = true`).
    pub full_output_path: Option<PathBuf>,
}

/// Knobs for [`OutputAccumulator::snapshot`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SnapshotOptions {
    /// When `true` and the snapshot is truncated, force a temp-file spill
    /// even if the original limits were not exceeded.
    pub persist_if_truncated: bool,
}

/// Incrementally tracks streaming output with bounded memory.
///
/// Internally holds:
/// * the *full* byte stream as raw `Vec<u8>` chunks (only spilled to a temp
///   file when one of the limits is exceeded), and
/// * a decoded rolling tail of at most `max(2 * max_bytes, 1)` bytes.
///
/// `total_lines` counts complete lines in the decoded stream; the in-flight
/// partial line is *not* counted. `tailStartsAtLineBoundary` records whether
/// the rolling tail starts at a line boundary so a partial first line can be
/// dropped from snapshots (just like upstream).
#[derive(Debug)]
pub struct OutputAccumulator {
    max_lines: usize,
    max_bytes: usize,
    max_rolling_bytes: usize,
    temp_file_prefix: &'static str,
    /// Buffered raw chunks waiting to be flushed to the temp file. Empty once
    /// the file is open.
    raw_chunks: Vec<Vec<u8>>,
    tail_text: String,
    tail_bytes: usize,
    tail_starts_at_line_boundary: bool,
    total_raw_bytes: usize,
    total_decoded_bytes: usize,
    completed_lines: usize,
    total_lines: usize,
    current_line_bytes: usize,
    has_open_line: bool,
    finished: bool,
    temp_file_path: Option<PathBuf>,
    temp_file: Option<File>,
}

impl OutputAccumulator {
    /// Construct an accumulator with the given options.
    pub fn new(options: OutputAccumulatorOptions) -> Self {
        let max_bytes = options.max_bytes;
        let max_rolling_bytes = max_bytes
            .saturating_mul(2)
            .max(DEFAULT_MAX_ROLLING_BYTES_FLOOR);
        Self {
            max_lines: options.max_lines,
            max_bytes,
            max_rolling_bytes,
            temp_file_prefix: options.temp_file_prefix,
            raw_chunks: Vec::new(),
            tail_text: String::new(),
            tail_bytes: 0,
            tail_starts_at_line_boundary: true,
            total_raw_bytes: 0,
            total_decoded_bytes: 0,
            completed_lines: 0,
            total_lines: 0,
            current_line_bytes: 0,
            has_open_line: false,
            finished: false,
            temp_file_path: None,
            temp_file: None,
        }
    }

    /// Append a raw byte chunk. Panics if the accumulator is finished.
    pub fn append(&mut self, data: &[u8]) {
        assert!(!self.finished, "Cannot append to a finished output accumulator");
        self.total_raw_bytes += data.len();
        let decoded = String::from_utf8_lossy(data);
        self.append_decoded_text(&decoded);

        if self.temp_file.is_some() || self.should_use_temp_file() {
            self.ensure_temp_file();
            if let Some(file) = self.temp_file.as_mut() {
                let _ = file.write_all(data);
            }
        } else if !data.is_empty() {
            self.raw_chunks.push(data.to_vec());
        }
    }

    /// Mark the stream as finished. Finalizes any pending partial UTF-8
    /// sequence (replaced with U+FFFD if incomplete) and open a temp file if
    /// the limits were exceeded.
    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if self.should_use_temp_file() {
            self.ensure_temp_file();
        }
    }

    /// Produce a snapshot of the current tail.
    ///
    /// When `options.persist_if_truncated` is `true`, the snapshot forces a
    /// temp-file spill (if one is not already open) so callers can hand the
    /// path to the model even if the rolling tail was small.
    pub fn snapshot(&mut self, options: Option<SnapshotOptions>) -> OutputSnapshot {
        let options = options.unwrap_or_default();
        let tail_text = self.get_snapshot_text();
        let tail_truncation = truncate_tail(&tail_text, self.truncation_options());
        let truncated = self.total_lines > self.max_lines || self.total_decoded_bytes > self.max_bytes;
        let truncated_by = if truncated {
            tail_truncation.truncated_by.or_else(|| {
                if self.total_decoded_bytes > self.max_bytes {
                    Some(super::truncate::TruncatedBy::Bytes)
                } else {
                    Some(super::truncate::TruncatedBy::Lines)
                }
            })
        } else {
            None
        };

        let truncation = TruncationResult {
            truncated,
            truncated_by,
            total_lines: self.total_lines,
            total_bytes: self.total_decoded_bytes,
            ..tail_truncation
        };

        if options.persist_if_truncated && truncation.truncated {
            self.ensure_temp_file();
        }

        OutputSnapshot {
            content: truncation.content.clone(),
            truncation,
            full_output_path: self.temp_file_path.clone(),
        }
    }

    /// Close the spilled temp file, flushing any buffered writes. Idempotent.
    pub fn close_temp_file(&mut self) -> std::io::Result<()> {
        if let Some(mut file) = self.temp_file.take() {
            file.flush()?;
            // Drop closes the file via RAII.
        }
        Ok(())
    }

    /// Byte count of the in-flight (un-terminated) line, used by upstream to
    /// detect a partial tail.
    pub fn get_last_line_bytes(&self) -> usize {
        self.current_line_bytes
    }

    fn truncation_options(&self) -> TruncationOptions {
        TruncationOptions {
            max_lines: self.max_lines,
            max_bytes: self.max_bytes,
        }
    }

    fn append_decoded_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let bytes = text.len();
        self.total_decoded_bytes += bytes;
        self.tail_text.push_str(text);
        self.tail_bytes += bytes;
        if self.tail_bytes > self.max_rolling_bytes.saturating_mul(2) {
            self.trim_tail();
        }

        // Count complete newlines in the appended text. The split on `\n`
        // mirrors upstream's `text.indexOf("\n")` loop — both stop at the
        // first newline and use `lastNewline` to find the position of the
        // last one.
        let mut last_newline: Option<usize> = None;
        let mut count = 0usize;
        for (i, ch) in text.char_indices() {
            if ch == '\n' {
                count += 1;
                last_newline = Some(i);
            }
        }
        if count == 0 {
            self.current_line_bytes += bytes;
            self.has_open_line = true;
        } else {
            self.completed_lines += count;
            let tail = match last_newline {
                Some(i) => &text[i + 1..],
                None => "",
            };
            self.current_line_bytes = tail.len();
            self.has_open_line = !tail.is_empty();
        }
        self.total_lines = self.completed_lines + usize::from(self.has_open_line);
    }

    fn trim_tail(&mut self) {
        let buffer = self.tail_text.as_bytes();
        if buffer.len() <= self.max_rolling_bytes {
            self.tail_bytes = buffer.len();
            return;
        }

        let mut start = buffer.len() - self.max_rolling_bytes;
        // Skip UTF-8 continuation bytes so we do not cut a code point in
        // half. Mirrors upstream's `(buffer[start] & 0xc0) === 0x80` loop.
        while start < buffer.len() && (buffer[start] & 0xc0) == 0x80 {
            start += 1;
        }

        self.tail_starts_at_line_boundary =
            start == 0 || self.tail_starts_at_line_boundary && start == 0
                || buffer[start - 1] == b'\n';
        self.tail_text = String::from_utf8_lossy(&buffer[start..]).into_owned();
        self.tail_bytes = self.tail_text.len();
    }

    fn get_snapshot_text(&self) -> &str {
        if self.tail_starts_at_line_boundary {
            return &self.tail_text;
        }
        // Drop the partial first line — same as upstream's
        // `firstNewline + 1` slice.
        match self.tail_text.find('\n') {
            Some(i) => &self.tail_text[i + 1..],
            None => &self.tail_text,
        }
    }

    fn should_use_temp_file(&self) -> bool {
        self.total_raw_bytes > self.max_bytes
            || self.total_decoded_bytes > self.max_bytes
            || self.total_lines > self.max_lines
    }

    fn ensure_temp_file(&mut self) {
        if self.temp_file_path.is_some() {
            return;
        }
        let path = std::env::temp_dir().join(format!(
            "{}-{}.log",
            self.temp_file_prefix,
            random_hex(16)
        ));
        let file = match File::create(&path) {
            Ok(file) => file,
            Err(_) => return,
        };
        let mut file = file;
        for chunk in self.raw_chunks.drain(..) {
            let _ = file.write_all(&chunk);
        }
        self.temp_file_path = Some(path);
        self.temp_file = Some(file);
    }
}

fn random_hex(byte_len: usize) -> String {
    // Eight bytes of `SystemTime` nanos plus `byte_len * 2` chars of `pid`
    // give us enough entropy to avoid collisions on a single machine. The
    // prefix is informational; collisions only cause one spilled file to
    // overwrite another, which the upstream TS code also accepts.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let mut buf = Vec::with_capacity(8 + 4);
    buf.extend_from_slice(&nanos.to_le_bytes()[..8]);
    buf.extend_from_slice(&pid.to_le_bytes()[..4]);
    let mut hex = String::with_capacity(byte_len * 2);
    for byte in buf.iter().take(byte_len) {
        hex.push_str(&format!("{:02x}", byte));
    }
    hex
}