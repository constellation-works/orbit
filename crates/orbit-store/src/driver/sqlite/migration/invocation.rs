use orbit_common::OrbitError;
use rusqlite::Connection;

use super::introspect::{add_column_if_missing, table_exists, table_has_column};

/// v10 `invocation_telemetry_columns` migration (ORB-10367): re-run the
/// idempotent invocation-schema step against databases that already recorded
/// the v1 baseline.
///
/// The 5m/1h cache split (`cache_create_1h_tokens`) and the token-derived
/// cost column (`provider_cost_usd`) were added to
/// [`ensure_invocation_schema`], which only ever runs as part of the v1
/// `baseline` migration. Every database created before those columns landed
/// is already at v1 or newer, so `run_migrations` skips baseline and the
/// `ALTER`s never reach it — the insert then binds columns the table lacks
/// and every agent-dispatching run dies at the telemetry write. Registering
/// the same idempotent step under its own version is what carries it to
/// existing databases.
pub(super) fn apply_invocation_telemetry_columns(conn: &Connection) -> Result<(), OrbitError> {
    // The `ALTER`s below address tables the v1 baseline creates. A database
    // without them has nothing to repair (and `ALTER TABLE` on a missing
    // table is an error, not a no-op), so skip rather than fail the open.
    if !table_exists(conn, "invocations")?
        || !table_exists(conn, "invocation_tasks")?
        || !table_exists(conn, "tool_calls")?
    {
        return Ok(());
    }
    add_column_if_missing(conn, "invocations", "provider_cost_usd", "REAL")?;
    add_column_if_missing(
        conn,
        "invocations",
        "cache_create_1h_tokens",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_invocation_schema_v1(conn)
}

pub(super) fn ensure_invocation_schema_v1(conn: &Connection) -> Result<(), OrbitError> {
    add_column_if_missing(conn, "invocations", "slot", "TEXT")?;
    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_invocations_job_run_id
            ON invocations(job_run_id);

            CREATE INDEX IF NOT EXISTS idx_invocations_activity_id
            ON invocations(activity_id);

            CREATE INDEX IF NOT EXISTS idx_invocations_ts
            ON invocations(ts DESC, id DESC);

            CREATE INDEX IF NOT EXISTS idx_invocation_tasks_task_id
            ON invocation_tasks(task_id);

            CREATE INDEX IF NOT EXISTS idx_tool_calls_tool_name
            ON tool_calls(tool_name);
        "#,
    )
    .map_err(|e| OrbitError::Store(e.to_string()))
}

/// v20 `invocations_ts_index`: cover the accounting window filters and the
/// newest-first invocation listing on `invocations(ts, id)`.
///
/// Guarded like v19: `ensure_invocation_schema_v1` declares the same index
/// for a database whose `invocations` table is created or upgraded at open.
pub(super) fn apply_invocations_ts_index(conn: &Connection) -> Result<(), OrbitError> {
    if !table_has_column(conn, "invocations", "ts")? {
        return Ok(());
    }
    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_invocations_ts
            ON invocations(ts DESC, id DESC);
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}

/// v36 `invocation_workspace_scope`: record the workspace each invocation
/// belongs to.
///
/// Run ids are minted per workspace, so two workspaces submitting in the same
/// minute hold the same id and an invocation keyed by `job_run_id` alone was
/// read by both. The backfill attributes a legacy row only when exactly one
/// workspace ever held its run id (`job_run_id_allocations`, plus `job_runs`);
/// an ambiguous or orphaned row stays `NULL` and no workspace-scoped read
/// matches it.
pub(super) fn apply_invocation_workspace_scope(conn: &Connection) -> Result<(), OrbitError> {
    if !table_exists(conn, "invocations")? {
        return Ok(());
    }
    add_column_if_missing(conn, "invocations", "workspace_id", "TEXT")?;

    let mut owners = Vec::new();
    if table_has_column(conn, "job_run_id_allocations", "workspace_id")? {
        owners.push("SELECT workspace_id, run_id FROM job_run_id_allocations");
    }
    if table_has_column(conn, "job_runs", "workspace_id")? {
        owners.push("SELECT workspace_id, run_id FROM job_runs");
    }
    if !owners.is_empty() {
        let owners = owners.join(" UNION ");
        conn.execute_batch(&format!(
            r#"
                UPDATE invocations
                SET workspace_id = (
                    SELECT CASE WHEN COUNT(DISTINCT owner.workspace_id) = 1
                                THEN MIN(owner.workspace_id) END
                    FROM ({owners}) owner
                    WHERE owner.run_id = invocations.job_run_id
                )
                WHERE workspace_id IS NULL;
            "#
        ))
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    }

    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_invocations_workspace_job_run_id
            ON invocations(workspace_id, job_run_id);
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}

/// v40 `provider_limit_observations` [ORB-14695]: the host's provider usage
/// limits, beside the invocation ledger in the host-global database, since a
/// limit belongs to the provider login on this host.
///
/// One row per provider, model scope and window; the empty string stands for
/// an account-wide scope or an unlabelled window so the key stays unique.
/// Purely additive: an older binary never reads the table.
pub(super) fn apply_provider_limit_observations(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS provider_limit_observations (
                provider TEXT NOT NULL,
                model_scope TEXT NOT NULL DEFAULT '',
                window_label TEXT NOT NULL DEFAULT '',
                exhausted INTEGER NOT NULL,
                source TEXT NOT NULL,
                resets_at TEXT,
                observed_at TEXT NOT NULL,
                run_id TEXT,
                crew TEXT,
                detail TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (provider, model_scope, window_label)
            );
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}
