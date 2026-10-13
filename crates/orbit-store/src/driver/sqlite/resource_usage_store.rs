//! Per-minute run and host resource history in `run_resource_minutes` and
//! `host_resource_minutes`, and admission pass forecasts in
//! `admission_forecasts`.
//!
//! Timestamps are stored as fixed-width UTC RFC3339 text (microseconds, `Z`),
//! so SQLite orders and compares them as text.

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{TransactionBehavior, params};

use crate::Store;
use crate::contracts::{
    AdmissionForecastPoint, AdmissionForecastRecord, DeliveryRunResources, DeliveryRunsFreshness,
    HostResourceMinute, ResourceMinuteLoad, ResourceMinutesTable, ResourceUsageStoreBackend,
    RunResourceMinute,
};

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// SQLite integers are signed; a byte count past `i64::MAX` saturates.
fn bytes(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn unsigned(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, OrbitError> {
    serde_json::to_string(value)
        .map_err(|error| OrbitError::Store(format!("encode an admission forecast: {error}")))
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
                        cpu_cores, memory_bytes, samples, cpu_accounting
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
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
                        cpu_accounting = excluded.cpu_accounting
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
            .prepare(
                r#"
                SELECT workspace_id, run_id, minute_index, minute_started_at, task_id,
                       work_class, complexity, resumed, delivery_admitted_at,
                       cpu_cores, memory_bytes, samples, cpu_accounting
                FROM run_resource_minutes
                WHERE workspace_id = ?1 AND run_id = ?2
                ORDER BY minute_index
                "#,
            )
            .map_err(sqlite_store_error)?;
        let rows = statement
            .query_map(params![workspace_id, run_id], |row| {
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
                    cpu_accounting: row.get(12)?,
                })
            })
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
                       r.minute_started_at, r.cpu_cores, r.memory_bytes, r.cpu_accounting
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
                    minutes: Vec::new(),
                };
                let minute = ResourceMinuteLoad {
                    minute_started_at: crate::parse_timestamp(&row.get::<_, String>(9)?)?,
                    cpu_cores: row.get(10)?,
                    memory_bytes: unsigned(row.get(11)?),
                    cpu_accounting: row.get(12)?,
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
        let conn = self.read()?;
        let mut statement = conn
            .prepare(
                r#"
                SELECT minute_at, cpu_cores, cpu_count, memory_bytes, memory_total_bytes,
                       samples, recorded_by_run_id
                FROM host_resource_minutes
                WHERE minute_at >= ?1
                ORDER BY minute_at
                "#,
            )
            .map_err(sqlite_store_error)?;
        let rows = statement
            .query_map(params![stamp(since)], |row| {
                Ok(HostResourceMinute {
                    minute_at: crate::parse_timestamp(&row.get::<_, String>(0)?)?,
                    cpu_cores: row.get(1)?,
                    cpu_count: row.get(2)?,
                    memory_bytes: unsigned(row.get(3)?),
                    memory_total_bytes: unsigned(row.get(4)?),
                    samples: row.get(5)?,
                    recorded_by_run_id: row.get(6)?,
                })
            })
            .map_err(sqlite_store_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    fn record_admission_forecast(
        &self,
        record: &AdmissionForecastRecord,
    ) -> Result<(), OrbitError> {
        let points = encode(&record.points)?;
        let admitted = encode(&record.admitted)?;
        let fit_admitted = encode(&record.fit_admitted)?;
        let fit_skipped = encode(&record.fit_skipped)?;
        let pull = record.pull.as_ref().map(encode).transpose()?;
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            tx.connection()
                .execute(
                    r#"
                    INSERT INTO admission_forecasts (
                        workspace_id, run_id, forecast_at, mode, cpu_count,
                        memory_total_bytes, points_json, admitted_json,
                        fit_admitted_json, fit_skipped_json, pull_decision_json
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
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
                       fit_admitted_json, fit_skipped_json, pull_decision_json
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
    ) -> Result<u64, OrbitError> {
        let conn = self.read()?;
        let sql = match table {
            ResourceMinutesTable::Run => {
                "SELECT COUNT(*) FROM run_resource_minutes WHERE minute_started_at < ?1"
            }
            ResourceMinutesTable::Host => {
                "SELECT COUNT(*) FROM host_resource_minutes WHERE minute_at < ?1"
            }
            ResourceMinutesTable::AdmissionForecast => {
                "SELECT COUNT(*) FROM admission_forecasts WHERE forecast_at < ?1"
            }
        };
        conn.query_row(sql, params![stamp(cutoff)], |row| row.get::<_, i64>(0))
            .map(unsigned)
            .map_err(sqlite_store_error)
    }

    fn prune_resource_minutes_batch(
        &self,
        table: ResourceMinutesTable,
        cutoff: DateTime<Utc>,
        limit: usize,
    ) -> Result<usize, OrbitError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let sql = match table {
            ResourceMinutesTable::Run => {
                "DELETE FROM run_resource_minutes WHERE rowid IN ( \
                     SELECT rowid FROM run_resource_minutes \
                     WHERE minute_started_at < ?1 LIMIT ?2)"
            }
            ResourceMinutesTable::Host => {
                "DELETE FROM host_resource_minutes WHERE rowid IN ( \
                     SELECT rowid FROM host_resource_minutes WHERE minute_at < ?1 LIMIT ?2)"
            }
            ResourceMinutesTable::AdmissionForecast => {
                "DELETE FROM admission_forecasts WHERE rowid IN ( \
                     SELECT rowid FROM admission_forecasts WHERE forecast_at < ?1 LIMIT ?2)"
            }
        };
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            tx.connection()
                .execute(sql, params![stamp(cutoff), limit])
                .map_err(sqlite_store_error)
        })
    }
}
