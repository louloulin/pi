//! `usage_ledger` reads (upstream v4 layout only).
//!
//! The upstream `packages/session-backends/sqlite-node` backend keeps an
//! append-only usage ledger next to `entries`:
//!
//! ```sql
//! usage_ledger(session_id, id, seq, entry_id, adjustment, usage TEXT, details TEXT)
//! PRIMARY KEY (session_id, id)
//! ```
//!
//! Each row is one `Usage` object (`usage-ledger.ts`
//! `decodeUsageLedgerRow`) recorded by the harness:
//!
//! * `seq` comes from the **same** counter as `entries.seq` — upstream
//!   `prepareStorageCommit` hands out one sequence per write kind — and
//!   the two tables even share the id namespace: `trg_entries_validate`
//!   and `trg_usage_ledger_validate` reject an id that exists in the
//!   other table. Nothing here may assume the id spaces are disjoint.
//! * `entry_id` is the entry the usage is attributed to (`NULL` for
//!   unattributed rows).
//! * `adjustment` is a **provenance** flag, not a sign: upstream
//!   `SqliteStorage.applyCommit` calls `addUsageToSessionStats` for
//!   *every* usage row, whether it came from a per-response ledger write
//!   (`adjustment: false`) or from a harness-recorded correction / hook /
//!   v3 import (`adjustment: true`). Adjustment rows therefore **count
//!   into the totals exactly like ordinary rows**.
//!
//! # Signed corrections
//!
//! Upstream's `Usage` counters are plain JavaScript numbers, so a
//! correction row can carry a negative counter (`addUsage` then
//! subtracts). [`pi_protocol::Usage`] stores `u32`, so this reader cannot
//! represent a negative counter: it logs a `tracing::warn!` and clamps
//! the counter to `0` (see [`usage_from_json`]). Totals use saturating
//! arithmetic for the same reason. A session whose ledger contains
//! negative counters therefore reports a total that is >= the upstream
//! one — the gap is logged, never silent.

use pi_protocol::Usage;
use rusqlite::{types::Value as SqlValue, Row};
use serde_json::Value;

use crate::error::{Result, SessionError};
use crate::reader::{DecodedEntry, SessionReader};
use crate::schema::SchemaLayout;

/// One decoded `usage_ledger` row.
///
/// Mirrors the upstream `UsageRow` (`packages/agent/src/harness/session/types.ts`)
/// field for field, minus the `seq` that the storage layer assigns.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageLedgerRow {
    /// Ledger id (`usage_ledger.id`, primary key together with `session_id`).
    /// Shares its namespace with `entries.id`.
    pub id: String,
    /// Sequence number from the shared entry/usage counter.
    pub seq: i64,
    /// Entry this usage is attributed to, when it has one.
    pub entry_id: Option<String>,
    /// Provenance flag: `true` for harness-recorded adjustments
    /// (corrections, hooks, v3 imports), `false` for per-response rows.
    /// Does **not** change how the row counts into the totals.
    pub adjustment: bool,
    /// Decoded usage counters.
    pub usage: Usage,
    /// Optional upstream `details` JSON (`null` in SQL is `None` here).
    pub details: Option<Value>,
}

/// Range / ordering for [`SessionReader::scan_usage`]. Mirrors the
/// upstream `UsageScan` type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UsageScan {
    /// Only rows with `seq >= from_seq`.
    pub from_seq: Option<i64>,
    /// Only rows with `seq <= to_seq`.
    pub to_seq: Option<i64>,
    /// Row order; mirrors upstream's `order` (`"asc"` default).
    pub order: UsageOrder,
    /// Maximum number of rows.
    pub limit: Option<usize>,
}

/// `usage_ledger` scan order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageOrder {
    /// Ascending `seq` (upstream default).
    #[default]
    Asc,
    /// Descending `seq`.
    Desc,
}

impl SessionReader {
    /// All usage-ledger rows of one session, ascending by `seq`.
    ///
    /// Equivalent to upstream `scanUsage({ order: "asc" })`.
    pub fn iter_usage(&self, session_id: &str) -> Result<Vec<UsageLedgerRow>> {
        self.scan_usage(session_id, &UsageScan::default())
    }

