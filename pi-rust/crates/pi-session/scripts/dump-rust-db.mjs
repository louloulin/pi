#!/usr/bin/env node
// Cross-tool validation helper: dump the entries from a Rust-written
// SQLite file using Node, and print them as JSON Lines.
//
// Usage:
//   node pi-rust/crates/pi-session/scripts/dump-rust-db.mjs \
//     /path/to/rust-written.sqlite

import { DatabaseSync } from "node:sqlite";
import { zstdDecompressSync } from "node:zlib";

const path = process.argv[2];
if (!path) {
    console.error("usage: dump-rust-db.mjs <rust-written.sqlite>");
    process.exit(2);
}

const db = new DatabaseSync(path);
const rows = db.prepare(
    "SELECT seq, type, payload FROM entries ORDER BY seq ASC",
).all();
for (const row of rows) {
    const compressed = row.payload;
    const json = zstdDecompressSync(compressed).toString("utf8");
    process.stdout.write(`${json}\n`);
}
db.close();