//! Concrete repository for landed-delivery diff stats (`landed_diff_stats`).
//!
//! Reached only through [`Snapshot::landed_diff`] (read-only) and
//! [`Tx::landed_diff`] (read-write inside [`crate::Store::write`]). Timestamps
//! are stored as fixed-width UTC RFC3339 text (microseconds, `Z`), so
//! `landed_at` compares lexically.

use std::collections::BTreeSet;
use std::marker::PhantomData;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, Row, params};

use crate::contract::{DiffClassCounts, LandedDiffSource, LandedDiffStats, ReviewableLines};
use crate::driver::sqlite::codec::stamp;
use crate::store::{Snapshot, Tx};

const COLUMNS: &str = "workspace_id, task_id, landed_commit, base_commit, job_run_id, \
     landed_at, recorded_at, source, crew, model, complexity, task_type, \
     code_added, code_deleted, code_files, tests_added, tests_deleted, tests_files, \
     other_added, other_deleted, other_files, reviewable_added, reviewable_deleted";

/// Concrete landed-diff repository scoped to a [`Snapshot`] (read-only) or a
/// [`Tx`] (read-write).
pub struct LandedDiffRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> LandedDiffRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> LandedDiffRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Insert `row` unless the workspace already holds one for its task and
    /// landed commit. Returns `true` when this call wrote it.
    pub fn put(&self, row: &LandedDiffStats) -> Result<bool, OrbitError> {
        let written = self
            .conn
            .execute(
                &format!(
                    "INSERT OR IGNORE INTO landed_diff_stats ({COLUMNS}) VALUES \
                     (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, \
                      ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)"
                ),
                params![
                    row.workspace_id,
                    row.task_id,
                    row.landed_commit,
                    row.base_commit,
                    row.job_run_id,
                    stamp(row.landed_at),
                    stamp(row.recorded_at),
                    row.source.as_str(),
                    row.crew,
                    row.model,
                    row.complexity,
                    row.task_type,
                    row.code.added,
                    row.code.deleted,
                    row.code.files,
                    row.tests.added,
                    row.tests.deleted,
                    row.tests.files,
                    row.other.added,
                    row.other.deleted,
                    row.other.files,
                    row.reviewable.map(|lines| lines.added),
                    row.reviewable.map(|lines| lines.deleted),
                ],
            )
            .map_err(sqlite_store_error)?;
        Ok(written > 0)
    }
}

impl<Access> LandedDiffRepository<'_, Access> {
    /// The workspace's rows that landed at or after `since` (every row when
    /// `None`), oldest first.
    pub fn list_since(
        &self,
        workspace_id: &str,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<LandedDiffStats>, OrbitError> {
        let since = since.map_or_else(String::new, stamp);
        let mut statement = self
            .conn
            .prepare(&format!(
                "SELECT {COLUMNS} FROM landed_diff_stats \
                 WHERE workspace_id = ?1 AND landed_at >= ?2 \
                 ORDER BY landed_at, task_id"
            ))
            .map_err(sqlite_store_error)?;
        let rows = statement
            .query_map(params![workspace_id, since], hydrate)
            .map_err(sqlite_store_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    /// The tasks that already have a row in the workspace.
    pub fn task_ids(&self, workspace_id: &str) -> Result<BTreeSet<String>, OrbitError> {
        let mut statement = self
            .conn
            .prepare("SELECT DISTINCT task_id FROM landed_diff_stats WHERE workspace_id = ?1")
            .map_err(sqlite_store_error)?;
        let rows = statement
            .query_map(params![workspace_id], |row| row.get::<_, String>(0))
            .map_err(sqlite_store_error)?;
        rows.collect::<Result<BTreeSet<_>, _>>()
            .map_err(sqlite_store_error)
    }
}

fn hydrate(row: &Row<'_>) -> rusqlite::Result<LandedDiffStats> {
    let counts = |offset: usize| -> rusqlite::Result<DiffClassCounts> {
        Ok(DiffClassCounts {
            added: row.get(offset)?,
            deleted: row.get(offset + 1)?,
            files: row.get(offset + 2)?,
        })
    };
    Ok(LandedDiffStats {
        workspace_id: row.get(0)?,
        task_id: row.get(1)?,
        landed_commit: row.get(2)?,
        base_commit: row.get(3)?,
        job_run_id: row.get(4)?,
        landed_at: crate::parse_timestamp(&row.get::<_, String>(5)?)?,
        recorded_at: crate::parse_timestamp(&row.get::<_, String>(6)?)?,
        source: LandedDiffSource::from_stored(&row.get::<_, String>(7)?),
        crew: row.get(8)?,
        model: row.get(9)?,
        complexity: row.get(10)?,
        task_type: row.get(11)?,
        code: counts(12)?,
        tests: counts(15)?,
        other: counts(18)?,
        reviewable: reviewable_lines(row)?,
    })
}

/// Both columns null, or either unreadable as a count, is an unavailable
/// denominator. A measured zero is `Some` with both counts present.
fn reviewable_lines(row: &Row<'_>) -> rusqlite::Result<Option<ReviewableLines>> {
    let added: Option<i64> = row.get(21)?;
    let deleted: Option<i64> = row.get(22)?;
    Ok(match (added, deleted) {
        (Some(added), Some(deleted)) if added >= 0 && deleted >= 0 => Some(ReviewableLines {
            added: added as u64,
            deleted: deleted as u64,
        }),
        _ => None,
    })
}
