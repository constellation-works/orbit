use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v21 `task_commit_journal` (ORB-12528).
///
/// `task_commit_journal` holds one row per intended publication of a task
/// transition together with its reservation and dependent coordination rows.
/// The row is inserted `prepared`, flipped to `committed` inside the same
/// transaction that writes those rows, and settled `applied` once the bundle
/// files carry the transition. Recovery reads exactly the unsettled rows:
/// `prepared` rolls back, `committed` rolls forward.
///
/// `task_coordination_rows` holds the dependent rows themselves. Its primary
/// key is the caller's `(workspace_id, kind, row_id)` identity, so replaying a
/// commit is refused at the database rather than duplicated.
pub(super) fn apply_task_commit_journal(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS task_commit_journal (
                journal_id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                state TEXT NOT NULL,
                intent_json TEXT NOT NULL,
                created_at TEXT NOT NULL,
                committed_at TEXT,
                applied_at TEXT,
                reservation_id TEXT,
                CHECK (state IN ('prepared', 'committed', 'applied', 'aborted'))
            );

            CREATE INDEX IF NOT EXISTS idx_task_commit_journal_workspace_state
            ON task_commit_journal(workspace_id, state, created_at);

            CREATE TABLE IF NOT EXISTS task_coordination_rows (
                workspace_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                row_id TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                journal_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (workspace_id, kind, row_id)
            );

            CREATE INDEX IF NOT EXISTS idx_task_coordination_rows_journal
            ON task_coordination_rows(journal_id);
        "#,
    )
    .map_err(sqlite_store_error)
}

/// v90 `task_coordination_claim_indexes`: partial expression indexes so a
/// task write and a footprint guard no longer read every claim payload.
///
/// A partial index applies only when the statement spells `kind` as the same
/// literal. The protecting-phase predicate is
/// `ExecutionClaimPhase::protects_footprint` (`claimed`, `running`,
/// `handed_off`, `repair_pending`). Lifecycle lookups use `$.claim.task_id`
/// and `$.claim.run_context.run_id`. The bind-uniqueness check compares
/// `bound_run`, which is the leaf run and not the admission run, so that
/// lookup has its own expression. Keep each expression identical to the
/// statement in `task_commit_journal/coordination.rs`, including spacing:
/// SQLite matches an expression index only when the text is the same.
///
/// Index-only and additive: an older binary keeps reading and writing the
/// table, and SQLite maintains the indexes for it.
pub(super) fn apply_task_coordination_claim_indexes(conn: &Connection) -> Result<(), OrbitError> {
    if !super::introspect::table_exists(conn, "task_coordination_rows")? {
        return Ok(());
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_task_coordination_claims_by_task
         ON task_coordination_rows(workspace_id, json_extract(payload_json,'$.task_id'))
         WHERE kind = 'distributed-execution-claim-v1';
         CREATE INDEX IF NOT EXISTS idx_task_coordination_claims_protecting
         ON task_coordination_rows(workspace_id)
         WHERE kind = 'distributed-execution-claim-v1'
           AND json_extract(payload_json,'$.phase') IN ('claimed','running','handed_off','repair_pending');
         CREATE INDEX IF NOT EXISTS idx_task_coordination_lifecycle_by_task
         ON task_coordination_rows(workspace_id, json_extract(payload_json,'$.claim.task_id'))
         WHERE kind = 'distributed-claim-lifecycle-v1';
         CREATE INDEX IF NOT EXISTS idx_task_coordination_lifecycle_by_run
         ON task_coordination_rows(workspace_id, json_extract(payload_json,'$.claim.run_context.run_id'))
         WHERE kind = 'distributed-claim-lifecycle-v1';
         CREATE INDEX IF NOT EXISTS idx_task_coordination_lifecycle_by_bound_run
         ON task_coordination_rows(workspace_id, json_extract(payload_json,'$.bound_run.run_id'))
         WHERE kind = 'distributed-claim-lifecycle-v1';",
    )
    .map_err(sqlite_store_error)
}
