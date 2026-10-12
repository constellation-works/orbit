//! The host admission lock over `job_runs`, `run_resource_minutes` and
//! `admission_reservations`.
//!
//! `job_runs` is read across every workspace: the workspace subquery lets
//! SQLite walk the per-workspace state and recency indexes instead of
//! scanning the table.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use orbit_common::OrbitError;
use rusqlite::{Connection, TransactionBehavior, params, params_from_iter};

use crate::Store;
use crate::contracts::{
    AdmissionDecision, AdmissionLoad, AdmissionLoadQuery, AdmissionLoadStoreBackend,
    AdmissionReservation, AdmissionRun, RunResourceMinute,
};

/// The run states that hold a delivery's work.
const LIVE_STATES: &str = "('pending','running','retrying')";

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn store_error(error: rusqlite::Error) -> OrbitError {
    OrbitError::Store(error.to_string())
}

fn unsigned(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

impl AdmissionLoadStoreBackend for Store {
    fn admit_under_host_lock(
        &self,
        query: &AdmissionLoadQuery<'_>,
        decide: AdmissionDecision<'_>,
    ) -> Result<Vec<AdmissionReservation>, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            conn.execute(
                "DELETE FROM admission_reservations WHERE reserved_at < ?1",
                params![stamp(query.reservations_expire_before)],
            )
            .map_err(store_error)?;
            let reservations = reservations(conn)?;
            let since = reservations.first().map(|first| first.reserved_at);
            let load = AdmissionLoad {
                runs: delivery_runs(conn, query.delivery_jobs, since)?,
                reservations,
            };
            let mut written = Vec::new();
            for reservation in decide(&load)? {
                conn.execute(
                    "INSERT INTO admission_reservations (
                        workspace_id, task_id, work_class, complexity, base_class, reserved_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        reservation.workspace_id,
                        reservation.task_id,
                        reservation.work_class,
                        reservation.complexity,
                        reservation.base_class,
                        stamp(query.now),
                    ],
                )
                .map_err(store_error)?;
                written.push(AdmissionReservation {
                    workspace_id: reservation.workspace_id,
                    task_id: reservation.task_id,
                    work_class: reservation.work_class,
                    complexity: reservation.complexity,
                    base_class: reservation.base_class,
                    reserved_at: query.now,
                });
            }
            Ok(written)
        })
    }
}

fn reservations(conn: &Connection) -> Result<Vec<AdmissionReservation>, OrbitError> {
    let mut statement = conn
        .prepare(
            "SELECT workspace_id, task_id, work_class, complexity, base_class, reserved_at
             FROM admission_reservations
             ORDER BY reserved_at, rowid",
        )
        .map_err(store_error)?;
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
        .map_err(store_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(store_error)
}

/// Live delivery runs of every workspace, and those created at or after
/// `since`, each with its newest resource minute.
fn delivery_runs(
    conn: &Connection,
    jobs: &[&str],
    since: Option<DateTime<Utc>>,
) -> Result<Vec<AdmissionRun>, OrbitError> {
    if jobs.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; jobs.len()].join(",");
    let select = format!(
        "SELECT workspace_id, run_id, job_id, state IN {LIVE_STATES}, created_at, input_json
         FROM job_runs
         WHERE workspace_id IN (SELECT DISTINCT workspace_id FROM job_runs)
           AND job_id IN ({placeholders})"
    );
    let mut runs: BTreeMap<(String, String), AdmissionRun> = BTreeMap::new();
    let mut collect = |sql: String, extra: Option<String>| -> Result<(), OrbitError> {
        let mut statement = conn.prepare(&sql).map_err(store_error)?;
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
                    newest_minute: None,
                })
            })
            .map_err(store_error)?;
        for run in rows {
            let run = run.map_err(store_error)?;
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
    let mut newest = conn
        .prepare(
            "SELECT workspace_id, run_id, minute_index, minute_started_at, task_id,
                    work_class, complexity, resumed, delivery_admitted_at,
                    cpu_cores, memory_bytes, samples
             FROM run_resource_minutes
             WHERE workspace_id = ?1 AND run_id = ?2
             ORDER BY minute_index DESC
             LIMIT 1",
        )
        .map_err(store_error)?;
    for run in runs.iter_mut().filter(|run| run.live) {
        let mut rows = newest
            .query_map(params![run.workspace_id, run.run_id], |row| {
                Ok(RunResourceMinute {
                    workspace_id: row.get(0)?,
                    run_id: row.get(1)?,
                    minute_index: row.get(2)?,
                    minute_started_at: crate::parse_timestamp(&row.get::<_, String>(3)?)?,
                    task_id: row.get(4)?,
                    work_class: row.get(5)?,
                    complexity: row.get(6)?,
                    resumed: row.get(7)?,
                    delivery_admitted_at: row
                        .get::<_, Option<String>>(8)?
                        .map(|at| crate::parse_timestamp(&at))
                        .transpose()?,
                    cpu_cores: row.get(9)?,
                    memory_bytes: unsigned(row.get(10)?),
                    samples: row.get(11)?,
                })
            })
            .map_err(store_error)?;
        run.newest_minute = rows.next().transpose().map_err(store_error)?;
    }
    runs.sort_by(|a, b| {
        (a.created_at, &a.workspace_id, &a.run_id).cmp(&(b.created_at, &b.workspace_id, &b.run_id))
    });
    Ok(runs)
}
