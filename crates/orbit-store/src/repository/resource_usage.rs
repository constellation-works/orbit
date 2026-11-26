//! Concrete repositories for per-minute run and host resource history
//! (`run_resource_minutes`, `host_resource_minutes`), admission pass forecasts
//! (`admission_forecasts`) and watchdog breach evidence
//! (`worker_resource_limits`).
//!
//! Reached only through [`Snapshot::resource_usage`] / [`Tx::resource_usage`]
//! and [`Snapshot::worker_resource_limit`] / [`Tx::worker_resource_limit`].
//! Timestamps are stored as fixed-width UTC RFC3339 text (microseconds, `Z`),
//! so SQLite orders and compares them as text. When to prune and in what
//! batches is the calling operation's; this repository owns SQL and row
//! mapping only.

mod delivery_run_load;

use std::marker::PhantomData;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use crate::contract::{
    AdmissionForecastPoint, AdmissionForecastRecord, CPU_ACCOUNTING_LINUX, CPU_ACCOUNTING_MACOS,
    CPU_ACCOUNTING_MACOS_TREE_ONLY, DeliveryRunInterval, DeliveryRunResources,
    DeliveryRunsFreshness, DeliveryRunsLoad, HostResourceMinute, ResourceHistoryKeep,
    ResourceMinutesTable, ResourceRetentionRow, RunResourceMinute, WorkerResourceLimitEvidence,
};
use crate::driver::sqlite::codec::{
    RUN_MINUTE_COLUMNS, host_minutes, run_minute_from_row, stamp, unsigned,
};
use crate::store::{Snapshot, Tx};

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

/// Concrete resource-usage repository scoped to a [`Snapshot`] (read-only)
/// or a [`Tx`] (read-write).
pub struct ResourceUsageRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> ResourceUsageRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> ResourceUsageRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Record one run minute. Writing the same minute again replaces it, so a
    /// sampler that re-flushes a minute never duplicates it.
    pub fn put_run_minute(&self, minute: &RunResourceMinute) -> Result<(), OrbitError> {
        self.conn
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
    }

    /// Offer the host's row for one minute. The first offer of a minute wins;
    /// returns whether this one was stored.
    pub fn put_host_minute(&self, minute: &HostResourceMinute) -> Result<bool, OrbitError> {
        self.conn
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
    }

    /// Record one admission pass's forecast.
    pub fn put_forecast(&self, record: &AdmissionForecastRecord) -> Result<(), OrbitError> {
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
        self.conn
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
    }

    /// Delete the selected rows of `table` whose timestamps still match, by
    /// rowid. Returns how many went.
    pub fn delete_retention_rows(
        &self,
        table: ResourceMinutesTable,
        rows: &[ResourceRetentionRow],
    ) -> Result<usize, OrbitError> {
        let sql = format!(
            "DELETE FROM {} WHERE rowid = ?1 AND {} = ?2",
            table.table_name(),
            resource_retention_timestamp(table)
        );
        let mut statement = self.conn.prepare(&sql).map_err(sqlite_store_error)?;
        let mut removed = 0;
        for row in rows {
            removed += statement
                .execute(params![row.row_id, row.timestamp])
                .map_err(sqlite_store_error)?;
        }
        Ok(removed)
    }
}

