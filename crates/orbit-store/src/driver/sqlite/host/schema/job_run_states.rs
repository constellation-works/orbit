use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

use super::introspect::table_has_column;

/// Runs whose pipeline state one v38 batch moves. Small enough that the pages
/// one batch frees in `job_runs` are reused by the next batch's side-table
/// rows, so the database file grows by about one batch rather than by the
/// whole payload.
const JOB_RUN_STATES_BATCH: i64 = 64;

/// v38 `job_run_states`: move every run's pipeline state out of `job_runs`
/// into a 1:1 side table, then drop the column.
///
/// `crew_model`, `archived_at` and `executed_on_json` were appended by
/// `ALTER TABLE ... ADD COLUMN`, so they were stored after
/// `pipeline_state_json`, and every run listing walked each checkpoint's
/// overflow-page chain to reach them (about 2.3 GB on a host with 25k runs).
///
/// The move copies a batch of states into `job_run_states`, then nulls the
/// same batch in `job_runs`, so the freed pages are reused by the next batch.
/// The ledger runs this in one transaction: an interrupted migration rolls
/// back to v37 with every state still in `job_runs`, and the next store open
/// starts the move again. Every statement is idempotent, so a rerun over a
/// partially filled side table converges on the same result.
///
/// Duration: about 18 s per GB of pipeline state on an SSD host (32 s for the
/// historical 1.8 GB benchmark fixture), so roughly 40 s for a
/// 2.3 GB `job_runs`, longer when its pages are cold. The open that applies it
/// holds the write lock for that long, and the WAL grows by about the payload
/// size until the next checkpoint. Because each batch reuses the pages the
/// previous one freed, the database file barely grows and the freelist left
/// behind is about one batch (5.8 MB in the bench); it is logged below, and
/// reclaiming it needs `VACUUM`, which this does not run.
///
/// Like v19, a legacy `job_runs` without `workspace_id` is left alone.
pub(super) fn apply_job_run_states(conn: &Connection) -> Result<(), OrbitError> {
    if !table_has_column(conn, "job_runs", "workspace_id")? {
        return Ok(());
    }
    let started = std::time::Instant::now();
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS job_run_states (
                workspace_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                pipeline_state_json TEXT NOT NULL,
                PRIMARY KEY(workspace_id, run_id),
                FOREIGN KEY(workspace_id, run_id)
                    REFERENCES job_runs(workspace_id, run_id)
                    ON DELETE CASCADE
            );
        "#,
    )
    .map_err(sqlite_store_error)?;
    if !table_has_column(conn, "job_runs", "pipeline_state_json")? {
        return Ok(());
    }

    let mut moved = 0usize;
    let mut after = i64::MIN;
    loop {
        // The NULL test reads only the record header, never the payload.
        let last: Option<i64> = conn
            .query_row(
                "SELECT MAX(rowid) FROM (SELECT rowid FROM job_runs \
                 WHERE rowid > ?1 AND pipeline_state_json IS NOT NULL \
                 ORDER BY rowid LIMIT ?2)",
                rusqlite::params![after, JOB_RUN_STATES_BATCH],
                |row| row.get(0),
            )
            .map_err(sqlite_store_error)?;
        let Some(last) = last else {
            break;
        };
        moved += conn
            .execute(
                "INSERT INTO job_run_states(workspace_id, run_id, pipeline_state_json) \
                 SELECT workspace_id, run_id, pipeline_state_json FROM job_runs \
                 WHERE rowid > ?1 AND rowid <= ?2 AND pipeline_state_json IS NOT NULL \
                 ON CONFLICT(workspace_id, run_id) DO UPDATE SET \
                     pipeline_state_json = excluded.pipeline_state_json",
                rusqlite::params![after, last],
            )
            .map_err(sqlite_store_error)?;
        conn.execute(
            "UPDATE job_runs SET pipeline_state_json = NULL \
             WHERE rowid > ?1 AND rowid <= ?2 AND pipeline_state_json IS NOT NULL",
            rusqlite::params![after, last],
        )
        .map_err(sqlite_store_error)?;
        after = last;
    }
    conn.execute_batch("ALTER TABLE job_runs DROP COLUMN pipeline_state_json")
        .map_err(sqlite_store_error)?;

    let pragma = |name: &str| -> Result<i64, OrbitError> {
        conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
            .map_err(sqlite_store_error)
    };
    orbit_common::tracing::info!(
        target: "orbit.store.sqlite",
        moved_runs = moved,
        elapsed_ms = started.elapsed().as_millis() as u64,
        freelist_bytes = pragma("freelist_count")? * pragma("page_size")?,
        "moved job run pipeline state into job_run_states; VACUUM reclaims the freelist",
    );
    Ok(())
}
