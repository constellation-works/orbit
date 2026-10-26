//! An experiment's readout, computed on read from its assignment rows joined
//! to the outcome records Orbit already keeps. The unit is the task and the
//! analysis is intention-to-treat: every enrolled, unforced task counts in
//! its arm whatever its outcome; forced assignments are left out and
//! counted.

mod outcomes;
mod stats;

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{ExperimentRecord, ExperimentStoreBackend as _};
use serde::Serialize;
use serde_json::Value;

use self::outcomes::{
    Better, Kind, METRIC_SPECS, MetricSpec, Observed, Outcome, REGRESSION_WINDOW_DAYS, TaskOutcome,
    metric_spec,
};
use self::stats::{Cell, Interval};
use super::definition::validate_metric;
use crate::OrbitRuntime;

/// The readout shape's version; bump it on any breaking change.
pub const EXPERIMENT_READOUT_SCHEMA_VERSION: u32 = 1;
/// Below this sample-ratio p-value the assignment is suspect.
const SAMPLE_RATIO_ALPHA: f64 = 0.001;

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
    /// Assigned tasks whose record could not be read.
    pub unreadable: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadoutArm {
    pub name: String,
    pub control: bool,
    pub weight: u32,
    pub retired_segment: Option<u32>,
    pub n: usize,
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
    pub segments: Vec<SegmentEstimate>,
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
    /// `sample_ratio`, `drift`, `lagging` or `primary_unmeasured`.
    pub kind: &'static str,
    pub message: String,
}

impl OrbitRuntime {
    /// The readout of the experiment whose id, or else whose name, is
    /// `id_or_name`; `metric` narrows the metric list to one.
    pub fn experiment_readout(
        &self,
        id_or_name: &str,
        metric: Option<&str>,
    ) -> Result<ExperimentReadout, OrbitError> {
        if let Some(metric) = metric {
            validate_metric("metric", metric)?;
        }
        let workspace = self.workspace_id()?;
        let store = self.sqlite_store()?;
        let record = store
            .experiment_record(&workspace, id_or_name)?
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!("no experiment `{id_or_name}` in this workspace"))
            })?;
        let rows = store.experiment_assignments(&workspace, &record.experiment.id)?;
        let now = Utc::now();
        let enrolled: Vec<_> = rows
            .iter()
            .filter(|row| row.arm.is_some() && !row.forced)
            .collect();
        let observed = self.experiment_outcomes(
            &record.experiment.id,
            record.experiment.created_at,
            &enrolled,
            now,
        )?;
        let enrollment = ReadoutEnrollment {
            analysed: observed.tasks.len(),
            forced_excluded: rows.iter().filter(|row| row.forced).count(),
            surface_conflict_excluded: rows
                .iter()
                .filter(|row| row.arm.is_none() && row.exclusion_reason.is_some())
                .count(),
            unreadable: enrolled.len() - observed.tasks.len(),
        };
        build_readout(&record, enrollment, &observed, metric, now)
    }
}