    /// Usage-ledger rows with explicit range, order and limit — the Rust
    /// analogue of upstream `Storage::scanUsage(query)`.
    pub fn scan_usage(&self, session_id: &str, scan: &UsageScan) -> Result<Vec<UsageLedgerRow>> {
        require_upstream(self, "usage_ledger")?;
        let mut sql = String::from(
            "SELECT id, seq, entry_id, adjustment, usage, details \
             FROM usage_ledger WHERE session_id = ?1",
        );
        let mut params: Vec<SqlValue> = vec![SqlValue::Text(session_id.to_string())];
        if let Some(from_seq) = scan.from_seq {
            params.push(SqlValue::Integer(from_seq));
            sql.push_str(&format!(" AND seq >= ?{}", params.len()));
        }
        if let Some(to_seq) = scan.to_seq {
            params.push(SqlValue::Integer(to_seq));
            sql.push_str(&format!(" AND seq <= ?{}", params.len()));
        }
        // `seq` alone is not guaranteed unique across a hand-edited file,
        // so `id` breaks ties deterministically (upstream only sorts by
        // `seq`; the tiebreak is unobservable for real writers).
        sql.push_str(match scan.order {
            UsageOrder::Asc => " ORDER BY seq ASC, id ASC",
            UsageOrder::Desc => " ORDER BY seq DESC, id ASC",
        });
        if let Some(limit) = scan.limit {
            // `usize` is not injectable; formatted directly to keep the
            // parameter list to the values above.
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        let mut stmt = self.connection().prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(decode_usage_ledger_row(row)?);
        }
        Ok(out)
    }

