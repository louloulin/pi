# `pi-session` — SQLite session backend

`pi-session` is the Rust analogue of
[`packages/session-backends/sqlite-node`](../../packages/session-backends/sqlite-node)
in the TS monorepo. Since Stage 55 its `SessionWriter` emits the
**upstream** `AgentHarness storage format 4 / storageVersion 1` layout —
DDL and payload shape identical to `001_initial.sql` — so a session file
written by Rust opens directly in the TS `SqliteStorage` (and vice
versa). The pre-Stage-55 Rust layout is still readable and can be
converted with `pi session migrate`.

## When to use

- Default session storage for `pi-coding-agent` (Stage 5).
- Programmatic session persistence from `pi-agent` or external tools.
- Migrating the Stage 4 JSONL session files into a single, queryable
  SQLite database (see `pi session migrate`).
- Migrating pre-Stage-55 Rust session files into the upstream layout.

## Layouts

Two incompatible layouts exist on disk. `SessionReader::open` detects
which one a file uses by probing the table structure
(`SchemaLayout::{RustLegacy, UpstreamV4}`) — never `PRAGMA user_version`,
which the upstream writer does not set.

| | upstream `session-backends/sqlite-node` (AgentHarness storage format 4 / `storageVersion 1`) | `pi-session` Rust legacy (Stage 5) |
| --- | --- | --- |
| written by | `SessionWriter` (since Stage 55) and the TS backend | the Rust writer before Stage 55 — read-only now |
| session row | `sessions(id, created_at, parent_session_id, storage_version, metadata, message_count, usage_payload, next_seq)` | `sessions(id, created_at, parent_session, cwd, version, metadata)` |
| entry row | `entries(session_id, id, parent_id, seq, type, custom_type, timestamp, payload TEXT)`, PK `(session_id, id)` | `entries(session_id, seq, parent_seq, entry_id, parent_entry_id, type, timestamp, payload BLOB)`, PK `(session_id, seq)` |
| payload | plain JSON text (`entries.ts` `JSON.parse(row.payload)`) | zstd (level 3) compressed JSON BLOB |
| other tables | `scalar_values`, `list_values`, `usage_ledger`, `branch_entries`, `branch_meta` + 2 triggers | `meta(key, value)` |
| version marker | `sessions.storage_version = 1` column (does **not** write `PRAGMA user_version`) | `PRAGMA user_version = 1` |

### Upstream v4 layout (what `SessionWriter` writes)

The DDL is embedded verbatim in `schema::UPSTREAM_INITIAL_SQL` and a test
(`tests/upstream_write.rs`) asserts it is byte-identical to
`packages/session-backends/sqlite-node/src/sqlite/migrations/001_initial.sql`,
so the two schemas cannot drift.

A [`SessionEntry`](pi_protocol::SessionEntry) maps onto the upstream
`entries` table like this:

| Rust entry | `entries.type` | `entries.custom_type` | `payload` |
| --- | --- | --- | --- |
| `Header` | — | — | written to `sessions` (`metadata = {"version": …}`) |
| `UserMessage` | `message` | — | `{"message": <AgentMessage>}` |
| `AssistantMessage` | `message` | — | `{"message": <AssistantMessage>}` (camelCase `stopReason`, `usage`, `errorMessage`) |
| `ToolResult` | `message` | — | `{"message": <ToolResultMessage>}` (role `toolResult`) |
| `ToolCall` (standalone) | `custom` | `tool_call` | `{"data": {id, name, arguments}}` |
| `Extension` (`extension = "branch_summary"`) | `branch_summary` | — | the payload verbatim |
| `Extension` (anything else) | `custom` | `kind` | `{"data": <payload>}` |
| `Compaction` | `compaction` | — | `{summary, retainedTail, tokensBefore, fromHook, …}` |

Additional writer behaviour:

- Entry ids are deterministic `e<seq>`, and `parent_id` chains each row
  to the previous one (restored from the database on
  `resume` / repeated `write_header`), which satisfies the
  `trg_entries_validate` trigger.
- `sessions.message_count` (count of `type = "message"` rows) and
  `sessions.next_seq` are updated inside the same transaction as the
  staged rows: a rejected batch (missing parent, duplicate id) rolls
  back completely and leaves no half-written rows.
