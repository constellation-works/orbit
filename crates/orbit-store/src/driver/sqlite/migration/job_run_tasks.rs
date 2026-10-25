//! v59: materialized task ownership in per-job recency order.
//!
//! Backfill once at upgrade, then maintain on every writer's insert, timing,
//! input and deletion changes, including writers from older additive-compatible
//! binaries. Reads never evaluate JSON against unrelated run history.
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

pub(super) fn apply_job_run_task_recency(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS job_run_tasks (
            workspace_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            job_id TEXT NOT NULL,
            recency TEXT NOT NULL,
            PRIMARY KEY(workspace_id, run_id, task_id),
            FOREIGN KEY(workspace_id, run_id) REFERENCES job_runs(workspace_id, run_id)
                ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_job_run_tasks_recency ON job_run_tasks(
            workspace_id, job_id, task_id, recency DESC, run_id ASC
        );",
    )
    .map_err(sqlite_store_error)?;

    // Keep exact text membership, as JobRunQuery.task_id does. Non-array,
    // non-text and nested bindings confer no ownership. Bad legacy JSON is
    // unowned rather than preventing every workspace from opening.
    let insert = |source: &str| {
        format!(
            "INSERT INTO job_run_tasks(workspace_id, run_id, task_id, job_id, recency)
         SELECT DISTINCT source.workspace_id, source.run_id, binding.value, source.job_id,
                COALESCE(source.finished_at, source.started_at, source.created_at)
         FROM {source}, json_each(json_insert(
             CASE WHEN json_type(source.input_json, '$.task_ids') = 'array'
                  THEN json_extract(source.input_json, '$.task_ids') ELSE '[]' END,
             '$[#]', CASE WHEN json_type(source.input_json, '$.task_id') = 'text'
                         THEN json_extract(source.input_json, '$.task_id') ELSE NULL END
         )) AS binding WHERE binding.type = 'text';"
        )
    };
    let columns = |row: &str| {
        format!(
            "{row}.workspace_id, {row}.run_id, {row}.job_id, {row}.finished_at,
         {row}.started_at, {row}.created_at,
         CASE WHEN json_valid({row}.input_json) THEN {row}.input_json ELSE '{{}}' END AS input_json"
        )
    };
    let backfill = insert(&format!(
        "(SELECT {} FROM job_runs) AS source",
        columns("job_runs")
    ));
    conn.execute_batch("DELETE FROM job_run_tasks;")
        .map_err(sqlite_store_error)?;
    conn.execute_batch(&backfill).map_err(sqlite_store_error)?;
    let incoming = insert(&format!("(SELECT {}) AS source", columns("NEW")));
    conn.execute_batch(&format!(
        "CREATE TRIGGER IF NOT EXISTS job_run_tasks_insert AFTER INSERT ON job_runs BEGIN
             DELETE FROM job_run_tasks WHERE workspace_id = NEW.workspace_id AND run_id = NEW.run_id;
             {incoming}
         END;
         CREATE TRIGGER IF NOT EXISTS job_run_tasks_update AFTER UPDATE OF workspace_id, run_id, job_id,
             input_json, finished_at, started_at, created_at ON job_runs BEGIN
             DELETE FROM job_run_tasks WHERE workspace_id = OLD.workspace_id AND run_id = OLD.run_id;
             {incoming}
         END;
         CREATE TRIGGER IF NOT EXISTS job_run_tasks_delete AFTER DELETE ON job_runs BEGIN
             DELETE FROM job_run_tasks WHERE workspace_id = OLD.workspace_id AND run_id = OLD.run_id;
         END;"
    )).map_err(sqlite_store_error)
}
