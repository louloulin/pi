//! `branch_entries` / `branch_meta` reads (upstream v4 layout only).
//!
//! The upstream SQLite backend keeps two private projection tables that
//! let a branch be scanned without walking `parent_id` links:
//!
//! ```sql
//! branch_entries(session_id, branch_id, entry_id, entry_seq, entry_type)
//! PRIMARY KEY (session_id, branch_id, entry_id)
//! branch_meta(session_id, branch_id, tip_entry_id, tip_seq, base_branch_id, base_seq)
//! PRIMARY KEY (session_id, branch_id)
//! ```
//!
//! `branch_meta` describes one branch: its tip and, for a divergent
//! branch, the base branch + sequence it forks from (the newest
//! compaction boundary, so the branch keeps the post-compaction tail).
//! `branch_entries` lists the entries on that branch.
//!
//! [`SessionReader::scan_branch`] is a faithful port of the read half of
//! upstream `branch-entries.ts` (`scanBranchEntries`): it resolves the
//! segment chain newest-first, reverses it for `oldestFirst`, applies the
//! stop predicate per segment and only then the cursor/type filters, and
//! returns decoded [`DecodedEntry`] values — the same shape upstream
//! `Storage::scanBranch` returns (`Entry[]`).
//!
//! Only the read side is ported. `appendEntryToBranchIndex` (which
//! creates/grows the branches and writes both tables) is still part of
//! the writer slice this port does not have, exactly like upstream's
//! `branch_*` rows are projections of the durable `entries` table.

use rusqlite::types::Value as SqlValue;

use crate::error::{Result, SessionError};
use crate::reader::{decode_upstream_entry, DecodedEntry, SessionReader};
use crate::usage::require_upstream;

/// One `branch_meta` row: the tip of a branch and, when it is divergent,
/// the base branch + sequence it forks from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchMeta {
    /// Branch identifier. A root branch uses the id of its first entry.
    pub branch_id: String,
    /// Entry id of the branch tip.
    pub tip_entry_id: String,
    /// Sequence number of the branch tip.
    pub tip_seq: i64,
    /// Branch this one forks from (`None` for a root branch).
    pub base_branch_id: Option<String>,
    /// Sequence within the base branch the fork starts after (`None` for
    /// a root branch).
    pub base_seq: Option<i64>,
}

/// One `branch_entries` row: an entry on a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchEntry {
    /// Branch the entry belongs to.
    pub branch_id: String,
    /// Entry id (matches `entries.id`).
    pub entry_id: String,
    /// Entry sequence within the session (`entries.seq`).
    pub entry_seq: i64,
    /// Entry type copied from `entries.type`.
    pub entry_type: String,
}

/// Branch scan order; mirrors upstream `BranchScan["order"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BranchOrder {
    /// From the tip towards the root (upstream default).
    #[default]
    NewestFirst,
    /// From the root towards the tip.
    OldestFirst,
}

/// A branch scan query; mirrors the upstream `StorageBranchScan` type
/// (where `start` is required and therefore not optional).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchScan {
    /// Entry id to start the walk from (used to resolve the branch
    /// membership and the segment chain).
    pub start: String,
    /// Walk direction.
    pub order: BranchOrder,
    /// Maximum number of entries to return.
    pub limit: Option<usize>,
    /// Only entries whose `entries.type` equals this value.
    pub type_: Option<String>,
    /// Only entries whose `entries.custom_type` equals this value.
    pub custom_type: Option<String>,
    /// Only entries after (`oldestFirst`) / before (`newestFirst`) this
    /// entry sequence — the upstream `cursor.seq`.
    pub cursor: Option<i64>,
    /// Stop after the outermost entry of this type (inclusive).
    pub stop_at_type: Option<String>,
    /// Stop after the entry with this id (inclusive).
    pub stop_at_id: Option<String>,
}

