//! Session statistics: the cached `sessions` columns and the
//! `entries` + `usage_ledger` recompute path.
//!
//! Upstream keeps two projections of the durable state on the `sessions`
//! row:
//!
//! * `sessions.message_count` — bumped once per committed entry whose
//!   `type = "message"` (`incrementMessageCount`, called from
//!   `SqliteStorage.applyCommit`). `compaction`, `branch_summary` and
//!   `custom` entries do **not** count.
//! * `sessions.usage_payload` — the JSON `Usage` object that
//!   `addUsageToSessionStats` maintains by adding every committed
//!   `usage_ledger` row (`addUsage(current, row.usage)`). It is a cache of
//!   the ledger sum, not an independently computed value.
//!
//! `readSessionStats()` serves `Storage::getStats()` straight from those
//! two columns. This module does the same, but adds a recompute path and
//! compares the two so a drifted cache is **reported** instead of being
//! returned as if it were authoritative.

use pi_protocol::Usage;

use crate::error::Result;
use crate::reader::SessionReader;
use crate::usage::require_upstream;

/// The upstream `SessionStats` shape: the cached message count plus the
/// aggregated usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionStats {
    /// Number of `type = "message"` entries the session has committed.
    pub message_count: i64,
    /// Aggregated usage (upstream `usage_payload`).
    pub usage: Usage,
}

/// Result of [`SessionReader::verify_stats`]: the cached `sessions`
/// values next to a recompute from the durable tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatsCheck {
    /// `sessions.message_count` + `sessions.usage_payload`.
    pub cached: SessionStats,
    /// `count(entries.type = "message")` + the `usage_ledger` sum.
    pub recomputed: SessionStats,
}

impl StatsCheck {
    /// `true` when the cache agrees with the recompute.
    pub fn is_consistent(&self) -> bool {
        self.cached == self.recomputed
    }
}

impl SessionReader {
    /// Cached stats read straight from the `sessions` row — the Rust
    /// equivalent of upstream `readSessionStats()` / `Storage::getStats()`.
    ///
    /// Returns `Ok(None)` for an unknown session id. Use
    /// [`verify_stats`](Self::verify_stats) when the cache must be trusted
    /// only after it has been checked against the durable tables.
    pub fn session_stats(&self, session_id: &str) -> Result<Option<SessionStats>> {
        require_upstream(self, "session stats")?;
        let row = self.connection().query_row(
            "SELECT message_count, usage_payload FROM sessions WHERE id = ?1",
            [session_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        );
        match row {
            Ok((message_count, payload)) => {
                let parsed: serde_json::Value = serde_json::from_str(&payload)?;
                Ok(Some(SessionStats {
                    message_count,
                    usage: crate::usage::usage_from_json(&parsed),
                }))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    /// Recompute stats from durable state: the count of
    /// `entries.type = "message"` rows plus the sum of every
    /// `usage_ledger` row.
    ///
    /// Returns `Ok(None)` for an unknown session id.
    ///
    /// # Message count
    ///
    /// Upstream's `incrementMessageCount` runs for `entry.type ===
    /// "message"` only, so this counts `type = "message"` rows — not
    /// every entry.
    ///
    /// # Usage
    ///
    /// Upstream has no per-entry usage projection: the ledger is the
    /// authoritative source and `adjustment` rows are included in the sum
    /// (see `usage.rs`).
    ///
    /// Note the signed-correction gap documented in [`crate::usage`]: a
    /// negative upstream counter is clamped to `0`, so a session whose
    /// ledger carries one recomputes *lower* than the upstream cache and
    /// [`verify_stats`](Self::verify_stats) reports a drift that is really
    /// a representation limit. The clamp is logged; the warning names both
    /// values so the two causes stay distinguishable.
    pub fn recompute_stats(&self, session_id: &str) -> Result<Option<SessionStats>> {
        require_upstream(self, "session stats")?;
        let exists: i64 = self.connection().query_row(
            "SELECT count(*) FROM sessions WHERE id = ?1",
            [session_id],
            |row| row.get(0),
        )?;
        if exists == 0 {
            return Ok(None);
        }
        let message_count: i64 = self.connection().query_row(
            "SELECT count(*) FROM entries WHERE session_id = ?1 AND type = 'message'",
            [session_id],
            |row| row.get(0),
        )?;
        Ok(Some(SessionStats {
            message_count,
            usage: self.usage_totals(session_id)?,
        }))
    }

    /// Read the cache **and** recompute it, warning when they disagree.
    ///
    /// The cached value is returned (the durable state has been observed
    /// to be correct in the common case, and callers such as `pi session
    /// stats` want the same number the agent itself reads), but a drift
    /// is never silent: it is logged with both values and surfaced through
    /// [`StatsCheck::is_consistent`].
    pub fn verify_stats(&self, session_id: &str) -> Result<Option<StatsCheck>> {
        let Some(cached) = self.session_stats(session_id)? else {
            return Ok(None);
        };
        let Some(recomputed) = self.recompute_stats(session_id)? else {
            return Ok(None);
        };
        let check = StatsCheck { cached, recomputed };
        if !check.is_consistent() {
            tracing::warn!(
                session_id,
                cached_message_count = cached.message_count,
                recomputed_message_count = recomputed.message_count,
                cached_usage = ?cached.usage,
                recomputed_usage = ?recomputed.usage,
                "pi-session: sessions cache disagrees with the entries/usage_ledger recompute; the cached value was returned but is stale"
            );
        }
        Ok(Some(check))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_check_reports_drift() {
        let stats = SessionStats {
            message_count: 3,
            usage: Usage {
                input: 1,
                output: 2,
                cache_read: 0,
                cache_write: 0,
                total: 3,
            },
        };
        let same = StatsCheck {
            cached: stats,
            recomputed: stats,
        };
        assert!(same.is_consistent());

        let drifted = StatsCheck {
            cached: stats,
            recomputed: SessionStats {
                message_count: 3,
                usage: Usage {
                    input: 1,
                    output: 3,
                    ..stats.usage
                },
            },
        };
        assert!(!drifted.is_consistent());
    }
}
