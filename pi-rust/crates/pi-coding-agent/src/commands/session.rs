//! `pi session` subcommand handlers.
//!
//! Stage 5 introduced the SQLite-backed `pi-session` crate and this
//! module wires the binary's CLI flags to the [`pi_session`] API, printing
//! machine-readable output (one JSON object per line) on stdout.
//!
//! `pi session migrate` accepts both inputs: a Stage 4 JSONL file and a
//! pre-Stage-55 **Rust legacy** SQLite database (detected from the
//! `SQLite format 3` magic). A legacy database is converted to the
//! upstream v4 layout without modifying the original; a JSONL file is
//! replayed into a fresh v4 database. In both cases the source stays on
//! disk.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use pi_session::{
    default_destination, export_jsonl, migrate_file, migrate_jsonl, render_jsonl, ExportReport,
    FileMigrationReport, MigrationReport, SessionError, SessionReader,
};

use crate::cli::SessionCommand;

/// First 16 bytes of every SQLite database file.
const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";

/// Generate a fresh session identifier.
///
/// The `session-<hex nanos>` shape is what the Stage 4 JSONL writer used
/// and what both `main.rs` and the interactive `/new` command use, so a
/// TUI-started session id is unique across modes.
pub fn new_session_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("session-{nanos:x}")
}

/// Run a `pi session ...` subcommand. Returns 0 on success, non-zero on
/// error.
pub fn run(action: SessionCommand) -> anyhow::Result<()> {
    match action {
        SessionCommand::List { database } => list(database.as_deref()),
        SessionCommand::Show {
            session_id,
            database,
        } => show(&database, &session_id),
        SessionCommand::Stats {
            session_id,
            database,
        } => stats(&database, &session_id),
        SessionCommand::Export {
            session_id,
            database,
            output,
        } => export(&database, &session_id, output.as_deref()),
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
        format!(
            "reading entries for session {session_id:?} in {}",
            database.display()
        )
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

/// Print one JSON object with the session's message count and aggregated
/// usage.
///
/// The `sessions` cache is read and recomputed from the durable tables at
/// the same time; a disagreement is logged by [`SessionReader::verify_stats`]
/// and reported as `"consistent": false` plus a `recomputed` object, so a
/// stale cache is never silently passed off as the truth.
///
/// The `usage` object uses the Rust `pi_protocol::Usage` shape (`input`,
/// `output`, `cache_read`, `cache_write`, `total`); `total` is the upstream
/// `totalTokens`, and upstream's `cost` / `cacheWrite1h` / `reasoning`
/// counters are not tracked by the port.
fn stats(database: &Path, session_id: &str) -> anyhow::Result<()> {
    let reader = SessionReader::open(database)
        .with_context(|| format!("opening session database {}", database.display()))?;
    let check = reader
        .verify_stats(session_id)
        .with_context(|| {
            format!(
                "reading stats for session {session_id:?} in {}",
                database.display()
            )
        })?
        .with_context(|| format!("session {session_id:?} not found in {}", database.display()))?;

    let mut payload = serde_json::json!({
        "session_id": session_id,
        "database": database.display().to_string(),
        "message_count": check.cached.message_count,
        "usage": check.cached.usage,
        "consistent": check.is_consistent(),
    });
    if !check.is_consistent() {
        payload["recomputed"] = serde_json::json!({
            "message_count": check.recomputed.message_count,
            "usage": check.recomputed.usage,
        });
    }
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "{payload}")?;
    Ok(())
}

fn export(database: &Path, session_id: &str, output: Option<&Path>) -> anyhow::Result<()> {
    let reader = SessionReader::open(database)
        .with_context(|| format!("opening session database {}", database.display()))?;
    let context = || {
        format!(
            "exporting session {session_id:?} from {}",
            database.display()
        )
    };
    let Some(destination) = output else {
        // No destination given — stream the JSONL to stdout.
        let jsonl = render_jsonl(&reader, session_id).with_context(context)?;
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        out.write_all(jsonl.as_bytes())?;
        return Ok(());
    };

    let report: ExportReport =
        export_jsonl(&reader, session_id, destination).with_context(context)?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "{}",
        serde_json::json!({
            "session_id": report.session_id,
            "destination": report.destination.display().to_string(),
            "entries_written": report.entries_written,
            "bytes_written": report.bytes_written,
        })
    )?;
    Ok(())
}

fn migrate(source: &Path, to: Option<&Path>) -> anyhow::Result<()> {
    if source.exists() && is_sqlite_file(source)? {
        return migrate_sqlite_layout(source, to);
    }
    migrate_jsonl_command(source, to)
}

/// True when the file starts with the SQLite magic header.
fn is_sqlite_file(path: &Path) -> anyhow::Result<bool> {
    use std::io::Read as _;

    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut magic = [0u8; 16];
    let read = file.read(&mut magic).unwrap_or(0);
    Ok(read >= SQLITE_MAGIC.len() && &magic[..SQLITE_MAGIC.len()] == SQLITE_MAGIC)
}

/// Convert a Rust legacy SQLite database into the upstream v4 layout.
/// The source is never modified; the default destination is a sibling
/// `<stem>.upstream.sqlite`.
fn migrate_sqlite_layout(source: &Path, to: Option<&Path>) -> anyhow::Result<()> {
    let report: FileMigrationReport = match migrate_file(source, to) {
        Ok(report) => report,
        Err(SessionError::NotFound(path)) => {
            anyhow::bail!("session file not found: {}", path.display());
        }
        Err(SessionError::AlreadyExists(path)) => {
            anyhow::bail!(
                "destination already exists, refusing to overwrite: {}",
                path.display()
            );
        }
        Err(other) => {
            anyhow::bail!("session migration failed; original preserved: {other}");
        }
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "{}",
        serde_json::json!({
            "kind": "sqlite-layout",
            "source": report.source.display().to_string(),
            "destination": report.destination.display().to_string(),
            "sessions_migrated": report.sessions_migrated,
            "entries_migrated": report.entries_migrated,
            "layout_before": format!("{:?}", report.layout_before),
            "layout_after": format!("{:?}", report.layout_after),
            "already_upstream": report.already_upstream,
            "source_preserved": report.source_preserved,
        })
    )?;
    Ok(())
}

fn migrate_jsonl_command(jsonl_path: &Path, to: Option<&Path>) -> anyhow::Result<()> {
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
            "kind": "jsonl",
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
    home.join(".local")
        .join("share")
        .join("pi")
        .join("sessions")
}
