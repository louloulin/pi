#!/usr/bin/env node
// Cross-tool validation helper: dump the entries from a Rust-written
// SQLite session file using Node, printing each entry's payload as one
// JSON line.
//
// Usage:
//   node pi-rust/crates/pi-session/scripts/dump-rust-db.mjs \
//     /path/to/session.sqlite
//
// Understands both layouts:
//   * upstream v4 (what the writer emits since Stage 55): plain-JSON
//     `entries.payload` TEXT;
//   * the pre-Stage-55 Rust legacy layout: zstd-compressed BLOBs (still
//     readable, with a fallback for uncompressed payloads).

import { DatabaseSync } from "node:sqlite";
import { zstdDecompressSync } from "node:zlib";

const path = process.argv[2];
if (!path) {
    console.error("usage: dump-rust-db.mjs <session.sqlite>");
    process.exit(2);
}

const db = new DatabaseSync(path, { readOnly: true });

function tableColumns(table) {
    return db
        .prepare(`PRAGMA table_info(${table})`)
        .all()
        .map((row) => row.name);
}

const entryColumns = tableColumns("entries");
const isUpstream = entryColumns.includes("parent_id");

const rows = isUpstream
    ? db
          .prepare(
              "SELECT id, parent_id, seq, type, custom_type, payload FROM entries ORDER BY seq ASC",
          )
          .all()
    : db
          .prepare("SELECT entry_id, parent_entry_id, seq, type, payload FROM entries ORDER BY seq ASC")
          .all();

function decodePayload(payload) {
    if (typeof payload === "string") {
        return payload;
    }
    const bytes = Buffer.from(payload);
    // zstd frame magic (0x28 0xB5 0x2F 0xFD) => compressed legacy payload.
    if (bytes.length >= 4 && bytes[0] === 0x28 && bytes[1] === 0xb5 && bytes[2] === 0x2f && bytes[3] === 0xfd) {
        return zstdDecompressSync(bytes).toString("utf8");
    }
    return bytes.toString("utf8");
}

for (const row of rows) {
    process.stdout.write(`${decodePayload(row.payload)}\n`);
}

db.close();
