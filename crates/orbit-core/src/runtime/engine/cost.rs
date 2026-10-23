use chrono::Utc;
use orbit_common::OrbitError;

use crate::OrbitRuntime;
use crate::metrics::cost::{
    ManagedExecutionCost, MetricsWindow, ORCHESTRATION_SCHEMA_VERSION, OrchestrationSummary,
};

impl OrbitRuntime {
    /// Managed-execution cost and token usage for `window`, ending now.
    ///
    /// A finite window also reports the normalized token total of the
    /// preceding window of equal length, so the Cost page can show a delta.
    pub fn managed_execution_cost(
        &self,
        window: MetricsWindow,
    ) -> Result<ManagedExecutionCost, OrbitError> {
        let now = Utc::now();
        let since_window = window.duration().map(|d| now - d);
        let orchestration = self.orchestrator_invocation_metrics(since_window, Some(now))?;
        let previous_normalized_tokens = match (since_window, window.duration()) {
            (Some(since), Some(duration)) => Some(
                self.orchestrator_invocation_metrics(Some(since - duration), Some(since))?
                    .normalized_tokens,
            ),
            _ => None,
        };
        Ok(ManagedExecutionCost {
            window: window.as_str().to_string(),
            window_since: since_window.map(|t| t.to_rfc3339()),
            generated_at: Utc::now().to_rfc3339(),
            orchestration: OrchestrationSummary {
                schema_version: ORCHESTRATION_SCHEMA_VERSION,
                scope: "managed_execution".to_string(),
                as_of: orchestration.as_of,
                since: orchestration.since,
                until: orchestration.until,
                buckets: orchestration.buckets,
                normalized_tokens: orchestration.normalized_tokens,
                previous_normalized_tokens,
            },
        })
    }
}