impl<Access> ResourceUsageRepository<'_, Access> {
    /// One run's minutes, in minute order.
    pub fn run_minutes(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Vec<RunResourceMinute>, OrbitError> {
        let conn = self.conn;
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

    /// Every sampled run of a delivery admitted at or after `since`, across
    /// every workspace on the host, with its minutes and `job_runs` lifecycle.
    pub fn delivery_runs(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<DeliveryRunResources>, OrbitError> {
        delivery_run_load::load_delivery_runs(self.conn, since)
    }

    /// Every sampled run since the epoch, as [`Self::delivery_runs`] would
    /// return it now. Unchanged runs in `previous` are reused when at most
    /// half of current run shapes changed, including exactly half. When more
    /// than half changed, every qualifying minute is read in one ordered scan.
    pub fn reload_delivery_runs(
        &self,
        previous: Option<DeliveryRunsLoad>,
    ) -> Result<DeliveryRunsLoad, OrbitError> {
        delivery_run_load::reload_delivery_runs(self.conn, previous)
    }

    /// The finished sampled runs on the host, as a cache freshness probe.
    pub fn delivery_runs_freshness(&self) -> Result<DeliveryRunsFreshness, OrbitError> {
        let conn = self.conn;
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

    /// A counter that moves whenever [`Self::delivery_runs_freshness`] may
    /// have changed, read without scanning: while it holds, the probe's
    /// answer holds too. `None` when the store keeps no such counter, so a
    /// caller always probes.
    pub fn delivery_runs_stamp(&self) -> Result<Option<i64>, OrbitError> {
        let conn = self.conn;
        // A write-free reader may open a store migrated before the counter.
        let kept: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master
                               WHERE type = 'table' AND name = 'delivery_runs_stamp')",
                [],
                |row| row.get(0),
            )
            .map_err(sqlite_store_error)?;
        if !kept {
            return Ok(None);
        }
        conn.query_row(
            "SELECT stamp FROM delivery_runs_stamp WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_store_error)
    }

    /// Host minutes at or after `since`, oldest first.
    pub fn host_minutes(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<HostResourceMinute>, OrbitError> {
        host_minutes(self.conn, since)
    }

    /// Admission forecasts made at or after `since`, across every workspace,
    /// oldest first.
    pub fn forecasts(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<AdmissionForecastRecord>, OrbitError> {
        let conn = self.conn;
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

    /// Rows of `table` from before `cutoff` that `keep` does not hold,
    /// across every workspace.
    pub fn count_prunable(
        &self,
        table: ResourceMinutesTable,
        cutoff: DateTime<Utc>,
        keep: ResourceHistoryKeep,
    ) -> Result<u64, OrbitError> {
        let conn = self.conn;
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

    /// At most `limit` of the rows [`Self::count_prunable`] counts, in safe
    /// deletion order (ineligible markers and minute zero last).
    pub fn retention_candidates(
        &self,
        table: ResourceMinutesTable,
        cutoff: DateTime<Utc>,
        keep: ResourceHistoryKeep,
        limit: usize,
    ) -> Result<Vec<ResourceRetentionRow>, OrbitError> {
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
            "SELECT r.rowid, r.{timestamp} FROM {table} r WHERE {predicate}{order} LIMIT ?3",
            table = table.table_name(),
            timestamp = resource_retention_timestamp(table),
            predicate = prunable(table)
        );
        let (cutoff, kept) = prune_bounds(table, cutoff, keep);
        let conn = self.conn;
        let mut stmt = conn.prepare(&sql).map_err(sqlite_store_error)?;
        stmt.query_map(params![cutoff, kept, limit], |row| {
            Ok(ResourceRetentionRow {
                row_id: row.get(0)?,
                timestamp: row.get(1)?,
            })
        })
        .map_err(sqlite_store_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
    }

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
        let conn = self.conn;
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

/// Concrete watchdog breach evidence repository scoped to a [`Snapshot`]
/// (read-only) or a [`Tx`] (read-write).
pub struct WorkerResourceLimitRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> WorkerResourceLimitRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> WorkerResourceLimitRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Store one run's breach evidence, replacing any earlier evidence of it.
    pub fn put(&self, evidence: &WorkerResourceLimitEvidence) -> Result<(), OrbitError> {
        let encoded = encode(evidence, "resource evidence")?;
        self.conn
            .execute(
                "INSERT INTO worker_resource_limits (workspace_id, run_id, evidence_json)
                 VALUES (?1, ?2, ?3) ON CONFLICT (workspace_id, run_id)
                 DO UPDATE SET evidence_json = excluded.evidence_json",
                params![evidence.workspace_id, evidence.run_id, encoded],
            )
            .map(|_| ())
            .map_err(sqlite_store_error)
    }
}

impl<Access> WorkerResourceLimitRepository<'_, Access> {
    /// One run's breach evidence.
    pub fn get(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Option<WorkerResourceLimitEvidence>, OrbitError> {
        self.conn.query_row(
            "SELECT evidence_json FROM worker_resource_limits WHERE workspace_id = ?1 AND run_id = ?2",
            params![workspace_id, run_id],
            |row| decode(0, &row.get::<_, String>(0)?),
        ).optional().map_err(sqlite_store_error)
    }
}

fn resource_retention_timestamp(table: ResourceMinutesTable) -> &'static str {
    match table {
        ResourceMinutesTable::Run => "minute_started_at",
        ResourceMinutesTable::Host => "minute_at",
        ResourceMinutesTable::AdmissionForecast => "forecast_at",
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
            // Materialize each run's newest known model, then each delivery's
            // first modeled run. A correlated ORDER BY run_id chose the
            // workspace-prefix index and reread its history per delivery.
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
                     WITH minute_models AS MATERIALIZED ( \
                         SELECT r.workspace_id, r.run_id, r.task_id, r.delivery_admitted_at, \
                             COALESCE(r.crew_model, j.crew_model) AS model, \
                             ROW_NUMBER() OVER (PARTITION BY r.workspace_id, r.run_id \
                                 ORDER BY r.minute_index DESC) AS newest_minute \
                         FROM run_resource_minutes r LEFT JOIN job_runs j \
                           ON j.workspace_id = r.workspace_id AND j.run_id = r.run_id \
                         WHERE r.delivery_admitted_at IS NOT NULL \
                           AND COALESCE(r.crew_model, j.crew_model) IS NOT NULL), \
                     delivery_models AS MATERIALIZED ( \
                         SELECT *, ROW_NUMBER() OVER ( \
                             PARTITION BY workspace_id, task_id, delivery_admitted_at \
                             ORDER BY run_id) AS first_run \
                         FROM minute_models WHERE newest_minute = 1), \
                     eligible AS ( \
                         SELECT r.workspace_id, r.task_id, r.delivery_admitted_at, \
                             MIN(r.work_class) AS work_class, \
                             CASE WHEN substr(MIN(r.work_class), 1, 10) = 'auto-task:' \
                                  THEN '' ELSE IFNULL(MIN(r.complexity), '') END AS complexity, \
                             MAX(julianday(j.finished_at)) AS finished, m.model \
                         FROM run_resource_minutes r LEFT JOIN job_runs j \
                           ON j.workspace_id = r.workspace_id AND j.run_id = r.run_id \
                         LEFT JOIN delivery_models m ON m.workspace_id = r.workspace_id \
                           AND m.task_id = r.task_id AND m.delivery_admitted_at = r.delivery_admitted_at \
                           AND m.first_run = 1 \
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
