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
// The `usage_ledger` rows follow `session/usage-ledger.ts` +
// `session-stats.ts`: `entry_id` is set when the usage is attributable to
// an entry, `adjustment` marks a harness-recorded correction (and still
// counts into `sessions.usage_payload`, because `applyCommit` calls
// `addUsageToSessionStats` for every ledger row). The `branch_*` rows
// follow the branch index `branch-entries.ts` maintains: a root branch
// named after the first entry, plus one divergent branch that forks at
// the newest compaction boundary and therefore re-owns the post-
// compaction tail (`branch_entries` rows for the shared entries exist
// once per branch). `seq` is handed out from the *shared* entry/usage
// counter, so the fork entry reuses no sequence number.
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

const usageInsert = db.prepare(
    `INSERT INTO usage_ledger
        (session_id, id, seq, entry_id, adjustment, usage, details)
     VALUES (?, ?, ?, ?, ?, ?, ?)`,
);

const branchEntryInsert = db.prepare(
    `INSERT INTO branch_entries
        (session_id, branch_id, entry_id, entry_seq, entry_type)
     VALUES (?, ?, ?, ?, ?)`,
);

const branchMetaInsert = db.prepare(
    `INSERT INTO branch_meta
        (session_id, branch_id, tip_entry_id, tip_seq, base_branch_id, base_seq)
     VALUES (?, ?, ?, ?, ?, ?)`,
);

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

/**
 * One upstream `UsageRow` written as a `usage_ledger` row. `adjustment`
 * is stored as the upstream 0/1 integer; `entry_id` and `details` are
 * NULL when the caller passes null.
 */
function usageRow({ id, seq, entryId = null, adjustment, usage: counters, details = null }) {
    usageInsert.run(
        sessionId,
        id,
        seq,
        entryId,
        adjustment ? 1 : 0,
        JSON.stringify(counters),
        details === null ? null : JSON.stringify(details),
    );
}

/** Upstream `addUsage`, including the `cost` sub-object. */
function sumUsage(rows) {
    const total = {
        input: 0,
        output: 0,
        cacheRead: 0,
        cacheWrite: 0,
        totalTokens: 0,
        cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
    };
    for (const row of rows) {
        total.input += row.input;
        total.output += row.output;
        total.cacheRead += row.cacheRead;
        total.cacheWrite += row.cacheWrite;
        total.totalTokens += row.totalTokens;
        for (const key of ["input", "output", "cacheRead", "cacheWrite", "total"]) {
            total.cost[key] += row.cost?.[key] ?? 0;
        }
    }
    return total;
}

// Usage-ledger rows. `seq` comes from the same counter as `entries.seq`
// (`prepareStorageCommit` allocates one sequence per write), which is why
// they sit at 7..9 between the six entries and the fork entry at 10.
//
// u1/u2 mirror the usage the assistant message and the compaction entry
// already carry — upstream records both the entry and its ledger row.
// u3 is a harness-recorded adjustment (`adjustment: true`, no `entry_id`)
// and is deliberately *not* excluded from the cached total: `applyCommit`
// feeds every ledger row to `addUsageToSessionStats`.
const usageLedgerRows = [
    {
        id: "u1-assistant",
        seq: 7,
        entryId: "e2-assistant",
        adjustment: false,
        usage: {
            input: 120,
            output: 8,
            cacheRead: 4,
            cacheWrite: 0,
            totalTokens: 128,
            cost: { input: 0.001, output: 0.002, cacheRead: 0, cacheWrite: 0, total: 0.003 },
        },
        details: null,
    },
    {
        id: "u2-compaction",
        seq: 8,
        entryId: "e4-compaction",
        adjustment: false,
        usage: {
            input: 100,
            output: 20,
            cacheRead: 0,
            cacheWrite: 0,
            totalTokens: 120,
            cost: { input: 0.001, output: 0.002, cacheRead: 0, cacheWrite: 0, total: 0.003 },
        },
        details: null,
    },
    {
        id: "u3-hook-adjustment",
        seq: 9,
        entryId: null,
        adjustment: true,
        usage: {
            input: 5,
            output: 5,
            cacheRead: 1,
            cacheWrite: 2,
            totalTokens: 13,
            cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
        },
        details: { reason: "hook", source: "fixture", note: "counts into the cached total" },
    },
];

// The session row. Upstream stores the pi version (if any) in `metadata`;
// the Rust reader reads `metadata.version` because upstream has no
// `sessions.version` column. `message_count` / `usage_payload` are the
// projections upstream maintains: four `type = "message"` entries
// (e1..e3 + the fork entry) and the sum of *all* ledger rows, adjustment
// included.
// `next_seq` is 11: six entries + three ledger rows + one fork entry.
sessionInsert.run(
    sessionId,
    T0,
    JSON.stringify({ version: "0.85.1-fixture", source: "sqlite-node" }),
    4,
    JSON.stringify(sumUsage(usageLedgerRows.map((row) => row.usage))),
    11,
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

// 7. Fork entry: a user message whose parent is the *tool result* (seq
//    3), not the branch tip. Upstream gives it a fresh branch that forks
//    at the newest compaction boundary (e4-compaction, seq 4) and copies
//    the post-compaction tail (e5, e6) into the new branch, so the fork
//    keeps the compacted context. Its `seq` is 10 because the shared
//    counter already spent 7..9 on the ledger rows above.
entry({
    id: "e7-fork",
    parentId: "e3-toolresult",
    seq: 10,
    type: "message",
    timestamp: T0 + 600,
    payload: {
        message: {
            role: "user",
            content: [{ type: "text", text: "try the other approach instead" }],
            timestamp: T0 + 600,
        },
    },
});

for (const row of usageLedgerRows) {
    usageRow(row);
}

// The branch index. `e1-user` is the linear root branch (every entry
// hangs off the previous tip); `e7-fork` is the divergent one described
// above — its `base_seq` is the compaction boundary, and entries 5 and 6
// appear in *both* branches because the tail is copied, not moved.
const branchEntriesByBranch = {
    "e1-user": [
        ["e1-user", 1, "message"],
        ["e2-assistant", 2, "message"],
        ["e3-toolresult", 3, "message"],
        ["e4-compaction", 4, "compaction"],
        ["e5-custom", 5, "custom"],
        ["e6-branch-summary", 6, "branch_summary"],
    ],
    "e7-fork": [
        ["e5-custom", 5, "custom"],
        ["e6-branch-summary", 6, "branch_summary"],
        ["e7-fork", 10, "message"],
    ],
};

for (const [branchId, rows] of Object.entries(branchEntriesByBranch)) {
    for (const [entryId, entrySeq, entryType] of rows) {
        branchEntryInsert.run(sessionId, branchId, entryId, entrySeq, entryType);
    }
}

branchMetaInsert.run(sessionId, "e1-user", "e6-branch-summary", 6, null, null);
branchMetaInsert.run(sessionId, "e7-fork", "e7-fork", 10, "e1-user", 4);

db.close();

console.log(`wrote upstream-v4 fixture: ${dest}`);
console.log(`DDL source: ${upstreamDdlPath}`);
