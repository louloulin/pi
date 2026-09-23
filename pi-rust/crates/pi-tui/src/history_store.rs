//! Cross-session composer history on disk (codex `ChatComposerHistory`'s
//! persistent half).
//!
//! The editor keeps the *session* history in memory; this module is the file
//! the next session reads it back from. It is deliberately tiny and
//! format-stable:
//!
//! * one JSON object per line — `{"text": "…"}` — appended in submission
//!   order (oldest first), so a reader can `tail` the file and a partial
//!   write can only ever damage its own last line;
//! * [`load`] is tolerant: a line that is not valid UTF-8 / JSON, or that
//!   carries no `text`, is skipped instead of failing the whole load. A
//!   corrupted history must never stop the TUI from starting;
//! * the newest [`crate::editor::HISTORY_LIMIT`] entries win, matching the
//!   in-memory cap.
//!
//! Location: `$PI_HISTORY_PATH` when set (tests and embeds use this),
//! otherwise `$PI_HOME/agent/history.jsonl`, otherwise
//! `~/.pi/agent/history.jsonl` — the same root the rest of the port uses for
//! user state (`crates/pi-coding-agent/src/paths.rs`). Nothing here touches
//! the network, and a read-only / missing file is not an error.
//!
//! Scope note (honest): only the prompt **text** is persisted. A composer
//! chip carries its image inline as base64 (`pi_protocol::ImageContent`),
//! so persisting chips would grow this file without bound; codex stores a
//! local image *path* instead. Chips therefore survive recall **within** a
//! session (the in-memory entry keeps them) but not across sessions.

use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::editor::HISTORY_LIMIT;

/// Cross-session composer history manager.
///
/// Wraps the file-based history operations into a simple interface.
#[derive(Debug)]
pub struct HistoryStore {
    path: PathBuf,
}

impl HistoryStore {
    /// Create a new history store for the given path.
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    /// Clear all history entries.
    pub fn clear(&self) -> std::io::Result<()> {
        // Rewrite with empty list to clear
        rewrite(&self.path, &[])
    }
}

/// Longest single record read from the file.
///
/// A pathological row (a pasted megabyte) is skipped rather than allowed to
/// balloon memory; the writer never produces one, so this only ever fires on
/// a hand-edited file.
const MAX_LINE_BYTES: usize = 256 * 1024;

/// One persisted history record.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    text: String,
}

/// The composer history file for this user, or `None` when no home
/// directory can be determined.
///
/// `$PI_HISTORY_PATH` wins so a test (or an embedder) can point at a
/// temporary file; `$PI_HOME` then overrides the `~/.pi` root, mirroring
/// `crates/pi-coding-agent/src/packages/installer.rs`.
pub fn default_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("PI_HISTORY_PATH") {
        if !explicit.is_empty() {
            return Some(PathBuf::from(explicit));
        }
    }
    if let Some(root) = std::env::var_os("PI_HOME") {
        if !root.is_empty() {
            return Some(PathBuf::from(root).join("agent").join("history.jsonl"));
        }
    }
    home_dir().map(|home| home.join(".pi").join("agent").join("history.jsonl"))
}

/// The user's home directory (`$HOME`, else `%USERPROFILE%`).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

/// Read the persisted prompts, oldest first, capped at [`HISTORY_LIMIT`].
///
/// A missing file yields an empty list; unparsable lines are skipped. The
/// cap keeps only the tail, because the newest entries are the ones a user
/// recalls.
///
/// The file is append-only, so this also **compacts** it: once it has grown
/// past the cap it is rewritten with just the entries returned (via a
/// temporary file + rename, so a crash mid-compaction leaves the old file
/// intact). Without that, a session a day would grow the file without bound
/// while the composer only ever offered the last [`HISTORY_LIMIT`] prompts.
/// Compaction is best-effort: a failure is ignored, because a read-only or
/// locked file must not stop the TUI from starting.
pub fn load(path: &Path) -> VecDeque<String> {
    let Ok(file) = fs::File::open(path) else {
        return VecDeque::new();
    };
    let mut lines: VecDeque<String> = VecDeque::new();
    let mut dropped = false;
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        let Some(text) = parse_line(&line) else {
            continue;
        };
        lines.push_back(text);
        if lines.len() > HISTORY_LIMIT {
            lines.pop_front();
            dropped = true;
        }
    }
    if dropped {
        let texts: Vec<String> = lines.iter().cloned().collect();
        let _ = rewrite(path, &texts);
    }
    lines
}

