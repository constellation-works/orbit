//! Shared attention settings. Later source adapters consume this same owner.
use crate::registry::ConfigSnapshot;
use orbit_common::OrbitError;

/// Validated settings for the derived attention feed and its source adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionSettings {
    /// Cursor validity after issuance, in hours.
    pub cursor_ttl_hours: u32,
    /// Strictly increasing severity age thresholds, in hours.
    pub age_bump_hours: Vec<u32>,
    /// Failed-run lookback, in hours.
    pub run_window_hours: u32,
    /// Stuck-run bound, in minutes.
    pub run_stuck_after_minutes: u32,
    /// Provider cache lifetime, in seconds.
    pub ci_cache_seconds: u32,
    /// Per-source deadline, in seconds.
    pub source_timeout_seconds: u32,
    /// Cross-host deadline, in seconds.
    pub fanout_budget_seconds: u32,
    /// Maximum candidates from one workspace.
    pub per_workspace_item_cap: u32,
}
impl AttentionSettings {
    /// Read the canonical admitted registry values.
    pub fn from_snapshot(snapshot: &ConfigSnapshot) -> Self {
        Self {
            cursor_ttl_hours: snapshot.attention_cursor_ttl_hours,
            run_window_hours: snapshot.attention_run_window_hours,
            run_stuck_after_minutes: snapshot.attention_run_stuck_after_minutes,
            ci_cache_seconds: snapshot.attention_ci_cache_seconds,
            source_timeout_seconds: snapshot.attention_source_timeout_seconds,
            fanout_budget_seconds: snapshot.attention_fanout_budget_seconds,
            per_workspace_item_cap: snapshot.attention_per_workspace_item_cap,
            age_bump_hours: snapshot.attention_age_bump_hours.clone(),
        }
    }
}
pub(crate) fn resolve_attention_age_bumps(raw: Option<Vec<u32>>) -> Result<Vec<u32>, OrbitError> {
    let values = raw.unwrap_or_else(|| vec![24, 72]);
    if values.iter().any(|v| *v == 0 || *v > 87600) || values.windows(2).any(|w| w[0] >= w[1]) {
        return Err(OrbitError::InvalidInput("attention.age_bump_hours must contain strictly increasing positive hours, at most 87600".into()));
    }
    Ok(values)
}
