//! Cross-session prompt history (`~/.pi/agent/history.jsonl`).
//!
//! Ports the persistence half of codex's `ChatComposerHistory`
//! (`codex-rs/tui/src/bottom_pane/chat_composer_history.rs` +
//! `codex-rs/core/src/message_history.rs`): every submitted prompt is
//! appended as one JSON line, and the tail of the file is read back when the
//! composer starts, so `Up` / `Down` reach prompts from previous processes.
//!
//! Deliberate differences from codex (see
//! `docs/LUM1319_HISTORY_PERSISTENCE.md` §4 for the row-by-row comparison):
//!
//! * codex fetches persistent entries **on demand**, one bounded batch at a
//!   time, and answers each request with an `AppEvent`; this port is
//!   synchronous and loads the tail once (`load`). A single file read costs
//!   far less than the request/response protocol would, and the composer has
//!   no async plumbing to carry a `Pending` search state.
//! * codex's `message_history.rs` trims by **bytes** (a 1 MiB cap) and keeps
//!   a separate append log per thread; this port trims by **rows** (the
//!   composer's own [`crate::editor::HISTORY_LIMIT`]).
//!
//! Failure policy (the issue's "文件损坏/缺字段要降级为只有会话内历史"):
//! every error — missing file, unreadable file, a line that is not JSON, a
//! JSON object without a string `text` — is absorbed here. [`HistoryStore::load`]
//! skips the offending line and returns what it could read; a store that
//! cannot read at all returns an empty vector rather than an error, and the
//! append path reports its `io::Error` so the caller can decide (the editor
//! ignores it and keeps the in-session history).
//!
//! Privacy: the file holds **prompt text only** — never attachments, never
//! tool output (codex stores attachments for in-session recall only, and this
//! port does the same). On Unix the file is created `0600` and re-chmodded on
//! every append, so a `history.jsonl` that arrived world-readable is fixed on
//! the next submission.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Rows kept in the file. Matches [`crate::HISTORY_LIMIT`], the in-session
/// bound, so a restart cannot silently widen the history the composer offers.
pub const DEFAULT_HISTORY_FILE_LIMIT: usize = crate::editor::HISTORY_LIMIT;

/// Maximum bytes read for a single line. A file that grew a pathological row
/// (a pasted megabyte) is truncated rather than allowed to balloon memory.
const MAX_LINE_BYTES: u64 = 256 * 1024;

/// One prompt history file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryStore {
    path: PathBuf,
    limit: usize,
}

impl HistoryStore {
    /// A store over `path`, keeping [`DEFAULT_HISTORY_FILE_LIMIT`] rows.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::with_limit(path, DEFAULT_HISTORY_FILE_LIMIT)
    }

    /// A store over `path` keeping at most `limit` rows (0 is raised to 1:
    /// a history file that can hold nothing could never be trimmed).
    pub fn with_limit(path: impl Into<PathBuf>, limit: usize) -> Self {
        Self {
            path: path.into(),
            limit: limit.max(1),
        }
    }

    /// The file this store reads and writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many rows the file keeps.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Read the file's `text` values, **oldest first**.
    ///
    /// Only the last `limit` rows are returned. Unparseable lines are skipped;
    /// a missing or unreadable file reads as empty.
    pub fn load(&self) -> Vec<String> {
        let Ok(file) = File::open(&self.path) else {
            return Vec::new();
        };
        let reader = BufReader::new(file);
        let mut texts: Vec<String> = Vec::new();
        for line in reader.split(b'\n') {
            let Ok(bytes) = line else {
                // A read error mid-file: keep what was parsed so far.
                break;
            };
            if bytes.len() as u64 > MAX_LINE_BYTES {
                continue;
            }
            let Ok(line) = std::str::from_utf8(&bytes) else {
                continue;
            };
            let Some(text) = parse_line(line) else {
                continue;
            };
            texts.push(text);
        }
        if texts.len() > self.limit {
            texts.drain(..texts.len() - self.limit);
        }
        texts
    }

    /// Append one prompt to the file, trimming it to `limit` rows when it
    /// grew past that.
    ///
    /// Returns the number of rows dropped by the trim (0 when nothing was).
    /// Empty / whitespace-only text is refused (in-session history drops it
    /// too), as is text carrying a raw newline: one row per prompt is what
    /// keeps the JSONL format line-addressable.
    pub fn append(&self, text: &str) -> std::io::Result<usize> {
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.contains('\n') {
            return Ok(0);
        }
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&self.path)?;
        let row = serde_json::json!({ "ts": now_unix(), "text": trimmed });
        let mut line = serde_json::to_string(&row).unwrap_or_else(|_| String::new());
        if line.is_empty() {
            return Ok(0);
        }
        line.push('\n');
        file.write_all(line.as_bytes())?;
        file.flush()?;
        restrict_permissions(&self.path);
        self.trim()
    }

    /// Delete the file (the explicit `/clear-history` entry point).
    ///
    /// A missing file is not an error: the caller asked for "no history",
    /// which is already true.
    pub fn clear(&self) -> std::io::Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Rewrite the file with only its newest `limit` rows; returns how many
    /// rows were dropped.
    fn trim(&self) -> std::io::Result<usize> {
        let Ok(file) = File::open(&self.path) else {
            return Ok(0);
        };
        let reader = BufReader::new(file);
        let mut lines: Vec<String> = Vec::new();
        let mut corrupt = 0usize;
        for line in reader.split(b'\n') {
            let Ok(bytes) = line else { break };
            let Ok(line) = std::str::from_utf8(&bytes) else {
                corrupt += 1;
                continue;
            };
            if line.trim().is_empty() {
                continue;
            }
            if parse_line(line).is_none() {
                corrupt += 1;
                continue;
            }
            lines.push(line.to_string());
        }
        if lines.len() <= self.limit && corrupt == 0 {
            return Ok(0);
        }
        let dropped = lines.len().saturating_sub(self.limit);
        let keep = lines.split_off(dropped);
        let mut body = String::new();
        for line in &keep {
            body.push_str(line);
            body.push('\n');
        }
        // Write through a sibling temp file and rename, so a crash mid-trim
        // cannot leave a half-written history behind.
        let tmp = self.path.with_extension("jsonl.tmp");
        {
            let mut options = OpenOptions::new();
            options.create(true).write(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp)?;
            file.write_all(body.as_bytes())?;
            file.flush()?;
        }
        fs::rename(&tmp, &self.path)?;
        restrict_permissions(&self.path);
        Ok(dropped + corrupt)
    }
}