/// Parse one JSONL record, returning the prompt text it carries.
fn parse_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_LINE_BYTES {
        return None;
    }
    let record: Record = serde_json::from_str(trimmed).ok()?;
    let text = record.text.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.to_string())
}

/// Append one prompt to the file, creating the parent directory when
/// needed.
///
/// Writes are `append` + `flush`, so a crash can only truncate the line
/// being written and never rewrites earlier history. An I/O failure is
/// returned to the caller rather than swallowed — but the caller treats it
/// as best-effort, because losing persistence must not lose the prompt.
///
/// On Unix the file is created — and, if it already existed, re-chmodded —
/// `0600`: it holds everything the user typed, so it must not be
/// world-readable even when the process umask would allow it.
pub fn append(path: &Path, text: &str) -> std::io::Result<()> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let record = Record {
        text: text.to_string(),
    };
    let mut line = serde_json::to_string(&record).map_err(std::io::Error::other)?;
    line.push('\n');
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    restrict_permissions(&file)?;
    file.write_all(line.as_bytes())?;
    file.flush()
}

/// Force `0600` on an already-open file (Unix).
///
/// A no-op elsewhere: Windows inherits the directory's ACL and the port has
/// no cross-platform ACL helper. Doing this on every append also repairs a
/// file that arrived world-readable, instead of only fixing it when it is
/// first created.
#[cfg(unix)]
fn restrict_permissions(file: &fs::File) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_permissions(_file: &fs::File) -> std::io::Result<()> {
    Ok(())
}

/// Rewrite the file with exactly `texts` (oldest first).
///
/// Used to compact a file that has grown past the in-memory cap; the write
/// goes to a sibling temporary file and is then renamed over the target, so
/// a crash mid-compaction leaves the old file intact.
pub fn rewrite(path: &Path, texts: &[String]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        restrict_permissions(&file)?;
        for text in texts {
            let record = Record { text: text.clone() };
            let mut line = serde_json::to_string(&record).map_err(std::io::Error::other)?;
            line.push('\n');
            file.write_all(line.as_bytes())?;
        }
        file.flush()?;
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_records_and_skips_damage() {
        let dir = std::env::temp_dir().join(format!(
            "pi-history-store-{}-{}",
            std::process::id(),
            line!()
        ));
        let path = dir.join("history.jsonl");
        let _ = fs::remove_dir_all(&dir);

        append(&path, "first").unwrap();
        append(&path, "  second  ").unwrap();
        append(&path, "   ").unwrap();
        // Hand-written damage: not JSON, no `text`, empty text.
        {
            let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(b"not json\n{\"other\":1}\n{\"text\":\"\"}\n")
                .unwrap();
        }

        let loaded: Vec<String> = load(&path).into_iter().collect();
        assert_eq!(loaded, vec!["first".to_string(), "second".to_string()]);

        rewrite(&path, &["only".to_string()]).unwrap();
        let loaded: Vec<String> = load(&path).into_iter().collect();
        assert_eq!(loaded, vec!["only".to_string()]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_of_a_missing_file_is_empty_not_an_error() {
        let path = std::env::temp_dir().join("pi-history-does-not-exist-9c1f.jsonl");
        let _ = fs::remove_file(&path);
        assert!(load(&path).is_empty());
    }

    #[test]
    fn load_keeps_only_the_newest_entries_and_compacts_the_file() {
        let dir =
            std::env::temp_dir().join(format!("pi-history-cap-{}-{}", std::process::id(), line!()));
        let path = dir.join("history.jsonl");
        let _ = fs::remove_dir_all(&dir);
        let texts: Vec<String> = (0..(HISTORY_LIMIT + 10)).map(|i| format!("p{i}")).collect();
        rewrite(&path, &texts).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.len(), HISTORY_LIMIT);
        assert_eq!(loaded.front().map(String::as_str), Some("p10"));
        assert_eq!(
            loaded.back().map(String::as_str),
            Some(format!("p{}", HISTORY_LIMIT + 9).as_str())
        );
        // The load compacted the file instead of leaving it unbounded.
        let on_disk = fs::read_to_string(&path).unwrap().lines().count();
        assert_eq!(on_disk, HISTORY_LIMIT, "the file is rewritten to the cap");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn the_file_is_private_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "pi-history-perm-{}-{}",
            std::process::id(),
            line!()
        ));
        let path = dir.join("history.jsonl");
        let _ = fs::remove_dir_all(&dir);
        append(&path, "secret prompt").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "history holds everything the user typed");
        // A world-readable file is repaired on the next append.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        append(&path, "second").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = fs::remove_dir_all(&dir);
    }
}
