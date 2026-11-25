use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

use super::introspect::{add_column_if_missing, table_exists, table_has_column};

pub(super) fn apply_flat_crew_model(conn: &Connection) -> Result<(), OrbitError> {
    // ADR-0213: keep the legacy role columns nullable for existing databases.
    // v94 `job_run_crew_model_backfill` copies a legacy `implementer_model`
    // into `crew_model`, so reads use `crew_model` alone.
    add_column_if_missing(conn, "job_runs", "crew_model", "TEXT")
}

pub(super) fn apply_job_run_archive_stage(conn: &Connection) -> Result<(), OrbitError> {
    add_column_if_missing(conn, "job_runs", "archived_at", "TEXT")
}

/// v19 `job_runs_created_index`: cover the listing's per-workspace
/// `ORDER BY created_at DESC, run_id ASC` so a bounded page stops scanning
/// and sorting the whole workspace history.
///
/// A legacy database may reach this entry with no `job_runs` table, or with
/// the pre-consolidation one that has no `workspace_id`; the current table
/// is created at open time by `ensure_v2_state_consolidation_schema`, which
/// declares the same index. So this entry indexes the table only when it is
/// already the current shape and otherwise leaves it to open time.
pub(super) fn apply_job_runs_created_index(conn: &Connection) -> Result<(), OrbitError> {
    if !table_has_column(conn, "job_runs", "workspace_id")?
        || !table_has_column(conn, "job_runs", "created_at")?
    {
        return Ok(());
    }
    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_job_runs_workspace_created
            ON job_runs(workspace_id, created_at DESC, run_id ASC);
        "#,
    )
    .map_err(sqlite_store_error)
}

/// v34 `job_runs_job_created_and_retry_indexes`: cover the two per-workspace
/// reads that had no index of their own.
///
/// - `(workspace_id, job_id, created_at DESC, run_id ASC)` serves every
///   per-job newest-first read (the job history, the keyed-submission window,
///   the catalog's last run per job). The older `(workspace_id, job_id,
///   scheduled_at DESC)` index narrows to the job but cannot order by
///   `created_at`, so each such read sorted every run the job had ever had.
/// - `(workspace_id, retry_source_run_id, created_at, run_id)` serves the
///   retry children read, which orders by `created_at, run_id`, and the
///   retry-lineage walk, which needs only the two-column prefix. Only the
///   automation feature indexed `retry_source_run_id`, and on that two-column
///   index the planner still preferred scanning the workspace in `created_at`
///   order for the children read, so a resume or an incident lookup walked
///   every run to find a handful of children. The automation index stays; it
///   is a prefix of this one and costs nothing to leave.
///
/// Like v19, this indexes only the current `job_runs` shape; a legacy table is
/// left to open time.
pub(super) fn apply_job_runs_job_created_and_retry_indexes(
    conn: &Connection,
) -> Result<(), OrbitError> {
    if !table_has_column(conn, "job_runs", "workspace_id")?
        || !table_has_column(conn, "job_runs", "job_id")?
        || !table_has_column(conn, "job_runs", "created_at")?
        || !table_has_column(conn, "job_runs", "retry_source_run_id")?
    {
        return Ok(());
    }
    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_job_runs_ws_job_created
            ON job_runs(workspace_id, job_id, created_at DESC, run_id ASC);

            CREATE INDEX IF NOT EXISTS idx_job_runs_ws_retry_created
            ON job_runs(workspace_id, retry_source_run_id, created_at ASC, run_id ASC);
        "#,
    )
    .map_err(sqlite_store_error)
}

/// v39 `job_runs_recency_index`: cover the dashboard's run listings, which
/// order each workspace by `COALESCE(finished_at, started_at, created_at)
/// DESC, run_id ASC` (`JobRunOrder::Recency`), with or without one `state`.
///
/// The order is an expression no column index covers, so a bounded recency
/// page materialized and sorted every run of the workspace before applying
/// its `LIMIT`. These expression indexes repeat the listing's `ORDER BY`
/// verbatim — SQLite matches an indexed expression only when it is written
/// identically — so the page walks the index and stops at its limit.
///
/// Like v19, this indexes only the current `job_runs` shape; a legacy table is
/// left to open time.
pub(super) fn apply_job_runs_recency_index(conn: &Connection) -> Result<(), OrbitError> {
    for column in [
        "workspace_id",
        "state",
        "finished_at",
        "started_at",
        "created_at",
    ] {
        if !table_has_column(conn, "job_runs", column)? {
            return Ok(());
        }
    }
    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_job_runs_ws_recency
            ON job_runs(workspace_id, COALESCE(finished_at, started_at, created_at) DESC, run_id ASC);

            CREATE INDEX IF NOT EXISTS idx_job_runs_ws_state_recency
            ON job_runs(workspace_id, state, COALESCE(finished_at, started_at, created_at) DESC, run_id ASC);
        "#,
    )
    .map_err(sqlite_store_error)
}

pub(super) fn apply_execution_provenance(conn: &Connection) -> Result<(), OrbitError> {
    if !table_exists(conn, "job_runs")? {
        return Ok(());
    }
    add_column_if_missing(conn, "job_runs", "executed_on_json", "TEXT")
}

/// Build provenance is nullable so old runs never inherit the upgrading binary's identity.
pub(super) fn apply_job_run_build(conn: &Connection) -> Result<(), OrbitError> {
    if !table_exists(conn, "job_runs")? {
        return Ok(());
    }
    add_column_if_missing(conn, "job_runs", "build_json", "TEXT")
}

/// Host-wide allocation probes retain the old workspace keys so historical
/// duplicate ids remain readable. These non-unique indexes bound each probe
/// without scanning every workspace's retained allocation ledger.
pub(super) fn apply_job_run_host_id_indexes(conn: &Connection) -> Result<(), OrbitError> {
    if table_exists(conn, "job_runs")? {
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_job_runs_host_run_id ON job_runs(run_id);",
        )
        .map_err(sqlite_store_error)?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_job_run_allocations_host_run_id
         ON job_run_id_allocations(run_id);",
    )
    .map_err(sqlite_store_error)
}

/// v94 `job_run_crew_model_backfill`: copy a run's legacy `implementer_model`
/// into `crew_model`, so reads no longer fall back to the legacy column.
///
/// Only rows with no `crew_model` and a recorded `implementer_model` change,
/// so a rerun over a migrated table updates nothing. The legacy column stays.
/// A table without both columns (a pre-consolidation shape) is left alone.
pub(super) fn apply_job_run_crew_model_backfill(conn: &Connection) -> Result<(), OrbitError> {
    if !table_has_column(conn, "job_runs", "crew_model")?
        || !table_has_column(conn, "job_runs", "implementer_model")?
    {
        return Ok(());
    }
    conn.execute_batch(
        "UPDATE job_runs SET crew_model = implementer_model \
         WHERE crew_model IS NULL AND implementer_model IS NOT NULL",
    )
    .map_err(sqlite_store_error)
}
