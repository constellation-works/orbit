//! Bounded audit, run-state and commit-journal retention, blob-reference
//! scans and page accounting over the host store.
//!
//! Sizes come from `octet_length`, which reads a value's length from its
//! record header instead of walking its overflow chain, so a plan over a
//! multi-gigabyte history does not read the payloads it measures.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::workflow::JobRunState;
use rusqlite::{TransactionBehavior, params};

use crate::Store;
use crate::contracts::{
    AuditRetentionTable, JournalRetentionCandidates, RetentionIdSource, RetentionSelection,
    RetentionWorkspaceScope, StoreRetentionBackend, StoreSpace,
};

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

impl StoreRetentionBackend for Store {
    fn list_task_lock_denials(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<crate::contracts::TaskLockDenialRecord>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(
                "SELECT execution_id, timestamp, job_run_id, arguments_json
             FROM task_lock_denials WHERE workspace_id = ?1 ORDER BY timestamp, execution_id",
            )
            .map_err(sqlite_store_error)?;
        stmt.query_map([workspace_id], |row| {
            let timestamp: String = row.get(1)?;
            Ok(crate::contracts::TaskLockDenialRecord {
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

    fn distinct_workspace_ids(&self, source: RetentionIdSource) -> Result<Vec<String>, OrbitError> {
        let conn = self.read()?;
        distinct_workspace_ids(&conn, source)
    }

    fn audit_retention_selection(
        &self,
        table: AuditRetentionTable,
        scope: RetentionWorkspaceScope<'_>,
        cutoff: DateTime<Utc>,
    ) -> Result<RetentionSelection, OrbitError> {
        let conn = self.read()?;
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
                &conn,
                &scope_ids(scope),
                &cutoff,
                "SELECT COUNT(*), COALESCE(SUM(octet_length(payload_json)), 0) \
                 FROM v2_audit_events WHERE workspace_id = ?1 AND ts < ?2",
            )?,
        };
        Ok(selection(rows, bytes))
    }

    fn prune_audit_retention_batch(
        &self,
        table: AuditRetentionTable,
        scope: RetentionWorkspaceScope<'_>,
        cutoff: DateTime<Utc>,
        limit: usize,
    ) -> Result<usize, OrbitError> {
        let cutoff = cutoff.to_rfc3339();
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let ids = scope_ids(scope);
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| match table {
            AuditRetentionTable::Command => tx
                .tx
                .execute(
                    "DELETE FROM audit_events WHERE id IN ( \
                         SELECT id FROM audit_events WHERE timestamp < ?1 LIMIT ?2)",
                    params![cutoff, limit],
                )
                .map_err(sqlite_store_error),
            AuditRetentionTable::Run => delete_scoped(
                &tx.tx,
                &ids,
                &cutoff,
                limit,
                "DELETE FROM v2_audit_events WHERE id IN ( \
                     SELECT id FROM v2_audit_events \
                     WHERE workspace_id = ?1 AND ts < ?2 LIMIT ?3)",
            ),
        })
    }

    fn run_state_retention_selection(
        &self,
        scope: RetentionWorkspaceScope<'_>,
        cutoff: DateTime<Utc>,
    ) -> Result<RetentionSelection, OrbitError> {
        let conn = self.read()?;
        let sql = format!(
            "SELECT COUNT(*), COALESCE(SUM(octet_length(s.pipeline_state_json)), 0) {}",
            archivable_runs_sql()
        );
        let (rows, bytes) = sum_scoped(&conn, &scope_ids(scope), &cutoff.to_rfc3339(), &sql)?;
        Ok(selection(rows, bytes))
    }

    fn archive_run_states_batch(
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
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let mut remaining = limit;
            let mut pairs = Vec::new();
            for workspace_id in &ids {
                if remaining <= 0 {
                    break;
                }
                let mut stmt = tx.tx.prepare(&select_sql).map_err(sqlite_store_error)?;
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
                tx.tx
                    .execute(
                        "DELETE FROM job_run_states WHERE workspace_id = ?1 AND run_id = ?2",
                        params![workspace_id, run_id],
                    )
                    .map_err(sqlite_store_error)?;
                tx.tx
                    .execute(
                        "UPDATE job_runs SET archived_at = ?3 \
                         WHERE workspace_id = ?1 AND run_id = ?2",
                        params![workspace_id, run_id, archived_at],
                    )
                    .map_err(sqlite_store_error)?;
            }
            Ok(pairs.len())
        })
    }

