#!/usr/bin/env node
// Cross-tool validation helper: dump the entries from the TS-recorded
// fixture using Node, and print them as JSON Lines. The matching Rust
// output is produced by `pi session show <id> --database <fixture>`.
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
const rows = db.prepare(
    "SELECT seq, type, payload FROM entries ORDER BY seq ASC",
).all();
for (const row of rows) {
    const compressed = row.payload;
    const json = zstdDecompressSync(compressed).toString("utf8");
    process.stdout.write(`${json}\n`);
}
db.close();