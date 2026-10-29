//! Per-minute run and host resource history in `run_resource_minutes` and
//! `host_resource_minutes`, and admission pass forecasts in
//! `admission_forecasts`.
//!
//! Timestamps are stored as fixed-width UTC RFC3339 text (microseconds, `Z`),
//! so SQLite orders and compares them as text.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{TransactionBehavior, params, params_from_iter};

use crate::Store;
use crate::contracts::{
    AdmissionForecastPoint, AdmissionForecastRecord, CPU_ACCOUNTING_LINUX, CPU_ACCOUNTING_MACOS,
    CPU_ACCOUNTING_MACOS_TREE_ONLY, DeliveryRunInterval, DeliveryRunResources,
    DeliveryRunsFreshness, HostResourceMinute, ResourceHistoryKeep, ResourceMinuteLoad,
    ResourceMinutesTable, ResourceUsageStoreBackend, RunResourceMinute,
};

use super::codec::{RUN_MINUTE_COLUMNS, host_minutes, run_minute_from_row, stamp, unsigned};

/// SQLite integers are signed; a byte count past `i64::MAX` saturates.
fn bytes(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn encode<T: serde::Serialize>(value: &T, what: &str) -> Result<String, OrbitError> {
    serde_json::to_string(value)
        .map_err(|error| OrbitError::Store(format!("encode {what}: {error}")))
}

/// A JSON column read back; a value this binary cannot read is a
/// conversion failure of that column.
fn decode<T: serde::de::DeserializeOwned>(index: usize, raw: &str) -> rusqlite::Result<T> {
    serde_json::from_str(raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, error.into())
    })
}