    fn journal_retention_candidates(
        &self,
        scope: RetentionWorkspaceScope<'_>,
        cutoff: DateTime<Utc>,
    ) -> Result<JournalRetentionCandidates, OrbitError> {
        let conn = self.read()?;
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
                candidates.bytes += u64::try_from(bytes).unwrap_or(0);
            }
        }
        Ok(candidates)
    }

    fn delete_applied_journal_rows(&self, journal_ids: &[String]) -> Result<usize, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let mut delete = tx
                .tx
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
        })
    }

    fn visit_blob_reference_text(
        &self,
        excluding: Option<(RetentionWorkspaceScope<'_>, DateTime<Utc>)>,
        visit: &mut dyn FnMut(&str),
    ) -> Result<(), OrbitError> {
        let (run_audit_sql, extra) = match excluding {
            None => (
                "SELECT id, payload_json FROM v2_audit_events \
                 WHERE id > ?1 ORDER BY id LIMIT ?2"
                    .to_string(),
                Vec::new(),
            ),
            Some((scope, cutoff)) => {
                let ids = scope_ids(scope);
                let placeholders = (0..ids.len())
                    .map(|index| format!("?{}", index + 4))
                    .collect::<Vec<_>>()
                    .join(", ");
                (
                    format!(
                        "SELECT id, payload_json FROM v2_audit_events \
                         WHERE id > ?1 AND NOT (ts < ?3 AND workspace_id IN ({placeholders})) \
                         ORDER BY id LIMIT ?2"
                    ),
                    std::iter::once(cutoff.to_rfc3339())
                        .chain(ids)
                        .collect::<Vec<_>>(),
                )
            }
        };
        scan_chunks(self, &run_audit_sql, &extra, visit)?;
        scan_chunks(
            self,
            "SELECT rowid, COALESCE(agent_response_json, '') || ' ' || COALESCE(error_message, '') \
             FROM job_run_steps WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
            &[],
            visit,
        )?;
        scan_chunks(
            self,
            "SELECT rowid, pipeline_state_json FROM job_run_states \
             WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
            &[],
            visit,
        )
    }

    fn store_space(&self) -> Result<StoreSpace, OrbitError> {
        let conn = self.read()?;
        let pragma = |name: &str| -> Result<u64, OrbitError> {
            conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get::<_, i64>(0))
                .map(|value| u64::try_from(value).unwrap_or(0))
                .map_err(sqlite_store_error)
        };
        Ok(StoreSpace {
            page_size: pragma("page_size")?,
            page_count: pragma("page_count")?,
            freelist_pages: pragma("freelist_count")?,
        })
    }
}

fn selection(rows: i64, bytes: i64) -> RetentionSelection {
    RetentionSelection {
        rows: u64::try_from(rows).unwrap_or(0),
        bytes: u64::try_from(bytes).unwrap_or(0),
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
    conn: &rusqlite::Connection,
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
    conn: &rusqlite::Connection,
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
    conn: &rusqlite::Connection,
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

/// Walk `sql` (`?1` the last key read, `?2` the chunk size, then `extra`)
/// chunk by chunk, each chunk on a fresh read connection.
fn scan_chunks(
    store: &Store,
    sql: &str,
    extra: &[String],
    visit: &mut dyn FnMut(&str),
) -> Result<(), OrbitError> {
    let mut after = i64::MIN;
    loop {
        let conn = store.read()?;
        let mut stmt = conn.prepare(sql).map_err(sqlite_store_error)?;
        let chunk = REFERENCE_SCAN_CHUNK;
        let mut bound: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(2 + extra.len());
        bound.push(&after);
        bound.push(&chunk);
        for value in extra {
            bound.push(value);
        }
        let map =
            |row: &rusqlite::Row<'_>| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?));
        let rows = stmt
            .query_map(rusqlite::params_from_iter(bound), map)
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)?;
        let Some(last) = rows.last().map(|(key, _)| *key) else {
            return Ok(());
        };
        for text in rows.iter().filter_map(|(_, text)| text.as_deref()) {
            visit(text);
        }
        after = last;
    }
}
