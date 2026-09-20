//! End-to-end tests for `pi session migrate` input detection.
//!
//! The command accepts a Stage 4 JSONL file *and* a SQLite session
//! database, chosen from the file magic rather than the extension. These
//! tests run the real binary so the report on stdout is exactly what a
//! user sees.

use std::path::{Path, PathBuf};
use std::process::Command;

use pi_protocol::{Content, Message, Role, SessionEntry};
use pi_session::{SchemaLayout, SessionReader};

fn pi() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pi"))
}

fn run(args: &[&str]) -> String {
    let output = Command::new(pi()).args(args).output().expect("run pi");
    assert!(
        output.status.success(),
        "pi {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

/// A SQLite file is detected by content and reported as a layout
/// migration. The committed upstream fixture is already v4, so the run is
/// a no-op that preserves the file.
#[test]
fn sqlite_input_is_detected_and_reported_as_a_layout_migration() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../pi-session/fixtures/ts_recorded.sqlite");
    let db = dir.path().join("upstream.sqlite");
    std::fs::copy(&fixture, &db).expect("copy fixture");
    let bytes_before = std::fs::read(&db).expect("read fixture copy");

    let stdout = run(&["session", "migrate", db.to_str().unwrap()]);
    let report: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("one JSON object on stdout");
    assert_eq!(report["kind"], "sqlite-layout");
    assert_eq!(report["already_upstream"], true);
    assert_eq!(report["source_preserved"], true);
    assert_eq!(report["layout_before"], "UpstreamV4");
    assert_eq!(report["layout_after"], "UpstreamV4");
    assert_eq!(report["destination"], db.display().to_string());
    // Nothing was rewritten.
    assert_eq!(std::fs::read(&db).expect("re-read"), bytes_before);
}

#[test]
fn jsonl_input_falls_back_to_the_jsonl_migrator() {
    let dir = tempfile::tempdir().expect("tempdir");
    let jsonl = dir.path().join("stage4.jsonl");
    let header = SessionEntry::Header {
        id: "stage4".into(),
        created_at: chrono::Utc::now(),
        version: "0.1.0".into(),
    };
    let user = SessionEntry::UserMessage(Message {
        role: Role::User,
        content: vec![Content::text("hello from jsonl")],
        model: None,
    });
    std::fs::write(
        &jsonl,
        format!(
            "{}\n{}\n",
            serde_json::to_string(&header).unwrap(),
            serde_json::to_string(&user).unwrap()
        ),
    )
    .expect("write jsonl");

    let stdout = run(&["session", "migrate", jsonl.to_str().unwrap()]);
    let report: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("one JSON object on stdout");
    assert_eq!(report["kind"], "jsonl");
    assert_eq!(report["entries_migrated"], 1);
    assert_eq!(report["header_id"], "stage4");

    // The default destination is `<stem>.jsonl.sqlite` next to the source
    // and is a real upstream v4 database.
    let destination = dir.path().join("stage4.jsonl.sqlite");
    assert_eq!(report["destination"], destination.display().to_string());
    let reader = SessionReader::open(&destination).expect("open destination");
    assert_eq!(reader.layout(), SchemaLayout::UpstreamV4);
    assert_eq!(reader.iter_entries("stage4").expect("entries").len(), 1);
    assert!(jsonl.exists(), "the JSONL source must be preserved");
}
