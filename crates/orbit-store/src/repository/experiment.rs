//! Concrete repositories for experiment records (`experiments`,
//! `experiment_arms`, `experiment_events`) and their sticky assignments
//! (`experiment_assignments`, `experiment_admissions`).
//!
//! Reached only through [`Snapshot::experiment`] / [`Tx::experiment`] and
//! [`Snapshot::experiment_assignment`] / [`Tx::experiment_assignment`]. The
//! lifecycle rules (revision checks, unique names, the running limit, sticky
//! assignment and admission) belong to the calling operation inside
//! [`crate::Store::write`]; these repositories own SQL and row mapping only.

use std::marker::PhantomData;

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::contract::{
    ExperimentArmRow, ExperimentAssignment, ExperimentEventRow, ExperimentRecord, ExperimentRow,
};
use crate::driver::sqlite::codec::stamp;
use crate::store::{Snapshot, Tx};

const EXPERIMENT_COLUMNS: &str = "workspace_id, id, name, hypothesis, population_json, \
     primary_metric, guardrails_json, min_sample_per_arm, max_days, salt, mode, status, \
     segment, concluded_arm, cloned_from, created_by, created_at, updated_at, revision";

const ARM_COLUMNS: &str =
    "name, position, control, weight, overlay_json, overlay_digest, retired_segment";

const EVENT_COLUMNS: &str =
    "seq, kind, from_status, to_status, segment, actor, at, reason, detail_json";

/// Concrete experiment repository scoped to a [`Snapshot`] (read-only) or a
/// [`Tx`] (read-write).
pub struct ExperimentRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> ExperimentRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> ExperimentRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Insert `experiment`, or replace the stored row with the same id. The
    /// salt, creator, creation time and clone source keep their stored values.
    pub fn put(&self, experiment: &ExperimentRow) -> Result<(), OrbitError> {
        self.conn
            .execute(
                &format!(
                    "INSERT INTO experiments ({EXPERIMENT_COLUMNS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
                             ?16, ?17, ?18, ?19) \
                     ON CONFLICT (workspace_id, id) DO UPDATE SET \
                        name = excluded.name, hypothesis = excluded.hypothesis, \
                        population_json = excluded.population_json, \
                        primary_metric = excluded.primary_metric, \
                        guardrails_json = excluded.guardrails_json, \
                        min_sample_per_arm = excluded.min_sample_per_arm, \
                        max_days = excluded.max_days, mode = excluded.mode, \
                        status = excluded.status, segment = excluded.segment, \
                        concluded_arm = excluded.concluded_arm, \
                        updated_at = excluded.updated_at, revision = excluded.revision"
                ),
                params![
                    experiment.workspace_id,
                    experiment.id,
                    experiment.name,
                    experiment.hypothesis,
                    experiment.population_json,
                    experiment.primary_metric,
                    experiment.guardrails_json,
                    experiment.min_sample_per_arm,
                    experiment.max_days,
                    experiment.salt,
                    experiment.mode,
                    experiment.status,
                    experiment.segment,
                    experiment.concluded_arm,
                    experiment.cloned_from,
                    experiment.created_by,
                    stamp(experiment.created_at),
                    stamp(experiment.updated_at),
                    experiment.revision,
                ],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }

    /// Replace every arm of `experiment` with `arms`.
    pub fn put_arms(
        &self,
        experiment: &ExperimentRow,
        arms: &[ExperimentArmRow],
    ) -> Result<(), OrbitError> {
        self.conn
            .execute(
                "DELETE FROM experiment_arms WHERE workspace_id = ?1 AND experiment_id = ?2",
                params![experiment.workspace_id, experiment.id],
            )
            .map_err(sqlite_store_error)?;
        let mut insert = self
            .conn
            .prepare(&format!(
                "INSERT INTO experiment_arms (workspace_id, experiment_id, {ARM_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
            ))
            .map_err(sqlite_store_error)?;
        for arm in arms {
            insert
                .execute(params![
                    experiment.workspace_id,
                    experiment.id,
                    arm.name,
                    arm.position,
                    arm.control,
                    arm.weight,
                    arm.overlay_json,
                    arm.overlay_digest,
                    arm.retired_segment,
                ])
                .map_err(sqlite_store_error)?;
        }
        Ok(())
    }

    /// Append `event` to the event log of `experiment`.
    pub fn put_event(
        &self,
        experiment: &ExperimentRow,
        event: &ExperimentEventRow,
    ) -> Result<(), OrbitError> {
        self.conn
            .execute(
                &format!(
                    "INSERT INTO experiment_events (workspace_id, experiment_id, {EVENT_COLUMNS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
                ),
                params![
                    experiment.workspace_id,
                    experiment.id,
                    event.seq,
                    event.kind,
                    event.from_status,
                    event.to_status,
                    event.segment,
                    event.actor,
                    stamp(event.at),
                    event.reason,
                    event.detail_json,
                ],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }

    /// Delete one experiment with its arms, events and assignment rows.
    pub fn delete(&self, workspace_id: &str, experiment_id: &str) -> Result<(), OrbitError> {
        // The experiment row goes first: its events are append-only while it
        // exists (v79).
        self.conn
            .execute(
                "DELETE FROM experiments WHERE workspace_id = ?1 AND id = ?2",
                params![workspace_id, experiment_id],
            )
            .map_err(sqlite_store_error)?;
        for table in [
            "experiment_events",
            "experiment_arms",
            "experiment_assignments",
        ] {
            self.conn
                .execute(
                    &format!("DELETE FROM {table} WHERE workspace_id = ?1 AND experiment_id = ?2"),
                    params![workspace_id, experiment_id],
                )
                .map_err(sqlite_store_error)?;
        }
        Ok(())
    }
}

