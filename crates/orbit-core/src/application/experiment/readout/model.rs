use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::application::experiment::readout::stats::Interval;

/// The readout shape's version; bump it on any breaking change.
pub const EXPERIMENT_READOUT_SCHEMA_VERSION: u32 = 1;
/// Below this sample-ratio p-value the assignment is suspect.
pub(super) const SAMPLE_RATIO_ALPHA: f64 = 0.001;

/// One experiment's readout.
#[derive(Debug, Clone, Serialize)]
pub struct ExperimentReadout {
    pub schema_version: u32,
    pub experiment: ReadoutExperiment,
    pub as_of: String,
    pub enrollment: ReadoutEnrollment,
    pub arms: Vec<ReadoutArm>,
    pub sample_ratio: SampleRatio,
    pub lagging: Lagging,
    pub metrics: Vec<ReadoutMetric>,
    pub guardrails: Vec<GuardrailReadout>,
    pub verdict: Verdict,
    pub warnings: Vec<ReadoutWarning>,
    /// The primary metric's settled per-task values, oldest assignment
    /// first, for the dashboard's cumulative series; not part of the JSON.
    #[serde(skip)]
    pub primary_observations: Vec<PrimaryObservation>,
}

/// One enrolled task's settled value of the primary metric.
#[derive(Debug, Clone)]
pub struct PrimaryObservation {
    pub arm: String,
    pub assigned_at: DateTime<Utc>,
    pub value: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadoutExperiment {
    pub id: String,
    pub name: String,
    pub status: String,
    pub mode: String,
    pub segment: u32,
    pub primary_metric: Option<String>,
    pub guardrails: BTreeMap<String, String>,
    pub min_sample: Option<u32>,
    pub max_days: Option<u32>,
    pub started_at: Option<String>,
    /// `started_at` plus `max_days`.
    pub horizon_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadoutEnrollment {
    /// Unforced assigned tasks: the analysed population.
    pub analysed: usize,
    /// Forced assignments, left out of every estimate.
    pub forced_excluded: usize,
    /// Tasks excluded at admission because another experiment owned a
    /// surface.
    pub surface_conflict_excluded: usize,
    /// Tasks excluded at admission because an explicit crew would have
    /// overridden this experiment's crew arms.
    pub explicit_crew_excluded: usize,
    /// Assigned tasks worked in an interactive session rather than by a
    /// job, so no arm treated them; left out of every estimate.
    pub interactive_excluded: usize,
    /// Assigned tasks whose record could not be read.
    pub unreadable: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadoutArm {
    /// Actual completed invocations linked to this arm's enrolled tasks.
    pub effort_breakdown: Vec<orbit_types::telemetry::AgentEffortUsage>,
    pub name: String,
    pub control: bool,
    pub weight: u32,
    pub retired_segment: Option<u32>,
    pub n: usize,
    /// Analysed tasks that recorded at least one explorer-delegation call
    /// during an implement invocation at or after assignment. A task counts
    /// once, however many such calls it recorded.
    pub tasks_with_explorer_call: usize,
    /// Analysed tasks with no recorded explorer call whose implement
    /// invocation at or after assignment lost early tool calls to the bounded
    /// output capture: their uptake is unknown, so `tasks_with_explorer_call`
    /// is a lower bound by this many tasks at most.
    pub tasks_explorer_unobserved: usize,
    pub segments: Vec<SegmentCount>,
    /// Tasks per implementing crew, most first.
    pub crew_mix: Vec<CrewCount>,
    /// The overlay digests recorded on the arm's assignment rows.
    pub assignment_digests: Vec<String>,
    /// The overlay digests the arm's runs received.
    pub run_digests: Vec<String>,
    /// More than one digest within the arm: its treatment changed.
    pub drifted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SegmentCount {
    pub segment: u32,
    pub n: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CrewCount {
    pub crew: String,
    pub tasks: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SampleRatio {
    pub chi_square: Option<f64>,
    pub df: Option<u32>,
    pub p_value: Option<f64>,
    pub alpha: f64,
    /// `p_value` is below `alpha`: the verdict is withheld.
    pub mismatch: bool,
    pub segments: Vec<SampleRatioSegment>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SampleRatioSegment {
    pub segment: u32,
    pub weights: BTreeMap<String, u32>,
    pub observed: BTreeMap<String, usize>,
    pub expected: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Lagging {
    /// Enrolled tasks not yet terminal: most of their outcomes are open.
    pub open_tasks: usize,
    /// Landed tasks still inside the escaped-regression window.
    pub open_regression_windows: usize,
    pub regression_window_days: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadoutMetric {
    pub metric: String,
    /// `mean` of per-task values, or `rate` of per-task 0/1 outcomes.
    pub kind: &'static str,
    /// `lower` or `higher` is better.
    pub better: &'static str,
    pub unit: &'static str,
    /// `false` while the metric's source series does not exist.
    pub measured: bool,
    pub arms: Vec<ArmEstimate>,
    pub differences: Vec<Difference>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArmEstimate {
    pub arm: String,
    #[serde(flatten)]
    pub estimate: Estimate,
    /// Outcomes still open (lagging).
    pub open: usize,
    /// Settled tasks with no value in the records.
    pub unmeasured: usize,
    /// Settled-task cost provenance, present only for `cost_usd`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_coverage: Option<CostCoverage>,
    pub segments: Vec<SegmentEstimate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CostCoverage {
    /// Every invocation has a valid provider-reported cost.
    pub reported: usize,
    /// Every invocation is priced, using a token-price estimate for at least one.
    pub estimated: usize,
    /// No invocations, or at least one invocation cannot be priced.
    pub unmeasured: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SegmentEstimate {
    pub segment: u32,
    #[serde(flatten)]
    pub estimate: Estimate,
}

/// n, mean and median with 95% intervals: bootstrap for a mean metric,
/// Wilson for a rate (whose `mean` is the rate).
#[derive(Debug, Clone, Serialize)]
pub struct Estimate {
    pub n: usize,
    pub mean: Option<f64>,
    pub mean_interval: Option<Interval>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_interval: Option<Interval>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub successes: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Difference {
    pub arm: String,
    /// Arm minus control, post-stratified by complexity.
    pub overall: DifferenceEstimate,
    pub by_complexity: Vec<ComplexityDifference>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DifferenceEstimate {
    pub estimate: Option<f64>,
    pub interval: Option<Interval>,
    /// Complexity cells only one arm has a value in, left out.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cells_dropped: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComplexityDifference {
    pub complexity: String,
    pub n_arm: usize,
    pub n_control: usize,
    pub estimate: Option<f64>,
    pub interval: Option<Interval>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GuardrailReadout {
    pub metric: String,
    pub threshold: String,
    pub arms: Vec<GuardrailArm>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GuardrailArm {
    pub arm: String,
    /// The threshold in the metric's units.
    pub limit: Option<f64>,
    pub estimate: Option<f64>,
    pub interval: Option<Interval>,
    /// `ok`, `breached` (point estimate past the threshold), `straddles`
    /// (interval spans it) or `unknown` (no difference to read).
    pub status: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    /// `collecting`, `withheld`, `winner`, `no_difference` or
    /// `inconclusive`.
    pub state: &'static str,
    pub arm: Option<String>,
    /// `collecting`, `no verdict`, `winner <arm>`, `no difference` or
    /// `inconclusive`.
    pub label: String,
    pub horizon_reached: bool,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadoutWarning {
    /// `sample_ratio`, `drift`, `lagging`, `primary_unmeasured`,
    /// `first_edit_incomplete`, `tool_trace_incomplete`,
    /// `explorer_trace_incomplete`, `crew_mismatch` or
    /// `crew_mismatch_unavailable`.
    pub kind: &'static str,
    pub message: String,
}
