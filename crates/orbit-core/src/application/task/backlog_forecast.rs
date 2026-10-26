//! Stored backlog forecasts behind Analytics › Forecast: the panel records at
//! most one per workspace per hour and scores the stored ones later.

use chrono::Duration;
use orbit_common::OrbitError;
use orbit_store::contracts::{BacklogForecastStoreBackend as _, StoredBacklogForecast};

use crate::OrbitRuntime;

impl OrbitRuntime {
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
