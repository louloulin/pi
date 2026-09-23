//! Error types for the `pi-session` crate.

use std::io;
use std::path::PathBuf;

/// Errors produced by [`SessionReader`](crate::SessionReader),
/// [`SessionWriter`](crate::SessionWriter), and the migrate helpers.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The session file does not exist (when `open` requires an existing
    /// file).
    #[error("session file not found: {0}")]
    NotFound(PathBuf),

    /// The session file exists but is not a valid SQLite database or its
    /// schema is unreadable / does not match the expected shape.
    #[error("session database is corrupt: {0}")]
    Corrupt(String),

    /// The session file uses the pre-Stage-55 Rust layout, which the
    /// writer no longer appends to. The file is still readable; convert
    /// it with `pi session migrate <path>` (or
    /// [`crate::migrate::migrate_file`]) before writing.
    #[error(
        "session database {0} uses the Rust legacy layout; run `pi session migrate <path>` to convert it to the upstream v4 format"
    )]
    LegacyLayout(PathBuf),

    /// The requested migration destination already exists; the migrate
    /// helpers never overwrite an existing file.
    #[error("refusing to overwrite existing file: {0}")]
    AlreadyExists(PathBuf),

    /// I/O error (open / read / write / rename).
    #[error("session I/O error: {0}")]
    Io(#[from] io::Error),

    /// SQLite error returned by rusqlite.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// zstd compression / decompression failed.
    #[error("zstd error: {0}")]
    Zstd(String),

    /// JSON (de)serialization failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// Migration from a JSONL session file — or from a Rust legacy
    /// database — failed; the original file is preserved untouched on
    /// disk.
    #[error("migration error: {0}")]
    Migration(String),

    /// Any other error not covered by the variants above.
    #[error("session error: {0}")]
    Other(String),
}

impl From<SessionError> for io::Error {
    fn from(value: SessionError) -> Self {
        match value {
            SessionError::Io(e) => e,
            other => io::Error::other(other.to_string()),
        }
    }
}

/// Crate-local result alias.
pub type Result<T> = std::result::Result<T, SessionError>;
