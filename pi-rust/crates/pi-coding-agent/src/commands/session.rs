//! `pi session` subcommand handlers.
//!
//! Stage 5 introduces the SQLite-backed `pi-session` crate; the binary
//! still ships a JSONL fallback for users who want to migrate Stage 4
//! sessions. The handlers in this module wire the binary's CLI flags to
//! the [`pi_session`] API and print machine-readable output (one JSON
//! object per line) on stdout.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use pi_session::{
    default_destination, migrate_jsonl, MigrationReport, SessionError, SessionReader,
};

use crate::cli::SessionCommand;

/// Run a `pi session ...` subcommand. Returns 0 on success, non-zero on
/// error.
pub fn run(action: SessionCommand) -> anyhow::Result<()> {
    match action {
        SessionCommand::List { database } => list(database.as_deref()),
        SessionCommand::Show { session_id, database } => show(&database, &session_id),
        SessionCommand::Export { session_id, database } => show(&database, &session_id),
        SessionCommand::Migrate { jsonl_path, to } => migrate(&jsonl_path, to.as_deref()),
    }
}

fn list(database: Option<&Path>) -> anyhow::Result<()> {
    let databases: Vec<PathBuf> = match database {
        Some(path) => vec![path.to_path_buf()],
        None => discover_sessions()?,
    };
    if databases.is_empty() {
        eprintln!("no session files found");
        return Ok(());
    }
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for db in databases {
        let reader = match SessionReader::open(&db) {
            Ok(reader) => reader,
            Err(err) => {
                eprintln!("skipping {}: {err}", db.display());
                continue;
            }
        };
        for session in reader.list_sessions()? {
            let entry = serde_json::json!({
                "database": db.display().to_string(),
                "id": session.id,
                "created_at": session.created_at,
                "version": session.version,
                "cwd": session.cwd,
            });
            writeln!(out, "{entry}")?;
        }
    }
    Ok(())
}

fn show(database: &Path, session_id: &str) -> anyhow::Result<()> {
    let reader = SessionReader::open(database)
        .with_context(|| format!("opening session database {}", database.display()))?;
    let entries = reader.iter_entries(session_id).with_context(|| {
        format!("reading entries for session {session_id:?} in {}", database.display())
    })?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for entry in entries {
        let json = serde_json::to_string(entry.entry())
            .with_context(|| format!("serialising entry seq={}", entry.seq))?;
        writeln!(out, "{json}")?;
    }
    Ok(())
}

fn migrate(jsonl_path: &Path, to: Option<&Path>) -> anyhow::Result<()> {
    let destination = match to {
        Some(path) => path.to_path_buf(),
        None => default_destination(jsonl_path),
    };
    // Non-blocking: on error, leave the JSONL on disk untouched. The
    // [`migrate_jsonl`] helper guarantees that the destination is only
    // removed (when pre-existing) before the first successful write, so
    // a crash mid-migration leaves both files in place.
    let report: MigrationReport = match migrate_jsonl(jsonl_path, &destination) {
        Ok(report) => report,
        Err(SessionError::NotFound(path)) => {
            anyhow::bail!("jsonl file not found: {}", path.display());
        }
        Err(other) => {
            anyhow::bail!("migration failed; original jsonl preserved: {other}");
        }
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "{}",
        serde_json::json!({
            "source": report.source.display().to_string(),
            "destination": report.destination.display().to_string(),
            "entries_migrated": report.entries_migrated,
            "header_id": report.header_id,
        })
    )?;
    Ok(())
}

/// Discover `*.sqlite` files in the default session directory. Honours
/// `XDG_DATA_HOME` (falls back to `~/.local/share/pi/sessions/`); the
/// `pi-coding-agent` binary also accepts `--session-dir` so we re-use
/// the same default in [`crate::cli::Cli`].
fn discover_sessions() -> anyhow::Result<Vec<PathBuf>> {
    let dir = default_session_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s == "sqlite")
                .unwrap_or(false)
        {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

fn default_session_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("pi").join("sessions");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".local").join("share").join("pi").join("sessions")
}