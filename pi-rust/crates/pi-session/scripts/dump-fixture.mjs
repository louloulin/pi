#!/usr/bin/env node
// Cross-tool validation helper: dump the entries from the upstream-v4
// fixture using Node, and print them as JSON Lines. The matching Rust
// output is produced by `pi session show <id> --database <fixture>`.
//
// The upstream fixture stores `entries.payload` as plain JSON text; a
// Rust-written database stores zstd-compressed JSON instead. Both are
// handled here so the same helper works on either file.
//
// Usage:
//   node pi-rust/crates/pi-session/scripts/dump-fixture.mjs \
//     pi-rust/crates/pi-session/fixtures/ts_recorded.sqlite

import { DatabaseSync } from "node:sqlite";
import { zstdDecompressSync } from "node:zlib";

const path = process.argv[2];
if (!path) {
    console.error("usage: dump-fixture.mjs <fixture.sqlite>");
    process.exit(2);
}

const db = new DatabaseSync(path);
// Column names differ per layout; `id` is upstream-only.
const upstreamIdCount = db
    .prepare("SELECT count(*) AS n FROM pragma_table_info('entries') WHERE name = 'id'")
    .get().n;
const select = upstreamIdCount > 0
    ? "SELECT id, seq, type, custom_type, timestamp, payload FROM entries ORDER BY seq ASC"
    : "SELECT NULL AS id, seq, type, NULL AS custom_type, timestamp, payload FROM entries ORDER BY seq ASC";
const rows = db.prepare(select).all();

for (const row of rows) {
    const json =
        typeof row.payload === "string"
            ? row.payload
            : zstdDecompressSync(row.payload).toString("utf8");
    process.stdout.write(`${json}\n`);
}
