# `pi-session` — SQLite session backend

`pi-session` is the Rust analogue of
[`packages/session-backends/sqlite-node`](../../packages/session-backends/sqlite-node)
in the TS monorepo. It opens session databases produced by **either**
side: this crate's own `SessionWriter` layout, or the upstream
`AgentHarness storage format 4 / storageVersion 1` layout the TS writer
produces. Reading the upstream layout is supported today; **writing** it
is not — `SessionWriter` still emits the Rust layout, so a file written
here is only readable by this crate. Aligning the write path is a later
slice (see [Layouts](#layouts) below).

## When to use

- Default session storage for `pi-coding-agent` (Stage 5).
- Programmatic session persistence from `pi-agent` or external tools.
- Migrating the Stage 4 JSONL session files into a single, queryable
  SQLite database (see `pi session migrate`).

## Layouts

Two incompatible layouts exist on disk. `SessionReader::open` detects
which one a file uses by probing the table structure
(`SchemaLayout::{RustLegacy, UpstreamV4}`) — never `PRAGMA user_version`,
which the upstream writer does not set.

| | upstream `session-backends/sqlite-node` (AgentHarness storage format 4 / `storageVersion 1`) | `pi-session` (`RustLegacy`) |
| --- | --- | --- |
| session row | `sessions(id, created_at, parent_session_id, storage_version, metadata, message_count, usage_payload, next_seq)` | `sessions(id, created_at, parent_session, cwd, version, metadata)` |
| entry row | `entries(session_id, id, parent_id, seq, type, custom_type, timestamp, payload TEXT)`, PK `(session_id, id)` | `entries(session_id, seq, parent_seq, entry_id, parent_entry_id, type, timestamp, payload BLOB)`, PK `(session_id, seq)` |
| payload | plain JSON text (`entries.ts` `JSON.parse(row.payload)`) | zstd (level 3) compressed JSON BLOB |
| other tables | `scalar_values`, `list_values`, `usage_ledger`, `branch_entries`, `branch_meta` + 3 triggers | `meta(key, value)` |
| version marker | `sessions.storage_version = 1` column (does **not** write `PRAGMA user_version`) | `PRAGMA user_version = 1` |

### Rust layout (what `SessionWriter` writes)

```sql
CREATE TABLE sessions (
    id              TEXT PRIMARY KEY,
    created_at      INTEGER NOT NULL,   -- millis since unix epoch
    parent_session  TEXT,
    cwd             TEXT,
    version         TEXT,
    metadata        TEXT                -- JSON-encoded, optional
) WITHOUT ROWID;

CREATE TABLE entries (
    session_id      TEXT NOT NULL,
    seq             INTEGER NOT NULL,   -- 1-based
    parent_seq      INTEGER,
    entry_id        TEXT,
    parent_entry_id TEXT,
    type            TEXT NOT NULL,      -- header / user_message / ...
    timestamp       INTEGER NOT NULL,   -- millis since unix epoch
    payload         BLOB NOT NULL,      -- zstd(JSON(SessionEntry))
    PRIMARY KEY (session_id, seq)
) WITHOUT ROWID;

CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
```

This layout is versioned via `PRAGMA user_version` (currently `1`). The
reader refuses a newer `user_version` with `SessionError::Corrupt`.

### Upstream layout (read-only for now)

`SessionReader` maps upstream rows onto the Rust types:

- `sessions.created_at` (milliseconds) → `SessionRow::created_at` and
  `SessionEntry::Header::created_at`; the unit is already milliseconds,
  so no conversion happens.
- upstream has no `sessions.version` and no `sessions.cwd`: the version
  is read from the `metadata` JSON (`{"version": "..."}`) and falls back
  to an empty string, while `SessionRow::cwd` is always `None`.
- `entries.type = "message"` is dispatched on `payload.message.role`
  into `UserMessage` / `AssistantMessage` / `ToolResult`;
  `"compaction"` becomes `SessionEntry::Compaction`; `"custom"` becomes
  `SessionEntry::Extension` with `kind = custom_type`.
- `entries.id` / `entries.parent_id` become `entry_id` /
  `parent_entry_id`; `get_entry` still looks up by `seq` and
  `get_message` matches the upstream primary key.

Known degradations (deliberate, all logged with `tracing::warn!` rather
than silently dropped):

- **`branch_summary`** has no `SessionEntry` variant because
  `pi-coding-agent` matches that enum exhaustively; the full upstream
  payload is passed through as an `Extension`
  (`extension = "branch_summary"`).
- Assistant **`thinking` content blocks** have no `pi_protocol::Content`
  variant yet and are skipped.
- A **tool result** with several content blocks is folded into the single
  block `pi_protocol::ToolResult::content` can hold: text blocks are
  joined with `\n`, a text/image mix keeps the first block.
- Upstream `terminate`, `fromHook`, `api`, `provider`, `responseId`,
  `diagnostics` and per-message `timestamp` fields have no Rust
  counterpart.
- A file whose `sessions` and `entries` tables disagree about the layout
  (or whose layout is unknown) is rejected as `SessionError::Corrupt`
  instead of being guessed at.

Still open (later slices): writing the upstream layout, `usage_ledger` /
`message_count` / `usage_payload` aggregation, and `branch_entries` /
`branch_meta` reads.

## Public API

```rust,no_run
use pi_session::{SessionWriter, SessionReader, SessionEntry};

let writer = SessionWriter::open("/tmp/example.sqlite")?;
writer.write_header(SessionEntry::Header {
    id: "demo".into(),
    created_at: chrono::Utc::now(),
    version: "0.1.0".into(),
})?;
writer.append(SessionEntry::Extension {
    extension: "demo".into(),
    kind: "ping".into(),
    payload: serde_json::json!({"hello": "world"}),
})?;
writer.checkpoint()?;

let reader = SessionReader::open("/tmp/example.sqlite")?;
let entries = reader.iter_entries("demo")?;
assert_eq!(entries.len(), 1);
```

## Migration

Stage 4 of the Rust port wrote sessions as JSONL (one
`SessionEntry` per line). The [`migrate::migrate_jsonl`] helper replays
those files into a fresh SQLite database; the original JSONL is
preserved on disk. The binary ships this as `pi session migrate
<path-to-jsonl>`.

## TS compatibility

The `tests/ts_compat.rs` fixture (`fixtures/ts_recorded.sqlite`) is built
from the **upstream** `001_initial.sql` DDL by
`scripts/make-ts-fixture.mjs`, with plain-JSON upstream entry payloads —
it is not a file the Rust writer could have produced. The generator is a
hand-written SQL script rather than an invocation of the real TS
`SqliteStorage`, because that would need a `node_modules` install; the
test asserts the fixture's columns, payload types and decoded entries so
the shape cannot silently drift back into the Rust layout.

```
$ node pi-rust/crates/pi-session/scripts/make-ts-fixture.mjs \
    pi-rust/crates/pi-session/fixtures/ts_recorded.sqlite
$ cargo test -p pi-session --test ts_compat
```

## Tests

```sh
cargo test -p pi-session
cargo clippy -p pi-session --all-targets -- -D warnings
```

The `tests/` directory contains:

- `round_trip.rs` — six tests covering header idempotency, full
  user/assistant/tool/tool-result/extension round-trips, compaction,
  latest-session resolution, corrupt-DB detection, and empty-DB behavior.
- `ts_compat.rs` — eight tests that open the upstream-format fixture and
  assert its structure, its decoded `SessionEntry` sequence, the
  upstream key lookups, and that Rust legacy files still read.

The fixture is generated by `scripts/make-ts-fixture.mjs` using Node 22's
built-in `node:sqlite` module — no npm install required.