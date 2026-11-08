use std::collections::{BTreeMap, BTreeSet};

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
    /// The primary metric over delivered runs only, beside the
    /// intention-to-treat `metrics`; present when a treatment arm's
    /// delivery is checked.
    pub per_protocol: Option<PerProtocolReadout>,
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
    /// The per-task value for a mean or rate primary. For a per-line
    /// (ratio) primary this is the task's numerator (`cost_usd` or
    /// `lead_time`), not a per-task ratio: an arm's value is the sum of
    /// numerators over the sum of `lines`, never a mean of ratios.
    pub value: f64,
    /// The task's measured reviewable lines for a per-line primary;
    /// `None` for a mean or rate primary. Zero-line and
    /// unavailable-denominator tasks have no observation at all.
    pub lines: Option<u64>,
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
    /// The fixed 0.9 minimum delivered rate: below it in any
    /// treatment arm the verdict is withheld.
    pub min_delivered_rate: f64,
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
    /// Whether the arm's treatment reached its tasks; `None` for the control
    /// and for an arm whose overlay changes no dispatch (crew or config
    /// only).
    pub delivery: Option<ArmDelivery>,
    /// A memory arm's tasks by the week of the crew memory revision their
    /// first dispatch received. A memory arm's notes change weekly by
    /// design, so a revision change is a stratum here, never drift; only a
    /// renderer change (part of every run digest) drifts the arm.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub memory_strata: Vec<MemoryRevisionStratum>,
}

/// The memory-arm tasks whose first delivered crew memory revision was
/// written in one ISO week.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryRevisionStratum {
    /// ISO week of the delivered revision (`2026-W45`), or `empty` for tasks
    /// that received an empty block because their model had no revision.
    pub week: String,
    pub tasks: usize,
    /// Distinct memory content digests the stratum's tasks received.
    pub digests: Vec<String>,
}

/// How many of an arm's analysed tasks received its treatment: every
/// activity role the overlay targets had a dispatch that received the
/// overlay, in a step that then finished successfully.
#[derive(Debug, Clone, Serialize)]
pub struct ArmDelivery {
    /// The activity roles the arm's overlay targets.
    pub roles: Vec<String>,
    /// Analysed tasks assigned to the arm.
    pub assigned: usize,
    pub delivered: usize,
    /// Tasks still in flight whose treatment has not been delivered yet;
    /// left out of `rate`.
    pub pending: usize,
    /// `delivered / (assigned - pending)`; `None` while no task is settled.
    pub rate: Option<f64>,
}

/// The primary metric over delivered runs only (per-protocol): each
/// checked treatment arm keeps only its delivered tasks, other arms all
/// theirs.
#[derive(Debug, Clone, Serialize)]
pub struct PerProtocolReadout {
    /// `per-protocol (delivered runs only)`.
    pub label: &'static str,
    pub metric: ReadoutMetric,
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
    /// Step-local metrics are exploratory and cannot determine a verdict.
    pub exploratory: bool,
    /// `mean` of per-task values, `rate` of per-task 0/1 outcomes, or
    /// `ratio` of summed numerators over summed reviewable lines.
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
    /// Done tasks whose reviewable-line total is zero. Present only for a
    /// per-line metric, including when the count is zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zero_lines_excluded: Option<usize>,
    /// Done tasks whose reviewable lines were not recorded. Present only for
    /// a per-line metric, and counted apart from [`Self::zero_lines_excluded`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_lines_excluded: Option<usize>,
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
/// Wilson for a rate (whose `mean` is the rate), or a ratio of sums whose
/// `mean` is that ratio and whose interval resamples tasks in pairs.
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
    /// `sample_ratio`, `drift`, `undelivered`, `lagging`, `primary_unmeasured`,
    /// `first_edit_incomplete`, `tool_trace_incomplete`,
    /// `explorer_trace_incomplete`, `crew_mismatch` or
    /// `crew_mismatch_unavailable`.
    pub kind: &'static str,
    pub message: String,
}

use orbit_types::workflow::TagInstructionRole;

/// Whether an arm does better with a lower or a higher value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Better {
    Lower,
    Higher,
}

/// How a metric is summarised: a mean of per-task values, a rate of
/// per-task 0/1 outcomes, or a ratio of sums over reviewable lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Mean,
    Rate,
    /// Arm total numerator divided by arm total reviewable lines.
    Ratio,
}

pub(super) struct MetricSpec {
    pub(super) name: &'static str,
    pub(super) kind: Kind,
    pub(super) better: Better,
    pub(super) unit: &'static str,
    /// `false` for a metric whose source series does not exist yet.
    pub(super) measured: bool,
}

const fn spec(
    name: &'static str,
    kind: Kind,
    better: Better,
    unit: &'static str,
    measured: bool,
) -> MetricSpec {
    MetricSpec {
        name,
        kind,
        better,
        unit,
        measured,
    }
}

