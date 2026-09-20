//! Stage 65: session-tree reads (`session_tree` / `entry_ancestry`), the
//! verbatim copy write path (`copy_entries_from`) and the `/tree` cursor
//! move (`set_leaf`).
//!
//! The fixture is a two-branch upstream-v4 session:
//!
//! ```text
//! e1 ─ e2 ─ e3 ─ e4
//!          └─ e5 ─ e6
//! ```
//!
//! `e1` is the root, `e3`/`e4` are one branch off `e2`, and `e5`/`e6` the
//! other (created by moving the leaf back onto `e2`).

use std::path::Path;

use pi_protocol::{Content, Message, Role, SessionEntry};
use pi_session::{SessionReader, SessionWriter};

mod common;

use common::fresh_dir;

fn user(text: &str) -> SessionEntry {
    SessionEntry::UserMessage(Message {
        role: Role::User,
        content: vec![Content::text(text)],
        model: None,
    })
}

fn header(id: &str) -> SessionEntry {
    SessionEntry::Header {
        id: id.to_string(),
        created_at: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(1_700_000_000_123)
            .expect("timestamp"),
        version: "0.1.0".into(),
    }
}

/// Build the two-branch fixture and return `(path, reader)`.
fn branched_fixture(dir_name: &str) -> (std::path::PathBuf, SessionReader) {
    let dir = fresh_dir(dir_name);
    let path = dir.join("branched.sqlite");
    let writer = SessionWriter::open(&path).expect("open writer");
    writer.write_header(header("branched")).expect("header");
    for text in ["u1", "a1", "u2", "a2"] {
        writer.append(user(text)).expect("append");
    }
    // Branch B: move the cursor back onto the second entry (`e2`).
    writer.set_leaf("branched", "e2").expect("set leaf");
    writer.append(user("u2b")).expect("append");
    writer.append(user("a2b")).expect("append");
    writer.checkpoint().expect("checkpoint");
    drop(writer);
    let reader = SessionReader::open(&path).expect("reader");
    (path, reader)
}

