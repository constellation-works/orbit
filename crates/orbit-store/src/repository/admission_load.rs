//! Concrete repository for host admission load (`admission_reservations`,
//! cross-workspace live delivery runs in `job_runs`, `host_resource_minutes`,
//! and `run_resource_minutes`).
//!
//! Reached only through [`Snapshot::admission_load`] (read-only) and
//! [`Tx::admission_load`] (read-write inside [`crate::Store::write`]).
//! Admission policy, forecast construction, and per-task reservation
//! replacement rules belong to the calling operation; this repository owns SQL
//! queries and row mapping only.

use std::collections::BTreeMap;
use std::marker::PhantomData;

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, params, params_from_iter};

use crate::contracts::{
    AdmissionReservation, AdmissionRun, AdmissionRunMinute, HostResourceMinute,
    NewAdmissionReservation,
};
use crate::driver::sqlite::codec::{
    RUN_MINUTE_COLUMNS, host_minutes, run_minute_from_row, stamp, unsigned,
};
use crate::store::{Snapshot, Tx};

/// The run states that hold a delivery's work.
const LIVE_STATES: &str = "('pending','running','retrying')";

/// Concrete admission-load repository scoped to a [`Snapshot`] (read-only) or a
/// [`Tx`] (read-write).
pub struct AdmissionLoadRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> AdmissionLoadRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> AdmissionLoadRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Delete every admission reservation stamped strictly before `before`.
    pub fn delete_expired_reservations(&self, before: DateTime<Utc>) -> Result<(), OrbitError> {
        self.conn
            .execute(
                "DELETE FROM admission_reservations WHERE reserved_at < ?1",
                params![stamp(before)],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }

    /// Delete all admission reservations held for `(workspace_id, task_id)`.
    pub fn delete_task_reservations(
        &self,
        workspace_id: &str,
        task_id: &str,
    ) -> Result<(), OrbitError> {
        self.conn
            .execute(
                "DELETE FROM admission_reservations WHERE workspace_id = ?1 AND task_id = ?2",
                params![workspace_id, task_id],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }

    /// Insert one admission reservation stamped `reserved_at` and return the
    /// stored row.
    pub fn put_reservation(
        &self,
        reservation: &NewAdmissionReservation,
        reserved_at: DateTime<Utc>,
    ) -> Result<AdmissionReservation, OrbitError> {
        self.conn
            .execute(
                "INSERT INTO admission_reservations (
                    workspace_id, task_id, work_class, complexity, base_class, reserved_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    reservation.workspace_id,
                    reservation.task_id,
                    reservation.work_class,
                    reservation.complexity,
                    reservation.base_class,
                    stamp(reserved_at),
                ],
            )
            .map_err(sqlite_store_error)?;
        Ok(AdmissionReservation {
            workspace_id: reservation.workspace_id.clone(),
            task_id: reservation.task_id.clone(),
            work_class: reservation.work_class.clone(),
            complexity: reservation.complexity.clone(),
            base_class: reservation.base_class.clone(),
            reserved_at,
        })
    }
}

