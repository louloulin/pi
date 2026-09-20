//! Shared helpers for the `pi-session` integration tests.
//!
//! The important one is [`write_rust_legacy_fixture`]: since Stage 55
//! [`pi_session::SessionWriter`] only writes the upstream v4 layout, so the
//! legacy layout can no longer be produced through the public API. Tests
//! that need a legacy file build it here from the crate's own
//! [`INITIAL_SQL`] with the same zstd payload encoding the Stage 5 writer
//! used.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use pi_protocol::SessionEntry;
use pi_session::schema::{INITIAL_SQL, SCHEMA_VERSION};
use rusqlite::{params, Connection};

/// Fixed wall-clock used by the fixtures (millis since the unix epoch).
pub const T0: i64 = 1_700_000_000_123;

/// A fresh, empty temporary directory unique to this process + call.
pub fn fresh_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pi-session-int-{label}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0),
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// Build a **Rust legacy** session database by hand: Stage 5 DDL,
/// `PRAGMA user_version = 1`, one `sessions` row and one `entries` row per
/// entry with a zstd-compressed `SessionEntry` payload.
///
/// `SessionEntry::Header` values are skipped — the header lives in the
/// `sessions` row.
pub fn write_rust_legacy_fixture(
    path: &Path,
    session_id: &str,
    version: &str,
    entries: &[SessionEntry],
) {
    let conn = Connection::open(path).expect("open legacy db");
    conn.execute_batch(INITIAL_SQL).expect("legacy ddl");
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)
        .expect("stamp version");
    conn.execute(
        "INSERT INTO sessions \
         (id, created_at, parent_session, cwd, version, metadata) \
         VALUES (?1, ?2, NULL, NULL, ?3, NULL)",
        params![session_id, T0, version],
    )
    .expect("sessions row");

    let mut parent_entry_id: Option<String> = None;
    for (index, entry) in entries.iter().enumerate() {
        assert!(
            !matches!(entry, SessionEntry::Header { .. }),
            "the header belongs in the sessions row, not entries"
        );
        let seq = (index + 1) as i64;
        let entry_id = format!("legacy-{seq}");
        let type_ = serde_json::to_value(entry).expect("to_value")["type"]
            .as_str()
            .expect("tagged entry")
            .to_string();
        let payload =
            zstd::encode_all(serde_json::to_vec(entry).expect("json").as_slice(), 3).expect("zstd");
        conn.execute(
            "INSERT INTO entries \
             (session_id, seq, parent_seq, entry_id, parent_entry_id, type, timestamp, payload) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                session_id,
                seq,
                if seq == 1 { None } else { Some(seq - 1) },
                entry_id,
                parent_entry_id,
                type_,
                T0 + seq,
                payload,
            ],
        )
        .expect("entries row");
        parent_entry_id = Some(entry_id);
    }
}
