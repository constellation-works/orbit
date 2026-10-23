//! Managed-execution cost and token accounting, as the dashboard Cost page
//! reads it, and the time window shared by the dashboard's windowed metrics.
//!
//! The accounting is built from the invocation store by
//! `OrbitRuntime::managed_execution_cost`. It replaced the agent-family
//! scoreboard summary, which carried this projection as its `orchestration`
//! field; the JSON shape of that field is unchanged.

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use serde::{Deserialize, Serialize};

/// Version of the [`OrchestrationSummary`] shape. v2 normalizes provider
/// input semantics and keeps model attribution beside each bucket.
pub const ORCHESTRATION_SCHEMA_VERSION: u32 = 2;

/// Time window for a windowed dashboard metric. `All` is the lifetime view;
/// every other variant carries a finite [`MetricsWindow::duration`].
///
/// String forms (the dashboard `?window=` query parameter and the serialized
/// `window` field): `1h`, `24h`, `7d`, `30d`, `all`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MetricsWindow {
    Hour,
    Day,
    Week,
    Month,
    #[default]
    All,
}

impl MetricsWindow {
    /// Window length, or `None` for `All` (no cutoff).
    pub fn duration(self) -> Option<Duration> {
        match self {
            MetricsWindow::Hour => Some(Duration::hours(1)),
            MetricsWindow::Day => Some(Duration::hours(24)),
            MetricsWindow::Week => Some(Duration::days(7)),
            MetricsWindow::Month => Some(Duration::days(30)),
            MetricsWindow::All => None,
        }
    }

    /// Canonical short string used in the query parameter and payloads.
    pub fn as_str(self) -> &'static str {
        match self {
            MetricsWindow::Hour => "1h",
            MetricsWindow::Day => "24h",
            MetricsWindow::Week => "7d",
            MetricsWindow::Month => "30d",
            MetricsWindow::All => "all",
        }
    }
}

impl std::str::FromStr for MetricsWindow {
    type Err = OrbitError;

    /// Parse a canonical window string. Accepts exactly `"1h"`, `"24h"`,
    /// `"7d"`, `"30d"`, or `"all"`. Any other input returns
    /// [`OrbitError::InvalidInput`] so HTTP callers can render an exact 400.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "1h" => Ok(MetricsWindow::Hour),
            "24h" => Ok(MetricsWindow::Day),
            "7d" => Ok(MetricsWindow::Week),
            "30d" => Ok(MetricsWindow::Month),
            "all" => Ok(MetricsWindow::All),
            other => Err(OrbitError::InvalidInput(format!(
                "unknown window '{other}' (expected one of 1h, 24h, 7d, 30d, all)"
            ))),
        }
    }
}

/// Conservative ownership class for managed invocation accounting.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationBucketKind {
    Missing,
    Unattributed,
    Orchestrator,
    Shared,
}

/// One managed-execution accounting bucket, classified by canonical task
/// orchestration ownership rather than executor agent or model identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrchestrationBucketSummary {
    pub kind: OrchestrationBucketKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orchestrator: Option<String>,
    pub invocation_count: u64,
    pub linked_task_count: u64,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_create_tokens: u64,
    pub cache_create_1h_tokens: u64,
    pub output_tokens: u64,
    pub provider_cost_usd: f64,
    pub provider_cost_count: u64,
    pub derived_cost_usd: f64,
    pub derived_cost_count: u64,
    pub comparable_provider_cost_usd: f64,
    pub comparable_derived_cost_usd: f64,
    pub comparable_cost_count: u64,
    pub comparable_cost_delta_usd: f64,
    pub missing_provider_count: u64,
    pub unpriced_derived_count: u64,
    /// Normalized, mutually-exclusive token usage for covered models only.
    #[serde(default)]
    pub normalized_tokens: NormalizedTokenSummary,
    /// Model attribution is descriptive only; tokenizers are not ranked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<OrchestrationModelSummary>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NormalizedTokenSummary {
    pub invocation_count: u64,
    pub linked_task_count: u64,
    pub uncached_input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_create_tokens: u64,
    pub cache_create_1h_tokens: u64,
    pub output_tokens: u64,
    pub normalized_token_total: u64,
    pub covered_invocation_count: u64,
    pub unknown_input_basis_or_model_count: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrchestrationModelSummary {
    pub model: String,
    pub tokens: NormalizedTokenSummary,
}

/// Independently-versioned accounting for managed execution only.
///
/// `until` is exclusive and no later than `as_of`. Provider, derived, and
/// comparable cost populations are separate so partial sums are never implied
/// to reconcile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrchestrationSummary {
    pub schema_version: u32,
    pub scope: String,
    pub as_of: DateTime<Utc>,
    pub since: Option<DateTime<Utc>>,
    pub until: DateTime<Utc>,
    pub buckets: Vec<OrchestrationBucketSummary>,
    #[serde(default)]
    pub normalized_tokens: NormalizedTokenSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_normalized_tokens: Option<NormalizedTokenSummary>,
}

/// Managed-execution cost for one dashboard window: the payload of
/// `GET /api/cost`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManagedExecutionCost {
    /// Selected window in canonical short form (`"1h"` … `"all"`).
    pub window: String,
    /// RFC3339 lower bound of the window, or `None` for `all`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_since: Option<String>,
    pub generated_at: String,
    pub orchestration: OrchestrationSummary,
}