/// Every metric an experiment may name, in the definition's order.
pub(super) const METRIC_SPECS: &[MetricSpec] = &[
    spec(
        "implement_tokens",
        Kind::Mean,
        Better::Lower,
        "normalized_tokens",
        true,
    ),
    spec(
        "total_tokens",
        Kind::Mean,
        Better::Lower,
        "normalized_tokens",
        true,
    ),
    spec("cost_usd", Kind::Mean, Better::Lower, "usd", true),
    spec("lead_time", Kind::Mean, Better::Lower, "hours", true),
    spec(
        "implement_duration",
        Kind::Mean,
        Better::Lower,
        "minutes",
        true,
    ),
    spec(
        "turns_before_first_edit",
        Kind::Mean,
        Better::Lower,
        "turns",
        true,
    ),
    spec(
        "result_bytes_before_first_edit",
        Kind::Mean,
        Better::Lower,
        "bytes",
        true,
    ),
    spec(
        "first_pass_acceptance",
        Kind::Rate,
        Better::Higher,
        "rate",
        true,
    ),
    spec("review_repairs", Kind::Mean, Better::Lower, "count", true),
    spec("step_recoveries", Kind::Mean, Better::Lower, "count", true),
    spec("final_recoveries", Kind::Mean, Better::Lower, "count", true),
    spec("failed_checks", Kind::Mean, Better::Lower, "count", true),
    spec("merge_conflicts", Kind::Mean, Better::Lower, "count", true),
    spec(
        "delivery_done_rate",
        Kind::Rate,
        Better::Higher,
        "rate",
        true,
    ),
    spec(
        "escaped_regressions",
        Kind::Mean,
        Better::Lower,
        "count",
        true,
    ),
    spec("lines_changed", Kind::Mean, Better::Lower, "lines", true),
    spec(
        "cost_per_line",
        Kind::Ratio,
        Better::Lower,
        "usd_per_line",
        true,
    ),
    spec(
        "lead_time_per_line",
        Kind::Ratio,
        Better::Lower,
        "hours_per_line",
        true,
    ),
];

pub(super) fn metric_spec(name: &str) -> Option<&'static MetricSpec> {
    METRIC_SPECS.iter().find(|spec| spec.name == name)
}

/// One task's value for one metric.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Outcome {
    Value(f64),
    /// Not settled yet: the task is in flight, or its regression window is
    /// still open.
    Open,
    /// Settled, but the records hold no value (no covered invocation, no
    /// review, nothing landed).
    Unmeasured,
}

/// Reviewable added plus deleted lines for one enrolled task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReviewableDenominator {
    /// The task is not done, so a denominator is not due.
    NotDone,
    /// Done, and no landed-diff row in the read window.
    Missing,
    /// Done, with a measured total. Zero is a real zero, not a missing row.
    Measured(u64),
    /// Done, but the row has no recoverable denominator.
    Unavailable,
}

/// One enrolled, unforced task with its outcomes.
#[derive(Clone)]
pub(super) struct TaskOutcome {
    pub(super) task_id: String,
    pub(super) arm: String,
    pub(super) segment: u32,
    pub(super) complexity: String,
    /// The crew pool recorded at admission for the task's latest
    /// delivery, when that evidence exists [ORB-19487]. `None` means the
    /// historical pool is unavailable, not that the crew mismatched.
    pub(super) admitted_pool: Option<BTreeSet<String>>,
    /// What `admitted_pool` is, for a mismatch message.
    pub(super) admitted_pool_label: Option<String>,
    /// `task.crew`, the crew of the task's latest delivery [ORB-19416].
    pub(super) crew: String,
    pub(super) terminal: bool,
    /// Latest admission applied this arm; absent for legacy evidence.
    pub(super) admitted_treatment: Option<bool>,
    /// An implement invocation at or after assignment recorded an
    /// explorer-delegation tool call.
    pub(super) explorer_call: bool,
    /// No explorer call was recorded, but an implement invocation at or after
    /// assignment lost its early tool calls to the bounded output capture, so
    /// the absence is unproven [ORB-20572].
    pub(super) explorer_unobserved: bool,
    /// The task's first implement invocation holds only the suffix of its tool
    /// calls, so its first-edit score is withheld rather than measured.
    pub(super) first_edit_trace_incomplete: bool,
    pub(super) assigned_at: DateTime<Utc>,
    pub(super) assignment_digest: Option<String>,
    /// Activity roles in which a dispatch received this task's arm overlay
    /// and the dispatching step then finished successfully, read from the
    /// run audit trail (`experiment_overlay_applied` joined to the same
    /// run's `step_finished`).
    pub(super) delivered_roles: BTreeSet<TagInstructionRole>,
    /// The crew memory the task's first memory-bearing dispatch received.
    pub(super) memory: Option<TaskMemoryDelivery>,
    /// Present only for settled tasks with complete invocation cost coverage.
    pub(super) cost_source: Option<CostSource>,
    /// Reviewable lines recorded for this task. Per-line metrics read this
    /// instead of a per-task ratio in `values`.
    pub(super) reviewable: ReviewableDenominator,
    pub(super) values: BTreeMap<&'static str, Outcome>,
}

/// The crew memory one task's first memory-bearing dispatch received.
#[derive(Clone)]
pub(super) struct TaskMemoryDelivery {
    /// ISO week of the delivered revision, or `None` for an empty block.
    pub(super) week: Option<String>,
    pub(super) digest: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CostSource {
    Reported,
    Estimated,
}

/// What the records say about the enrolled tasks.
pub(super) struct Observed {
    pub(super) tasks: Vec<TaskOutcome>,
    /// The shared scan could not confirm tasks' earliest implement invocations.
    pub(super) first_edit_incomplete: bool,
    /// Each arm's overlay digests as its runs received them.
    pub(super) run_digests: BTreeMap<String, BTreeSet<String>>,
    /// Landed tasks whose regression window is still open.
    pub(super) open_regression_windows: usize,
    /// Assigned tasks worked in an interactive session rather than by a
    /// job: not treated by any arm, so left out of every estimate.
    pub(super) interactive_excluded: usize,
}
