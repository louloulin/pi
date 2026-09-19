#!/usr/bin/env node
// Generate a "TS-recorded" SQLite fixture for the pi-session crate.
//
// Mirrors the schema the Rust reader expects:
//   - sessions(id, created_at, parent_session, cwd, version, metadata)
//   - entries(session_id, seq, parent_seq, entry_id, parent_entry_id,
//             type, timestamp, payload)
//   - meta(key, value)
//
// The payload column carries zstd-compressed JSON matching the Rust
// SessionEntry enum tagging ({"type": "user_message", ...} etc).
//
// Run from the workspace root:
//   node pi-rust/crates/pi-session/scripts/make-ts-fixture.mjs \
//     pi-rust/crates/pi-session/fixtures/ts_recorded.sqlite
//
// This file does NOT run as part of `cargo test`; it is invoked once
// during scaffolding and the resulting fixture is committed to the repo.

import { DatabaseSync } from "node:sqlite";
import { writeFileSync, mkdirSync } from "node:fs";
import { dirname } from "node:path";
import { zstdCompressSync } from "node:zlib";

const dest = process.argv[2];
if (!dest) {
    console.error("usage: make-ts-fixture.mjs <dest.sqlite>");
    process.exit(2);
}

mkdirSync(dirname(dest), { recursive: true });

const db = new DatabaseSync(dest);

db.exec(`
    CREATE TABLE IF NOT EXISTS sessions (
        id TEXT PRIMARY KEY,
        created_at INTEGER NOT NULL,
        parent_session TEXT,
        cwd TEXT,
        version TEXT,
        metadata TEXT
    ) WITHOUT ROWID;

    CREATE TABLE IF NOT EXISTS entries (
        session_id TEXT NOT NULL,
        seq INTEGER NOT NULL,
        parent_seq INTEGER,
        entry_id TEXT,
        parent_entry_id TEXT,
        type TEXT NOT NULL,
        timestamp INTEGER NOT NULL,
        payload BLOB NOT NULL,
        PRIMARY KEY (session_id, seq)
    ) WITHOUT ROWID;

    CREATE INDEX IF NOT EXISTS ix_entries_session_type
        ON entries(session_id, type, seq);
    CREATE INDEX IF NOT EXISTS ix_entries_session_parent
        ON entries(session_id, parent_seq);
    CREATE INDEX IF NOT EXISTS ix_entries_entry_id
        ON entries(session_id, entry_id);

    CREATE TABLE IF NOT EXISTS meta (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    ) WITHOUT ROWID;
`);

db.exec(`PRAGMA user_version = 1`);

// Helper: zstd-compress a JSON string at level 3 (matches Rust ZSTD_LEVEL).
function encode(payload) {
    const json = JSON.stringify(payload);
    return zstdCompressSync(Buffer.from(json, "utf8"), { level: 3 });
}

const sessionId = "ts-recorded-fixture";
const now = Date.now();

// 1) Header row → sessions table.
db.prepare(
    `INSERT INTO sessions (id, created_at, parent_session, cwd, version, metadata)
     VALUES (?, ?, NULL, ?, ?, NULL)`,
).run(sessionId, now, "/home/example", "0.85.1-fixture");

// 2) Header entry → entries table (so `iter_entries` surfaces the header too).
const headerPayload = {
    type: "header",
    id: sessionId,
    created_at: new Date(now).toISOString(),
    version: "0.85.1-fixture",
};
db.prepare(
    `INSERT INTO entries
        (session_id, seq, parent_seq, entry_id, parent_entry_id,
         type, timestamp, payload)
     VALUES (?, ?, NULL, NULL, NULL, ?, ?, ?)`,
).run(sessionId, 1, "header", now, encode(headerPayload));

// 3) User message.
const userPayload = {
    type: "user_message",
    role: "user",
    content: [{ type: "text", text: "what is the capital of france?" }],
};
db.prepare(
    `INSERT INTO entries
        (session_id, seq, parent_seq, entry_id, parent_entry_id,
         type, timestamp, payload)
     VALUES (?, ?, NULL, NULL, NULL, ?, ?, ?)`,
).run(sessionId, 2, "user_message", now + 100, encode(userPayload));

// 4) Assistant message.
const assistantPayload = {
    type: "assistant_message",
    model: "faux/faux-model",
    content: [{ type: "text", text: "Paris." }],
    stop_reason: "stop",
    usage: { input: 0, output: 0, cache_read: 0, cache_write: 0, total: 0 },
};
db.prepare(
    `INSERT INTO entries
        (session_id, seq, parent_seq, entry_id, parent_entry_id,
         type, timestamp, payload)
     VALUES (?, ?, NULL, NULL, NULL, ?, ?, ?)`,
).run(sessionId, 3, "assistant_message", now + 200, encode(assistantPayload));

// 5) Extension entry — provenance kind of test marker.
const extPayload = {
    type: "extension",
    extension: "ts-fixture",
    kind: "marker",
    payload: { fixture: true, recorded_by: "node:sqlite", version: 1 },
};
db.prepare(
    `INSERT INTO entries
        (session_id, seq, parent_seq, entry_id, parent_entry_id,
         type, timestamp, payload)
     VALUES (?, ?, NULL, NULL, NULL, ?, ?, ?)`,
).run(sessionId, 4, "extension", now + 300, encode(extPayload));

// 6) Tool call + result.
const toolCallPayload = {
    type: "tool_call",
    id: "ts-call-1",
    name: "bash",
    arguments: { cmd: "echo hello" },
};
db.prepare(
    `INSERT INTO entries
        (session_id, seq, parent_seq, entry_id, parent_entry_id,
         type, timestamp, payload)
     VALUES (?, ?, NULL, NULL, NULL, ?, ?, ?)`,
).run(sessionId, 5, "tool_call", now + 400, encode(toolCallPayload));

const toolResultPayload = {
    type: "tool_result",
    tool_call_id: "ts-call-1",
    content: { type: "text", text: "hello\n" },
    is_error: false,
};
db.prepare(
    `INSERT INTO entries
        (session_id, seq, parent_seq, entry_id, parent_entry_id,
         type, timestamp, payload)
     VALUES (?, ?, NULL, NULL, NULL, ?, ?, ?)`,
).run(sessionId, 6, "tool_result", now + 500, encode(toolResultPayload));

db.exec(`PRAGMA wal_checkpoint(TRUNCATE)`);
db.close();

console.log(`wrote fixture: ${dest}`);