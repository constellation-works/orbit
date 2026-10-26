//! Experiment records in `experiments`, `experiment_arms` and
//! `experiment_events`.
//!
//! Every write is one immediate transaction that checks the experiment's
//! stored revision, so a lifecycle change computed from a stale read is
//! refused rather than applied over a newer one.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use crate::Store;
use crate::contracts::{
    ExperimentArmRow, ExperimentEventRow, ExperimentRecord, ExperimentRow, ExperimentStoreBackend,
    ExperimentWrite, ExperimentWriteOutcome,
};

use super::codec::stamp;

const EXPERIMENT_COLUMNS: &str = "workspace_id, id, name, hypothesis, population_json, \
     primary_metric, guardrails_json, min_sample_per_arm, max_days, salt, mode, status, \
     segment, concluded_arm, cloned_from, created_by, created_at, updated_at, revision";

const ARM_COLUMNS: &str =
    "name, position, control, weight, overlay_json, overlay_digest, retired_segment";

const EVENT_COLUMNS: &str =
    "seq, kind, from_status, to_status, segment, actor, at, reason, detail_json";

impl ExperimentStoreBackend for Store {
    fn write_experiment(
        &self,
        write: &ExperimentWrite,
    ) -> Result<ExperimentWriteOutcome, OrbitError> {
        let experiment = &write.experiment;
        if write.event.seq != experiment.revision {
            return Err(OrbitError::Store(format!(
                "experiment event seq {} does not match revision {}",
                write.event.seq, experiment.revision
            )));
        }
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let stored: Option<u32> = conn
                .query_row(
                    "SELECT revision FROM experiments WHERE workspace_id = ?1 AND id = ?2",
                    params![experiment.workspace_id, experiment.id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sqlite_store_error)?;
            if stored != write.expected_revision {
                return Ok(ExperimentWriteOutcome::Stale);
            }
            let name_taken: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM experiments \
                     WHERE workspace_id = ?1 AND name = ?2 AND id <> ?3)",
                    params![experiment.workspace_id, experiment.name, experiment.id],
                    |row| row.get(0),
                )
                .map_err(sqlite_store_error)?;
            if name_taken {
                return Ok(ExperimentWriteOutcome::NameTaken);
            }
            if let Some(limit) = write.running_limit {
                let running: u32 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM experiments \
                         WHERE workspace_id = ?1 AND status = 'running' AND id <> ?2",
                        params![experiment.workspace_id, experiment.id],
                        |row| row.get(0),
                    )
                    .map_err(sqlite_store_error)?;
                if running >= limit {
                    return Ok(ExperimentWriteOutcome::RunningLimitReached { running });
                }
            }
            upsert_experiment(conn, experiment)?;
            if let Some(arms) = &write.arms {
                replace_arms(conn, experiment, arms)?;
            }
            insert_event(conn, experiment, &write.event)?;
            Ok(ExperimentWriteOutcome::Written)
        })
    }

    fn experiment_record(
        &self,
        workspace_id: &str,
        id_or_name: &str,
    ) -> Result<Option<ExperimentRecord>, OrbitError> {
        let conn = self.read()?;
        let experiment = conn
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
        let arms = load_arms(&conn, &experiment)?;
        let events = load_events(&conn, &experiment)?;
        Ok(Some(ExperimentRecord {
            experiment,
            arms,
            events,
        }))
    }

    fn experiment_records(
        &self,
        workspace_id: &str,
        status: Option<&str>,
    ) -> Result<Vec<ExperimentRecord>, OrbitError> {
        let conn = self.read()?;
        let mut statement = conn
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
                let arms = load_arms(&conn, &experiment)?;
                Ok(ExperimentRecord {
                    experiment,
                    arms,
                    events: Vec::new(),
                })
            })
            .collect()
    }
}

fn upsert_experiment(conn: &Connection, experiment: &ExperimentRow) -> Result<(), OrbitError> {
    conn.execute(
        &format!(
            "INSERT INTO experiments ({EXPERIMENT_COLUMNS}) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
                     ?17, ?18, ?19) \
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

fn replace_arms(
    conn: &Connection,
    experiment: &ExperimentRow,
    arms: &[ExperimentArmRow],
) -> Result<(), OrbitError> {
    conn.execute(
        "DELETE FROM experiment_arms WHERE workspace_id = ?1 AND experiment_id = ?2",
        params![experiment.workspace_id, experiment.id],
    )
    .map_err(sqlite_store_error)?;
    let mut insert = conn
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

fn insert_event(
    conn: &Connection,
    experiment: &ExperimentRow,
    event: &ExperimentEventRow,
) -> Result<(), OrbitError> {
    conn.execute(
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