- `sessions.usage_payload` is initialised to the upstream-shaped zero
  usage object (`zeroUsage()`), **not** `{}` — the upstream read path
  (`addUsageToSessionStats`) indexes the individual counters and would
  produce `NaN` on an empty object. No `usage_ledger` rows are emitted
  yet; usage stays inside each assistant/compaction entry payload.

Deliberate, documented degradations:

- A standalone `ToolCall` has no upstream entry type (upstream keeps tool
  calls inside the assistant `message` content array), so it is stored as
  a `custom` entry with `custom_type = "tool_call"` and reads back as an
  `Extension` with `extension = "custom"`.
- An `Extension`'s **name** is not stored: upstream models that namespace
  with a single `custom_type` string, so the value lands in `kind` and
  reads back as `extension = "custom"`.
- `pi_protocol::Usage` has no cost fields; usage is written with a zeroed
  `cost` object so the JSON parses into the upstream `Usage` type.

### Rust legacy layout (read-only since Stage 55)

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

This layout is versioned via `PRAGMA user_version` (currently `1`).
`SessionWriter::open` refuses to append to a legacy file
(`SessionError::LegacyLayout`); convert it first with `pi session
migrate` (see [Migration](#migration)).

### Reading upstream rows

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
  `SessionEntry::Extension` with `kind = custom_type`;
  `"branch_summary"` passes the payload through as an `Extension`.
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

Still open (later slices): `usage_ledger` / `usage_payload` aggregation
and `branch_entries` / `branch_meta` reads.

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

Two migration directions are supported; both preserve the source file.

- **Stage 4 JSONL → SQLite.** `migrate::migrate_jsonl` replays one
  `SessionEntry` per line into a fresh upstream v4 database. The binary
  exposes it as `pi session migrate <path-to-jsonl>`; the original JSONL
  is left on disk.
- **Rust legacy → upstream v4.** `migrate::migrate_file` reads a legacy
  database read-only and writes the converted copy to a sibling
  `<stem>.upstream.sqlite` (unless a destination is given). The binary
  detects SQLite input from its magic and exposes the same command:
  `pi session migrate <legacy.sqlite>`. An already-upstream input is a
  no-op; an existing destination is never overwritten.

## TS compatibility

The upstream-ness of the write path is pinned from both sides:

- `tests/upstream_write.rs` asserts the embedded DDL is byte-identical to
  the real `001_initial.sql`, checks the written file's columns,
  triggers, payload type, `storage_version` and counters, and
  round-trips six entries (user / assistant+toolCall / toolResult /
  compaction / custom / branch_summary) field for field.
- `tests/node_compat.rs` runs `scripts/verify-upstream-db.mjs` under
  Node's built-in `node:sqlite` — the same engine the TS backend uses —
  against a Rust-written database and asserts the read rows.
- `tests/ts_compat.rs` opens the committed upstream fixture
  (`fixtures/ts_recorded.sqlite`, built by `scripts/make-ts-fixture.mjs`
  from the verbatim upstream DDL) and asserts the Rust reader decodes it.

```
$ node pi-rust/crates/pi-session/scripts/make-ts-fixture.mjs \
    pi-rust/crates/pi-session/fixtures/ts_recorded.sqlite
$ node pi-rust/crates/pi-session/scripts/verify-upstream-db.mjs \
    /path/to/rust-written.sqlite
$ cargo test -p pi-session
```

`scripts/dump-rust-db.mjs` prints a session's entry payloads as JSON
lines and understands both layouts (plain JSON for upstream v4, zstd for
legacy).

## Tests

```sh
cargo test -p pi-session
cargo clippy -p pi-session --all-targets -- -D warnings
```

The `tests/` directory contains:

- `round_trip.rs` — header idempotency, full
  user/assistant/tool/tool-result/extension round-trips, compaction,
  latest-session resolution, corrupt-DB detection, and empty-DB behavior.
- `upstream_write.rs` — the write path's column/shape assertions, the
  DDL byte-identity check, the six-entry field-for-field round trip, and
  the "a rejected batch writes nothing" trigger test.
- `node_compat.rs` — the `node:sqlite` cross-implementation read.
- `migrate_layout.rs` — legacy → upstream conversion, source
  preservation, explicit destinations, idempotence and refusals.
- `ts_compat.rs` — the upstream-format fixture: structure, decoded
  `SessionEntry` sequence, upstream key lookups, and legacy reads.
- `export.rs` / `migrate.rs` unit tests — JSONL export and the JSONL
  migrator.
