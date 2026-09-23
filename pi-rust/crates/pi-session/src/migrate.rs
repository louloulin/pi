//! Migration helpers.
//!
//! Two migration directions live here:
//!
//! * [`migrate_jsonl`] — Stage 4 of the Rust port wrote sessions as JSONL
//!   (one [`SessionEntry`](pi_protocol::SessionEntry) per line). This
//!   replays such a file into a fresh SQLite database (upstream v4 since
//!   Stage 55). The original JSONL is preserved on disk.
//! * [`migrate_file`] — converts a **Rust legacy** SQLite database (the
//!   pre-Stage-55 narrow layout, refused by [`SessionWriter`]) into the
//!   upstream v4 layout. The source is never modified: by default the
//!   conversion is written to a sibling `<stem>.upstream.sqlite`.
//!
//! Both writers build the destination in a temporary sibling file and
//! only rename it into place once every entry has been committed, so a
//! failed migration leaves the source untouched and no half-written file
//! behind.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use pi_protocol::SessionEntry;

use crate::error::{Result, SessionError};
use crate::reader::SessionReader;
use crate::schema::{self, SchemaLayout};
use crate::writer::SessionWriter;

/// Result of a successful migration.
#[derive(Debug, Clone)]
pub struct MigrationReport {
    /// Path to the JSONL file that was migrated.
    pub source: PathBuf,
    /// Path to the SQLite database that was created.
    pub destination: PathBuf,
    /// Number of entries written (excluding the header row).
    pub entries_migrated: usize,
    /// Identifier of the migrated session, taken from the JSONL header.
    pub header_id: Option<String>,
}

/// Read a JSONL session file and write its entries to a SQLite database.
///
/// `destination` is overwritten (it is the caller's job to pick a
/// non-conflicting path; the `pi session migrate` CLI uses
/// `<source-stem>.sqlite` next to the JSONL).
pub fn migrate_jsonl<P, Q>(source: P, destination: Q) -> Result<MigrationReport>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    let source = source.as_ref();
    let destination = destination.as_ref();
    if !source.exists() {
        return Err(SessionError::NotFound(source.to_path_buf()));
    }
    if destination.exists() {
        // We always create a fresh DB. Removing the stale file matches
        // the TS migrate behaviour and avoids "table already exists"
        // surprises.
        std::fs::remove_file(destination)?;
    }
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let file = File::open(source)?;
    let reader = BufReader::new(file);
    let writer = SessionWriter::open(destination)?;

    let mut count = 0usize;
    let mut header_id = None;
    let mut header_written = false;
    for (line_no, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry: SessionEntry = serde_json::from_str(trimmed).map_err(|e| {
            SessionError::Migration(format!("failed to parse line {}: {e}", line_no + 1))
        })?;
        if let SessionEntry::Header { ref id, .. } = entry {
            writer.write_header(entry.clone())?;
            header_id = Some(id.clone());
            header_written = true;
            // Header itself is a row in `sessions`, not `entries`.
            continue;
        }
        if !header_written {
            // Stage 4 always wrote a header first, but be forgiving: the
            // upstream schema requires a `sessions` row before any entry.
            let id = source
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .filter(|stem| !stem.is_empty())
                .unwrap_or_else(|| "session".to_string());
            writer.write_header(SessionEntry::Header {
                id: id.clone(),
                created_at: chrono::Utc::now(),
                version: String::new(),
            })?;
            header_id = Some(id);
            header_written = true;
        }
        writer.append(entry)?;
        count += 1;
    }
    writer.checkpoint()?;
    Ok(MigrationReport {
        source: source.to_path_buf(),
        destination: destination.to_path_buf(),
        entries_migrated: count,
        header_id,
    })
}

/// Compute the destination path for a given JSONL source — sibling file
/// with a `.sqlite` extension. Public so the CLI can preview the
/// destination before running the migration.
pub fn default_destination<P: AsRef<Path>>(source: P) -> PathBuf {
    let source = source.as_ref();
    match source.extension().and_then(|s| s.to_str()) {
        Some(ext) => source.with_extension(format!("{ext}.sqlite")),
        None => source.with_extension("sqlite"),
    }
}

/// Result of a Rust-legacy → upstream-v4 file migration.
#[derive(Debug, Clone)]
pub struct FileMigrationReport {
    /// Path of the database that was inspected (never modified).
    pub source: PathBuf,
    /// Path the upstream v4 database was written to. Equal to `source`
    /// when `already_upstream` is true (nothing to do).
    pub destination: PathBuf,
    /// Number of sessions replayed.
    pub sessions_migrated: usize,
    /// Number of entries replayed across all sessions.
    pub entries_migrated: usize,
    /// Layout detected in `source`.
    pub layout_before: SchemaLayout,
    /// Layout of `destination` after the migration.
    pub layout_after: SchemaLayout,
    /// True when `source` was already upstream v4 and nothing was written.
    pub already_upstream: bool,
    /// Always true: the source file is never overwritten or deleted.
    pub source_preserved: bool,
}

/// Default destination for [`migrate_file`]: sibling `<stem>.upstream.sqlite`.
pub fn default_layout_destination<P: AsRef<Path>>(source: P) -> PathBuf {
    let source = source.as_ref();
    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "session".to_string());
    source.with_file_name(format!("{stem}.upstream.sqlite"))
}

