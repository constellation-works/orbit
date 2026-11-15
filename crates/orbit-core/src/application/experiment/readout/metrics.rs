use std::collections::BTreeSet;

use orbit_store::contracts::ExperimentRecord;

use crate::application::experiment::definition::{Better, Kind, MetricSpec, metric_spec};
use crate::application::experiment::readout::model::{
    CostSource, Outcome, ReviewableDenominator, TaskOutcome,
};
use crate::application::experiment::readout::stats::Cell;

use super::model::{
    ArmEstimate, ComplexityDifference, CostCoverage, Difference, DifferenceEstimate, Estimate,
    ReadoutMetric, SegmentEstimate,
};
use super::stats;
use crate::application::statistics;

pub(super) fn values<'a>(tasks: impl Iterator<Item = &'a TaskOutcome>, metric: &str) -> Vec<f64> {
    tasks
        .filter_map(|task| match task.values.get(metric) {
            Some(Outcome::Value(value)) => Some(*value),
            _ => None,
        })
        .collect()
}

pub(super) fn estimate(label: &str, spec: &MetricSpec, values: &[f64]) -> Estimate {
    let n = values.len();
    match spec.kind {
        Kind::Rate => {
            let successes = values.iter().filter(|value| **value > 0.5).count();
            Estimate {
                n,
                mean: stats::mean(values),
                mean_interval: statistics::wilson_95(successes, n),
                median: None,
                median_interval: None,
                successes: Some(successes),
            }
        }
        Kind::Mean => Estimate {
            n,
            mean: stats::mean(values),
            mean_interval: stats::bootstrap(&format!("{label}:mean"), values, |sample| {
                stats::mean(sample)
            }),
            median: statistics::median(&mut values.to_vec()),
            median_interval: stats::bootstrap(
                &format!("{label}:median"),
                values,
                statistics::median,
            ),
            successes: None,
        },
        // A ratio is not a mean of per-task values. Callers use `ratio_estimate`.
        Kind::Ratio => Estimate {
            n: 0,
            mean: None,
            mean_interval: None,
            median: None,
            median_interval: None,
            successes: None,
        },
    }
}

fn ratio_estimate(label: &str, pairs: &[stats::WeightedPair]) -> Estimate {
    Estimate {
        n: pairs.len(),
        mean: stats::ratio_of_sums(pairs),
        mean_interval: stats::paired_ratio_bootstrap(&format!("{label}:ratio"), pairs),
        median: None,
        median_interval: None,
        successes: None,
    }
}

pub(super) fn included_pair(task: &TaskOutcome, metric: &str) -> Option<stats::WeightedPair> {
    let numerator = metric_spec(metric)?.line_numerator?;
    let ReviewableDenominator::Measured(lines) = task.reviewable else {
        return None;
    };
    if lines == 0 {
        return None;
    }
    match task.values.get(numerator) {
        Some(Outcome::Value(value)) => Some(stats::WeightedPair {
            numerator: *value,
            lines,
        }),
        _ => None,
    }
}

fn line_pairs<'a>(
    tasks: impl Iterator<Item = &'a TaskOutcome>,
    metric: &str,
) -> Vec<stats::WeightedPair> {
    tasks
        .filter_map(|task| included_pair(task, metric))
        .collect()
}

/// Open, unmeasured, zero-line and unavailable counts for one arm.
/// Zero-line and unavailable tasks are excluded even when their numerator
/// is missing. A non-terminal task is open. A done task with no row, or a
/// terminal task that is not done, is unmeasured.
fn ratio_counts<'a>(
    tasks: impl Iterator<Item = &'a TaskOutcome>,
    metric: &str,
) -> (usize, usize, usize, usize) {
    let numerator = metric_spec(metric)
        .and_then(|spec| spec.line_numerator)
        .unwrap_or("");
    let mut open = 0;
    let mut unmeasured = 0;
    let mut zero = 0;
    let mut unavailable = 0;
    for task in tasks {
        if !task.terminal {
            open += 1;
            continue;
        }
        match task.reviewable {
            ReviewableDenominator::Measured(0) => zero += 1,
            ReviewableDenominator::Unavailable => unavailable += 1,
            ReviewableDenominator::Measured(_) => {
                if !matches!(task.values.get(numerator), Some(Outcome::Value(_))) {
                    unmeasured += 1;
                }
            }
            ReviewableDenominator::NotDone | ReviewableDenominator::Missing => unmeasured += 1,
        }
    }
    (open, unmeasured, zero, unavailable)
}