impl<Access> ExperimentRepository<'_, Access> {
    /// The stored revision of one experiment, when it exists.
    pub fn revision(
        &self,
        workspace_id: &str,
        experiment_id: &str,
    ) -> Result<Option<u32>, OrbitError> {
        self.conn
            .query_row(
                "SELECT revision FROM experiments WHERE workspace_id = ?1 AND id = ?2",
                params![workspace_id, experiment_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_store_error)
    }

    /// Whether an experiment other than `except_id` already has `name`.
    pub fn name_exists(
        &self,
        workspace_id: &str,
        name: &str,
        except_id: &str,
    ) -> Result<bool, OrbitError> {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM experiments \
                 WHERE workspace_id = ?1 AND name = ?2 AND id <> ?3)",
                params![workspace_id, name, except_id],
                |row| row.get(0),
            )
            .map_err(sqlite_store_error)
    }

    /// How many experiments other than `except_id` are `running`.
    pub fn count_running(&self, workspace_id: &str, except_id: &str) -> Result<u32, OrbitError> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM experiments \
                 WHERE workspace_id = ?1 AND status = 'running' AND id <> ?2",
                params![workspace_id, except_id],
                |row| row.get(0),
            )
            .map_err(sqlite_store_error)
    }

    /// The experiment whose id, or else whose name, is `id_or_name`, with its
    /// arms and events.
    pub fn get(
        &self,
        workspace_id: &str,
        id_or_name: &str,
    ) -> Result<Option<ExperimentRecord>, OrbitError> {
        let experiment = self
            .conn
            .query_row(
                &format!(
                    "SELECT {EXPERIMENT_COLUMNS} FROM experiments \
                     WHERE workspace_id = ?1 AND (id = ?2 OR name = ?2) \
                     ORDER BY id = ?2 DESC LIMIT 1"
                ),
                params![workspace_id, id_or_name],
                hydrate_experiment,
            )
            .optional()
            .map_err(sqlite_store_error)?;
        let Some(experiment) = experiment else {
            return Ok(None);
        };
        let arms = load_arms(self.conn, &experiment)?;
        let events = load_events(self.conn, &experiment)?;
        Ok(Some(ExperimentRecord {
            experiment,
            arms,
            events,
        }))
    }

    /// The workspace's experiments with their arms, oldest first, optionally
    /// only those in `status`. Events are not loaded.
    pub fn list(
        &self,
        workspace_id: &str,
        status: Option<&str>,
    ) -> Result<Vec<ExperimentRecord>, OrbitError> {
        let mut statement = self
            .conn
            .prepare(&format!(
                "SELECT {EXPERIMENT_COLUMNS} FROM experiments \
                 WHERE workspace_id = ?1 AND (?2 IS NULL OR status = ?2) \
                 ORDER BY created_at, id"
            ))
            .map_err(sqlite_store_error)?;
        let experiments = statement
            .query_map(params![workspace_id, status], hydrate_experiment)
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)?;
        experiments
            .into_iter()
            .map(|experiment| {
                let arms = load_arms(self.conn, &experiment)?;
                Ok(ExperimentRecord {
                    experiment,
                    arms,
                    events: Vec::new(),
                })
            })
            .collect()
    }
}

