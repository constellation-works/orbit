//! Selection and serialized forecast-accuracy report types.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use orbit_types::workflow::{DrainPassCounters, DrainPassOutcome};
use serde::Serialize;

use crate::runtime::host_resource::profile::PROFILE_HISTORY_DAYS;

/// An operator-selected cohort of stored forecasts, with inclusive time bounds.
/// Drain and mode are exact matches; these rows do not attest source or settings.
#[derive(Debug, Clone, Serialize)]
pub struct AccuracySelection {
    pub since: DateTime<Utc>,
    pub drain: Option<String>,
    pub mode: Option<String>,
}

impl AccuracySelection {
    /// The legacy population: all drains and modes over the last 30 days.
    pub fn last_30_days(report_at: DateTime<Utc>) -> Self {
        Self {
            since: report_at - Duration::days(PROFILE_HISTORY_DAYS),
            drain: None,
            mode: None,
        }
    }
}

/// The forecast accuracy report.
#[derive(Debug, Clone, Serialize)]
pub struct ForecastAccuracy {
    /// The oldest forecast considered (inclusive); retained for compatibility.
    pub since: DateTime<Utc>,
    /// One snapshot for the forecast upper bound and target-minute maturity.
    pub report_at: DateTime<Utc>,
    /// Effective selection, applied to points, errors and decisions alike.
    pub selection: AccuracySelection,
    /// Recorded admission passes.
    pub forecasts: u64,
    /// Accuracy at each recorded offset.
    pub offsets: Vec<OffsetAccuracy>,
    /// Shadow decisions against actual admissions, per drain.
    pub drains: Vec<DrainShadowDecisions>,
    /// What limited each selected drain's admission passes.
    pub limiters: Vec<DrainLimiters>,
    /// Every finalized forecast/measured-minute pair, oldest first.
    pub pairs: Vec<ForecastPair>,
}

/// One offset's forecast points against the measured host.
#[derive(Debug, Clone, Serialize)]
pub struct OffsetAccuracy {
    pub offset_minutes: u32,
    pub paired: u64,
    /// Compatible total of pending and missing points.
    pub unmeasured: u64,
    /// Target minutes not fully ended at report time, including partial rows.
    pub pending: u64,
    /// Ended target minutes with no measured row. The sum of [`Self::unsampled`],
    /// [`Self::partial`] and [`Self::gap`].
    pub missing: u64,
    /// Ended target minutes with no host row and no delivery run live.
    pub unsampled: u64,
    /// Ended target minutes a delivery overlapped, but none saw whole.
    ///
    /// The sampler writes a host row only for a minute a worker saw whole, so
    /// the minute a delivery starts or finishes in is partial unless another
    /// worker covered it. That is not a sampling defect.
    pub partial: u64,
    /// Ended target minutes a delivery ran all the way across, with no host row.
    pub gap: u64,
    /// In cores.
    pub cpu: ErrorSummary,
    /// In bytes.
    pub memory: ErrorSummary,
    /// Raw pairs with resolvable attribution metadata (legacy rows and
    /// unresolved reservation cohorts excluded).
    pub attributed_paired: u64,
    /// Forecast minus (baseline-at-forecast plus covered deliveries' measured load).
    pub attributed_cpu: ErrorSummary,
    pub attributed_memory: ErrorSummary,
}

/// Forecast errors over one offset's pairs; every field is `None` without
/// pairs.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ErrorSummary {
    /// Mean of forecast minus measured.
    pub mean_error: Option<f64>,
    pub mean_absolute_error: Option<f64>,
    /// Share of pairs, `0..=1`, whose forecast was below the measured value.
    pub under_rate: Option<f64>,
    /// The pair with the largest absolute error.
    pub worst_miss: Option<WorstMiss>,
}

/// The largest miss at one offset.
#[derive(Debug, Clone, Serialize)]
pub struct WorstMiss {
    /// Forecast minus measured.
    pub error: f64,
    pub forecast: f64,
    pub measured: f64,
    pub forecast_at: DateTime<Utc>,
    pub minute_at: DateTime<Utc>,
    pub workspace_id: String,
    pub run_id: String,
}

/// One forecast point and the host minute it forecast.
#[derive(Debug, Clone, Serialize)]
pub struct ForecastPair {
    pub workspace_id: String,
    pub run_id: String,
    pub mode: String,
    pub forecast_at: DateTime<Utc>,
    pub offset_minutes: u32,
    pub minute_at: DateTime<Utc>,
    pub forecast_cpu_cores: f64,
    pub measured_cpu_cores: f64,
    pub forecast_memory_bytes: u64,
    pub measured_memory_bytes: u64,
    /// Baseline-at-forecast plus only the covered deliveries' measured load.
    /// Absent when the forecast predates attribution recording or includes an
    /// unresolved pending reservation.
    pub attributed_cpu_cores: Option<f64>,
    pub attributed_memory_bytes: Option<f64>,
}

/// One drain's shadow and enforce decisions against what it admitted.
#[derive(Debug, Clone, Serialize)]
pub struct DrainShadowDecisions {
    pub workspace_id: String,
    pub run_id: String,
    pub shadow_passes: u64,
    pub enforce_passes: u64,
    /// Tasks the drain admitted during its shadow passes.
    pub admitted: u64,
    /// Admitted while gap fit would have held them. Shadow passes only.
    pub would_hold: Vec<String>,
    /// Gap fit would have admitted them in a pass that did not. Shadow only.
    pub would_add: Vec<String>,
    /// Distinct tasks local enforce passes admitted.
    pub enforce_admitted: u64,
    /// Distinct tasks gap fit held on local enforce passes (`fit_skipped`).
    pub gap_held: Vec<String>,
    /// Pull request forecasts, including ones enforce held.
    pub pull_requests: u64,
    /// Requests actually sent to the owner, in either mode.
    pub pull_sent: u64,
    /// Shadow requests enforce would have held (`forecast_no_headroom`).
    pub pull_would_hold: u64,
    /// Enforce pull requests held because no work fit (`forecast_no_headroom`).
    pub pull_held: u64,
}

/// What limited one drain's admission passes.
#[derive(Debug, Clone, Serialize)]
pub struct DrainLimiters {
    pub workspace_id: String,
    pub run_id: String,
    pub counters: DrainPassCounters,
    /// Each outcome's passes as a share of `counters.passes`, `0..=1`; every
    /// outcome is present and they sum to one.
    pub shares: BTreeMap<DrainPassOutcome, f64>,
    /// Each waiting reason's passes as a share of `counters.passes`. A pass
    /// can count under several reasons.
    pub waiting_shares: BTreeMap<String, f64>,
}

impl DrainLimiters {
    pub fn new(workspace_id: String, run_id: String, counters: DrainPassCounters) -> Self {
        let shares = DrainPassOutcome::ALL
            .into_iter()
            .map(|outcome| (outcome, counters.share(counters.outcome(outcome))))
            .collect();
        let waiting_shares = counters
            .waiting_by_reason
            .iter()
            .map(|(reason, passes)| (reason.clone(), counters.share(*passes)))
            .collect();
        Self {
            workspace_id,
            run_id,
            counters,
            shares,
            waiting_shares,
        }
    }
}
