#!/usr/bin/env node
// Cross-implementation check: open a session database written by the Rust
// `pi-session` writer with Node's built-in `node:sqlite` (the same engine
// the TS `packages/session-backends/sqlite-node` backend uses), read every
// row, and print the result as JSON on stdout.
//
// Usage:
//   node scripts/verify-upstream-db.mjs <database.sqlite>
//
// Exit code 0 = the file is a readable upstream v4 (AgentHarness storage
// format 4 / storageVersion 1) database. Anything else prints a diagnostic
// on stderr and exits non-zero. The database is opened read-only, so this
// never mutates the file under test.

import { DatabaseSync } from "node:sqlite";

const EXPECTED_TABLES = [
  "sessions",
  "entries",
  "scalar_values",
  "list_values",
  "usage_ledger",
  "branch_entries",
  "branch_meta",
];

function fail(message) {
  process.stderr.write(`verify-upstream-db: ${message}\n`);
  process.exit(1);
}

const path = process.argv[2];
if (!path) {
  fail("usage: node scripts/verify-upstream-db.mjs <database.sqlite>");
}

let db;
try {
  db = new DatabaseSync(path, { readOnly: true });
} catch (error) {
  fail(`cannot open ${path} read-only: ${error.message}`);
}

try {
  const tableNames = db
    .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
    .all()
    .map((row) => row.name);
  for (const table of EXPECTED_TABLES) {
    if (!tableNames.includes(table)) {
      fail(`missing upstream table ${table} (found: ${tableNames.join(", ")})`);
    }
  }
  if (tableNames.includes("meta")) {
    fail("found the Rust legacy `meta` table; this is not an upstream file");
  }

  const triggerNames = db
    .prepare("SELECT name FROM sqlite_master WHERE type='trigger' ORDER BY name")
    .all()
    .map((row) => row.name);
  for (const trigger of ["trg_entries_validate", "trg_usage_ledger_validate"]) {
    if (!triggerNames.includes(trigger)) {
      fail(`missing upstream trigger ${trigger}`);
    }
  }

  const userVersion = db.prepare("PRAGMA user_version").get().user_version;
  if (userVersion !== 0) {
    fail(`expected PRAGMA user_version == 0, got ${userVersion}`);
  }

  const sessions = db
    .prepare(
      "SELECT id, message_count, next_seq, storage_version, metadata, usage_payload FROM sessions ORDER BY id",
    )
    .all()
    .map((row) => {
      if (row.storage_version !== 1) {
        fail(`session ${row.id} has storage_version ${row.storage_version}, want 1`);
      }
      return {
        id: row.id,
        messageCount: row.message_count,
        nextSeq: row.next_seq,
        storageVersion: row.storage_version,
        metadata: row.metadata === null ? null : JSON.parse(row.metadata),
        usage: JSON.parse(row.usage_payload),
      };
    });

  const entries = db
    .prepare(
      "SELECT session_id, id, parent_id, seq, type, custom_type, payload FROM entries ORDER BY session_id, seq",
    )
    .all()
    .map((row) => ({
      sessionId: row.session_id,
      id: row.id,
      parentId: row.parent_id,
      seq: row.seq,
      type: row.type,
      customType: row.custom_type,
      // Plain JSON TEXT — a throw here means the payload is not upstream-shaped.
      payload: JSON.parse(row.payload),
    }));

  process.stdout.write(
    `${JSON.stringify({ path, tables: tableNames, triggers: triggerNames, userVersion, sessions, entries }, null, 2)}\n`,
  );
} catch (error) {
  fail(error.stack ?? String(error));
} finally {
  db.close();
}