/// Concrete experiment-assignment repository scoped to a [`Snapshot`]
/// (read-only) or a [`Tx`] (read-write).
pub struct ExperimentAssignmentRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> ExperimentAssignmentRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> ExperimentAssignmentRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Insert `assignment` for its experiment and task. A stored row for the
    /// same experiment and task is replaced only by a forced arm assignment
    /// over an exclusion. Returns whether a row was written.
    pub fn put(
        &self,
        workspace_id: &str,
        assignment: &ExperimentAssignment,
    ) -> Result<bool, OrbitError> {
        let written = self
            .conn
            .execute(
                "INSERT INTO experiment_assignments (workspace_id, experiment_id, task_id, arm, \
                 segment, overlay_digest, assigned_at, forced, exclusion_reason) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 ON CONFLICT (workspace_id, experiment_id, task_id) DO UPDATE SET \
                 arm = excluded.arm, segment = excluded.segment, \
                 overlay_digest = excluded.overlay_digest, assigned_at = excluded.assigned_at, \
                 forced = excluded.forced, exclusion_reason = excluded.exclusion_reason \
                 WHERE experiment_assignments.arm IS NULL AND excluded.forced = 1 \
                 AND excluded.arm IS NOT NULL",
                params![
                    workspace_id,
                    assignment.experiment,
                    assignment.task_id,
                    assignment.arm,
                    assignment.segment,
                    assignment.digest,
                    stamp(assignment.assigned_at),
                    assignment.forced,
                    assignment.exclusion_reason,
                ],
            )
            .map_err(sqlite_store_error)?;
        Ok(written > 0)
    }

    /// Record that `task_id` had its experiment admission.
    pub fn put_admission(&self, workspace_id: &str, task_id: &str) -> Result<(), OrbitError> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO experiment_admissions (workspace_id, task_id) VALUES (?1, ?2)",
                params![workspace_id, task_id],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }
}

impl<Access> ExperimentAssignmentRepository<'_, Access> {
    /// Whether `task_id` already had its experiment admission.
    pub fn admitted(&self, workspace_id: &str, task_id: &str) -> Result<bool, OrbitError> {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM experiment_admissions \
                 WHERE workspace_id = ?1 AND task_id = ?2)",
                params![workspace_id, task_id],
                |row| row.get(0),
            )
            .map_err(sqlite_store_error)
    }

    /// All assignments and exclusions of one task, in experiment creation
    /// order.
    pub fn list_for_task(
        &self,
        workspace_id: &str,
        task_id: &str,
    ) -> Result<Vec<ExperimentAssignment>, OrbitError> {
        query_assignments(
            self.conn,
            "SELECT a.experiment_id, a.task_id, a.arm, a.segment, a.overlay_digest, \
             a.assigned_at, a.forced, a.exclusion_reason FROM experiment_assignments a \
             JOIN experiments e ON e.workspace_id = a.workspace_id AND e.id = a.experiment_id \
             WHERE a.workspace_id = ?1 AND a.task_id = ?2 ORDER BY e.created_at, e.id",
            workspace_id,
            task_id,
        )
    }

    /// Every assignment and exclusion of one experiment, oldest first.
    pub fn list_for_experiment(
        &self,
        workspace_id: &str,
        experiment_id: &str,
    ) -> Result<Vec<ExperimentAssignment>, OrbitError> {
        query_assignments(
            self.conn,
            "SELECT experiment_id, task_id, arm, segment, overlay_digest, assigned_at, forced, \
             exclusion_reason FROM experiment_assignments \
             WHERE workspace_id = ?1 AND experiment_id = ?2 ORDER BY assigned_at, task_id",
            workspace_id,
            experiment_id,
        )
    }
}

