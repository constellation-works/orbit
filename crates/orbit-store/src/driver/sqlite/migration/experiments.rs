use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v63 experiments: operator-defined experiments, their arms, an append-only
/// event log, and per-task assignments. `revision` counts writes to an
/// experiment and numbers its events, so a lifecycle write can refuse a stale
/// read. Timestamps are fixed-width UTC RFC3339 text, so they compare
/// lexically. Triggers keep `experiment_events` append-only. New tables
/// only: an older binary never reads or writes them.
pub(super) fn apply_experiments(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS experiments (
                workspace_id TEXT NOT NULL,
                id TEXT NOT NULL,
                name TEXT NOT NULL,
                hypothesis TEXT NOT NULL,
                population_json TEXT NOT NULL,
                primary_metric TEXT,
                guardrails_json TEXT NOT NULL,
                min_sample_per_arm INTEGER,
                max_days INTEGER,
                salt TEXT NOT NULL,
                mode TEXT NOT NULL,
                status TEXT NOT NULL,
                segment INTEGER NOT NULL,
                concluded_arm TEXT,
                cloned_from TEXT,
                created_by TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                revision INTEGER NOT NULL,
                PRIMARY KEY (workspace_id, id)
            );
            CREATE UNIQUE INDEX IF NOT EXISTS experiments_workspace_name
                ON experiments (workspace_id, name);
            CREATE INDEX IF NOT EXISTS experiments_workspace_status
                ON experiments (workspace_id, status);
            CREATE TABLE IF NOT EXISTS experiment_arms (
                workspace_id TEXT NOT NULL,
                experiment_id TEXT NOT NULL,
                name TEXT NOT NULL,
                position INTEGER NOT NULL,
                control INTEGER NOT NULL,
                weight INTEGER NOT NULL,
                overlay_json TEXT NOT NULL,
                overlay_digest TEXT NOT NULL,
                retired_segment INTEGER,
                PRIMARY KEY (workspace_id, experiment_id, name)
            );
            CREATE TABLE IF NOT EXISTS experiment_events (
                workspace_id TEXT NOT NULL,
                experiment_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                kind TEXT NOT NULL,
                from_status TEXT,
                to_status TEXT,
                segment INTEGER NOT NULL,
                actor TEXT NOT NULL,
                at TEXT NOT NULL,
                reason TEXT,
                detail_json TEXT NOT NULL,
                PRIMARY KEY (workspace_id, experiment_id, seq)
            );
            CREATE TRIGGER IF NOT EXISTS experiment_events_no_update
                BEFORE UPDATE ON experiment_events
                BEGIN SELECT RAISE(ABORT, 'experiment_events is append-only'); END;
            CREATE TRIGGER IF NOT EXISTS experiment_events_no_delete
                BEFORE DELETE ON experiment_events
                BEGIN SELECT RAISE(ABORT, 'experiment_events is append-only'); END;
            CREATE TABLE IF NOT EXISTS experiment_assignments (
                workspace_id TEXT NOT NULL,
                experiment_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                arm TEXT,
                segment INTEGER,
                overlay_digest TEXT,
                assigned_at TEXT NOT NULL,
                forced INTEGER NOT NULL DEFAULT 0,
                exclusion_reason TEXT,
                PRIMARY KEY (workspace_id, experiment_id, task_id)
            );
            CREATE INDEX IF NOT EXISTS experiment_assignments_task
                ON experiment_assignments (workspace_id, task_id);
        "#,
    )
    .map_err(sqlite_store_error)
}

/// Remember first admissions even when no experiment matched the task.
pub(super) fn apply_experiment_admissions(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS experiment_admissions (workspace_id TEXT NOT NULL, task_id TEXT NOT NULL, PRIMARY KEY (workspace_id, task_id));")
        .map_err(sqlite_store_error)
}

/// v70: reopen admission-only rows. Before ORB-19390, admission recorded a
/// row even while experiments were disabled, and that row then kept the task
/// out of every experiment for good. Those rows cannot be told apart from an
/// enabled admission that matched no population, so each such task is
/// admitted once more; rows beside an assignment or exclusion stay sticky.
/// Every later row comes from an enabled admission and is final.
pub(super) fn apply_reopen_legacy_experiment_admissions(
    conn: &Connection,
) -> Result<(), OrbitError> {
    conn.execute_batch(
        "DELETE FROM experiment_admissions WHERE NOT EXISTS (SELECT 1 FROM experiment_assignments a WHERE a.workspace_id = experiment_admissions.workspace_id AND a.task_id = experiment_admissions.task_id);",
    )
    .map_err(sqlite_store_error)
}
