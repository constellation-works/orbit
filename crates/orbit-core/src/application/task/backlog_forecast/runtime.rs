use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{BacklogForecastStoreBackend as _, StoredBacklogForecast};
use orbit_types::task::TaskStatus;
use serde_json::Value;

use crate::OrbitRuntime;

use super::options;
use super::options::BacklogForecastHistory;
use super::persistence::STORE_INTERVAL_MINUTES;

impl OrbitRuntime {
    /// Forecast this workspace's backlog and score earlier stored forecasts.
    ///
    /// Defaults use 30 days of history, reaching back to at least the newest
    /// 50 completions, the live drain ceiling and no arrivals. Canonical
    /// reads may record a snapshot when the hourly slot is free. Lookbacks
    /// outside 7–180 days, history counts outside 1–5000, both together,
    /// concurrency outside 1–64 and arrivals other than `on` or `off` are
    /// invalid inputs.
    pub fn backlog_forecast(
        &self,
        history: BacklogForecastHistory,
        concurrency: Option<u32>,
        arrivals: Option<&str>,
    ) -> Result<Value, OrbitError> {
        let options = options::parse_options(history, concurrency, arrivals)?;
        super::calculate::calculate_backlog_forecast(self, &options, Utc::now())
    }

    /// Fill the canonical hourly slot on the owner clock, without requiring
    /// an endpoint read. The transaction remains the authority if a read races
    /// this early check. Recent slots skip history reads and simulation.
    pub(crate) fn record_scheduled_backlog_forecast(
        &self,
        now: DateTime<Utc>,
    ) -> Result<(), OrbitError> {
        if self.worker_invocation().is_some() || self.coordination_write_owner().is_some() {
            return Ok(());
        }
        if self.backlog_forecasts(1)?.first().is_some_and(|latest| {
            latest.forecast_at > now - Duration::minutes(STORE_INTERVAL_MINUTES)
        }) {
            return Ok(());
        }
        // The canonical store keeps every non-proposed forecast item, so the
        // scheduled admission must match that base cohort: proposed-only
        // work stores nothing and stays skipped here, as before.
        if !self.list_task_metadata()?.iter().any(|task| {
            matches!(
                task.status,
                TaskStatus::Backlog | TaskStatus::InProgress | TaskStatus::Blocked
            )
        }) {
            return Ok(());
        }
        let options = options::parse_options(BacklogForecastHistory::default(), None, None)?;
        super::calculate::calculate_backlog_forecast(self, &options, now)?;
        Ok(())
    }

    /// Store `forecast` for this workspace unless one newer than
    /// `forecast.forecast_at - min_interval` is already stored. `true` when
    /// this call wrote it. The workspace id on `forecast` is replaced by this
    /// runtime's.
    pub fn record_backlog_forecast(
        &self,
        mut forecast: StoredBacklogForecast,
        min_interval: Duration,
    ) -> Result<bool, OrbitError> {
        forecast.workspace_id = self.workspace_id()?;
        self.sqlite_store()?
            .record_backlog_forecast(&forecast, min_interval)
    }

    /// This workspace's newest `limit` stored forecasts, newest first.
    pub fn backlog_forecasts(
        &self,
        limit: usize,
    ) -> Result<Vec<StoredBacklogForecast>, OrbitError> {
        self.sqlite_store()?
            .backlog_forecasts(&self.workspace_id()?, limit)
    }
}