    /// Usage-ledger rows attributed to one entry id (`entry_id` column).
    ///
    /// Upstream allows several ledger rows per entry (a response plus a
    /// later adjustment), so this returns a `Vec` ordered by `seq`.
    pub fn usage_for_entry(&self, session_id: &str, entry_id: &str) -> Result<Vec<UsageLedgerRow>> {
        require_upstream(self, "usage_ledger")?;
        let mut stmt = self.connection().prepare(
            "SELECT id, seq, entry_id, adjustment, usage, details \
             FROM usage_ledger \
             WHERE session_id = ?1 AND entry_id = ?2 \
             ORDER BY seq ASC, id ASC",
        )?;
        let mut rows = stmt.query(rusqlite::params![session_id, entry_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(decode_usage_ledger_row(row)?);
        }
        Ok(out)
    }

    /// Sum of every ledger row of a session — the authoritative usage
    /// aggregate, with `adjustment` rows included (see the module docs).
    ///
    /// Returns [`Usage::default`] when the session has no ledger rows.
    pub fn usage_totals(&self, session_id: &str) -> Result<Usage> {
        Ok(sum_usage(
            self.iter_usage(session_id)?.iter().map(|row| row.usage),
        ))
    }
}

/// Upstream `addUsage(left, right)` — component-wise addition including
/// `totalTokens`.
///
/// The upstream function also adds the optional `cacheWrite1h` /
/// `reasoning` counters; [`pi_protocol::Usage`] has no fields for them, so
/// they are dropped here (documented in `reader.rs`).
///
/// Saturating: token counters cannot overflow into a wrong-but-plausible
/// number.
pub fn add_usage(left: Usage, right: Usage) -> Usage {
    Usage {
        input: left.input.saturating_add(right.input),
        output: left.output.saturating_add(right.output),
        cache_read: left.cache_read.saturating_add(right.cache_read),
        cache_write: left.cache_write.saturating_add(right.cache_write),
        total: left.total.saturating_add(right.total),
    }
}

/// Fold an iterator of usages with [`add_usage`].
pub fn sum_usage(usages: impl IntoIterator<Item = Usage>) -> Usage {
    usages.into_iter().fold(Usage::default(), add_usage)
}

/// Decode one upstream `Usage` JSON object.
///
/// Field names are the upstream camelCase ones (`cacheRead`, `cacheWrite`,
/// `totalTokens`); `cost`, `cacheWrite1h` and `reasoning` have no
/// `pi_protocol::Usage` counterpart and are ignored. Negative counters —
/// legal upstream as signed corrections — cannot be represented and are
/// clamped to `0` with a warning.
pub(crate) fn usage_from_json(value: &Value) -> Usage {
    Usage {
        input: counter(value, "input"),
        output: counter(value, "output"),
        cache_read: counter(value, "cacheRead"),
        cache_write: counter(value, "cacheWrite"),
        total: counter(value, "totalTokens"),
    }
}

/// Read one token counter out of an upstream usage object.
fn counter(value: &Value, name: &str) -> u32 {
    let Some(raw) = value.get(name) else {
        return 0;
    };
    if let Some(number) = raw.as_u64() {
        return match u32::try_from(number) {
            Ok(number) => number,
            Err(_) => {
                tracing::warn!(
                    field = name,
                    value = number,
                    "pi-session: usage counter exceeds u32::MAX; clamping"
                );
                u32::MAX
            }
        };
    }
    if let Some(number) = raw.as_i64() {
        if number < 0 {
            tracing::warn!(
                field = name,
                value = number,
                "pi-session: negative usage counter (signed upstream correction) cannot be represented by pi_protocol::Usage; clamping to 0"
            );
            return 0;
        }
        return u32::try_from(number).unwrap_or(u32::MAX);
    }
    if let Some(number) = raw.as_f64() {
        if number < 0.0 {
            tracing::warn!(
                field = name,
                value = number,
                "pi-session: negative usage counter (signed upstream correction) cannot be represented by pi_protocol::Usage; clamping to 0"
            );
            return 0;
        }
        return number.round().clamp(0.0, f64::from(u32::MAX)) as u32;
    }
    if !raw.is_null() {
        tracing::warn!(
            field = name,
            "pi-session: usage counter is not a number; treating it as 0"
        );
    }
    0
}

/// Turn one `usage_ledger` row into a [`UsageLedgerRow`].
fn decode_usage_ledger_row(row: &Row<'_>) -> Result<UsageLedgerRow> {
    let usage: String = row.get(4)?;
    let details: Option<String> = row.get(5)?;
    let parsed: Value = serde_json::from_str(&usage)?;
    Ok(UsageLedgerRow {
        id: row.get(0)?,
        seq: row.get(1)?,
        entry_id: row.get(2)?,
        adjustment: row.get::<_, i64>(3)? != 0,
        usage: usage_from_json(&parsed),
        details: details
            .map(|raw| serde_json::from_str::<Value>(&raw))
            .transpose()?,
    })
}

/// `usage_ledger` exists only in the upstream v4 layout; a Rust legacy
/// file has no ledger to read.
pub(crate) fn require_upstream(reader: &SessionReader, table: &str) -> Result<()> {
    if reader.layout() == SchemaLayout::RustLegacy {
        return Err(SessionError::Other(format!(
            "{table} reads require the upstream v4 layout; {} uses the Rust legacy layout — convert it with `pi session migrate`",
            reader.path().display()
        )));
    }
    Ok(())
}

impl UsageLedgerRow {
    /// The entry this row is attributed to, decoded — `None` when the row
    /// has no `entry_id` or the entry is not in the database any more.
    pub fn entry(&self, reader: &SessionReader, session_id: &str) -> Result<Option<DecodedEntry>> {
        match &self.entry_id {
            Some(entry_id) => reader.get_message(session_id, entry_id),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn upstream_field_names_decode_into_the_rust_usage() {
        let usage = usage_from_json(&json!({
            "input": 120,
            "output": 8,
            "cacheRead": 4,
            "cacheWrite": 2,
            "totalTokens": 134,
            "cost": {"input": 0.1, "output": 0.2, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.3},
        }));
        assert_eq!(
            usage,
            Usage {
                input: 120,
                output: 8,
                cache_read: 4,
                cache_write: 2,
                total: 134,
            }
        );
    }

    #[test]
    fn missing_counters_default_to_zero() {
        assert_eq!(usage_from_json(&json!({ "input": 3 })).input, 3);
        assert_eq!(usage_from_json(&json!({ "input": 3 })).total, 0);
        assert_eq!(usage_from_json(&json!({})), Usage::default());
    }

    #[test]
    fn negative_counters_clamp_to_zero_instead_of_wrapping() {
        // A signed upstream correction cannot be represented by `u32`;
        // wrapping would turn -5 into ~4 billion.
        assert_eq!(usage_from_json(&json!({ "input": -5 })).input, 0);
    }

    #[test]
    fn counters_above_u32_max_saturate() {
        assert_eq!(
            usage_from_json(&json!({ "output": u64::from(u32::MAX) + 1 })).output,
            u32::MAX
        );
    }

    #[test]
    fn add_usage_folds_every_counter_including_total() {
        let left = Usage {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            total: 10,
        };
        let right = Usage {
            input: 5,
            output: 6,
            cache_read: 7,
            cache_write: 8,
            total: 26,
        };
        assert_eq!(
            add_usage(left, right),
            Usage {
                input: 6,
                output: 8,
                cache_read: 10,
                cache_write: 12,
                total: 36,
            }
        );
    }

    #[test]
    fn sum_usage_of_nothing_is_zero() {
        assert_eq!(sum_usage(std::iter::empty()), Usage::default());
    }
}