fn query_assignments(
    conn: &Connection,
    sql: &str,
    workspace_id: &str,
    key: &str,
) -> Result<Vec<ExperimentAssignment>, OrbitError> {
    let mut statement = conn.prepare(sql).map_err(sqlite_store_error)?;
    statement
        .query_map(params![workspace_id, key], |row| {
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

fn load_arms(
    conn: &Connection,
    experiment: &ExperimentRow,
) -> Result<Vec<ExperimentArmRow>, OrbitError> {
    let mut statement = conn
        .prepare(&format!(
            "SELECT {ARM_COLUMNS} FROM experiment_arms \
             WHERE workspace_id = ?1 AND experiment_id = ?2 ORDER BY position"
        ))
        .map_err(sqlite_store_error)?;
    let rows = statement
        .query_map(params![experiment.workspace_id, experiment.id], |row| {
            Ok(ExperimentArmRow {
                name: row.get(0)?,
                position: row.get(1)?,
                control: row.get(2)?,
                weight: row.get(3)?,
                overlay_json: row.get(4)?,
                overlay_digest: row.get(5)?,
                retired_segment: row.get(6)?,
            })
        })
        .map_err(sqlite_store_error)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
}

fn load_events(
    conn: &Connection,
    experiment: &ExperimentRow,
) -> Result<Vec<ExperimentEventRow>, OrbitError> {
    let mut statement = conn
        .prepare(&format!(
            "SELECT {EVENT_COLUMNS} FROM experiment_events \
             WHERE workspace_id = ?1 AND experiment_id = ?2 ORDER BY seq"
        ))
        .map_err(sqlite_store_error)?;
    let rows = statement
        .query_map(params![experiment.workspace_id, experiment.id], |row| {
            Ok(ExperimentEventRow {
                seq: row.get(0)?,
                kind: row.get(1)?,
                from_status: row.get(2)?,
                to_status: row.get(3)?,
                segment: row.get(4)?,
                actor: row.get(5)?,
                at: crate::parse_timestamp(&row.get::<_, String>(6)?)?,
                reason: row.get(7)?,
                detail_json: row.get(8)?,
            })
        })
        .map_err(sqlite_store_error)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
}

fn hydrate_experiment(row: &Row<'_>) -> rusqlite::Result<ExperimentRow> {
    Ok(ExperimentRow {
        workspace_id: row.get(0)?,
        id: row.get(1)?,
        name: row.get(2)?,
        hypothesis: row.get(3)?,
        population_json: row.get(4)?,
        primary_metric: row.get(5)?,
        guardrails_json: row.get(6)?,
        min_sample_per_arm: row.get(7)?,
        max_days: row.get(8)?,
        salt: row.get(9)?,
        mode: row.get(10)?,
        status: row.get(11)?,
        segment: row.get(12)?,
        concluded_arm: row.get(13)?,
        cloned_from: row.get(14)?,
        created_by: row.get(15)?,
        created_at: crate::parse_timestamp(&row.get::<_, String>(16)?)?,
        updated_at: crate::parse_timestamp(&row.get::<_, String>(17)?)?,
        revision: row.get(18)?,
    })
}
