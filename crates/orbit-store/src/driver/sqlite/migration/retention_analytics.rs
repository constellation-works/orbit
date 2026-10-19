//! Analytics facts that must outlive raw audit and pipeline-state retention.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

use super::introspect::add_column_if_missing;

/// Capture and backfill in the migration transaction, before GC can prune.
/// Triggers also cover narrow audit appenders and older compatible writers.
/// No foreign key points back at audit rows: pruning must not cascade here.
pub(super) fn apply_retention_analytics(conn: &Connection) -> Result<(), OrbitError> {
    add_column_if_missing(conn, "job_runs", "commit_decision", "TEXT")?;
    add_column_if_missing(conn, "job_runs", "commit_committed", "INTEGER")?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS task_lock_denials (
            execution_id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            timestamp TEXT NOT NULL,
            job_run_id TEXT,
            arguments_json TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_task_lock_denials_workspace_time
            ON task_lock_denials(workspace_id, timestamp);

        CREATE TRIGGER IF NOT EXISTS retain_task_lock_denial
        AFTER INSERT ON audit_events
        WHEN NEW.tool_name = 'orbit.task.locks.reserve' AND NEW.status = 'denied'
             AND NEW.command != 'authorization' AND NEW.workspace_id IS NOT NULL
             AND CASE WHEN json_valid(NEW.arguments_json)
                 THEN json_type(NEW.arguments_json, '$.conflicts') = 'array'
                      AND json_array_length(NEW.arguments_json, '$.conflicts') > 0
                 ELSE 0 END
        BEGIN
            INSERT OR IGNORE INTO task_lock_denials
                (execution_id, workspace_id, timestamp, job_run_id, arguments_json)
            VALUES (NEW.execution_id, NEW.workspace_id, NEW.timestamp, NEW.job_run_id,
                json_object('task_ids', json_extract(NEW.arguments_json, '$.task_ids'),
                            'owner_run_id', json_extract(NEW.arguments_json, '$.owner_run_id'),
                            'files', json_extract(NEW.arguments_json, '$.files'),
                            'conflicts', json_extract(NEW.arguments_json, '$.conflicts')));
        END;

        INSERT OR IGNORE INTO task_lock_denials
            (execution_id, workspace_id, timestamp, job_run_id, arguments_json)
        SELECT execution_id, workspace_id, timestamp, job_run_id,
            json_object('task_ids', json_extract(arguments_json, '$.task_ids'),
                        'owner_run_id', json_extract(arguments_json, '$.owner_run_id'),
                        'files', json_extract(arguments_json, '$.files'),
                        'conflicts', json_extract(arguments_json, '$.conflicts'))
        FROM audit_events
        WHERE tool_name = 'orbit.task.locks.reserve' AND status = 'denied'
              AND command != 'authorization' AND workspace_id IS NOT NULL
              AND CASE WHEN json_valid(arguments_json)
                  THEN json_type(arguments_json, '$.conflicts') = 'array'
                       AND json_array_length(arguments_json, '$.conflicts') > 0
                  ELSE 0 END;

        CREATE TRIGGER IF NOT EXISTS retain_run_commit_insert
        AFTER INSERT ON job_run_states
        BEGIN
            UPDATE job_runs SET
                commit_decision = CASE WHEN json_valid(NEW.pipeline_state_json)
                    THEN json_extract(NEW.pipeline_state_json, '$.pipeline.commit.decision') END,
                commit_committed = CASE WHEN json_valid(NEW.pipeline_state_json)
                    THEN json_extract(NEW.pipeline_state_json, '$.pipeline.commit.committed') END
            WHERE workspace_id = NEW.workspace_id AND run_id = NEW.run_id;
        END;
        CREATE TRIGGER IF NOT EXISTS retain_run_commit_update
        AFTER UPDATE OF pipeline_state_json ON job_run_states
        BEGIN
            UPDATE job_runs SET
                commit_decision = CASE WHEN json_valid(NEW.pipeline_state_json)
                    THEN json_extract(NEW.pipeline_state_json, '$.pipeline.commit.decision') END,
                commit_committed = CASE WHEN json_valid(NEW.pipeline_state_json)
                    THEN json_extract(NEW.pipeline_state_json, '$.pipeline.commit.committed') END
            WHERE workspace_id = NEW.workspace_id AND run_id = NEW.run_id;
        END;

        UPDATE job_runs SET (commit_decision, commit_committed) = (
            SELECT CASE WHEN json_valid(s.pipeline_state_json)
                       THEN json_extract(s.pipeline_state_json, '$.pipeline.commit.decision') END,
                   CASE WHEN json_valid(s.pipeline_state_json)
                       THEN json_extract(s.pipeline_state_json, '$.pipeline.commit.committed') END
            FROM job_run_states s
            WHERE s.workspace_id = job_runs.workspace_id AND s.run_id = job_runs.run_id)
        WHERE EXISTS (SELECT 1 FROM job_run_states s
                      WHERE s.workspace_id = job_runs.workspace_id AND s.run_id = job_runs.run_id);
        "#,
    )
    .map_err(sqlite_store_error)
}
