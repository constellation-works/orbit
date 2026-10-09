//! Bounded audit and run-state retention, blob-reference scans and page
//! accounting over the host store.
//!
//! Sizes come from `octet_length`, which reads a value's length from its
//! record header instead of walking its overflow chain, so a plan over a
//! multi-gigabyte history does not read the payloads it measures.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::JobRunState;
use rusqlite::{TransactionBehavior, params};

use crate::Store;
use crate::contracts::{
    AuditRetentionTable, RetentionSelection, StoreRetentionBackend, StoreSpace,
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

fn store_error(error: rusqlite::Error) -> OrbitError {
    OrbitError::Store(error.to_string())
}

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

impl StoreRetentionBackend for Store {
    fn audit_retention_selection(
        &self,
        table: AuditRetentionTable,
        workspace_id: &str,
        cutoff: DateTime<Utc>,
    ) -> Result<RetentionSelection, OrbitError> {
        let conn = self.read()?;
        let cutoff = cutoff.to_rfc3339();
        let (rows, bytes) = match table {
            AuditRetentionTable::Command => conn.query_row(
                "SELECT COUNT(*), COALESCE(SUM( \
                     octet_length(command) + COALESCE(octet_length(arguments_json), 0) \
                     + COALESCE(octet_length(stdout_truncated), 0) \
                     + COALESCE(octet_length(stderr_truncated), 0) \
                     + COALESCE(octet_length(error_message), 0) \
                     + octet_length(working_directory)), 0) \
                 FROM audit_events WHERE timestamp < ?1",
                params![cutoff],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            ),
            AuditRetentionTable::Run => conn.query_row(
                "SELECT COUNT(*), COALESCE(SUM(octet_length(payload_json)), 0) \
                 FROM v2_audit_events WHERE workspace_id = ?1 AND ts < ?2",
                params![workspace_id, cutoff],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            ),
        }
        .map_err(store_error)?;
        Ok(selection(rows, bytes))
    }

    fn prune_audit_retention_batch(
        &self,
        table: AuditRetentionTable,
        workspace_id: &str,
        cutoff: DateTime<Utc>,
        limit: usize,
    ) -> Result<usize, OrbitError> {
        let cutoff = cutoff.to_rfc3339();
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            match table {
                AuditRetentionTable::Command => tx.tx.execute(
                    "DELETE FROM audit_events WHERE id IN ( \
                         SELECT id FROM audit_events WHERE timestamp < ?1 LIMIT ?2)",
                    params![cutoff, limit],
                ),
                AuditRetentionTable::Run => tx.tx.execute(
                    "DELETE FROM v2_audit_events WHERE id IN ( \
                         SELECT id FROM v2_audit_events \
                         WHERE workspace_id = ?1 AND ts < ?2 LIMIT ?3)",
                    params![workspace_id, cutoff, limit],
                ),
            }
            .map_err(store_error)
        })
    }

    fn run_state_retention_selection(
        &self,
        workspace_id: &str,
        cutoff: DateTime<Utc>,
    ) -> Result<RetentionSelection, OrbitError> {
        let conn = self.read()?;
        let (rows, bytes) = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*), COALESCE(SUM(octet_length(s.pipeline_state_json)), 0) {}",
                    archivable_runs_sql()
                ),
                params![workspace_id, cutoff.to_rfc3339()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .map_err(store_error)?;
        Ok(selection(rows, bytes))
    }

    fn archive_run_states_batch(
        &self,
        workspace_id: &str,
        cutoff: DateTime<Utc>,
        archived_at: DateTime<Utc>,
        limit: usize,
    ) -> Result<usize, OrbitError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let run_ids = {
                let mut stmt = tx
                    .tx
                    .prepare(&format!(
                        "SELECT r.run_id {} LIMIT ?3",
                        archivable_runs_sql()
                    ))
                    .map_err(store_error)?;
                stmt.query_map(params![workspace_id, cutoff.to_rfc3339(), limit], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(store_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(store_error)?
            };
            let archived_at = archived_at.to_rfc3339();
            for run_id in &run_ids {
                tx.tx
                    .execute(
                        "DELETE FROM job_run_states WHERE workspace_id = ?1 AND run_id = ?2",
                        params![workspace_id, run_id],
                    )
                    .map_err(store_error)?;
                tx.tx
                    .execute(
                        "UPDATE job_runs SET archived_at = ?3 \
                         WHERE workspace_id = ?1 AND run_id = ?2",
                        params![workspace_id, run_id, archived_at],
                    )
                    .map_err(store_error)?;
            }
            Ok(run_ids.len())
        })
    }

    fn visit_blob_reference_text(
        &self,
        excluding: Option<(&str, DateTime<Utc>)>,
        visit: &mut dyn FnMut(&str),
    ) -> Result<(), OrbitError> {
        // An exclusion that matches nothing keeps one query shape.
        let (workspace_id, cutoff) = excluding
            .map(|(workspace_id, cutoff)| (workspace_id.to_string(), cutoff.to_rfc3339()))
            .unwrap_or_default();
        scan_chunks(
            self,
            "SELECT id, payload_json FROM v2_audit_events \
             WHERE id > ?1 AND NOT (workspace_id = ?3 AND ts < ?4) \
             ORDER BY id LIMIT ?2",
            Some((&workspace_id, &cutoff)),
            visit,
        )?;
        scan_chunks(
            self,
            "SELECT rowid, COALESCE(agent_response_json, '') || ' ' || COALESCE(error_message, '') \
             FROM job_run_steps WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
            None,
            visit,
        )?;
        scan_chunks(
            self,
            "SELECT rowid, pipeline_state_json FROM job_run_states \
             WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
            None,
            visit,
        )
    }

    fn store_space(&self) -> Result<StoreSpace, OrbitError> {
        let conn = self.read()?;
        let pragma = |name: &str| -> Result<u64, OrbitError> {
            conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get::<_, i64>(0))
                .map(|value| u64::try_from(value).unwrap_or(0))
                .map_err(store_error)
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

/// Walk `sql` (`?1` the last key read, `?2` the chunk size, then `extra`)
/// chunk by chunk, each chunk on a fresh read connection.
fn scan_chunks(
    store: &Store,
    sql: &str,
    extra: Option<(&String, &String)>,
    visit: &mut dyn FnMut(&str),
) -> Result<(), OrbitError> {
    let mut after = i64::MIN;
    loop {
        let conn = store.read()?;
        let mut stmt = conn.prepare(sql).map_err(store_error)?;
        let map =
            |row: &rusqlite::Row<'_>| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?));
        let rows = match extra {
            Some((first, second)) => stmt
                .query_map(params![after, REFERENCE_SCAN_CHUNK, first, second], map)
                .map_err(store_error)?
                .collect::<Result<Vec<_>, _>>(),
            None => stmt
                .query_map(params![after, REFERENCE_SCAN_CHUNK], map)
                .map_err(store_error)?
                .collect::<Result<Vec<_>, _>>(),
        }
        .map_err(store_error)?;
        let Some(last) = rows.last().map(|(key, _)| *key) else {
            return Ok(());
        };
        for text in rows.iter().filter_map(|(_, text)| text.as_deref()) {
            visit(text);
        }
        after = last;
    }
}