pub(super) fn metric_readout(
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
            let per_line = spec.kind == Kind::Ratio;
            let (open, unmeasured, zero_lines, unavailable_lines) = if per_line {
                ratio_counts(mine(), spec.name)
            } else {
                (
                    count(&arm.name, Outcome::Open),
                    count(&arm.name, Outcome::Unmeasured),
                    0,
                    0,
                )
            };
            ArmEstimate {
                arm: arm.name.clone(),
                estimate: if per_line {
                    ratio_estimate(
                        &format!("{}:{}", spec.name, arm.name),
                        &line_pairs(mine(), spec.name),
                    )
                } else {
                    estimate(
                        &format!("{}:{}", spec.name, arm.name),
                        spec,
                        &values(mine(), spec.name),
                    )
                },
                open,
                unmeasured,
                cost_coverage: (spec.name == "cost_usd").then(|| CostCoverage {
                    reported: mine()
                        .filter(|task| task.cost_source == Some(CostSource::Reported))
                        .count(),
                    estimated: mine()
                        .filter(|task| task.cost_source == Some(CostSource::Estimated))
                        .count(),
                    unmeasured: count(&arm.name, Outcome::Unmeasured),
                }),
                zero_lines_excluded: per_line.then_some(zero_lines),
                unavailable_lines_excluded: per_line.then_some(unavailable_lines),
                segments: segments
                    .into_iter()
                    .map(|segment| SegmentEstimate {
                        segment,
                        estimate: if per_line {
                            ratio_estimate(
                                &format!("{}:{}:{segment}", spec.name, arm.name),
                                &line_pairs(
                                    mine().filter(|task| task.segment == segment),
                                    spec.name,
                                ),
                            )
                        } else {
                            estimate(
                                &format!("{}:{}:{segment}", spec.name, arm.name),
                                spec,
                                &values(mine().filter(|task| task.segment == segment), spec.name),
                            )
                        },
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
        exploratory: spec.step_local,
        kind: match spec.kind {
            Kind::Mean => "mean",
            Kind::Rate => "rate",
            Kind::Ratio => "ratio",
        },
        better: match spec.better {
            Better::Lower => "lower",
            Better::Higher => "higher",
        },
        unit: spec.unit,
        measured: true,
        arms,
        differences,
    }
}

pub(super) fn difference(
    spec: &MetricSpec,
    control: &str,
    arm: &str,
    tasks: &[TaskOutcome],
) -> Difference {
    if spec.kind == Kind::Ratio {
        return ratio_difference(spec, control, arm, tasks);
    }
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

/// Treatment ratio of sums minus control ratio of sums. Overall ignores
/// complexity weights. A complexity cell is dropped when only one arm has
/// an included task there, and its interval needs two included tasks a side.
fn ratio_difference(
    spec: &MetricSpec,
    control: &str,
    arm: &str,
    tasks: &[TaskOutcome],
) -> Difference {
    let pairs_of = |name: &str, complexity: Option<&str>| {
        line_pairs(
            tasks.iter().filter(|task| {
                task.arm == name && complexity.is_none_or(|wanted| task.complexity == wanted)
            }),
            spec.name,
        )
    };
    let complexities: BTreeSet<&str> = tasks
        .iter()
        .filter(|task| task.arm == control || task.arm == arm)
        .map(|task| task.complexity.as_str())
        .collect();
    let mut cells = Vec::new();
    for complexity in complexities {
        cells.push((
            complexity.to_string(),
            pairs_of(control, Some(complexity)),
            pairs_of(arm, Some(complexity)),
        ));
    }
    let by_complexity = cells
        .iter()
        .filter(|(_, control_pairs, treatment)| !control_pairs.is_empty() || !treatment.is_empty())
        .map(|(complexity, control_pairs, treatment)| {
            let result = stats::paired_ratio_difference(
                &format!("{}:{arm}-{control}:{complexity}", spec.name),
                control_pairs,
                treatment,
            );
            ComplexityDifference {
                complexity: complexity.clone(),
                n_arm: treatment.len(),
                n_control: control_pairs.len(),
                estimate: result.map(|(estimate, _)| estimate),
                interval: result.and_then(|(_, interval)| interval),
            }
        })
        .collect();
    let control_all = pairs_of(control, None);
    let treatment_all = pairs_of(arm, None);
    let overall = stats::paired_ratio_difference(
        &format!("{}:{arm}-{control}", spec.name),
        &control_all,
        &treatment_all,
    );
    Difference {
        arm: arm.to_string(),
        overall: DifferenceEstimate {
            estimate: overall.map(|(estimate, _)| estimate),
            interval: overall.and_then(|(_, interval)| interval),
            cells_dropped: cells
                .iter()
                .filter(|(_, control_pairs, treatment)| {
                    (control_pairs.is_empty() || treatment.is_empty())
                        && !(control_pairs.is_empty() && treatment.is_empty())
                })
                .map(|(complexity, _, _)| complexity.clone())
                .collect(),
        },
        by_complexity,
    }
}

use super::outcomes::REGRESSION_WINDOW_DAYS;
use chrono::{DateTime, Duration, Utc};
use orbit_types::task::{Task, TaskHistoryEntry, TaskRelationType, TaskStatus};
use orbit_types::workflow::{PR_AUTO_FIX_ADMITTED_EVENT, PrAutoFixAttempt, PrAutoFixBlocker};
use serde::Deserialize;
use std::collections::BTreeMap;
/// The task history event of an auto-fix started outside the run.
const AUTO_FIX_OUT_OF_RUN_EVENT: &str = "pr_auto_fix_out_of_run";

/// The head and blocker an out-of-run auto-fix start names.
#[derive(Deserialize)]
struct OutOfRunStart {
    pr_number: String,
    head_sha: String,
    blocker: PrAutoFixBlocker,
}

/// When the task last moved to done.
pub(super) fn landing(history: &[TaskHistoryEntry]) -> Option<DateTime<Utc>> {
    history
        .iter()
        .rev()
        .find(|entry| entry.to_status == Some(TaskStatus::Done))
        .map(|entry| entry.at)
}

/// `regression_from` filings per landed culprit, filed within the window
/// after it landed, whatever their escape class.
pub(super) fn regressions(
    tasks: &[Task],
    landed_at: &BTreeMap<String, DateTime<Utc>>,
) -> BTreeMap<String, u64> {
    let window = Duration::days(REGRESSION_WINDOW_DAYS);
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for task in tasks {
        let culprits: BTreeSet<String> = task
            .relations
            .iter()
            .filter(|relation| relation.relation_type == TaskRelationType::RegressionFrom)
            .map(|relation| relation.target.to_string())
            .collect();
        for culprit in culprits {
            if let Some(at) = landed_at.get(&culprit)
                && task.created_at >= *at
                && task.created_at <= *at + window
            {
                *counts.entry(culprit).or_default() += 1;
            }
        }
    }
    counts
}

/// Distinct pull-request heads the task's auto-fix history saw fail a
/// required check.
pub(super) fn failed_check_heads(history: &[TaskHistoryEntry]) -> usize {
    let mut heads = BTreeSet::new();
    for entry in history {
        let Some(note) = entry.note.as_deref() else {
            continue;
        };
        let (pr, head, blocker) = if entry.event == PR_AUTO_FIX_ADMITTED_EVENT {
            let Ok(attempt) = serde_json::from_str::<PrAutoFixAttempt>(note) else {
                continue;
            };
            (attempt.pr_number, attempt.head_sha, attempt.blocker)
        } else if entry.event == AUTO_FIX_OUT_OF_RUN_EVENT {
            let Ok(start) = serde_json::from_str::<OutOfRunStart>(note) else {
                continue;
            };
            (start.pr_number, start.head_sha, start.blocker)
        } else {
            continue;
        };
        if blocker == PrAutoFixBlocker::FailedChecks {
            heads.insert((pr, head));
        }
    }
    heads.len()
}
