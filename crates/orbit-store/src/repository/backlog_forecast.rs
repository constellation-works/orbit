//! Concrete repository for stored backlog forecasts (`backlog_forecasts`).
//!
//! Reached only through [`Snapshot::backlog_forecast`] (read-only) and
//! [`Tx::backlog_forecast`] (read-write inside [`crate::Store::write`]).
//! `forecast_at` is stored as fixed-width UTC RFC3339 text (microseconds, `Z`),
//! so it compares lexically. Minimum-interval throttling belongs to the calling
//! operation; this repository owns SQL queries and row mapping only.

use std::marker::PhantomData;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::contracts::StoredBacklogForecast;
use crate::driver::sqlite::codec::stamp;
use crate::store::{Snapshot, Tx};

const COLUMNS: &str =
    "workspace_id, forecast_at, model_version, inputs_json, result_json, tasks_json";

/// Concrete backlog-forecast repository scoped to a [`Snapshot`] (read-only) or
/// a [`Tx`] (read-write).
pub struct BacklogForecastRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> BacklogForecastRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> BacklogForecastRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Insert `row`, ignoring duplicates on primary key. Returns whether a row
    /// was inserted.
    pub fn put(&self, row: &StoredBacklogForecast) -> Result<bool, OrbitError> {
        let written = self
            .conn
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
    }
}

impl<Access> BacklogForecastRepository<'_, Access> {
    /// The workspace's newest `limit` forecasts, newest first.
    pub fn list(
        &self,
        workspace_id: &str,
        limit: usize,
    ) -> Result<Vec<StoredBacklogForecast>, OrbitError> {
        let mut statement = self
            .conn
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

    /// The newest mature forecasts per model version, bounded by
    /// `limit_per_version`; excludes model versions below `min_model_version`.
    pub fn list_mature(
        &self,
        workspace_id: &str,
        cutoff: DateTime<Utc>,
        min_model_version: u32,
        limit_per_version: usize,
    ) -> Result<Vec<StoredBacklogForecast>, OrbitError> {
        let mut statement = self
            .conn
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

    /// Whether the workspace holds a forecast strictly newer than `after`.
    pub fn has_after(&self, workspace_id: &str, after: DateTime<Utc>) -> Result<bool, OrbitError> {
        let recent: Option<String> = self
            .conn
            .query_row(
                "SELECT forecast_at FROM backlog_forecasts \
                 WHERE workspace_id = ?1 AND forecast_at > ?2 LIMIT 1",
                params![workspace_id, stamp(after)],
                |found| found.get(0),
            )
            .optional()
            .map_err(sqlite_store_error)?;
        Ok(recent.is_some())
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
