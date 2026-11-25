//! v57: restore the lifecycle vocabulary released clients understand.
//!
//! Frozen data repair: move the complete handoff settlement to a row family
//! older binaries never read or rewrite. The migration engine supplies one
//! IMMEDIATE transaction for both the repair and its ledger/compatibility stamp.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, OptionalExtension, params};

const STATE: &str = "distributed-claim-lifecycle-v1";
const HANDOFF_SETTLEMENT: &str = "distributed-handoff-settlement-v1";

pub(super) fn apply_handoff_settlement_sidecars(conn: &Connection) -> Result<(), OrbitError> {
    let rows = {
        let mut query = conn
            .prepare(
                "SELECT workspace_id,row_id,payload_json,journal_id,created_at
             FROM task_coordination_rows WHERE kind=?1
             AND json_extract(payload_json,'$.settlement.kind')='handoff'",
            )
            .map_err(sqlite_store_error)?;
        query
            .query_map([STATE], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)?
    };
    for (workspace, claim, payload, journal, created) in rows {
        let mut lifecycle: serde_json::Value = serde_json::from_str(&payload)
            .map_err(|error| OrbitError::Migration(error.to_string()))?;
        let settlement = lifecycle
            .as_object_mut()
            .and_then(|object| object.remove("settlement"))
            .ok_or_else(|| OrbitError::Migration("handoff settlement object required".into()))?;
        let existing: Option<String> = conn
            .query_row(
                "SELECT payload_json FROM task_coordination_rows
             WHERE workspace_id=?1 AND kind=?2 AND row_id=?3",
                params![workspace, HANDOFF_SETTLEMENT, claim],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_store_error)?;
        if let Some(existing) = existing {
            let existing: serde_json::Value = serde_json::from_str(&existing)
                .map_err(|error| OrbitError::Migration(error.to_string()))?;
            if existing != settlement {
                return Err(OrbitError::Migration(format!(
                    "conflicting handoff settlement for workspace {workspace}, claim {claim}; \
                     restore consistent attribution before retrying the upgrade"
                )));
            }
        } else {
            conn.execute(
                "INSERT INTO task_coordination_rows(workspace_id,kind,row_id,payload_json,journal_id,created_at)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![workspace, HANDOFF_SETTLEMENT, claim, settlement.to_string(), journal, created],
            ).map_err(sqlite_store_error)?;
        }
        conn.execute(
            "UPDATE task_coordination_rows SET payload_json=?3
             WHERE workspace_id=?1 AND kind=?4 AND row_id=?2",
            params![workspace, claim, lifecycle.to_string(), STATE],
        )
        .map_err(sqlite_store_error)?;
    }
    // Enforce compatibility on already-open connections too. The intermediate
    // inline-handoff writer must not reintroduce the value after the repair.
    // ABORT fails its statement; the journal's transaction then rolls back
    // every authority and receipt change. Fail/release writers are unaffected.
    conn.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS handoff_settlement_insert_guard
         BEFORE INSERT ON task_coordination_rows
         WHEN NEW.kind='distributed-claim-lifecycle-v1'
          AND json_extract(NEW.payload_json,'$.settlement.kind')='handoff'
         BEGIN SELECT RAISE(ABORT,
          'incompatible inline handoff settlement writer; restart this Orbit client on the upgraded binary'); END;
         CREATE TRIGGER IF NOT EXISTS handoff_settlement_update_guard
         BEFORE UPDATE ON task_coordination_rows
         WHEN NEW.kind='distributed-claim-lifecycle-v1'
          AND json_extract(NEW.payload_json,'$.settlement.kind')='handoff'
         BEGIN SELECT RAISE(ABORT,
          'incompatible inline handoff settlement writer; restart this Orbit client on the upgraded binary'); END;",
    ).map_err(sqlite_store_error)?;
    Ok(())
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
