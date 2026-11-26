//! Stored backlog forecasts: the Analytics › Forecast panel writes at most
//! one per workspace per hour and scores the stored ones against what their
//! tasks did.

use chrono::{DateTime, Utc};

/// One stored forecast. The JSON documents are owned by the forecast model,
/// which versions them with `model_version`; the store keeps them opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBacklogForecast {
    pub workspace_id: String,
    pub forecast_at: DateTime<Utc>,
    pub model_version: u32,
    /// The inputs summary: lookback, concurrency and its source, sample counts.
    pub inputs_json: String,
    /// The quantiles the forecast reported.
    pub result_json: String,
    /// The tasks the forecast covered, each with what it was forecast as.
    pub tasks_json: String,
}
