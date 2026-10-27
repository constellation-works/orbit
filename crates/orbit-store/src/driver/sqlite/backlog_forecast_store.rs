//! Stored backlog forecasts in `backlog_forecasts`.
//!
//! `forecast_at` is stored as fixed-width UTC RFC3339 text (microseconds,
//! `Z`), so it compares lexically.

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{OptionalExtension, Row, TransactionBehavior, params};

use crate::Store;
use crate::contracts::{BacklogForecastStoreBackend, StoredBacklogForecast};

use super::codec::stamp;

const COLUMNS: &str =
    "workspace_id, forecast_at, model_version, inputs_json, result_json, tasks_json";

impl BacklogForecastStoreBackend for Store {
    fn mature_backlog_forecasts(
        &self,
        workspace_id: &str,
        cutoff: DateTime<Utc>,
        min_model_version: u32,
        limit_per_version: usize,
    ) -> Result<Vec<StoredBacklogForecast>, OrbitError> {
        let conn = self.read()?;
        let mut statement = conn
            .prepare(&format!(
                "SELECT {COLUMNS} FROM (SELECT {COLUMNS}, \
             ROW_NUMBER() OVER (PARTITION BY model_version ORDER BY forecast_at DESC) AS rank \
             FROM backlog_forecasts WHERE workspace_id = ?1 AND forecast_at <= ?2 \
             AND model_version >= ?3) WHERE rank <= ?4 ORDER BY forecast_at DESC"
            ))
            .map_err(sqlite_store_error)?;
        let rows = statement
            .query_map(
                params![
                    workspace_id,
                    stamp(cutoff),
                    min_model_version,
                    i64::try_from(limit_per_version).unwrap_or(i64::MAX)
                ],
                hydrate,
            )
            .map_err(sqlite_store_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    fn record_backlog_forecast(
        &self,
        row: &StoredBacklogForecast,
        min_interval: Duration,
    ) -> Result<bool, OrbitError> {
        // The check and the insert share one immediate transaction, so two
        // concurrent readers cannot both store a forecast for the same hour.
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let recent: Option<String> = conn
                .query_row(
                    "SELECT forecast_at FROM backlog_forecasts \
                     WHERE workspace_id = ?1 AND forecast_at > ?2 LIMIT 1",
                    params![row.workspace_id, stamp(row.forecast_at - min_interval)],
                    |found| found.get(0),
                )
                .optional()
                .map_err(sqlite_store_error)?;
            if recent.is_some() {
                return Ok(false);
            }
            let written = conn
                .execute(
                    &format!(
                        "INSERT OR IGNORE INTO backlog_forecasts ({COLUMNS}) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
                    ),
                    params![
                        row.workspace_id,
                        stamp(row.forecast_at),
                        row.model_version,
                        row.inputs_json,
                        row.result_json,
                        row.tasks_json,
                    ],
                )
                .map_err(sqlite_store_error)?;
            Ok(written > 0)
        })
    }

    fn backlog_forecasts(
        &self,
        workspace_id: &str,
        limit: usize,
    ) -> Result<Vec<StoredBacklogForecast>, OrbitError> {
        let conn = self.read()?;
        let mut statement = conn
            .prepare(&format!(
                "SELECT {COLUMNS} FROM backlog_forecasts \
                 WHERE workspace_id = ?1 ORDER BY forecast_at DESC LIMIT ?2"
            ))
            .map_err(sqlite_store_error)?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = statement
            .query_map(params![workspace_id, limit], hydrate)
            .map_err(sqlite_store_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }
}

fn hydrate(row: &Row<'_>) -> rusqlite::Result<StoredBacklogForecast> {
    Ok(StoredBacklogForecast {
        workspace_id: row.get(0)?,
        forecast_at: crate::parse_timestamp(&row.get::<_, String>(1)?)?,
        model_version: row.get(2)?,
        inputs_json: row.get(3)?,
        result_json: row.get(4)?,
        tasks_json: row.get(5)?,
    })
}