impl<Access> AdmissionLoadRepository<'_, Access> {
    /// Every stored admission reservation, oldest first.
    pub fn reservations(&self) -> Result<Vec<AdmissionReservation>, OrbitError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT workspace_id, task_id, work_class, complexity, base_class, reserved_at
                 FROM admission_reservations
                 ORDER BY reserved_at, rowid",
            )
            .map_err(sqlite_store_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(AdmissionReservation {
                    workspace_id: row.get(0)?,
                    task_id: row.get(1)?,
                    work_class: row.get(2)?,
                    complexity: row.get(3)?,
                    base_class: row.get(4)?,
                    reserved_at: crate::parse_timestamp(&row.get::<_, String>(5)?)?,
                })
            })
            .map_err(sqlite_store_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    /// Live delivery runs of every workspace, and those created at or after
    /// `since`, each with its newest resource minute.
    ///
    /// `job_runs` is read across every workspace: the workspace subquery lets
    /// SQLite walk the per-workspace state and recency indexes instead of
    /// scanning the table.
    pub fn delivery_runs(
        &self,
        jobs: &[&str],
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<AdmissionRun>, OrbitError> {
        if jobs.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; jobs.len()].join(",");
        let select = format!(
            "SELECT workspace_id, run_id, job_id, state IN {LIVE_STATES}, created_at, input_json,
                    started_at
             FROM job_runs
             WHERE workspace_id IN (SELECT DISTINCT workspace_id FROM job_runs)
               AND job_id IN ({placeholders})"
        );
        let mut runs: BTreeMap<(String, String), AdmissionRun> = BTreeMap::new();
        let mut collect = |sql: String, extra: Option<String>| -> Result<(), OrbitError> {
            let mut statement = self.conn.prepare(&sql).map_err(sqlite_store_error)?;
            let values = jobs
                .iter()
                .map(|job| (*job).to_string())
                .chain(extra)
                .collect::<Vec<_>>();
            let rows = statement
                .query_map(params_from_iter(values), |row| {
                    Ok(AdmissionRun {
                        workspace_id: row.get(0)?,
                        run_id: row.get(1)?,
                        job_id: row.get(2)?,
                        live: row.get(3)?,
                        created_at: crate::parse_timestamp(&row.get::<_, String>(4)?)?,
                        input: row
                            .get::<_, Option<String>>(5)?
                            .and_then(|raw| serde_json::from_str(&raw).ok()),
                        started_at: row
                            .get::<_, Option<String>>(6)?
                            .map(|at| crate::parse_timestamp(&at))
                            .transpose()?,
                        newest_minute: None,
                    })
                })
                .map_err(sqlite_store_error)?;
            for run in rows {
                let run = run.map_err(sqlite_store_error)?;
                runs.insert((run.workspace_id.clone(), run.run_id.clone()), run);
            }
            Ok(())
        };
        collect(format!("{select} AND state IN {LIVE_STATES}"), None)?;
        if let Some(since) = since {
            // `created_at` is variable-width RFC3339 text, so the text bound is a
            // second early and the exact bound is applied to the parsed time.
            collect(
                format!(
                    "{select} AND created_at >= ?{}",
                    jobs.len().saturating_add(1)
                ),
                Some((since - Duration::seconds(1)).to_rfc3339()),
            )?;
        }
        let mut runs: Vec<AdmissionRun> = runs
            .into_values()
            .filter(|run| run.live || since.is_some_and(|since| run.created_at >= since))
            .collect();
        let mut newest = self
            .conn
            .prepare(&format!(
                "SELECT {RUN_MINUTE_COLUMNS}
                 FROM run_resource_minutes
                 WHERE workspace_id = ?1 AND run_id = ?2
                 ORDER BY minute_index DESC
                 LIMIT 1",
            ))
            .map_err(sqlite_store_error)?;
        for run in runs.iter_mut().filter(|run| run.live) {
            let mut rows = newest
                .query_map(params![run.workspace_id, run.run_id], run_minute_from_row)
                .map_err(sqlite_store_error)?;
            run.newest_minute = rows.next().transpose().map_err(sqlite_store_error)?;
        }
        runs.sort_by(|a, b| {
            (a.created_at, &a.workspace_id, &a.run_id).cmp(&(
                b.created_at,
                &b.workspace_id,
                &b.run_id,
            ))
        });
        Ok(runs)
    }

    /// The host's recorded minutes starting at or after `since`, oldest first.
    pub fn host_minutes(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<HostResourceMinute>, OrbitError> {
        host_minutes(self.conn, since)
    }

    /// Every run's minutes starting at or after `since`, with the run's
    /// `finished_at`, oldest first.
    pub fn run_minutes(&self, since: DateTime<Utc>) -> Result<Vec<AdmissionRunMinute>, OrbitError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT m.workspace_id, m.run_id, m.minute_started_at, m.cpu_cores,
                        m.memory_bytes, m.samples, r.finished_at
                 FROM run_resource_minutes m
                 LEFT JOIN job_runs r ON r.workspace_id = m.workspace_id AND r.run_id = m.run_id
                 WHERE m.minute_started_at >= ?1
                 ORDER BY m.minute_started_at, m.workspace_id, m.run_id",
            )
            .map_err(sqlite_store_error)?;
        let rows = statement
            .query_map(params![stamp(since)], |row| {
                Ok(AdmissionRunMinute {
                    workspace_id: row.get(0)?,
                    run_id: row.get(1)?,
                    minute_started_at: crate::parse_timestamp(&row.get::<_, String>(2)?)?,
                    cpu_cores: row.get(3)?,
                    memory_bytes: unsigned(row.get(4)?),
                    samples: row.get(5)?,
                    run_finished_at: row
                        .get::<_, Option<String>>(6)?
                        .map(|at| crate::parse_timestamp(&at))
                        .transpose()?,
                })
            })
            .map_err(sqlite_store_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }
}
