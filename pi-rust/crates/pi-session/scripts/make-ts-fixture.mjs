#!/usr/bin/env node
// Generate the upstream-format `fixtures/ts_recorded.sqlite` fixture.
//
// IMPORTANT — this script is NOT a substitute for the real TS writer.
// It used to recreate the *Rust* schema (the old version of this file
// literally said "Mirrors the schema the Rust reader expects"), which made
// `tests/ts_compat.rs` a tautology: the fixture encoded the shape the
// reader wanted, so of course it read back. That acceptance claim was
// wrong and has been removed.
//
// This script now produces the shape the *upstream* TS writer produces:
// AgentHarness storage format 4 / storageVersion 1, exactly as defined by
// `packages/session-backends/sqlite-node/src/sqlite/migrations/001_initial.sql`.
// The DDL below is executed verbatim from that file (read from the repo at
// generation time), and the rows follow
// `packages/session-backends/sqlite-node/src/sqlite/session/entries.ts`:
//
//   * sessions(id, created_at, parent_session_id, storage_version,
//     metadata, message_count, usage_payload, next_seq)
//   * entries(session_id, id, parent_id, seq, type, custom_type,
//     timestamp, payload TEXT) with PK (session_id, id)
//   * payload is plain JSON text, not zstd
//   * `PRAGMA user_version` is left at 0 — upstream does not write it
//
// The entry payloads are copied by hand from the upstream `Entry` union
// (`packages/agent/src/harness/session/types.ts`): `message` wraps an
// `AgentMessage`, `compaction` / `branch_summary` carry a summary and an
// optional usage, `custom` carries an application-defined `data` value.
// Building the same rows through the real TS `SqliteStorage` would need a
// node_modules install, so this fixture is hand-written SQL; the shape is
// asserted structurally by `tests/ts_compat.rs` so it cannot silently
// drift into looking like the Rust layout again. The Rust-side entry
// field mapping (camelCase → snake_case, millisecond timestamps) is the
// reader's job, documented in `src/reader.rs`.
//
// Run from the repo root:
//   node pi-rust/crates/pi-session/scripts/make-ts-fixture.mjs \
//     pi-rust/crates/pi-session/fixtures/ts_recorded.sqlite

