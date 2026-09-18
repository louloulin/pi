//! Migration from the JSONL session format to the SQLite backend.
//!
//! Stage 4 of the Rust port writes sessions as JSONL (one
//! [`SessionEntry`](pi_protocol::SessionEntry) per line). This module
//! reads those files, opens a fresh SQLite database, and replays each
//! entry. The original JSONL is preserved on disk: any error during the
//! migration leaves the input untouched (the `pi-coding-agent` CLI's
//! `pi session migrate` command only deletes the JSONL after the SQLite
//! commit succeeds).

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use pi_protocol::SessionEntry;

use crate::error::{Result, SessionError};
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
    for (line_no, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry: SessionEntry = serde_json::from_str(trimmed).map_err(|e| {
            SessionError::Migration(format!(
                "failed to parse line {}: {e}",
                line_no + 1
            ))
        })?;
        if let SessionEntry::Header { ref id, .. } = entry {
            writer.write_header(entry.clone())?;
            header_id = Some(id.clone());
            // Header itself is a row in `sessions`, not `entries`.
            continue;
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
            content: vec![Content::Text(TextContent {
                text: "hi".into(),
            })],
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
            SessionEntry::Extension { extension, kind, .. } => {
                assert_eq!(extension, "test");
                assert_eq!(kind, "marker");
            }
            other => panic!("expected extension entry, got {other:?}"),
        }
    }
}