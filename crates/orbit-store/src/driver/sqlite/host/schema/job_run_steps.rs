use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

use super::introspect::{add_column_if_missing, table_has_column};

/// Add optional timing without inventing observations for historical steps.
/// Idempotent: a rewound ledger replays this over a table that already has it.
pub(super) fn apply_job_run_step_lock_wait(conn: &Connection) -> Result<(), OrbitError> {
    add_column_if_missing(conn, "job_run_steps", "lock_wait_ms", "INTEGER")
}

/// v76 `job_run_step_activity_window_index`: cover the succeeded-candidate
/// predicate, which looks up one run's activity steps and then reads only
/// `started_at` and `finished_at` (compared with `julianday`, so those columns
/// are payload, not a range key).
///
/// The primary key is `(workspace_id, run_id, step_index)`, so that lookup
/// fetched every column of the matching steps, including `agent_response_json`.
/// On a retained-history store the predicate's invocation read dropped from
/// about 27 ms to about 18 ms once the lookup was covering. An older binary
/// never names the index; SQLite maintains it on writes.
pub(super) fn apply_job_run_step_activity_window_index(
    conn: &Connection,
) -> Result<(), OrbitError> {
    for column in [
        "workspace_id",
        "run_id",
        "target_type",
        "target_id",
        "state",
        "started_at",
        "finished_at",
    ] {
        if !table_has_column(conn, "job_run_steps", column)? {
            return Ok(());
        }
    }
    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_job_run_steps_activity_window
            ON job_run_steps(
                workspace_id, run_id, target_type, target_id, state, started_at, finished_at
            );
        "#,
    )
    .map_err(sqlite_store_error)
}