impl BranchScan {
    /// A scan from `start`, newest first, no limit or filters — the shape
    /// upstream `scanBranchEntries` gets for a plain branch read.
    pub fn from(start: impl Into<String>) -> Self {
        Self {
            start: start.into(),
            order: BranchOrder::default(),
            limit: None,
            type_: None,
            custom_type: None,
            cursor: None,
            stop_at_type: None,
            stop_at_id: None,
        }
    }
}

/// One segment of a branch chain. `lower_seq` is exclusive, `upper_seq`
/// inclusive, matching upstream `readBranchSegmentsNewestFirst`.
struct BranchSegment {
    branch_id: String,
    lower_seq: i64,
    upper_seq: i64,
}

impl SessionReader {
    /// Every `branch_meta` row of a session, ordered by `branch_id`.
    pub fn branch_meta(&self, session_id: &str) -> Result<Vec<BranchMeta>> {
        require_upstream(self, "branch_meta")?;
        let mut stmt = self.connection().prepare(
            "SELECT branch_id, tip_entry_id, tip_seq, base_branch_id, base_seq \
             FROM branch_meta WHERE session_id = ?1 ORDER BY branch_id ASC",
        )?;
        let mut rows = stmt.query([session_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(BranchMeta {
                branch_id: row.get(0)?,
                tip_entry_id: row.get(1)?,
                tip_seq: row.get(2)?,
                base_branch_id: row.get(3)?,
                base_seq: row.get(4)?,
            });
        }
        Ok(out)
    }

    /// One `branch_meta` row by branch id.
    pub fn branch_meta_for(&self, session_id: &str, branch_id: &str) -> Result<Option<BranchMeta>> {
        require_upstream(self, "branch_meta")?;
        let mut stmt = self.connection().prepare(
            "SELECT branch_id, tip_entry_id, tip_seq, base_branch_id, base_seq \
             FROM branch_meta WHERE session_id = ?1 AND branch_id = ?2",
        )?;
        let mut rows = stmt.query(rusqlite::params![session_id, branch_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(BranchMeta {
                branch_id: row.get(0)?,
                tip_entry_id: row.get(1)?,
                tip_seq: row.get(2)?,
                base_branch_id: row.get(3)?,
                base_seq: row.get(4)?,
            })),
            None => Ok(None),
        }
    }

    /// Every `branch_entries` row of one branch, ordered by `entry_seq`
    /// then `entry_id`.
    pub fn branch_entries(&self, session_id: &str, branch_id: &str) -> Result<Vec<BranchEntry>> {
        require_upstream(self, "branch_entries")?;
        let mut stmt = self.connection().prepare(
            "SELECT branch_id, entry_id, entry_seq, entry_type \
             FROM branch_entries \
             WHERE session_id = ?1 AND branch_id = ?2 \
             ORDER BY entry_seq ASC, entry_id ASC",
        )?;
        let mut rows = stmt.query(rusqlite::params![session_id, branch_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(BranchEntry {
                branch_id: row.get(0)?,
                entry_id: row.get(1)?,
                entry_seq: row.get(2)?,
                entry_type: row.get(3)?,
            });
        }
        Ok(out)
    }

    /// Scan a branch, walking the segment chain like upstream
    /// `scanBranchEntries` / `scanBranchEntryStructures`.
    ///
    /// Returns decoded entries. An unknown `start` (no `branch_entries`
    /// row for it) is [`SessionError::Other`] — upstream throws
    /// `Branch cache missing entry …` there, because a session with
    /// entries but no branch index is corrupt.
    pub fn scan_branch(&self, session_id: &str, scan: &BranchScan) -> Result<Vec<DecodedEntry>> {
        require_upstream(self, "branch_entries")?;
        let oldest_first = scan.order == BranchOrder::OldestFirst;
        let mut segments = self.branch_segments_newest_first(session_id, &scan.start)?;
        if oldest_first {
            segments.reverse();
        }
        if scan.limit == Some(0) {
            return Ok(Vec::new());
        }

        let mut out = Vec::new();
        for segment in &segments {
            // `None` means "no limit" — formatting `usize::MAX` into
            // `LIMIT` would overflow SQLite's integer type and fail with a
            // datatype mismatch.
            let remaining = match scan.limit {
                Some(limit) => {
                    let left = limit.saturating_sub(out.len());
                    if left == 0 {
                        break;
                    }
                    Some(left)
                }
                None => None,
            };
            let stop_seq = self.read_stop_seq(session_id, segment, scan, oldest_first)?;
            out.extend(self.scan_segment(
                session_id,
                segment,
                scan,
                oldest_first,
                stop_seq,
                remaining,
            )?);
            // The stop predicate matched in this segment, so older segments
            // are out of range (upstream breaks here too).
            if stop_seq.is_some() {
                break;
            }
        }
        Ok(out)
    }

    /// Resolve the segment chain ending at `start`, newest first
    /// (upstream `readBranchSegmentsNewestFirst`).
    fn branch_segments_newest_first(
        &self,
        session_id: &str,
        start: &str,
    ) -> Result<Vec<BranchSegment>> {
        let membership: Option<(String, i64)> = self
            .connection()
            .query_row(
                "SELECT b.branch_id, b.entry_seq \
                 FROM branch_entries b \
                 JOIN branch_meta m \
                   ON m.session_id = b.session_id AND m.branch_id = b.branch_id \
                 WHERE b.session_id = ?1 \
                   AND b.entry_id = ?2 \
                   AND ((m.base_seq IS NULL AND b.entry_seq > 0) \
                        OR (m.base_seq IS NOT NULL AND b.entry_seq > m.base_seq)) \
                   AND b.entry_seq <= m.tip_seq \
                 ORDER BY m.tip_seq DESC, b.branch_id ASC \
                 LIMIT 1",
                rusqlite::params![session_id, start],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional_row()?;
        let Some((mut branch_id, mut upper_seq)) = membership else {
            return Err(SessionError::Other(format!(
                "branch cache missing entry {start:?} in session {session_id:?}"
            )));
        };

        let mut segments = Vec::new();
        loop {
            let meta = self
                .branch_meta_for(session_id, &branch_id)?
                .ok_or_else(|| {
                    SessionError::Other(format!(
                        "branch metadata missing for branch {branch_id:?} in session {session_id:?}"
                    ))
                })?;
            let lower_seq = meta.base_seq.unwrap_or(0);
            segments.push(BranchSegment {
                branch_id: branch_id.clone(),
                lower_seq,
                upper_seq,
            });
            match meta.base_branch_id {
                None => break,
                Some(base_branch_id) => {
                    let Some(base_seq) = meta.base_seq else {
                        return Err(SessionError::Other(format!(
                            "branch {branch_id:?} has a base branch but no base_seq"
                        )));
                    };
                    branch_id = base_branch_id;
                    upper_seq = base_seq;
                }
            }
        }
        Ok(segments)
    }

    /// Outermost matching `entry_seq` of the stop predicate within one
    /// segment (upstream `readStopSeq`).
    fn read_stop_seq(
        &self,
        session_id: &str,
        segment: &BranchSegment,
        scan: &BranchScan,
        oldest_first: bool,
    ) -> Result<Option<i64>> {
        let mut predicates = Vec::new();
        let mut params: Vec<SqlValue> = vec![
            SqlValue::Text(session_id.to_string()),
            SqlValue::Text(segment.branch_id.clone()),
            SqlValue::Integer(segment.lower_seq),
            SqlValue::Integer(segment.upper_seq),
        ];
        if let Some(stop_at_type) = &scan.stop_at_type {
            params.push(SqlValue::Text(stop_at_type.clone()));
            predicates.push(format!("b.entry_type = ?{}", params.len()));
        }
        if let Some(stop_at_id) = &scan.stop_at_id {
            params.push(SqlValue::Text(stop_at_id.clone()));
            predicates.push(format!("b.entry_id = ?{}", params.len()));
        }
        if predicates.is_empty() {
            return Ok(None);
        }
        let aggregate = if oldest_first {
            "MIN(b.entry_seq)"
        } else {
            "MAX(b.entry_seq)"
        };
        let sql = format!(
            "SELECT {aggregate} AS stop_seq FROM branch_entries b \
             WHERE b.session_id = ?1 AND b.branch_id = ?2 \
               AND b.entry_seq > ?3 AND b.entry_seq <= ?4 \
               AND ({})",
            predicates.join(" OR ")
        );
        let stop: Option<i64> =
            self.connection()
                .query_row(&sql, rusqlite::params_from_iter(params), |row| row.get(0))?;
        Ok(stop)
    }

    /// Read one segment (upstream `scanEntrySegmentRows`).
    fn scan_segment(
        &self,
        session_id: &str,
        segment: &BranchSegment,
        scan: &BranchScan,
        oldest_first: bool,
        stop_seq: Option<i64>,
        limit: Option<usize>,
    ) -> Result<Vec<DecodedEntry>> {
        let mut predicates = vec![
            "b.session_id = ?1".to_string(),
            "b.branch_id = ?2".to_string(),
            "b.entry_seq > ?3".to_string(),
            "b.entry_seq <= ?4".to_string(),
            "e.session_id = b.session_id".to_string(),
        ];
        let mut params: Vec<SqlValue> = vec![
            SqlValue::Text(session_id.to_string()),
            SqlValue::Text(segment.branch_id.clone()),
            SqlValue::Integer(segment.lower_seq),
            SqlValue::Integer(segment.upper_seq),
        ];
        if let Some(stop_seq) = stop_seq {
            params.push(SqlValue::Integer(stop_seq));
            if oldest_first {
                predicates.push(format!("b.entry_seq <= ?{}", params.len()));
            } else {
                predicates.push(format!("b.entry_seq >= ?{}", params.len()));
            }
        }
        if let Some(type_) = &scan.type_ {
            params.push(SqlValue::Text(type_.clone()));
            predicates.push(format!("b.entry_type = ?{}", params.len()));
        }
        if let Some(custom_type) = &scan.custom_type {
            params.push(SqlValue::Text(custom_type.clone()));
            predicates.push(format!("e.custom_type = ?{}", params.len()));
        }
        if let Some(cursor) = scan.cursor {
            params.push(SqlValue::Integer(cursor));
            if oldest_first {
                predicates.push(format!("b.entry_seq > ?{}", params.len()));
            } else {
                predicates.push(format!("b.entry_seq < ?{}", params.len()));
            }
        }
        let order = if oldest_first { "ASC" } else { "DESC" };
        let mut sql = format!(
            "SELECT e.id, e.parent_id, e.seq, e.type, e.custom_type, e.timestamp, e.payload \
             FROM branch_entries b \
             CROSS JOIN entries e \
               ON e.session_id = b.session_id AND e.id = b.entry_id \
             WHERE {} \
             ORDER BY b.entry_seq {order}",
            predicates.join(" AND ")
        );
        if let Some(limit) = limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        let mut stmt = self.connection().prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let parent_id: Option<String> = row.get(1)?;
            let seq: i64 = row.get(2)?;
            let type_: String = row.get(3)?;
            let custom_type: Option<String> = row.get(4)?;
            let timestamp: i64 = row.get(5)?;
            let payload: String = row.get(6)?;
            out.push(DecodedEntry {
                seq,
                parent_seq: None,
                entry_id: Some(id),
                parent_entry_id: parent_id,
                type_: type_.clone(),
                timestamp,
                entry: decode_upstream_entry(&type_, custom_type.as_deref(), &payload)?,
            });
        }
        Ok(out)
    }
}

/// Small helper so a `query_row` returning no rows is `None` instead of
/// an error, without another `OptionalExtension` import at each call.
trait OptionalRow<T> {
    fn optional_row(self) -> Result<Option<T>>;
}

impl<T> OptionalRow<T> for rusqlite::Result<T> {
    fn optional_row(self) -> Result<Option<T>> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }
}
