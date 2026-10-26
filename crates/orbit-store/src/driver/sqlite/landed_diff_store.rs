//! Landed-delivery diff stats in `landed_diff_stats`.
//!
//! Timestamps are stored as fixed-width UTC RFC3339 text (microseconds,
//! `Z`), so `landed_at` compares lexically.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Row, TransactionBehavior, params};

use crate::Store;
use crate::contracts::{
    DiffClassCounts, LandedDiffSource, LandedDiffStats, LandedDiffStoreBackend,
};

use super::codec::stamp;

const COLUMNS: &str = "workspace_id, task_id, landed_commit, base_commit, job_run_id, \
     landed_at, recorded_at, source, crew, model, complexity, task_type, \
     code_added, code_deleted, code_files, tests_added, tests_deleted, tests_files, \
     other_added, other_deleted, other_files";

impl LandedDiffStoreBackend for Store {
    fn record_landed_diff_stats(&self, row: &LandedDiffStats) -> Result<bool, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let written = tx
                .connection()
                .execute(
                    &format!(
                        "INSERT OR IGNORE INTO landed_diff_stats ({COLUMNS}) VALUES \
                     (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, \
                      ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)"
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
                    ],
                )
                .map_err(sqlite_store_error)?;
            Ok(written > 0)
        })
    }

    fn landed_diff_stats_since(
        &self,
        workspace_id: &str,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<LandedDiffStats>, OrbitError> {
        let conn = self.read()?;
        let since = since.map_or_else(String::new, stamp);
        let mut statement = conn
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

    fn landed_diff_task_ids(&self, workspace_id: &str) -> Result<BTreeSet<String>, OrbitError> {
        let conn = self.read()?;
        let mut statement = conn
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
    })
}