impl ResourceUsageStoreBackend for Store {
    fn record_run_resource_minute(&self, minute: &RunResourceMinute) -> Result<(), OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            tx.connection()
                .execute(
                    r#"
                    INSERT INTO run_resource_minutes (
                        workspace_id, run_id, minute_index, minute_started_at, task_id,
                        work_class, complexity, resumed, delivery_admitted_at,
                        cpu_cores, memory_bytes, samples, cpu_accounting,
                        crew_model, orbit_version
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                    ON CONFLICT (workspace_id, run_id, minute_index) DO UPDATE SET
                        minute_started_at = excluded.minute_started_at,
                        task_id = excluded.task_id,
                        work_class = excluded.work_class,
                        complexity = excluded.complexity,
                        resumed = excluded.resumed,
                        delivery_admitted_at = excluded.delivery_admitted_at,
                        cpu_cores = excluded.cpu_cores,
                        memory_bytes = excluded.memory_bytes,
                        samples = excluded.samples,
                        cpu_accounting = excluded.cpu_accounting,
                        crew_model = excluded.crew_model,
                        orbit_version = excluded.orbit_version
                    "#,
                    params![
                        minute.workspace_id,
                        minute.run_id,
                        minute.minute_index,
                        stamp(minute.minute_started_at),
                        minute.task_id,
                        minute.work_class,
                        minute.complexity,
                        minute.resumed,
                        minute.delivery_admitted_at.map(stamp),
                        minute.cpu_cores,
                        bytes(minute.memory_bytes),
                        minute.samples,
                        minute.cpu_accounting,
                        minute.crew_model,
                        minute.orbit_version,
                    ],
                )
                .map(|_| ())
                .map_err(sqlite_store_error)
        })
    }

    fn offer_host_resource_minute(&self, minute: &HostResourceMinute) -> Result<bool, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            tx.connection()
                .execute(
                    r#"
                    INSERT INTO host_resource_minutes (
                        minute_at, cpu_cores, cpu_count, memory_bytes,
                        memory_total_bytes, samples, recorded_by_run_id
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                    ON CONFLICT (minute_at) DO NOTHING
                    "#,
                    params![
                        stamp(minute.minute_at),
                        minute.cpu_cores,
                        minute.cpu_count,
                        bytes(minute.memory_bytes),
                        bytes(minute.memory_total_bytes),
                        minute.samples,
                        minute.recorded_by_run_id,
                    ],
                )
                .map(|inserted| inserted > 0)
                .map_err(sqlite_store_error)
        })
    }

    fn run_resource_minutes(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Vec<RunResourceMinute>, OrbitError> {
        let conn = self.read()?;
        let mut statement = conn
            .prepare(&format!(
                "SELECT {RUN_MINUTE_COLUMNS}
                 FROM run_resource_minutes
                 WHERE workspace_id = ?1 AND run_id = ?2
                 ORDER BY minute_index",
            ))
            .map_err(sqlite_store_error)?;
        let rows = statement
            .query_map(params![workspace_id, run_id], run_minute_from_row)
            .map_err(sqlite_store_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    fn delivery_run_resources(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<DeliveryRunResources>, OrbitError> {
        let conn = self.read()?;
        let mut statement = conn
            .prepare(
                r#"
                SELECT r.workspace_id, r.run_id, j.job_id, r.task_id, r.work_class,
                       r.complexity, r.resumed, r.delivery_admitted_at, j.finished_at,
                       r.minute_started_at, r.cpu_cores, r.memory_bytes, r.cpu_accounting, r.samples,
                       COALESCE(r.crew_model, j.crew_model), r.orbit_version
                FROM run_resource_minutes r
                LEFT JOIN job_runs j
                  ON j.workspace_id = r.workspace_id AND j.run_id = r.run_id
                WHERE r.delivery_admitted_at >= ?1
                ORDER BY r.workspace_id, r.run_id, r.minute_index
                "#,
            )
            .map_err(sqlite_store_error)?;
        let rows = statement
            .query_map(params![stamp(since)], |row| {
                let run = DeliveryRunResources {
                    workspace_id: row.get(0)?,
                    run_id: row.get(1)?,
                    job_id: row.get(2)?,
                    task_id: row.get(3)?,
                    work_class: row.get(4)?,
                    complexity: row.get(5)?,
                    resumed: row.get(6)?,
                    delivery_admitted_at: crate::parse_timestamp(&row.get::<_, String>(7)?)?,
                    finished_at: row
                        .get::<_, Option<String>>(8)?
                        .map(|at| crate::parse_timestamp(&at))
                        .transpose()?,
                    crew_model: row.get(14)?,
                    orbit_version: row.get(15)?,
                    minutes: Vec::new(),
                };
                let minute = ResourceMinuteLoad {
                    minute_started_at: crate::parse_timestamp(&row.get::<_, String>(9)?)?,
                    cpu_cores: row.get(10)?,
                    memory_bytes: unsigned(row.get(11)?),
                    cpu_accounting: row.get(12)?,
                    samples: row.get(13)?,
                };
                Ok((run, minute))
            })
            .map_err(sqlite_store_error)?;
        let mut runs: Vec<DeliveryRunResources> = Vec::new();
        for row in rows {
            let (run, minute) = row.map_err(sqlite_store_error)?;
            match runs.last_mut() {
                Some(last)
                    if last.workspace_id == run.workspace_id && last.run_id == run.run_id =>
                {
                    last.resumed |= run.resumed;
                    // Minutes come oldest first: the newest that recorded a
                    // crew model or version names the run's.
                    if run.crew_model.is_some() {
                        last.crew_model = run.crew_model;
                    }
                    if run.orbit_version.is_some() {
                        last.orbit_version = run.orbit_version;
                    }
                    last.minutes.push(minute);
                }
                _ => {
                    runs.push(DeliveryRunResources {
                        minutes: vec![minute],
                        ..run
                    });
                }
            }
        }
        Ok(runs)
    }

    fn delivery_runs_freshness(&self) -> Result<DeliveryRunsFreshness, OrbitError> {
        let conn = self.read()?;
        // Every sampled run counts from its first recorded minute, whatever its
        // index: a failed first read or insert can leave no minute-zero row. The
        // probe reads only retained minute rows, never all of `job_runs`.
        let (newest, finished) = conn
            .query_row(
                r#"
                SELECT MAX(j.finished_at), COUNT(j.finished_at)
                FROM (SELECT DISTINCT workspace_id, run_id FROM run_resource_minutes) r
                JOIN job_runs j ON j.workspace_id = r.workspace_id AND j.run_id = r.run_id
                "#,
                [],
                |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?)),
            )
            .map_err(sqlite_store_error)?;
        Ok(DeliveryRunsFreshness {
            newest_finished_at: newest
                .map(|at| crate::parse_timestamp(&at))
                .transpose()
                .map_err(sqlite_store_error)?,
            finished_runs: unsigned(finished),
        })
    }

    fn host_resource_minutes(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<HostResourceMinute>, OrbitError> {
        host_minutes(&*self.read()?, since)
    }

    fn record_admission_forecast(
        &self,
        record: &AdmissionForecastRecord,
    ) -> Result<(), OrbitError> {
        let points = encode(&record.points, "an admission forecast")?;
        let admitted = encode(&record.admitted, "an admission forecast")?;
        let fit_admitted = encode(&record.fit_admitted, "an admission forecast")?;
        let fit_skipped = encode(&record.fit_skipped, "an admission forecast")?;
        let pull = record
            .pull
            .as_ref()
            .map(|value| encode(value, "an admission forecast"))
            .transpose()?;
        let covered = record
            .covered_deliveries
            .as_ref()
            .map(|value| encode(value, "an admission forecast"))
            .transpose()?;
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            tx.connection()
                .execute(
                    r#"
                    INSERT INTO admission_forecasts (
                        workspace_id, run_id, forecast_at, mode, cpu_count,
                        memory_total_bytes, points_json, admitted_json,
                        fit_admitted_json, fit_skipped_json, pull_decision_json,
                        baseline_cpu_cores, baseline_memory_bytes, covered_deliveries_json
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                    "#,
                    params![
                        record.workspace_id,
                        record.run_id,
                        stamp(record.forecast_at),
                        record.mode,
                        record.cpu_count,
                        record.memory_total_bytes.map(bytes),
                        points,
                        admitted,
                        fit_admitted,
                        fit_skipped,
                        pull,
                        record.baseline_cpu_cores,
                        record.baseline_memory_bytes.map(bytes),
                        covered,
                    ],
                )
                .map(|_| ())
                .map_err(sqlite_store_error)
        })
    }

    fn admission_forecasts(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<AdmissionForecastRecord>, OrbitError> {
        let conn = self.read()?;
        let mut statement = conn
            .prepare(
                r#"
                SELECT workspace_id, run_id, forecast_at, mode, cpu_count,
                       memory_total_bytes, points_json, admitted_json,
                       fit_admitted_json, fit_skipped_json, pull_decision_json,
                       baseline_cpu_cores, baseline_memory_bytes, covered_deliveries_json
                FROM admission_forecasts
                WHERE forecast_at >= ?1
                ORDER BY forecast_at, rowid
                "#,
            )
            .map_err(sqlite_store_error)?;
        let rows = statement
            .query_map(params![stamp(since)], |row| {
                Ok(AdmissionForecastRecord {
                    workspace_id: row.get(0)?,
                    run_id: row.get(1)?,
                    forecast_at: crate::parse_timestamp(&row.get::<_, String>(2)?)?,
                    mode: row.get(3)?,
                    cpu_count: row.get(4)?,
                    memory_total_bytes: row.get::<_, Option<i64>>(5)?.map(unsigned),
                    baseline_cpu_cores: row.get(11)?,
                    baseline_memory_bytes: row.get::<_, Option<i64>>(12)?.map(unsigned),
                    covered_deliveries: row
                        .get::<_, Option<String>>(13)?
                        .map(|value| decode(13, &value))
                        .transpose()?,
                    points: decode::<Vec<AdmissionForecastPoint>>(6, &row.get::<_, String>(6)?)?,
                    admitted: decode(7, &row.get::<_, String>(7)?)?,
                    fit_admitted: decode(8, &row.get::<_, String>(8)?)?,
                    fit_skipped: decode(9, &row.get::<_, String>(9)?)?,
                    pull: row
                        .get::<_, Option<String>>(10)?
                        .map(|value| decode(10, &value))
                        .transpose()?,
                })
            })
            .map_err(sqlite_store_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    fn resource_minutes_older_than(
        &self,
        table: ResourceMinutesTable,
        cutoff: DateTime<Utc>,
        keep: ResourceHistoryKeep,
    ) -> Result<u64, OrbitError> {
        let conn = self.read()?;
        let sql = format!(
            "SELECT COUNT(*) FROM {} WHERE {}",
            table.table_name(),
            prunable(table)
        );
        let (cutoff, kept) = prune_bounds(table, cutoff, keep);
        conn.query_row(&sql, params![cutoff, kept], |row| row.get::<_, i64>(0))
            .map(unsigned)
            .map_err(sqlite_store_error)
    }

    fn prune_resource_minutes_batch(
        &self,
        table: ResourceMinutesTable,
        cutoff: DateTime<Utc>,
        keep: ResourceHistoryKeep,
        limit: usize,
    ) -> Result<usize, OrbitError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        // Keep an ineligible marker until its good siblings have been pruned.
        // Otherwise the next batch can mistake a partial delivery for a profile.
        let order = if matches!(table, ResourceMinutesTable::Run) {
            format!(
                " ORDER BY CASE WHEN resumed != 0 OR \
                 (cpu_accounting IS NULL AND {}) OR \
                 (cpu_accounting IS NOT NULL AND cpu_accounting NOT IN ('{}', '{}', '{}')) OR \
                 NOT EXISTS (SELECT 1 FROM job_runs j WHERE j.workspace_id = r.workspace_id \
                     AND j.run_id = r.run_id AND j.job_id IS NOT NULL AND j.finished_at IS NOT NULL) \
                 THEN 1 ELSE 0 END, CASE WHEN minute_index = 0 THEN 1 ELSE 0 END",
                if cfg!(target_os = "linux") { 0 } else { 1 },
                CPU_ACCOUNTING_LINUX,
                CPU_ACCOUNTING_MACOS,
                CPU_ACCOUNTING_MACOS_TREE_ONLY
            )
        } else {
            String::new()
        };
        let sql = format!(
            "DELETE FROM {table} WHERE rowid IN ( \
                 SELECT r.rowid FROM {table} r WHERE {predicate}{order} LIMIT ?3)",
            table = table.table_name(),
            predicate = prunable(table)
        );
        let (cutoff, kept) = prune_bounds(table, cutoff, keep);
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            tx.connection()
                .execute(&sql, params![cutoff, kept, limit])
                .map_err(sqlite_store_error)
        })
    }
}

/// [`prunable`]'s `?1`, the cutoff, and `?2`, how many of the table's own
/// kind `keep` holds: deliveries per key for run minutes, forecasts for
/// forecasts and host minutes.
fn prune_bounds(
    table: ResourceMinutesTable,
    cutoff: DateTime<Utc>,
    keep: ResourceHistoryKeep,
) -> (String, i64) {
    let kept = match table {
        ResourceMinutesTable::Run => keep.deliveries_per_key,
        ResourceMinutesTable::Host | ResourceMinutesTable::AdmissionForecast => keep.forecasts,
    };
    (stamp(cutoff), i64::try_from(kept).unwrap_or(i64::MAX))
}

/// The rows of `table` retention may remove: older than the cutoff `?1` and
/// not among the `?2` that [`ResourceHistoryKeep`] holds.
///
/// - A run minute stays for the newest `?2` deliveries by admission, and
///   the newest `?2` profile-eligible deliveries by finish time, per key
///   and, for each crew model, per key and crew model. A delivery with
///   recent minutes keeps its older minutes too.
/// - A forecast stays while it is among the newest `?2`.
/// - A host minute stays from the minute the oldest of the newest `?2`
///   forecasts was made in, so every kept forecast can still pair with the
///   minutes it forecast.
fn prunable(table: ResourceMinutesTable) -> String {
    match table {
        ResourceMinutesTable::Run => {
            // One text key per delivery: SQLite builds a scalar `NOT IN`
            // list once, where a row-value one is rescanned per row.
            format!(
                "minute_started_at < ?1 AND (delivery_admitted_at IS NULL \
                 OR workspace_id || char(31) || task_id || char(31) || delivery_admitted_at \
                 NOT IN ( \
                     SELECT workspace_id || char(31) || task_id || char(31) || delivery_admitted_at \
                     FROM ( \
                         SELECT workspace_id, task_id, delivery_admitted_at, \
                                ROW_NUMBER() OVER ( \
                                    PARTITION BY workspace_id, work_class, IFNULL(complexity, '') \
                                    ORDER BY delivery_admitted_at DESC, task_id DESC) AS newest \
                         FROM (SELECT DISTINCT workspace_id, task_id, delivery_admitted_at, \
                                      work_class, complexity \
                               FROM run_resource_minutes \
                               WHERE delivery_admitted_at IS NOT NULL)) \
                     WHERE newest <= ?2) \
                 AND workspace_id || char(31) || task_id || char(31) || delivery_admitted_at \
                 NOT IN ( \
                     SELECT workspace_id || char(31) || task_id || char(31) || delivery_admitted_at \
                     FROM (SELECT *, ROW_NUMBER() OVER ( \
                         PARTITION BY workspace_id, work_class, \
                             CASE WHEN substr(work_class, 1, 10) = 'auto-task:' THEN '' ELSE IFNULL(complexity, '') END \
                         ORDER BY finished DESC, delivery_admitted_at DESC, task_id DESC) AS newest \
                         FROM (SELECT r.workspace_id, r.task_id, r.delivery_admitted_at, \
                                      MIN(r.work_class) AS work_class, MIN(r.complexity) AS complexity, \
                                      MAX(julianday(j.finished_at)) AS finished \
                               FROM run_resource_minutes r LEFT JOIN job_runs j \
                                 ON j.workspace_id = r.workspace_id AND j.run_id = r.run_id \
                               WHERE r.delivery_admitted_at IS NOT NULL \
                               GROUP BY r.workspace_id, r.task_id, r.delivery_admitted_at \
                               HAVING MAX(CASE WHEN r.resumed != 0 OR j.job_id IS NULL \
                                   OR j.finished_at IS NULL OR \
                                   (r.cpu_accounting IS NULL AND {null_untrusted}) OR \
                                   (r.cpu_accounting IS NOT NULL AND r.cpu_accounting NOT IN \
                                       ('{linux}', '{macos}', '{macos_tree}')) \
                                   THEN 1 ELSE 0 END) = 0)) \
                     WHERE newest <= ?2) \
                 AND workspace_id || char(31) || task_id || char(31) || delivery_admitted_at \
                 NOT IN ( \
                     WITH eligible AS ( \
                         SELECT r.workspace_id, r.task_id, r.delivery_admitted_at, \
                             MIN(r.work_class) AS work_class, \
                             CASE WHEN substr(MIN(r.work_class), 1, 10) = 'auto-task:' \
                                  THEN '' ELSE IFNULL(MIN(r.complexity), '') END AS complexity, \
                             MAX(julianday(j.finished_at)) AS finished, \
                             (SELECT (SELECT COALESCE(m.crew_model, jj.crew_model) FROM run_resource_minutes m \
                                  WHERE m.workspace_id = rr.workspace_id AND m.run_id = rr.run_id \
                                    AND (m.crew_model IS NOT NULL OR jj.crew_model IS NOT NULL) \
                                  ORDER BY m.minute_index DESC LIMIT 1) AS model \
                              FROM run_resource_minutes rr LEFT JOIN job_runs jj \
                                ON jj.workspace_id = rr.workspace_id AND jj.run_id = rr.run_id \
                              WHERE rr.workspace_id = r.workspace_id AND rr.task_id = r.task_id \
                                AND rr.delivery_admitted_at = r.delivery_admitted_at \
                              GROUP BY rr.run_id HAVING model IS NOT NULL ORDER BY rr.run_id LIMIT 1) AS model \
                         FROM run_resource_minutes r LEFT JOIN job_runs j \
                           ON j.workspace_id = r.workspace_id AND j.run_id = r.run_id \
                         WHERE r.delivery_admitted_at IS NOT NULL \
                         GROUP BY r.workspace_id, r.task_id, r.delivery_admitted_at \
                         HAVING MAX(CASE WHEN r.resumed != 0 OR j.job_id IS NULL \
                             OR j.finished_at IS NULL OR (r.cpu_accounting IS NULL AND {null_untrusted}) OR \
                             (r.cpu_accounting IS NOT NULL AND r.cpu_accounting NOT IN \
                                 ('{linux}', '{macos}', '{macos_tree}')) THEN 1 ELSE 0 END) = 0), \
                     ranked AS (SELECT *, \
                         ROW_NUMBER() OVER (PARTITION BY workspace_id, work_class, complexity, model \
                             ORDER BY finished DESC, delivery_admitted_at DESC, task_id DESC) AS newest \
                         FROM eligible WHERE model IS NOT NULL) \
                     SELECT workspace_id || char(31) || task_id || char(31) || delivery_admitted_at \
                     FROM ranked WHERE newest <= ?2) \
                 AND workspace_id || char(31) || task_id || char(31) || delivery_admitted_at \
                 NOT IN (SELECT workspace_id || char(31) || task_id || char(31) || delivery_admitted_at \
                     FROM run_resource_minutes WHERE delivery_admitted_at IS NOT NULL \
                     AND minute_started_at >= ?1))",
                null_untrusted = if cfg!(target_os = "linux") { 0 } else { 1 },
                linux = CPU_ACCOUNTING_LINUX,
                macos = CPU_ACCOUNTING_MACOS,
                macos_tree = CPU_ACCOUNTING_MACOS_TREE_ONLY
            )
        }
        ResourceMinutesTable::Host => "minute_at < ?1 AND minute_at < IFNULL(( \
                 SELECT substr(MIN(forecast_at), 1, 17) || '00.000000Z' FROM ( \
                     SELECT forecast_at FROM admission_forecasts \
                     ORDER BY forecast_at DESC LIMIT ?2)), ?1)"
            .into(),
        ResourceMinutesTable::AdmissionForecast => "forecast_at < ?1 AND rowid NOT IN ( \
                 SELECT rowid FROM admission_forecasts \
                 ORDER BY forecast_at DESC LIMIT ?2)"
            .into(),
    }
}

impl Store {
    /// Delivery workers of `job_ids` whose lifetime overlaps `[since, until)`.
    ///
    /// A worker is live from `started_at` until `finished_at`, and still live
    /// when `finished_at` is absent. Runs that never started are omitted.
    /// `job_runs` timestamps are variable-width RFC3339, so the overlap is
    /// applied after parsing rather than as a text comparison.
    pub fn delivery_runs_overlapping(
        &self,
        job_ids: &[&str],
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Result<Vec<DeliveryRunInterval>, OrbitError> {
        if job_ids.is_empty() || since >= until {
            return Ok(Vec::new());
        }
        let conn = self.read()?;
        let placeholders = vec!["?"; job_ids.len()].join(",");
        // The workspace subquery lets SQLite walk each workspace's job index
        // instead of scanning `job_runs` as one table. See admission load.
        let sql = format!(
            "SELECT started_at, finished_at
             FROM job_runs
             WHERE workspace_id IN (SELECT DISTINCT workspace_id FROM job_runs)
               AND job_id IN ({placeholders})
               AND started_at IS NOT NULL"
        );
        let mut statement = conn.prepare(&sql).map_err(sqlite_store_error)?;
        let values = job_ids
            .iter()
            .map(|job| (*job).to_string())
            .collect::<Vec<_>>();
        let rows = statement
            .query_map(params_from_iter(values), |row| {
                let started_at = crate::parse_timestamp(&row.get::<_, String>(0)?)?;
                let finished_at = row
                    .get::<_, Option<String>>(1)?
                    .map(|at| crate::parse_timestamp(&at))
                    .transpose()?;
                Ok(DeliveryRunInterval {
                    started_at,
                    finished_at,
                })
            })
            .map_err(sqlite_store_error)?;
        let mut intervals = Vec::new();
        for row in rows {
            let interval = row.map_err(sqlite_store_error)?;
            let overlaps = interval.started_at < until
                && interval.finished_at.is_none_or(|finished| finished > since);
            if overlaps {
                intervals.push(interval);
            }
        }
        Ok(intervals)
    }
}

impl crate::contracts::WorkerResourceLimitStoreBackend for Store {
    fn record_worker_resource_limit_evidence(
        &self,
        evidence: &crate::contracts::WorkerResourceLimitEvidence,
    ) -> Result<(), OrbitError> {
        let encoded = encode(evidence, "resource evidence")?;
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            tx.connection()
                .execute(
                    "INSERT INTO worker_resource_limits (workspace_id, run_id, evidence_json)
                 VALUES (?1, ?2, ?3) ON CONFLICT (workspace_id, run_id)
                 DO UPDATE SET evidence_json = excluded.evidence_json",
                    params![evidence.workspace_id, evidence.run_id, encoded],
                )
                .map(|_| ())
                .map_err(sqlite_store_error)
        })
    }

    fn worker_resource_limit_evidence(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Option<crate::contracts::WorkerResourceLimitEvidence>, OrbitError> {
        use rusqlite::OptionalExtension;
        self.read()?.query_row(
            "SELECT evidence_json FROM worker_resource_limits WHERE workspace_id = ?1 AND run_id = ?2",
            params![workspace_id, run_id],
            |row| decode(0, &row.get::<_, String>(0)?),
        ).optional().map_err(sqlite_store_error)
    }
}