/// Raw `entries` rows, ordered exactly like the copy path reads them.
fn raw_rows(path: &Path, session_id: &str) -> Vec<(String, Option<String>, i64, String, String)> {
    let conn = rusqlite::Connection::open(path).expect("open db");
    let mut stmt = conn
        .prepare(
            "SELECT id, parent_id, seq, type, payload FROM entries \
             WHERE session_id = ?1 ORDER BY seq ASC, id ASC",
        )
        .expect("prepare");
    let rows = stmt
        .query_map([session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows");
    rows
}

#[test]
fn session_tree_nests_branches_and_preorders_the_active_branch_first() {
    let (_path, reader) = branched_fixture("tree-shape");
    let roots = reader.session_tree("branched").expect("tree");
    assert_eq!(roots.len(), 1, "one root");
    let root = &roots[0];
    assert_eq!(root.entry.entry_id.as_deref(), Some("e1"));
    let chain = root.preorder();
    let ids = chain
        .iter()
        .map(|node| node.entry.entry_id.clone().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["e1", "e2", "e3", "e4", "e5", "e6"]);

    // The branch point has both branches, in stored (seq) order.
    let branch = chain
        .iter()
        .find(|node| node.entry.entry_id.as_deref() == Some("e2"))
        .expect("e2");
    let children = branch
        .children
        .iter()
        .map(|node| node.entry.entry_id.clone().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(children, vec!["e3", "e5"]);

    // Leaf nodes have no children.
    assert!(chain
        .iter()
        .filter(|node| node.entry.entry_id.as_deref() == Some("e4"))
        .all(|node| node.children.is_empty()));
    assert!(!branch.children[1].children.is_empty());
}

#[test]
fn entry_ancestry_is_the_root_to_entry_path() {
    let (_path, reader) = branched_fixture("tree-ancestry");
    let ids = reader
        .entry_ancestry("branched", "e5")
        .expect("ancestry")
        .into_iter()
        .map(|entry| entry.entry_id.unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["e1", "e2", "e5"]);

    assert!(reader.entry_ancestry("branched", "missing").is_err());
}

#[test]
fn copy_entries_from_clones_every_row_verbatim() {
    let (path, reader) = branched_fixture("tree-clone");
    let dir = fresh_dir("tree-clone-dest");
    let dest = dir.join("clone.sqlite");

    let before_bytes = std::fs::read(&path).expect("source bytes");
    let before_mtime = std::fs::metadata(&path)
        .expect("source metadata")
        .modified()
        .expect("mtime");

    let writer = SessionWriter::open(&dest).expect("open dest");
    writer.write_header(header("clone")).expect("header");
    let copied = writer
        .copy_entries_from(&reader, "branched", None)
        .expect("copy");
    writer.checkpoint().expect("checkpoint");
    drop(writer);

    assert_eq!(copied, 6, "every entry is copied");
    assert_eq!(
        raw_rows(&dest, "clone"),
        raw_rows(&path, "branched"),
        "clone is entry-for-entry identical"
    );

    // The new file's cached stats agree with a recompute (Stage 56
    // `verify_stats`), i.e. `message_count` was carried across the copy.
    let clone_reader = SessionReader::open(&dest).expect("clone reader");
    let check = clone_reader
        .verify_stats("clone")
        .expect("verify")
        .expect("stats row");
    assert!(check.is_consistent(), "{check:?}");

    // The source is only ever read.
    assert_eq!(std::fs::read(&path).expect("source bytes"), before_bytes);
    assert_eq!(
        std::fs::metadata(&path)
            .expect("source metadata")
            .modified()
            .expect("mtime"),
        before_mtime,
        "cloning must not touch the source file"
    );
}

#[test]
fn copy_entries_from_a_fork_path_stops_at_the_selected_entry() {
    let (path, reader) = branched_fixture("tree-fork");
    let dir = fresh_dir("tree-fork-dest");
    let dest = dir.join("fork.sqlite");

    let path_ids = reader
        .entry_ancestry("branched", "e5")
        .expect("ancestry")
        .into_iter()
        .map(|entry| entry.entry_id.unwrap())
        .collect::<Vec<_>>();

    let writer = SessionWriter::open(&dest).expect("open dest");
    writer.write_header(header("forked")).expect("header");
    let copied = writer
        .copy_entries_from(&reader, "branched", Some(&path_ids))
        .expect("copy");
    writer.checkpoint().expect("checkpoint");
    drop(writer);

    assert_eq!(copied, 3);
    let ids = raw_rows(&dest, "forked")
        .into_iter()
        .map(|row| row.0)
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["e1", "e2", "e5"], "up to and including e5");

    // A header row exists for the new session (upstream keeps it in
    // `sessions`, so `entries` stays header-free).
    let reader = SessionReader::open(&dest).expect("reader");
    assert!(reader.session_row("forked").expect("row").is_some());
    let check = reader
        .verify_stats("forked")
        .expect("verify")
        .expect("stats row");
    assert!(check.is_consistent(), "{check:?}");
    assert_eq!(
        reader
            .iter_entries("forked")
            .expect("entries")
            .first()
            .and_then(|entry| entry.entry_id.clone())
            .as_deref(),
        Some("e1"),
        "the first copied entry follows the header"
    );
    // The source is untouched.
    assert_eq!(
        raw_rows(&path, "branched").len(),
        6,
        "source keeps every entry"
    );
}

#[test]
fn set_leaf_moves_the_cursor_so_the_next_append_attaches_there() {
    let (path, _reader) = branched_fixture("tree-leaf");
    // A second, independent writer must observe the recorded leaf too.
    let writer = SessionWriter::open(&path).expect("open writer");
    writer.set_leaf("branched", "e2").expect("set leaf");
    writer.append(user("e7-later")).expect("append");
    writer.checkpoint().expect("checkpoint");
    drop(writer);

    let reader = SessionReader::open(&path).expect("reader");
    let appended = reader
        .iter_entries("branched")
        .expect("entries")
        .into_iter()
        .find(|entry| entry.seq == 7)
        .expect("appended entry");
    assert_eq!(
        appended.entry_id.as_deref(),
        Some("e7"),
        "next_seq continues after the copied/append history"
    );
    assert_eq!(
        appended.parent_entry_id.as_deref(),
        Some("e2"),
        "the append hangs off the selected leaf"
    );
}

#[test]
fn copy_entries_from_rejects_an_empty_entry_only_source() {
    // A header-only session copies zero rows (the `/fork` empty-transcript
    // guard lives in the coding agent, which never calls this).
    let dir = fresh_dir("tree-empty");
    let src = dir.join("empty.sqlite");
    let writer = SessionWriter::open(&src).expect("open");
    writer.write_header(header("empty")).expect("header");
    writer.checkpoint().expect("checkpoint");
    drop(writer);
    let reader = SessionReader::open(&src).expect("reader");

    let dest_dir = fresh_dir("tree-empty-dest");
    let writer = SessionWriter::open(dest_dir.join("empty-clone.sqlite")).expect("open dest");
    writer.write_header(header("empty-clone")).expect("header");
    let copied = writer
        .copy_entries_from(&reader, "empty", None)
        .expect("copy");
    assert_eq!(copied, 0);
}

#[test]
fn copy_entries_from_refuses_a_rust_legacy_source() {
    // The legacy layout stores its payload zstd-compressed, so it is not a
    // verbatim raw-copy target; the caller has to migrate first.
    let dir = fresh_dir("tree-legacy");
    let src = dir.join("legacy.sqlite");
    common::write_rust_legacy_fixture(&src, "legacy", "0.1.0", &[user("u1")]);
    let reader = SessionReader::open(&src).expect("reader");

    let dest_dir = fresh_dir("tree-legacy-dest");
    let writer = SessionWriter::open(dest_dir.join("dest.sqlite")).expect("open dest");
    writer.write_header(header("dest")).expect("header");
    let err = writer
        .copy_entries_from(&reader, "legacy", None)
        .expect_err("legacy source is not a raw-copy target");
    assert!(err.to_string().contains("legacy layout"), "{err}");
}
