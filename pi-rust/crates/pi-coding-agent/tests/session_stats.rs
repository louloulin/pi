//! End-to-end test for `pi session stats`.
//!
//! Runs the real binary against a copy of the committed upstream fixture
//! so the JSON on stdout is exactly what a user sees. The fixture's
//! `usage_ledger` carries an `adjustment` row, which must be part of the
//! aggregate, and its `sessions` cache is consistent with the durable
//! tables — the drift case is exercised by rewriting the copy.

use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::Connection;

fn pi() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pi"))
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../pi-session/fixtures/ts_recorded.sqlite")
}

/// Copy the fixture into a fresh directory and return the copy's path.
fn fixture_copy(dir: &Path) -> PathBuf {
    let db = dir.join("upstream.sqlite");
    std::fs::copy(fixture(), &db).expect("copy fixture");
    db
}

fn run(args: &[&str]) -> (bool, String, String) {
    let output = Command::new(pi()).args(args).output().expect("run pi");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn stats_prints_the_cached_message_count_and_usage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fixture_copy(dir.path());
    let (ok, stdout, stderr) = run(&[
        "session",
        "stats",
        "ts-recorded-fixture",
        "--database",
        db.to_str().unwrap(),
    ]);
    assert!(ok, "pi session stats failed\nstderr:\n{stderr}");

    let report: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("one JSON object on stdout");
    assert_eq!(report["session_id"], "ts-recorded-fixture");
    assert_eq!(report["message_count"], 4, "e1..e3 + the fork entry");
    // u1 (120/8/4) + u2 (100/20) + the u3 adjustment (5/5/1/2): the
    // adjustment counts, exactly like upstream `addUsageToSessionStats`.
    assert_eq!(report["usage"]["input"], 225);
    assert_eq!(report["usage"]["output"], 33);
    assert_eq!(report["usage"]["cache_read"], 5);
    assert_eq!(report["usage"]["cache_write"], 2);
    assert_eq!(report["usage"]["total"], 261);
    assert_eq!(report["consistent"], true);
    assert!(
        report.get("recomputed").is_none(),
        "a consistent cache has nothing to report: {report}"
    );
}

#[test]
fn stats_reports_a_stale_cache_without_failing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fixture_copy(dir.path());
    {
        let conn = Connection::open(&db).expect("open copy");
        conn.execute(
            "UPDATE sessions SET message_count = 9, usage_payload = ?1 WHERE id = ?2",
            rusqlite::params![
                r#"{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"totalTokens":2}"#,
                "ts-recorded-fixture"
            ],
        )
        .expect("corrupt the cache");
    }

    let (ok, stdout, stderr) = run(&[
        "session",
        "stats",
        "ts-recorded-fixture",
        "--database",
        db.to_str().unwrap(),
    ]);
    assert!(ok, "stats must still report on a drifted cache");

    let report: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("one JSON object on stdout");
    assert_eq!(report["consistent"], false);
    assert_eq!(report["message_count"], 9, "the cached value is returned");
    assert_eq!(report["usage"]["total"], 2);
    assert_eq!(report["recomputed"]["message_count"], 4);
    assert_eq!(report["recomputed"]["usage"]["total"], 261);
    assert!(
        stderr.contains("disagrees"),
        "the drift must be logged, not silent\nstderr:\n{stderr}"
    );
}

#[test]
fn stats_fails_for_an_unknown_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fixture_copy(dir.path());
    let (ok, _, stderr) = run(&[
        "session",
        "stats",
        "no-such-session",
        "--database",
        db.to_str().unwrap(),
    ]);
    assert!(!ok, "an unknown session is an error");
    assert!(stderr.contains("not found"), "unexpected stderr:\n{stderr}");
}
