//! Concrete repository for bounded retention over the host store: audit
//! rows, terminal run state, applied task commit journal rows, task-lock
//! denial evidence, the text that can name an audit blob, and the file's page
//! accounting.
//!
//! Reached through [`Snapshot::retention`] (read-only) and [`Tx::retention`]
//! (read-write inside [`crate::Store::write`]). The caller sizes each batch,
//! in the unit the writer works in: [`RetentionRepository::delete_audit_batch`]
//! deletes at most `limit` rows, [`RetentionRepository::archive_run_states`]
//! archives at most `limit` runs (each run costs one state-row delete and one
//! run-row update), and [`RetentionRepository::delete_applied_journal_rows`]
//! takes no limit and processes every ID in the slice it is given, deleting
//! only the rows still `applied`, so the caller must chunk the IDs. Keeping
//! each call to one batch means a sweep of a large
//! history never holds the write lock for longer than one batch; when to sweep
//! and how to batch is the calling operation's. [`visit_blob_reference_text`] is the one read that
//! spans snapshots, one per chunk, so a scan of the whole history never pins
//! one read transaction.
//!
//! Sizes come from `octet_length`, which reads a value's length from its
//! record header instead of walking its overflow chain, so a plan over a
//! multi-gigabyte history does not read the payloads it measures.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::workflow::JobRunState;
use rusqlite::{Connection, params};
use std::marker::PhantomData;

use crate::Store;
use crate::contract::{
    AuditRetentionTable, JournalRetentionCandidates, RetentionIdSource, RetentionSelection,
    RetentionWorkspaceScope, StoreSpace, TaskLockDenialRecord,
};
use crate::driver::sqlite::codec::unsigned;
use crate::store::{Snapshot, Tx};

/// Rows read per blob-reference chunk. Each chunk is its own read, so a scan
/// never pins one snapshot across the whole history.
const REFERENCE_SCAN_CHUNK: i64 = 2_000;

/// Terminal states whose pipeline state retention may drop. `held` is
/// terminal but still resumable by review evidence, so it is absent.
const ARCHIVABLE_RUN_STATES: [JobRunState; 5] = [
    JobRunState::Success,
    JobRunState::Failed,
    JobRunState::Timeout,
    JobRunState::Cancelled,
    JobRunState::Interrupted,
];