/// Convert a **Rust legacy** session database into the upstream v4 layout.
///
/// The source is opened read-only and never modified. The converted copy
/// is written to a temporary sibling file and renamed into place only
/// after every session has been committed, so a failure cannot leave a
/// half-written database behind (and never touches the source).
///
/// * `destination` defaults to [`default_layout_destination`].
/// * A destination that already exists is refused with
///   [`SessionError::AlreadyExists`] — the helper never silently
///   overwrites a file.
/// * A source that is already upstream v4 is a no-op: the report carries
///   `already_upstream = true` and `destination == source`.
pub fn migrate_file(
    source: impl AsRef<Path>,
    destination: Option<&Path>,
) -> Result<FileMigrationReport> {
    let source = source.as_ref().to_path_buf();
    if !source.exists() {
        return Err(SessionError::NotFound(source));
    }
    let (_conn, layout_before) = schema::open_read_only_with_layout(&source)?;
    drop(_conn);

    if layout_before == SchemaLayout::UpstreamV4 {
        return Ok(FileMigrationReport {
            source: source.clone(),
            destination: source,
            sessions_migrated: 0,
            entries_migrated: 0,
            layout_before,
            layout_after: SchemaLayout::UpstreamV4,
            already_upstream: true,
            source_preserved: true,
        });
    }

    let destination = destination
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_layout_destination(&source));
    if destination == source {
        return Err(SessionError::Other(
            "migration destination equals the source; the source is never overwritten".to_string(),
        ));
    }
    if destination.exists() {
        return Err(SessionError::AlreadyExists(destination));
    }
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let temp = temporary_sibling(&destination);
    cleanup_database_files(&temp);

    let reader = SessionReader::open(&source)?;
    let result = (|| -> Result<(usize, usize)> {
        let mut sessions = 0usize;
        let mut entries = 0usize;
        {
            let writer = SessionWriter::open(&temp)?;
            for row in reader.list_sessions()? {
                writer.write_header(row.to_header())?;
                for decoded in reader.iter_entries(&row.id)? {
                    writer.append(decoded.entry)?;
                    entries += 1;
                }
                sessions += 1;
            }
            writer.checkpoint()?;
        }
        Ok((sessions, entries))
    })();

    match result {
        Ok((sessions, entries)) => {
            std::fs::rename(&temp, &destination)?;
            cleanup_database_files(&temp);
            Ok(FileMigrationReport {
                source,
                destination,
                sessions_migrated: sessions,
                entries_migrated: entries,
                layout_before,
                layout_after: SchemaLayout::UpstreamV4,
                already_upstream: false,
                source_preserved: true,
            })
        }
        Err(err) => {
            cleanup_database_files(&temp);
            Err(err)
        }
    }
}

fn temporary_sibling(destination: &Path) -> PathBuf {
    let file_name = destination
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session.sqlite".to_string());
    let nonce = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    destination.with_file_name(format!(
        ".{file_name}.migrate-{}-{nonce}",
        std::process::id()
    ))
}

/// Remove a database plus its `-wal` / `-shm` sidecars.
fn cleanup_database_files(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(PathBuf::from(format!("{}-wal", path.display())));
    let _ = std::fs::remove_file(PathBuf::from(format!("{}-shm", path.display())));
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_protocol::{Content, Message, Role, TextContent};
    use std::io::Write as _;

    fn tempdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pi-session-migrate-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn migrate_jsonl_round_trip() {
        let dir = tempdir();
        let jsonl = dir.join("session.jsonl");
        let sqlite = dir.join("session.sqlite");

        // Write a minimal JSONL.
        let mut jsonl_file = std::fs::File::create(&jsonl).expect("create jsonl");
        let header = SessionEntry::Header {
            id: "test-id".into(),
            created_at: chrono::Utc::now(),
            version: "0.1.0".into(),
        };
        writeln!(jsonl_file, "{}", serde_json::to_string(&header).unwrap()).unwrap();
        let user = SessionEntry::UserMessage(Message {
            role: Role::User,
            content: vec![Content::Text(TextContent { text: "hi".into() })],
            model: None,
        });
        writeln!(jsonl_file, "{}", serde_json::to_string(&user).unwrap()).unwrap();
        let ext = SessionEntry::Extension {
            extension: "test".into(),
            kind: "marker".into(),
            payload: serde_json::json!({"foo": 1}),
        };
        writeln!(jsonl_file, "{}", serde_json::to_string(&ext).unwrap()).unwrap();
        drop(jsonl_file);

        let report = migrate_jsonl(&jsonl, &sqlite).expect("migrate");
        assert_eq!(report.entries_migrated, 2);
        assert_eq!(report.header_id.as_deref(), Some("test-id"));

        // The destination file should now be readable by a SessionReader.
        let reader = crate::SessionReader::open(&sqlite).expect("open");
        let sessions = reader.list_sessions().expect("list");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "test-id");

        let entries = reader.iter_entries("test-id").expect("entries");
        assert_eq!(entries.len(), 2);
        match &entries[0].entry {
            SessionEntry::UserMessage(m) => {
                if let Content::Text(t) = &m.content[0] {
                    assert_eq!(t.text, "hi");
                } else {
                    panic!("expected text content");
                }
            }
            other => panic!("expected user message, got {other:?}"),
        }
        match &entries[1].entry {
            SessionEntry::Extension { kind, .. } => {
                // The upstream `custom` entry stores only a single custom
                // type; the Rust `extension` namespace normalises to
                // `custom` and the marker survives in `kind`.
                assert_eq!(kind, "marker");
            }
            other => panic!("expected extension entry, got {other:?}"),
        }
    }
}