fn build_readout(
    record: &ExperimentRecord,
    enrollment: ReadoutEnrollment,
    observed: &Observed,
    metric: Option<&str>,
    now: DateTime<Utc>,
) -> Result<ExperimentReadout, OrbitError> {
    let experiment = &record.experiment;
    let guardrails: BTreeMap<String, String> = serde_json::from_str(&experiment.guardrails_json)
        .map_err(|error| OrbitError::Store(format!("stored guardrails are not JSON: {error}")))?;
    let started_at = record
        .events
        .iter()
        .find(|event| event.kind == "started")
        .map(|event| event.at);
    let horizon_at = started_at
        .zip(experiment.max_days)
        .map(|(at, days)| at + Duration::days(i64::from(days)));
    let control = record
        .arms
        .iter()
        .find(|arm| arm.control)
        .map(|arm| arm.name.clone());
    let tasks = &observed.tasks;
    let mut warnings = Vec::new();

    let arms: Vec<ReadoutArm> = record
        .arms
        .iter()
        .map(|arm| arm_readout(arm, tasks, observed.run_digests.get(&arm.name)))
        .collect();
    for arm in arms.iter().filter(|arm| arm.drifted) {
        warnings.push(ReadoutWarning {
            kind: "drift",
            message: format!(
                "arm `{}` received more than one overlay digest ({}); its runs before and after the change are not the same treatment",
                arm.name,
                arm.assignment_digests
                    .iter()
                    .chain(&arm.run_digests)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }

    let sample_ratio = sample_ratio(record, tasks);
    if sample_ratio.mismatch {
        warnings.push(ReadoutWarning {
            kind: "sample_ratio",
            message: format!(
                "arm counts do not match the weights (chi-square p = {:.2e} < {SAMPLE_RATIO_ALPHA}); the assignment is suspect and no verdict is given",
                sample_ratio.p_value.unwrap_or(0.0)
            ),
        });
    }

    let lagging = Lagging {
        open_tasks: tasks.iter().filter(|task| !task.terminal).count(),
        open_regression_windows: observed.open_regression_windows,
        regression_window_days: REGRESSION_WINDOW_DAYS,
    };
    if lagging.open_tasks > 0 || lagging.open_regression_windows > 0 {
        warnings.push(ReadoutWarning {
            kind: "lagging",
            message: format!(
                "{} enrolled task(s) are still in flight and {} landed task(s) are inside the {REGRESSION_WINDOW_DAYS}-day regression window; their outcomes are open",
                lagging.open_tasks, lagging.open_regression_windows
            ),
        });
    }

    let all_metrics: Vec<ReadoutMetric> = METRIC_SPECS
        .iter()
        .map(|spec| metric_readout(spec, record, control.as_deref(), tasks))
        .collect();
    let guardrail_readouts: Vec<GuardrailReadout> = guardrails
        .iter()
        .map(|(metric, threshold)| guardrail_readout(metric, threshold, &all_metrics))
        .collect();
    let primary = experiment
        .primary_metric
        .as_deref()
        .and_then(|name| all_metrics.iter().find(|metric| metric.metric == name));
    if let Some(primary) = primary
        && !primary.measured
    {
        warnings.push(ReadoutWarning {
            kind: "primary_unmeasured",
            message: format!(
                "the primary metric `{}` has no source series yet, so no verdict can be read on it",
                primary.metric
            ),
        });
    }
    let horizon_reached = horizon_reached(record, primary, horizon_at, now);
    let verdict = verdict(
        horizon_reached,
        sample_ratio.mismatch,
        started_at.is_some(),
        primary,
        &guardrail_readouts,
    );
    let mut primary_observations: Vec<PrimaryObservation> = experiment
        .primary_metric
        .as_deref()
        .and_then(metric_spec)
        .map(|spec| {
            tasks
                .iter()
                .filter_map(|task| match task.values.get(spec.name) {
                    Some(Outcome::Value(value)) => Some(PrimaryObservation {
                        arm: task.arm.clone(),
                        assigned_at: task.assigned_at,
                        value: *value,
                    }),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    primary_observations.sort_by_key(|observation| observation.assigned_at);
    let metrics = match metric {
        Some(name) => all_metrics
            .into_iter()
            .filter(|metric| metric.metric == name)
            .collect(),
        None => all_metrics,
    };
    Ok(ExperimentReadout {
        schema_version: EXPERIMENT_READOUT_SCHEMA_VERSION,
        experiment: ReadoutExperiment {
            id: experiment.id.clone(),
            name: experiment.name.clone(),
            status: experiment.status.clone(),
            mode: experiment.mode.clone(),
            segment: experiment.segment,
            primary_metric: experiment.primary_metric.clone(),
            guardrails,
            min_sample: experiment.min_sample_per_arm,
            max_days: experiment.max_days,
            started_at: started_at.map(|at| at.to_rfc3339()),
            horizon_at: horizon_at.map(|at| at.to_rfc3339()),
        },
        as_of: now.to_rfc3339(),
        enrollment,
        arms,
        sample_ratio,
        lagging,
        metrics,
        guardrails: guardrail_readouts,
        verdict,
        warnings,
        primary_observations,
    })
}

fn arm_readout(
    arm: &orbit_store::contracts::ExperimentArmRow,
    tasks: &[TaskOutcome],
    run_digests: Option<&BTreeSet<String>>,
) -> ReadoutArm {
    let mine: Vec<&TaskOutcome> = tasks.iter().filter(|task| task.arm == arm.name).collect();
    let mut segments: BTreeMap<u32, usize> = BTreeMap::new();
    let mut crews: BTreeMap<&str, usize> = BTreeMap::new();
    let mut assignment_digests = BTreeSet::new();
    for task in &mine {
        *segments.entry(task.segment).or_default() += 1;
        *crews.entry(task.crew.as_str()).or_default() += 1;
        if let Some(digest) = &task.assignment_digest {
            assignment_digests.insert(digest.clone());
        }
    }
    let mut crew_mix: Vec<CrewCount> = crews
        .into_iter()
        .map(|(crew, tasks)| CrewCount {
            crew: crew.to_string(),
            tasks,
        })
        .collect();
    crew_mix.sort_by(|left, right| {
        right
            .tasks
            .cmp(&left.tasks)
            .then(left.crew.cmp(&right.crew))
    });
    let run_digests: Vec<String> = run_digests.into_iter().flatten().cloned().collect();
    ReadoutArm {
        name: arm.name.clone(),
        control: arm.control,
        weight: arm.weight,
        retired_segment: arm.retired_segment,
        n: mine.len(),
        segments: segments
            .into_iter()
            .map(|(segment, n)| SegmentCount { segment, n })
            .collect(),
        crew_mix,
        drifted: assignment_digests.len() > 1 || run_digests.len() > 1,
        assignment_digests: assignment_digests.into_iter().collect(),
        run_digests,
    }
}

/// Each segment's active-arm weights, from the start and reweight events.
fn segment_weights(record: &ExperimentRecord) -> BTreeMap<u32, BTreeMap<String, u32>> {
    let mut weights = BTreeMap::new();
    for event in &record.events {
        let Ok(detail) = serde_json::from_str::<Value>(&event.detail_json) else {
            continue;
        };
        let (segment, map) = match event.kind.as_str() {
            "started" => (detail.get("segment"), detail.get("weights")),
            "reweighted" | "arm_retired" => (detail.get("segment_to"), detail.get("weights_after")),
            _ => continue,
        };
        let segment = segment
            .and_then(Value::as_u64)
            .and_then(|segment| u32::try_from(segment).ok());
        let map =
            map.and_then(|map| serde_json::from_value::<BTreeMap<String, u32>>(map.clone()).ok());
        if let (Some(segment), Some(map)) = (segment, map) {
            weights.insert(segment, map);
        }
    }
    weights
}

fn sample_ratio(record: &ExperimentRecord, tasks: &[TaskOutcome]) -> SampleRatio {
    let mut segments = Vec::new();
    let mut groups = Vec::new();
    for (segment, weights) in segment_weights(record) {
        let in_segment: Vec<&TaskOutcome> = tasks
            .iter()
            .filter(|task| task.segment == segment)
            .collect();
        if in_segment.is_empty() {
            continue;
        }
        let total_weight: u64 = weights.values().map(|weight| u64::from(*weight)).sum();
        let n = in_segment.len() as f64;
        let mut observed = BTreeMap::new();
        let mut expected = BTreeMap::new();
        let mut group = Vec::new();
        for (arm, weight) in &weights {
            let count = in_segment.iter().filter(|task| &task.arm == arm).count();
            let share = if total_weight == 0 {
                0.0
            } else {
                n * f64::from(*weight) / total_weight as f64
            };
            observed.insert(arm.clone(), count);
            expected.insert(arm.clone(), share);
            group.push((count as f64, share));
        }
        groups.push(group);
        segments.push(SampleRatioSegment {
            segment,
            weights,
            observed,
            expected,
        });
    }
    let test = stats::chi_square(&groups);
    let p_value = test.map(|(_, _, p)| p);
    SampleRatio {
        chi_square: test.map(|(statistic, _, _)| statistic),
        df: test.map(|(_, df, _)| df),
        p_value,
        alpha: SAMPLE_RATIO_ALPHA,
        mismatch: p_value.is_some_and(|p| p < SAMPLE_RATIO_ALPHA),
        segments,
    }
}

fn values<'a>(tasks: impl Iterator<Item = &'a TaskOutcome>, metric: &str) -> Vec<f64> {
    tasks
        .filter_map(|task| match task.values.get(metric) {
            Some(Outcome::Value(value)) => Some(*value),
            _ => None,
        })
        .collect()
}

fn estimate(label: &str, spec: &MetricSpec, values: &[f64]) -> Estimate {
    let n = values.len();
    match spec.kind {
        Kind::Rate => {
            let successes = values.iter().filter(|value| **value > 0.5).count();
            Estimate {
                n,
                mean: stats::mean(values),
                mean_interval: stats::wilson(successes, n),
                median: None,
                median_interval: None,
                successes: Some(successes),
            }
        }
        Kind::Mean => Estimate {
            n,
            mean: stats::mean(values),
            mean_interval: stats::bootstrap(&format!("{label}:mean"), values, stats::mean),
            median: stats::median(values),
            median_interval: stats::bootstrap(&format!("{label}:median"), values, stats::median),
            successes: None,
        },
    }
}

fn metric_readout(
    spec: &MetricSpec,
    record: &ExperimentRecord,
    control: Option<&str>,
    tasks: &[TaskOutcome],
) -> ReadoutMetric {
    let count = |arm: &str, wanted: Outcome| {
        tasks
            .iter()
            .filter(|task| task.arm == arm && task.values.get(spec.name) == Some(&wanted))
            .count()
    };
    let arms = record
        .arms
        .iter()
        .map(|arm| {
            let mine = || tasks.iter().filter(|task| task.arm == arm.name);
            let segments: BTreeSet<u32> = mine().map(|task| task.segment).collect();
            ArmEstimate {
                arm: arm.name.clone(),
                estimate: estimate(
                    &format!("{}:{}", spec.name, arm.name),
                    spec,
                    &values(mine(), spec.name),
                ),
                open: count(&arm.name, Outcome::Open),
                unmeasured: count(&arm.name, Outcome::Unmeasured),
                segments: segments
                    .into_iter()
                    .map(|segment| SegmentEstimate {
                        segment,
                        estimate: estimate(
                            &format!("{}:{}:{segment}", spec.name, arm.name),
                            spec,
                            &values(mine().filter(|task| task.segment == segment), spec.name),
                        ),
                    })
                    .collect(),
            }
        })
        .collect();
    let differences = control
        .map(|control| {
            record
                .arms
                .iter()
                .filter(|arm| arm.name != control)
                .map(|arm| difference(spec, control, &arm.name, tasks))
                .collect()
        })
        .unwrap_or_default();
    ReadoutMetric {
        metric: spec.name.to_string(),
        kind: match spec.kind {
            Kind::Mean => "mean",
            Kind::Rate => "rate",
        },
        better: match spec.better {
            Better::Lower => "lower",
            Better::Higher => "higher",
        },
        unit: spec.unit,
        measured: spec.measured,
        arms,
        differences,
    }
}

fn difference(spec: &MetricSpec, control: &str, arm: &str, tasks: &[TaskOutcome]) -> Difference {
    let complexities: BTreeSet<&str> = tasks
        .iter()
        .filter(|task| task.arm == control || task.arm == arm)
        .map(|task| task.complexity.as_str())
        .collect();
    let mut cells = Vec::new();
    for complexity in complexities {
        let of = |name: &str| {
            values(
                tasks
                    .iter()
                    .filter(|task| task.arm == name && task.complexity == complexity),
                spec.name,
            )
        };
        cells.push((complexity.to_string(), of(control), of(arm)));
    }
    let by_complexity = cells
        .iter()
        .filter(|(_, control, treatment)| !control.is_empty() || !treatment.is_empty())
        .map(|(complexity, control_values, treatment)| {
            let result = stats::stratified_difference(
                &format!("{}:{arm}-{control}:{complexity}", spec.name),
                &[Cell {
                    control: control_values,
                    treatment,
                }],
            );
            ComplexityDifference {
                complexity: complexity.clone(),
                n_arm: treatment.len(),
                n_control: control_values.len(),
                estimate: result.map(|(estimate, _)| estimate),
                interval: result.and_then(|(_, interval)| interval),
            }
        })
        .collect();
    let overall = stats::stratified_difference(
        &format!("{}:{arm}-{control}", spec.name),
        &cells
            .iter()
            .map(|(_, control, treatment)| Cell { control, treatment })
            .collect::<Vec<_>>(),
    );
    Difference {
        arm: arm.to_string(),
        overall: DifferenceEstimate {
            estimate: overall.map(|(estimate, _)| estimate),
            interval: overall.and_then(|(_, interval)| interval),
            cells_dropped: cells
                .iter()
                .filter(|(_, control, treatment)| {
                    (control.is_empty() || treatment.is_empty())
                        && !(control.is_empty() && treatment.is_empty())
                })
                .map(|(complexity, _, _)| complexity.clone())
                .collect(),
        },
        by_complexity,
    }
}

/// A guardrail threshold: its direction (`true` for an upper limit) and
/// its value in the metric's units, given the control's mean.
fn guardrail_limit(
    spec: &MetricSpec,
    threshold: &str,
    control_mean: Option<f64>,
) -> Option<(bool, f64)> {
    let upper = threshold.starts_with('+');
    let rest = threshold.get(1..)?;
    let (number, unit) = if let Some(number) = rest.strip_suffix("pt") {
        (number, "pt")
    } else if let Some(number) = rest.strip_suffix('%') {
        (number, "%")
    } else {
        (rest, "")
    };
    let magnitude: f64 = number.parse().ok()?;
    let magnitude = match unit {
        // Percentage points of a rate, which the readout keeps as a fraction.
        "pt" if spec.kind == Kind::Rate => magnitude / 100.0,
        "pt" => magnitude,
        "%" => magnitude / 100.0 * control_mean?.abs(),
        _ => magnitude,
    };
    Some((upper, if upper { magnitude } else { -magnitude }))
}

fn guardrail_readout(metric: &str, threshold: &str, metrics: &[ReadoutMetric]) -> GuardrailReadout {
    let readout = metrics.iter().find(|candidate| candidate.metric == metric);
    let spec = metric_spec(metric);
    let control_mean = readout.and_then(|readout| {
        let differenced: BTreeSet<&str> =
            readout.differences.iter().map(|d| d.arm.as_str()).collect();
        readout
            .arms
            .iter()
            .find(|arm| !differenced.contains(arm.arm.as_str()))
            .and_then(|arm| arm.estimate.mean)
    });
    let arms = readout
        .map(|readout| {
            readout
                .differences
                .iter()
                .map(|difference| {
                    let limit =
                        spec.and_then(|spec| guardrail_limit(spec, threshold, control_mean));
                    let estimate = difference.overall.estimate;
                    let interval = difference.overall.interval;
                    let status = match (limit, estimate) {
                        (Some((upper, limit)), Some(estimate)) => {
                            let past =
                                |value: f64| if upper { value > limit } else { value < limit };
                            if past(estimate) {
                                "breached"
                            } else if interval.is_none_or(|[low, high]| past(low) || past(high)) {
                                "straddles"
                            } else {
                                "ok"
                            }
                        }
                        _ => "unknown",
                    };
                    GuardrailArm {
                        arm: difference.arm.clone(),
                        limit: limit.map(|(_, limit)| limit),
                        estimate,
                        interval,
                        status,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    GuardrailReadout {
        metric: metric.to_string(),
        threshold: threshold.to_string(),
        arms,
    }
}

/// Every active arm has `min_sample` primary outcomes, or `max_days` have
/// passed since start.
fn horizon_reached(
    record: &ExperimentRecord,
    primary: Option<&ReadoutMetric>,
    horizon_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    if horizon_at.is_some_and(|at| now >= at) {
        return true;
    }
    let (Some(min), Some(primary)) = (record.experiment.min_sample_per_arm, primary) else {
        return false;
    };
    record
        .arms
        .iter()
        .filter(|arm| arm.retired_segment.is_none())
        .all(|arm| {
            primary
                .arms
                .iter()
                .find(|estimate| estimate.arm == arm.name)
                .is_some_and(|estimate| estimate.estimate.n >= min as usize)
        })
}

fn verdict(
    horizon_reached: bool,
    mismatch: bool,
    started: bool,
    primary: Option<&ReadoutMetric>,
    guardrails: &[GuardrailReadout],
) -> Verdict {
    let make =
        |state: &'static str, arm: Option<String>, label: String, reasons: Vec<String>| Verdict {
            state,
            arm,
            label,
            horizon_reached,
            reasons,
        };
    if mismatch {
        return make(
            "withheld",
            None,
            "no verdict".into(),
            vec!["the sample-ratio check failed".into()],
        );
    }
    if !started || !horizon_reached {
        return make(
            "collecting",
            None,
            "collecting".into(),
            vec!["not every arm has reached min_sample and max_days has not passed".into()],
        );
    }
    let Some(primary) = primary.filter(|primary| primary.measured) else {
        return make(
            "inconclusive",
            None,
            "inconclusive".into(),
            vec!["the primary metric is not measured".into()],
        );
    };
    let higher = primary.better == "higher";
    let mut winners: Vec<(f64, &str)> = Vec::new();
    let mut reasons = Vec::new();
    let mut inconclusive = false;
    for difference in &primary.differences {
        let arm = difference.arm.as_str();
        let (Some(estimate), Some([low, high])) =
            (difference.overall.estimate, difference.overall.interval)
        else {
            reasons.push(format!("arm `{arm}` has no interval on {}", primary.metric));
            continue;
        };
        let better = if higher { low > 0.0 } else { high < 0.0 };
        if !better {
            reasons.push(format!(
                "arm `{arm}`'s {} interval does not exclude zero in the better direction",
                primary.metric
            ));
            continue;
        }
        let statuses: Vec<(&str, &str)> = guardrails
            .iter()
            .filter_map(|guardrail| {
                guardrail
                    .arms
                    .iter()
                    .find(|candidate| candidate.arm == arm)
                    .map(|candidate| (guardrail.metric.as_str(), candidate.status))
            })
            .collect();
        if let Some((metric, _)) = statuses.iter().find(|(_, status)| *status == "breached") {
            inconclusive = true;
            reasons.push(format!("arm `{arm}` breaches the {metric} guardrail"));
        } else if let Some((metric, status)) = statuses
            .iter()
            .find(|(_, status)| matches!(*status, "straddles" | "unknown"))
        {
            inconclusive = true;
            reasons.push(format!("arm `{arm}`'s {metric} guardrail {status}"));
        } else {
            winners.push((estimate, arm));
        }
    }
    // The largest improvement wins when several treatments do.
    winners.sort_by(|left, right| {
        let order = left.0.total_cmp(&right.0);
        if higher { order.reverse() } else { order }
    });
    if let Some((_, arm)) = winners.first() {
        return make(
            "winner",
            Some((*arm).to_string()),
            format!("winner {arm}"),
            reasons,
        );
    }
    if inconclusive {
        make("inconclusive", None, "inconclusive".into(), reasons)
    } else {
        make("no_difference", None, "no difference".into(), reasons)
    }
}