import { DatabaseSync } from "node:sqlite";
import { mkdirSync, readFileSync, rmSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const dest = process.argv[2];
if (!dest) {
    console.error("usage: make-ts-fixture.mjs <dest.sqlite>");
    process.exit(2);
}

const here = dirname(fileURLToPath(import.meta.url));
const upstreamDdlPath = resolve(
    here,
    "../../../../packages/session-backends/sqlite-node/src/sqlite/migrations/001_initial.sql",
);
const upstreamDdl = readFileSync(upstreamDdlPath, "utf8");

mkdirSync(dirname(dest), { recursive: true });
// Start from a clean file so re-running is deterministic.
rmSync(dest, { force: true });

const db = new DatabaseSync(dest);
db.exec(upstreamDdl);

const sessionId = "ts-recorded-fixture";
// Fixed timestamps keep the committed fixture byte-stable.
const T0 = 1_700_000_000_123;

const sessionInsert = db.prepare(
    `INSERT INTO sessions
        (id, created_at, parent_session_id, storage_version, metadata,
         message_count, usage_payload, next_seq)
     VALUES (?, ?, NULL, 1, ?, ?, ?, ?)`,
);

const entryInsert = db.prepare(
    `INSERT INTO entries
        (session_id, id, parent_id, seq, type, custom_type, timestamp, payload)
     VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
);

const zeroUsage = {
    input: 0,
    output: 0,
    cacheRead: 0,
    cacheWrite: 0,
    totalTokens: 0,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
};

/** One upstream `Entry` written as a plain-JSON `entries` row. */
function entry({ id, parentId, seq, type, customType = null, timestamp, payload }) {
    entryInsert.run(
        sessionId,
        id,
        parentId,
        seq,
        type,
        customType,
        timestamp,
        JSON.stringify(payload),
    );
}

// The session row. Upstream stores the pi version (if any) in `metadata`;
// the Rust reader reads `metadata.version` because upstream has no
// `sessions.version` column.
sessionInsert.run(
    sessionId,
    T0,
    JSON.stringify({ version: "0.85.1-fixture", source: "sqlite-node" }),
    3,
    JSON.stringify(zeroUsage),
    7,
);

// 1. User message.
entry({
    id: "e1-user",
    parentId: null,
    seq: 1,
    type: "message",
    timestamp: T0,
    payload: {
        message: {
            role: "user",
            content: [{ type: "text", text: "what is the capital of france?" }],
            timestamp: T0,
        },
    },
});

// 2. Assistant message: text, a thinking block (which the Rust
//    `pi_protocol::Content` enum cannot represent yet — the reader logs a
//    warning and skips it) and a tool call.
entry({
    id: "e2-assistant",
    parentId: "e1-user",
    seq: 2,
    type: "message",
    timestamp: T0 + 100,
    payload: {
        message: {
            role: "assistant",
            content: [
                { type: "text", text: "Paris." },
                { type: "thinking", thinking: "recalling European capitals" },
                { type: "toolCall", id: "ts-call-1", name: "bash", arguments: { cmd: "echo hello" } },
            ],
            api: "faux",
            provider: "faux",
            model: "faux/faux-model",
            usage: {
                input: 120,
                output: 8,
                cacheRead: 4,
                cacheWrite: 0,
                totalTokens: 128,
                cost: { input: 0.001, output: 0.002, cacheRead: 0, cacheWrite: 0, total: 0.003 },
            },
            stopReason: "toolUse",
            timestamp: T0 + 100,
        },
    },
});

// 3. Tool result for the call above.
entry({
    id: "e3-toolresult",
    parentId: "e2-assistant",
    seq: 3,
    type: "message",
    timestamp: T0 + 200,
    payload: {
        message: {
            role: "toolResult",
            toolCallId: "ts-call-1",
            toolName: "bash",
            content: [{ type: "text", text: "hello\n" }],
            isError: false,
            details: { exitCode: 0 },
            timestamp: T0 + 200,
        },
    },
});

// 4. Compaction checkpoint with a retained tail.
entry({
    id: "e4-compaction",
    parentId: "e3-toolresult",
    seq: 4,
    type: "compaction",
    timestamp: T0 + 300,
    payload: {
        summary: "## Goal\nkeep the fixture small",
        retainedTail: [
            { role: "user", content: "summarised earlier", timestamp: T0 + 250 },
            {
                role: "assistant",
                content: [{ type: "text", text: "ok" }],
                model: "faux/faux-model",
                timestamp: T0 + 260,
            },
        ],
        tokensBefore: 12345,
        details: { readFiles: ["src/lib.rs"] },
        usage: {
            input: 100,
            output: 20,
            cacheRead: 0,
            cacheWrite: 0,
            totalTokens: 120,
            cost: { input: 0.001, output: 0.002, cacheRead: 0, cacheWrite: 0, total: 0.003 },
        },
        fromHook: false,
    },
});

// 5. Custom entry (`custom_type` column + `payload.data`).
entry({
    id: "e5-custom",
    parentId: "e4-compaction",
    seq: 5,
    type: "custom",
    customType: "ts-fixture:marker",
    timestamp: T0 + 400,
    payload: { data: { fixture: true, version: 1 } },
});

// 6. Branch summary. The Rust `SessionEntry` enum has no variant for it,
//    so the reader passes the payload through as an extension rather than
//    dropping it.
entry({
    id: "e6-branch-summary",
    parentId: "e5-custom",
    seq: 6,
    type: "branch_summary",
    timestamp: T0 + 500,
    payload: {
        fromId: "e4-compaction",
        summary: "explored branch A",
        details: { files: ["a.rs"] },
        fromHook: true,
    },
});

db.close();

console.log(`wrote upstream-v4 fixture: ${dest}`);
console.log(`DDL source: ${upstreamDdlPath}`);