/// Parse one history row, returning its `text` value.
///
/// Tolerant by construction: any JSON value that is not an object with a
/// non-empty string `text` field yields `None` and the row is skipped.
fn parse_line(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let text = value.get("text")?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.to_string())
}

/// Seconds since the Unix epoch (0 when the clock predates it).
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|delta| delta.as_secs())
        .unwrap_or(0)
}

/// Tighten the file to owner-only.
///
/// Best effort: a failure here must never cost the user the appended row.
#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

/// Windows has no `mode`; nothing to tighten.
#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(label: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("pi-tui-history-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("history.jsonl")
    }

    #[test]
    fn appends_and_reads_back_oldest_first() {
        let path = temp_path("roundtrip");
        let store = HistoryStore::new(&path);
        store.append("first").unwrap();
        store.append("second").unwrap();
        store.append("third").unwrap();
        assert_eq!(store.load(), vec!["first", "second", "third"]);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn skips_corrupt_rows_and_missing_fields() {
        let path = temp_path("corrupt");
        let store = HistoryStore::new(&path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = "not json at all\n\
                    {\"session_id\":\"abc\",\"ts\":1,\"text\":\"kept one\"}\n\
                    {\"text\":42}\n\
                    {\"ts\":2}\n\
                    \n\
                    {\"text\":\"  \"}\n\
                    {\"text\":\"kept two\"}\n";
        fs::write(&path, body).unwrap();
        assert_eq!(store.load(), vec!["kept one", "kept two"]);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_missing_file_reads_as_empty_and_clear_is_idempotent() {
        let path = temp_path("missing");
        let store = HistoryStore::new(&path);
        assert!(store.load().is_empty());
        store.clear().unwrap();
        store.clear().unwrap();
    }

    #[test]
    fn trims_the_file_to_the_limit() {
        let path = temp_path("trim");
        let store = HistoryStore::with_limit(&path, 3);
        let mut dropped = 0;
        for index in 0..7 {
            dropped += store.append(&format!("entry-{index}")).unwrap();
        }
        assert_eq!(dropped, 4, "entries 0..3 are dropped as they age out");
        assert_eq!(store.load(), vec!["entry-4", "entry-5", "entry-6"]);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn refuses_blank_and_multiline_rows() {
        let path = temp_path("blank");
        let store = HistoryStore::new(&path);
        assert_eq!(store.append("   ").unwrap(), 0);
        assert_eq!(store.append("two\nlines").unwrap(), 0);
        assert!(store.load().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_path("mode");
        let store = HistoryStore::new(&path);
        // A pre-existing world-readable file is tightened on the next append.
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        store.append("secret").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "history.jsonl must be owner-only");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_long_row_is_written_and_read_back() {
        let path = temp_path("long");
        let store = HistoryStore::new(&path);
        let long = "x".repeat(4096);
        store.append(&long).unwrap();
        assert_eq!(store.load(), vec![long]);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn load_keeps_only_the_newest_rows() {
        let path = temp_path("load-window");
        let store = HistoryStore::with_limit(&path, 2);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = "{\"text\":\"a\"}\n{\"text\":\"b\"}\n{\"text\":\"c\"}\n";
        fs::write(&path, body).unwrap();
        assert_eq!(store.load(), vec!["b", "c"]);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
