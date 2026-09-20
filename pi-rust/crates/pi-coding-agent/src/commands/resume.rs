//! `/resume` slash command handler.
//!
//! Lists every SQLite-backed session in the configured session
//! directory, sorted by `created_at` descending (newest first), and
//! returns the user's pick as a [`SessionRef`] the TUI can attach to.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Context;
use pi_session::SessionReader;

/// One resumable session as the TUI surfaces it.
#[derive(Debug, Clone)]
pub struct SessionRef {
    /// Path to the SQLite file that holds the session.
    pub database: PathBuf,
    /// Session identifier (the `id` field of the header row).
    pub session_id: String,
    /// Wall-clock timestamp the session was created (milliseconds since
    /// the unix epoch).
    pub created_at: i64,
    /// Optional pi version stamped on the session.
    pub version: Option<String>,
    /// Optional cwd stamped on the session header.
    pub cwd: Option<String>,
    /// Total number of entries in the session.
    pub entry_count: i64,
    /// Display name set with `/name`, when the session has one (stored in
    /// `sessions.metadata.name`).
    pub name: Option<String>,
}

impl SessionRef {
    /// Human-readable display string used by the TUI selector.
    pub fn display(&self) -> String {
        let ts = format_timestamp(self.created_at);
        let ver = self.version.as_deref().unwrap_or("?");
        match self.name.as_deref() {
            Some(name) => format!(
                "{ts} · {ver} · {} entries · {name} · {}",
                self.entry_count, self.session_id
            ),
            None => format!(
                "{ts} · {ver} · {} entries · {}",
                self.entry_count, self.session_id
            ),
        }
    }
}

/// Discover every SQLite session in `directory` (one entry per
/// `(database, session_id)` pair). When the directory does not exist
/// the result is an empty Vec — `/resume` reports "no saved sessions".
pub fn list_resumable(directory: &Path) -> anyhow::Result<Vec<SessionRef>> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut refs = Vec::new();
    let entries = std::fs::read_dir(directory)
        .with_context(|| format!("reading session directory {}", directory.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s != "sqlite")
            .unwrap_or(true)
        {
            continue;
        }
        let reader = match SessionReader::open(&path) {
            Ok(reader) => reader,
            Err(err) => {
                // Non-blocking: skip corrupt files but keep listing the
                // rest. The TUI surfaces the error inline if the user
                // picks a damaged file.
                eprintln!("pi: skipping {}: {err}", path.display());
                continue;
            }
        };
        for session in reader
            .list_sessions()
            .with_context(|| format!("listing sessions in {}", path.display()))?
        {
            let count = reader
                .count_entries(&session.id)
                .with_context(|| format!("counting entries for {}", session.id))?;
            refs.push(SessionRef {
                database: path.clone(),
                session_id: session.id,
                created_at: session.created_at,
                version: session.version,
                cwd: session.cwd,
                entry_count: count,
                name: pi_session::session_name_from_metadata(session.metadata.as_deref()),
            });
        }
    }
    // Newest first; tiebreak on session id for stability.
    refs.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    Ok(refs)
}

/// Format an i64 millisecond timestamp as `YYYY-MM-DD HH:MM:SSZ` for the
/// TUI's selector display.
fn format_timestamp(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(ms, 0)
        .map(|t| t.format("%Y-%m-%d %H:%M:%SZ").to_string())
        .unwrap_or_else(|| format!("@{ms}"))
}

/// Resolve a `/resume <arg>` argument into a [`SessionRef`]. `arg` may be
/// either a session id (matched against every database in `directory`)
/// or a direct path to a `.sqlite` file.
pub fn resolve(directory: &Path, arg: &str) -> anyhow::Result<SessionRef> {
    let direct = Path::new(arg);
    if direct.is_file() && direct.extension().and_then(|s| s.to_str()) == Some("sqlite") {
        let reader = SessionReader::open(direct)?;
        let session = reader
            .latest_session()?
            .or_else(|| {
                reader
                    .list_sessions()
                    .ok()
                    .and_then(|s| s.into_iter().next())
            })
            .ok_or_else(|| anyhow::anyhow!("session database {arg:?} is empty"))?;
        let entry_count = reader.count_entries(&session.id)?;
        return Ok(SessionRef {
            database: direct.to_path_buf(),
            session_id: session.id,
            created_at: session.created_at,
            version: session.version,
            cwd: session.cwd,
            entry_count,
            name: pi_session::session_name_from_metadata(session.metadata.as_deref()),
        });
    }
    let candidates = list_resumable(directory)?;
    let matched = candidates
        .into_iter()
        .find(|cand| cand.session_id == arg)
        .ok_or_else(|| anyhow::anyhow!("session {arg:?} not found in {}", directory.display()))?;
    Ok(matched)
}

#[allow(dead_code)]
fn _touch_system_time(_t: SystemTime) {}