fn archivable_states_sql() -> String {
    ARCHIVABLE_RUN_STATES
        .iter()
        .map(|state| format!("'{state}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The selection predicate shared by a run-state plan and an archive batch.
fn archivable_runs_sql() -> String {
    format!(
        "FROM job_runs r JOIN job_run_states s \
           ON s.workspace_id = r.workspace_id AND s.run_id = r.run_id \
         WHERE r.workspace_id = ?1 AND r.state IN ({}) \
           AND COALESCE(r.finished_at, r.created_at) < ?2",
        archivable_states_sql()
    )
}

/// One workspace's prunable journal rows: applied before `?2`, less each
/// task's newest applied row. The index on `(workspace_id, state, created_at)`
/// finds the applied rows; one window pass ranks them per task, so the cost
/// is a sort, not a scan per candidate. Sizes are computed before the window,
/// so the sort carries an integer, never the intent text.
const JOURNAL_CANDIDATES_SQL: &str = "\
    SELECT journal_id, bytes FROM ( \
        SELECT journal_id, \
            COALESCE(applied_at, created_at) AS settled_at, \
            octet_length(journal_id) + octet_length(workspace_id) + octet_length(task_id) \
                + octet_length(state) + octet_length(intent_json) + octet_length(created_at) \
                + COALESCE(octet_length(committed_at), 0) \
                + COALESCE(octet_length(applied_at), 0) \
                + COALESCE(octet_length(reservation_id), 0) AS bytes, \
            ROW_NUMBER() OVER ( \
                PARTITION BY task_id \
                ORDER BY COALESCE(applied_at, created_at) DESC, journal_id DESC) AS recency \
        FROM task_commit_journal \
        WHERE workspace_id = ?1 AND state = 'applied') \
    WHERE recency > 1 AND settled_at < ?2 \
    ORDER BY settled_at, journal_id";

/// Concrete retention repository scoped to a [`Snapshot`] (read-only) or a
/// [`Tx`] (read-write).
pub struct RetentionRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> RetentionRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> RetentionRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Delete at most `limit` of the rows [`Self::audit_selection`] selects.
    /// Returns how many went.
    pub fn delete_audit_batch(
        &self,
        table: AuditRetentionTable,
        scope: RetentionWorkspaceScope<'_>,
        cutoff: DateTime<Utc>,
        limit: usize,
    ) -> Result<usize, OrbitError> {
        let cutoff = cutoff.to_rfc3339();
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let ids = scope_ids(scope);
        match table {
            AuditRetentionTable::Command => self
                .conn
                .execute(
                    "DELETE FROM audit_events WHERE id IN ( \
                         SELECT id FROM audit_events WHERE timestamp < ?1 LIMIT ?2)",
                    params![cutoff, limit],
                )
                .map_err(sqlite_store_error),
            AuditRetentionTable::Run => delete_scoped(
                self.conn,
                &ids,
                &cutoff,
                limit,
                "DELETE FROM v2_audit_events WHERE id IN ( \
                     SELECT id FROM v2_audit_events \
                     WHERE workspace_id = ?1 AND ts < ?2 LIMIT ?3)",
            ),
        }
    }

    /// Drop the pipeline state of at most `limit` runs
    /// [`Self::run_state_selection`] selects and stamp their `archived_at`.
    /// The run row and its steps stay. Each row is deleted under its own
    /// `workspace_id`. Returns how many runs were archived.
    pub fn archive_run_states(
        &self,
        scope: RetentionWorkspaceScope<'_>,
        cutoff: DateTime<Utc>,
        archived_at: DateTime<Utc>,
        limit: usize,
    ) -> Result<usize, OrbitError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let cutoff = cutoff.to_rfc3339();
        let ids = scope_ids(scope);
        let select_sql = format!("SELECT r.run_id {} LIMIT ?3", archivable_runs_sql());
        {
            let mut remaining = limit;
            let mut pairs = Vec::new();
            for workspace_id in &ids {
                if remaining <= 0 {
                    break;
                }
                let mut stmt = self.conn.prepare(&select_sql).map_err(sqlite_store_error)?;
                let found = stmt
                    .query_map(params![workspace_id, cutoff, remaining], |row| {
                        row.get::<_, String>(0)
                    })
                    .map_err(sqlite_store_error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(sqlite_store_error)?;
                drop(stmt);
                remaining -= i64::try_from(found.len()).unwrap_or(i64::MAX);
                pairs.extend(
                    found
                        .into_iter()
                        .map(|run_id| (workspace_id.clone(), run_id)),
                );
            }
            let archived_at = archived_at.to_rfc3339();
            for (workspace_id, run_id) in &pairs {
                self.conn
                    .execute(
                        "DELETE FROM job_run_states WHERE workspace_id = ?1 AND run_id = ?2",
                        params![workspace_id, run_id],
                    )
                    .map_err(sqlite_store_error)?;
                self.conn
                    .execute(
                        "UPDATE job_runs SET archived_at = ?3 \
                         WHERE workspace_id = ?1 AND run_id = ?2",
                        params![workspace_id, run_id, archived_at],
                    )
                    .map_err(sqlite_store_error)?;
            }
            Ok(pairs.len())
        }
    }

    /// Delete the named journal rows that are still `applied`. Returns how
    /// many went. No limit applies: every ID in `journal_ids` is processed,
    /// so the caller chunks the slice.
    pub fn delete_applied_journal_rows(&self, journal_ids: &[String]) -> Result<usize, OrbitError> {
        {
            let mut delete = self
                .conn
                .prepare(
                    "DELETE FROM task_commit_journal WHERE journal_id = ?1 AND state = 'applied'",
                )
                .map_err(sqlite_store_error)?;
            let mut removed = 0;
            for journal_id in journal_ids {
                removed += delete
                    .execute(params![journal_id])
                    .map_err(sqlite_store_error)?;
            }
            Ok(removed)
        }
    }
}

impl<Access> RetentionRepository<'_, Access> {
    /// All lock-conflict attempts in one workspace, independent of audit
    /// retention.
    pub fn task_lock_denials(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<TaskLockDenialRecord>, OrbitError> {
        let conn = self.conn;
        let mut stmt = conn
            .prepare(
                "SELECT execution_id, timestamp, job_run_id, arguments_json
             FROM task_lock_denials WHERE workspace_id = ?1 ORDER BY timestamp, execution_id",
            )
            .map_err(sqlite_store_error)?;
        stmt.query_map([workspace_id], |row| {
            let timestamp: String = row.get(1)?;
            Ok(TaskLockDenialRecord {
                execution_id: row.get(0)?,
                timestamp: crate::parse_timestamp(&timestamp)?,
                job_run_id: row.get(2)?,
                arguments_json: row.get(3)?,
            })
        })
        .map_err(sqlite_store_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
    }

    /// Distinct `workspace_id` values in `source`, read from the index that
    /// leads with that column. The caller decides which of them are in scope.
    pub fn workspace_ids(&self, source: RetentionIdSource) -> Result<Vec<String>, OrbitError> {
        let conn = self.conn;
        distinct_workspace_ids(conn, source)
    }

    /// Rows of `table` older than `cutoff`. A run-audit selection covers
    /// `scope`; the command audit is host-wide and ignores `scope`.
    pub fn audit_selection(
        &self,
        table: AuditRetentionTable,
        scope: RetentionWorkspaceScope<'_>,
        cutoff: DateTime<Utc>,
    ) -> Result<RetentionSelection, OrbitError> {
        let conn = self.conn;
        let cutoff = cutoff.to_rfc3339();
        let (rows, bytes) = match table {
            AuditRetentionTable::Command => conn
                .query_row(
                    "SELECT COUNT(*), COALESCE(SUM( \
                         octet_length(command) + COALESCE(octet_length(arguments_json), 0) \
                         + COALESCE(octet_length(stdout_truncated), 0) \
                         + COALESCE(octet_length(stderr_truncated), 0) \
                         + COALESCE(octet_length(error_message), 0) \
                         + octet_length(working_directory)), 0) \
                     FROM audit_events WHERE timestamp < ?1",
                    params![cutoff],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .map_err(sqlite_store_error)?,
            AuditRetentionTable::Run => sum_scoped(
                conn,
                &scope_ids(scope),
                &cutoff,
                "SELECT COUNT(*), COALESCE(SUM(octet_length(payload_json)), 0) \
                 FROM v2_audit_events WHERE workspace_id = ?1 AND ts < ?2",
            )?,
        };
        Ok(selection(rows, bytes))
    }

    /// Pipeline state of terminal runs in `scope` that finished before
    /// `cutoff`. A held run still awaits review evidence that resumes it, so
    /// it is never selected; neither is a run that is not terminal.
    pub fn run_state_selection(
        &self,
        scope: RetentionWorkspaceScope<'_>,
        cutoff: DateTime<Utc>,
    ) -> Result<RetentionSelection, OrbitError> {
        let conn = self.conn;
        let sql = format!(
            "SELECT COUNT(*), COALESCE(SUM(octet_length(s.pipeline_state_json)), 0) {}",
            archivable_runs_sql()
        );
        let (rows, bytes) = sum_scoped(conn, &scope_ids(scope), &cutoff.to_rfc3339(), &sql)?;
        Ok(selection(rows, bytes))
    }

    /// `applied` task commit journal rows in `scope` settled before `cutoff`,
    /// except each task's newest applied row. Recovery replays only
    /// `prepared` and `committed` rows, so neither is ever selected, and
    /// `aborted` rows are left alone.
    pub fn journal_candidates(
        &self,
        scope: RetentionWorkspaceScope<'_>,
        cutoff: DateTime<Utc>,
    ) -> Result<JournalRetentionCandidates, OrbitError> {
        let conn = self.conn;
        let cutoff = cutoff.to_rfc3339();
        let mut stmt = conn
            .prepare(JOURNAL_CANDIDATES_SQL)
            .map_err(sqlite_store_error)?;
        let mut candidates = JournalRetentionCandidates::default();
        for workspace_id in scope_ids(scope) {
            let rows = stmt
                .query_map(params![workspace_id, cutoff], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(sqlite_store_error)?;
            for row in rows {
                let (journal_id, bytes) = row.map_err(sqlite_store_error)?;
                candidates.journal_ids.push(journal_id);
                candidates.bytes += unsigned(bytes);
            }
        }
        Ok(candidates)
    }

    /// The database file's page accounting.
    pub fn space(&self) -> Result<StoreSpace, OrbitError> {
        let conn = self.conn;
        let pragma = |name: &str| -> Result<u64, OrbitError> {
            conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get::<_, i64>(0))
                .map(unsigned)
                .map_err(sqlite_store_error)
        };
        Ok(StoreSpace {
            page_size: pragma("page_size")?,
            page_count: pragma("page_count")?,
            freelist_pages: pragma("freelist_count")?,
        })
    }

    /// The highest key of one blob-reference source, or `i64::MIN` when empty.
    fn blob_reference_high_water(&self, table: &str) -> Result<i64, OrbitError> {
        self.conn
            .query_row(
                &format!("SELECT COALESCE(MAX(rowid), ?) FROM {table}"),
                [i64::MIN],
                |row| row.get::<_, i64>(0),
            )
            .map_err(sqlite_store_error)
    }

    /// One chunk of `sql` (`?1` the last key read, `?2` the chunk size, `?3`
    /// the scan's maximum key, then `extra`): each row's key and text.
    fn blob_reference_chunk(
        &self,
        sql: &str,
        after: i64,
        through: i64,
        extra: &[String],
    ) -> Result<Vec<(i64, Option<String>)>, OrbitError> {
        let mut stmt = self.conn.prepare(sql).map_err(sqlite_store_error)?;
        let chunk = REFERENCE_SCAN_CHUNK;
        let mut bound: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(3 + extra.len());
        bound.push(&after);
        bound.push(&chunk);
        bound.push(&through);
        for value in extra {
            bound.push(value);
        }
        let map =
            |row: &rusqlite::Row<'_>| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?));
        stmt.query_map(rusqlite::params_from_iter(bound), map)
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }
}

/// Call `visit` with every stored text that can name an audit blob: run audit
/// payloads, step responses and errors, and pipeline state. Run-audit rows of
/// `excluding` older than the cutoff are skipped, so a plan sees the
/// references a prune would leave.
///
/// A named second read path: it opens one [`Store::read`] snapshot per chunk
/// instead of taking a [`Snapshot`], because one read transaction over the
/// whole history would pin the WAL for the length of the scan. Each table
/// stops at its initial maximum rowid; concurrent blob publications must
/// retain their pending/mtime protection.
pub fn visit_blob_reference_text(
    store: &Store,
    excluding: Option<(RetentionWorkspaceScope<'_>, DateTime<Utc>)>,
    visit: &mut dyn FnMut(&str),
) -> Result<(), OrbitError> {
    let (run_audit_sql, extra) = match excluding {
        None => (
            "SELECT id, payload_json FROM v2_audit_events \
             WHERE id > ?1 AND id <= ?3 ORDER BY id LIMIT ?2"
                .to_string(),
            Vec::new(),
        ),
        Some((scope, cutoff)) => {
            let ids = scope_ids(scope);
            let placeholders = (0..ids.len())
                .map(|index| format!("?{}", index + 5))
                .collect::<Vec<_>>()
                .join(", ");
            (
                format!(
                    "SELECT id, payload_json FROM v2_audit_events \
                     WHERE id > ?1 AND id <= ?3 AND NOT (ts < ?4 AND workspace_id IN ({placeholders})) \
                     ORDER BY id LIMIT ?2"
                ),
                std::iter::once(cutoff.to_rfc3339())
                    .chain(ids)
                    .collect::<Vec<_>>(),
            )
        }
    };
    scan_chunks(store, "v2_audit_events", &run_audit_sql, &extra, visit)?;
    scan_chunks(
        store,
        "job_run_steps",
        "SELECT rowid, COALESCE(agent_response_json, '') || ' ' || COALESCE(error_message, '') \
         FROM job_run_steps WHERE rowid > ?1 AND rowid <= ?3 ORDER BY rowid LIMIT ?2",
        &[],
        visit,
    )?;
    scan_chunks(
        store,
        "job_run_states",
        "SELECT rowid, pipeline_state_json FROM job_run_states \
         WHERE rowid > ?1 AND rowid <= ?3 ORDER BY rowid LIMIT ?2",
        &[],
        visit,
    )
}

fn selection(rows: i64, bytes: i64) -> RetentionSelection {
    RetentionSelection {
        rows: unsigned(rows),
        bytes: unsigned(bytes),
    }
}

/// `workspace_id`, then each legacy id that is not already that workspace.
fn scope_ids(scope: RetentionWorkspaceScope<'_>) -> Vec<String> {
    let mut ids = vec![scope.workspace_id.to_string()];
    if let Some(legacy) = scope.legacy_ids {
        for id in legacy {
            if !id.is_empty() && id != scope.workspace_id && !ids.iter().any(|seen| seen == id) {
                ids.push(id.clone());
            }
        }
    }
    ids
}

fn distinct_workspace_ids(
    conn: &Connection,
    source: RetentionIdSource,
) -> Result<Vec<String>, OrbitError> {
    let sql = match source {
        RetentionIdSource::RunAudit => "SELECT DISTINCT workspace_id FROM v2_audit_events",
        RetentionIdSource::JobRuns => "SELECT DISTINCT workspace_id FROM job_runs",
        RetentionIdSource::CommitJournal => "SELECT DISTINCT workspace_id FROM task_commit_journal",
    };
    let mut stmt = conn.prepare(sql).map_err(sqlite_store_error)?;
    stmt.query_map([], |row| row.get::<_, String>(0))
        .map_err(sqlite_store_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
}

/// Sum one indexed `workspace_id = ?1 AND time < ?2` predicate across `ids`.
fn sum_scoped(
    conn: &Connection,
    ids: &[String],
    cutoff: &str,
    sql: &str,
) -> Result<(i64, i64), OrbitError> {
    let mut rows = 0i64;
    let mut bytes = 0i64;
    for id in ids {
        let (part_rows, part_bytes) = conn
            .query_row(sql, params![id, cutoff], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(sqlite_store_error)?;
        rows += part_rows;
        bytes += part_bytes;
    }
    Ok((rows, bytes))
}

/// Delete up to `limit` rows, filling the batch from each id in order.
fn delete_scoped(
    conn: &Connection,
    ids: &[String],
    cutoff: &str,
    mut limit: i64,
    sql: &str,
) -> Result<usize, OrbitError> {
    let mut removed = 0usize;
    for id in ids {
        if limit <= 0 {
            break;
        }
        let deleted = conn
            .execute(sql, params![id, cutoff, limit])
            .map_err(sqlite_store_error)?;
        let deleted = i64::try_from(deleted).unwrap_or(0);
        removed += usize::try_from(deleted).unwrap_or(0);
        limit -= deleted;
    }
    Ok(removed)
}

/// Walk `sql` (`?1` the last key read, `?2` the chunk size, `?3` the initial
/// maximum key, then `extra`) once, without chasing concurrent inserts, one
/// read snapshot per chunk. New blob writes are protected by the pending
/// marker and mtime grace.
fn scan_chunks(
    store: &Store,
    table: &str,
    sql: &str,
    extra: &[String],
    visit: &mut dyn FnMut(&str),
) -> Result<(), OrbitError> {
    let through = store.read()?.retention().blob_reference_high_water(table)?;
    let mut after = i64::MIN;
    while after < through {
        let rows = store
            .read()?
            .retention()
            .blob_reference_chunk(sql, after, through, extra)?;
        let Some(last) = rows.last().map(|(key, _)| *key) else {
            return Ok(());
        };
        for text in rows.iter().filter_map(|(_, text)| text.as_deref()) {
            visit(text);
        }
        after = last;
    }
    Ok(())
}
