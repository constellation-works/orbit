//! Atomic first-admission assignment, serialized with experiment lifecycle writes.
use super::{
    codec::stamp,
    experiment_store::{EXPERIMENT_COLUMNS, hydrate_experiment, load_arms},
};
use crate::{
    Store,
    contracts::{ExperimentAssignment, ExperimentAssignmentDecision, ExperimentRecord},
};
use orbit_common::{OrbitError, storage::sqlite::sqlite_store_error};
use rusqlite::{Connection, TransactionBehavior, params};

pub(super) fn assign_experiments(
    store: &Store,
    workspace: &str,
    task: &str,
    mark_admitted: bool,
    experiments_enabled: bool,
    decide: &ExperimentAssignmentDecision<'_>,
) -> Result<Vec<ExperimentAssignment>, OrbitError> {
    store.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
        let conn = tx.connection();
        let admitted: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM experiment_admissions WHERE workspace_id=?1 AND task_id=?2)",
            params![workspace, task], |row| row.get(0),
        ).map_err(sqlite_store_error)?;
        let existing = load_assignments(conn, workspace, task)?;
        // A prior assignment or exclusion is sticky. Admission-only rows can
        // be old writes from a disabled process and must be reconsidered.
        if mark_admitted && admitted && !existing.is_empty() {
            return Ok(existing);
        }
        let mut statement = conn.prepare(&format!(
            "SELECT {EXPERIMENT_COLUMNS} FROM experiments WHERE workspace_id=?1 ORDER BY created_at, id"
        )).map_err(sqlite_store_error)?;
        let definitions = statement.query_map([workspace], hydrate_experiment)
            .map_err(sqlite_store_error)?.collect::<Result<Vec<_>, _>>().map_err(sqlite_store_error)?;
        let records = definitions.into_iter().map(|experiment| {
            Ok(ExperimentRecord { arms: load_arms(conn, &experiment)?, experiment, events: Vec::new() })
        }).collect::<Result<Vec<_>, OrbitError>>()?;
        let running_experiment_exists = records.iter().any(|record| {
            record.experiment.status == "running"
                && matches!(record.experiment.mode.as_str(), "shadow" | "live")
        });
        let admission_enabled = experiments_enabled || running_experiment_exists;
        if mark_admitted && !admission_enabled {
            return Ok(existing);
        }
        for assignment in decide(&records, &existing, admitted)? {
            if assignment.task_id != task {
                return Err(OrbitError::Store("experiment assignment task mismatch".into()));
            }
            conn.execute(
                "INSERT INTO experiment_assignments (workspace_id, experiment_id, task_id, arm, segment, overlay_digest, assigned_at, forced, exclusion_reason) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![workspace, assignment.experiment, task, assignment.arm, assignment.segment, assignment.digest, stamp(assignment.assigned_at), assignment.forced, assignment.exclusion_reason],
            ).map_err(sqlite_store_error)?;
        }
        if mark_admitted && admission_enabled {
            conn.execute("INSERT OR IGNORE INTO experiment_admissions (workspace_id, task_id) VALUES (?1,?2)", params![workspace, task]).map_err(sqlite_store_error)?;
        }
        load_assignments(conn, workspace, task)
    })
}

pub(super) fn load_assignments(
    conn: &Connection,
    workspace: &str,
    task: &str,
) -> Result<Vec<ExperimentAssignment>, OrbitError> {
    query_assignments(
        conn,
        "SELECT a.experiment_id, a.task_id, a.arm, a.segment, a.overlay_digest, a.assigned_at, a.forced, a.exclusion_reason FROM experiment_assignments a JOIN experiments e ON e.workspace_id=a.workspace_id AND e.id=a.experiment_id WHERE a.workspace_id=?1 AND a.task_id=?2 ORDER BY e.created_at, e.id",
        workspace,
        task,
    )
}

/// One experiment's rows, oldest assignment first.
pub(super) fn load_experiment_assignments(
    conn: &Connection,
    workspace: &str,
    experiment: &str,
) -> Result<Vec<ExperimentAssignment>, OrbitError> {
    query_assignments(
        conn,
        "SELECT experiment_id, task_id, arm, segment, overlay_digest, assigned_at, forced, exclusion_reason FROM experiment_assignments WHERE workspace_id=?1 AND experiment_id=?2 ORDER BY assigned_at, task_id",
        workspace,
        experiment,
    )
}

fn query_assignments(
    conn: &Connection,
    sql: &str,
    workspace: &str,
    key: &str,
) -> Result<Vec<ExperimentAssignment>, OrbitError> {
    let mut statement = conn.prepare(sql).map_err(sqlite_store_error)?;
    statement
        .query_map(params![workspace, key], |row| {
            Ok(ExperimentAssignment {
                experiment: row.get(0)?,
                task_id: row.get(1)?,
                arm: row.get(2)?,
                segment: row.get(3)?,
                digest: row.get(4)?,
                assigned_at: crate::parse_timestamp(&row.get::<_, String>(5)?)?,
                forced: row.get(6)?,
                exclusion_reason: row.get(7)?,
            })
        })
        .map_err(sqlite_store_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
}
