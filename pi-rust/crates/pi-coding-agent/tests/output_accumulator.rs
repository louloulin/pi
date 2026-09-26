//! Tests for [`OutputAccumulator`].
//!
//! Mirrors the behavioural contracts from upstream
//! `OutputAccumulator` (TS `output-accumulator.ts`):
//!
//! * small streams are left untouched and not spilled to disk
//! * streams above the byte/line limits force a temp-file spill and the
//!   snapshot is tail-truncated
//! * a finished accumulator refuses further appends
//! * `persist_if_truncated` creates the temp file even when the limits were
//!   not exceeded
//! * a partial UTF-8 sequence is replaced with U+FFFD (no panic, no half char)

#![cfg(not(target_arch = "wasm32"))]

use pi_coding_agent::tools::output_accumulator::{
    OutputAccumulator, OutputAccumulatorOptions, SnapshotOptions,
};
use pi_coding_agent::tools::truncate::{
    TruncatedBy, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES,
};

#[test]
fn small_stream_is_returned_intact_without_a_temp_file() {
    let mut acc = OutputAccumulator::new(OutputAccumulatorOptions {
        max_lines: 50,
        max_bytes: 1024,
        ..OutputAccumulatorOptions::default()
    });
    acc.append(b"hello\nworld\n");
    let snap = acc.snapshot(None);
    assert_eq!(snap.content, "hello\nworld\n");
    assert!(!snap.truncation.truncated);
    assert!(snap.full_output_path.is_none());
    assert_eq!(snap.truncation.total_lines, 2);
    assert_eq!(snap.truncation.total_bytes, "hello\nworld\n".len());
}

#[test]
fn line_limit_triggers_tail_truncation_and_temp_file_spill() {
    let mut acc = OutputAccumulator::new(OutputAccumulatorOptions {
        max_lines: 5,
        max_bytes: 64 * 1024,
        ..OutputAccumulatorOptions::default()
    });
    // 10 lines, well above max_lines (5).
    let payload = (1..=10)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    acc.append(payload.as_bytes());
    let snap = acc.snapshot(None);
    assert!(snap.truncation.truncated);
    assert_eq!(snap.truncation.truncated_by, Some(TruncatedBy::Lines));
    assert_eq!(snap.truncation.total_lines, 10);
    assert_eq!(snap.truncation.output_lines, 5);
    assert!(snap.content.contains("line 6"));
    assert!(snap.content.contains("line 10"));
    let temp_path = snap.full_output_path.expect("temp file path when truncated");
    assert!(temp_path.exists(), "spilled temp file must exist");
    assert_eq!(
        std::fs::read(&temp_path).unwrap(),
        payload.as_bytes(),
        "spilled file holds the full byte stream"
    );
    let _ = std::fs::remove_file(temp_path);
}

#[test]
fn byte_limit_triggers_tail_truncation_and_temp_file_spill() {
    let mut acc = OutputAccumulator::new(OutputAccumulatorOptions {
        max_lines: 1_000_000,
        max_bytes: 128,
        ..OutputAccumulatorOptions::default()
    });
    let payload = "x".repeat(1024);
    acc.append(payload.as_bytes());
    let snap = acc.snapshot(None);
    assert!(snap.truncation.truncated);
    assert_eq!(snap.truncation.truncated_by, Some(TruncatedBy::Bytes));
    assert!(snap.truncation.output_bytes <= 128);
    let temp_path = snap.full_output_path.expect("temp file path when bytes overflow");
    assert_eq!(std::fs::read(&temp_path).unwrap(), payload.as_bytes());
    let _ = std::fs::remove_file(temp_path);
}

#[test]
fn persist_if_truncated_creates_temp_file_only_when_truncated() {
    // Mirrors upstream: `persistIfTruncated` is a "spill to disk IF the
    // snapshot was truncated" hint. A short, in-bounds stream does not get
    // a temp file even when the caller asks for it.
    let mut acc = OutputAccumulator::new(OutputAccumulatorOptions {
        max_lines: DEFAULT_MAX_LINES,
        max_bytes: DEFAULT_MAX_BYTES,
        ..OutputAccumulatorOptions::default()
    });
    acc.append(b"two\nlines\n");
    let snap = acc.snapshot(Some(SnapshotOptions {
        persist_if_truncated: true,
    }));
    assert!(!snap.truncation.truncated);
    assert!(
        snap.full_output_path.is_none(),
        "short streams stay in memory when not truncated, even with persist_if_truncated"
    );
}

#[test]
fn persist_if_truncated_spills_a_truncated_snapshot_to_disk() {
    let mut acc = OutputAccumulator::new(OutputAccumulatorOptions {
        max_lines: 2,
        max_bytes: 1024,
        ..OutputAccumulatorOptions::default()
    });
    let payload = "line1\nline2\nline3\nline4\n".to_string();
    acc.append(payload.as_bytes());
    let snap = acc.snapshot(Some(SnapshotOptions {
        persist_if_truncated: true,
    }));
    assert!(snap.truncation.truncated);
    let temp_path = snap.full_output_path.expect("temp file path when truncated");
    assert_eq!(std::fs::read(&temp_path).unwrap(), payload.as_bytes());
    let _ = std::fs::remove_file(temp_path);
}

#[test]
fn finished_accumulator_rejects_further_appends() {
    let mut acc = OutputAccumulator::new(OutputAccumulatorOptions::default());
    acc.append(b"a");
    acc.finish();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        acc.append(b"b");
    }));
    assert!(
        result.is_err(),
        "appending to a finished accumulator must panic"
    );
}

#[test]
fn rolling_tail_trims_at_a_utf_8_boundary() {
    // 200 bytes of multi-byte content with a 32-byte rolling budget — every
    // trim must land on a code-point boundary, so the tail must remain valid
    // UTF-8.
    let mut acc = OutputAccumulator::new(OutputAccumulatorOptions {
        max_lines: 1_000_000,
        max_bytes: 32,
        ..OutputAccumulatorOptions::default()
    });
    let payload = "é".repeat(100); // 2 bytes each = 200 bytes total
    acc.append(payload.as_bytes());
    let snap = acc.snapshot(None);
    // Each `é` is 2 bytes; the tail must contain only whole characters.
    assert!(snap.content.chars().all(|c| c == 'é'));
    assert!(snap.content.len() <= 64); // up to 2 * max_rolling_bytes
}

#[test]
fn get_last_line_bytes_accumulates_across_chunks() {
    // Mirrors upstream `getLastLineBytes`: when a chunk has no `\n`, the
    // bytes are added to `currentLineBytes` (the open in-flight line). When
    // a chunk has at least one `\n`, the open line is replaced with the tail
    // after the last newline — i.e. the previous open line is "committed"
    // by the closing `\n`.
    let mut acc = OutputAccumulator::new(OutputAccumulatorOptions::default());
    acc.append(b"first line\npartial");
    // "partial" is the open line after the closing `\n`.
    assert_eq!(acc.get_last_line_bytes(), b"partial".len());
    // No newline in this chunk — `currentLineBytes` grows by 5.
    acc.append(b" rest");
    assert_eq!(
        acc.get_last_line_bytes(),
        b"partial".len() + b" rest".len(),
        "the open line keeps growing across newline-free chunks"
    );
    // Closing newline commits the open line; no bytes remain.
    acc.append(b" end\n");
    assert_eq!(acc.get_last_line_bytes(), 0);
}