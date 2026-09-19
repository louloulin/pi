//! JSONL export — the inverse of [`crate::migrate_jsonl`].
//!
//! [`export_jsonl`] serialises a session stored in the SQLite backend
//! back to the Stage 4 JSONL format (one
//! [`SessionEntry`](pi_protocol::SessionEntry) per line, preceded by the
//! session header line). The output is exactly what
//! [`crate::migrate_jsonl`] consumes, so `export` followed by `migrate`
//! is the identity on the `entries` table.
//!
//! The function mirrors `exportSessionToJsonl` in
//! `packages/coding-agent/src/core/session-export.ts` (write the current
//! session branch as JSONL), with two deliberate differences:
//!
//! 1. **The header line uses the Rust `session_entry` shape**
//!    (`{"type":"header","id":…,"created_at":…,"version":…}`) rather than
//!    the TS `{"type":"session",…}` spelling, because the Rust
//!    [`SessionEntry`](pi_protocol::SessionEntry) enum — and therefore
//!    [`crate::migrate_jsonl`] — tags the header variant as `header`.
//!    Emitting the TS spelling here would produce a file this port
//!    cannot read back.
//! 2. **Exactly one header line, always first.** The `sessions` table is
//!    the source of truth for the header; a `header` row stored in the
//!    `entries` table (the TS writer emits one) is folded into that line
//!    instead of being written a second time.
//!
//! The header does not carry `cwd`: [`SessionEntry::Header`] has no such
//! field, so the working directory recorded in `sessions.cwd` survives in
//! the database but not in the JSONL.

use std::io::Write;
use std::path::{Path, PathBuf};

use pi_protocol::SessionEntry;

use crate::error::{Result, SessionError};
use crate::reader::SessionReader;

/// Result of a successful [`export_jsonl`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportReport {
    /// Session identifier that was exported.
    pub session_id: String,
    /// Path the JSONL was written to.
    pub destination: PathBuf,
    /// Number of entry lines written (the header line is not counted).
    pub entries_written: usize,
    /// Size of the written file in bytes.
    pub bytes_written: usize,
}

/// Default file name for an exported session: `session-<sanitised-id>.jsonl`
/// in the current working directory.
///
/// Characters outside `[A-Za-z0-9._-]` are replaced with `_` so the name
/// stays portable (session ids are usually uuids, but callers are free to
/// use arbitrary strings).
pub fn default_export_path(session_id: &str) -> PathBuf {
    PathBuf::from(format!("session-{}.jsonl", sanitise(session_id)))
}

/// Render a session as JSONL without touching the filesystem.
///
/// Returns the full file content — the header line followed by one line
/// per stored entry, with a trailing newline. `SessionEntry::Header` rows
/// found in the `entries` table are skipped (see the module docs).
///
/// Returns [`SessionError::Other`] when the database has no session with
/// that id.
pub fn render_jsonl(reader: &SessionReader, session_id: &str) -> Result<String> {
    let row = reader.session_row(session_id)?.ok_or_else(|| {
        SessionError::Other(format!(
            "unknown session {session_id:?} in {}",
            reader.path().display()
        ))
    })?;
    let entries = reader.iter_entries(session_id)?;

    let mut out = String::new();
    out.push_str(&serde_json::to_string(&row.to_header())?);
    out.push('\n');
    for entry in entries {
        if matches!(entry.entry, SessionEntry::Header { .. }) {
            continue;
        }
        out.push_str(&serde_json::to_string(&entry.entry)?);
        out.push('\n');
    }
    Ok(out)
}

/// Write a session to `destination` as JSONL.
///
/// Missing parent directories are created. An existing file at
/// `destination` is overwritten.
pub fn export_jsonl(
    reader: &SessionReader,
    session_id: &str,
    destination: impl AsRef<Path>,
) -> Result<ExportReport> {
    let destination = destination.as_ref();
    let content = render_jsonl(reader, session_id)?;
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut file = std::fs::File::create(destination)?;
    file.write_all(content.as_bytes())?;
    file.flush()?;

    Ok(ExportReport {
        session_id: session_id.to_string(),
        destination: destination.to_path_buf(),
        entries_written: content.lines().count().saturating_sub(1),
        bytes_written: content.len(),
    })
}

/// Export a session, falling back to [`default_export_path`] when no
/// destination is given. Convenience wrapper for CLI callers.
pub fn export_session(
    reader: &SessionReader,
    session_id: &str,
    destination: Option<&Path>,
) -> Result<ExportReport> {
    match destination {
        Some(path) => export_jsonl(reader, session_id, path),
        None => export_jsonl(reader, session_id, default_export_path(session_id)),
    }
}

fn sanitise(session_id: &str) -> String {
    let mut out = String::with_capacity(session_id.len());
    for ch in session_id.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "untitled".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_export_path_sanitises_the_session_id() {
        assert_eq!(
            default_export_path("9f8c-abc"),
            PathBuf::from("session-9f8c-abc.jsonl")
        );
        assert_eq!(
            default_export_path("a/b c"),
            PathBuf::from("session-a_b_c.jsonl")
        );
        assert_eq!(
            default_export_path(""),
            PathBuf::from("session-untitled.jsonl")
        );
    }
}
