use std::collections::BTreeSet;

use orbit_store::contracts::ExperimentRecord;

use crate::application::experiment::readout::outcomes::{
    Better, CostSource, Kind, MetricSpec, Outcome, TaskOutcome,
};
use crate::application::experiment::readout::stats::Cell;

use super::model::{
    ArmEstimate, ComplexityDifference, CostCoverage, Difference, DifferenceEstimate, Estimate,
    ReadoutMetric, SegmentEstimate,
};
use super::stats;

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
            ArmEstimate {
                arm: arm.name.clone(),
                estimate: estimate(
                    &format!("{}:{}", spec.name, arm.name),
                    spec,
                    &values(mine(), spec.name),
                ),
                open: count(&arm.name, Outcome::Open),
                unmeasured: count(&arm.name, Outcome::Unmeasured),
                cost_coverage: (spec.name == "cost_usd").then(|| CostCoverage {
                    reported: mine()
                        .filter(|task| task.cost_source == Some(CostSource::Reported))
                        .count(),
                    estimated: mine()
                        .filter(|task| task.cost_source == Some(CostSource::Estimated))
                        .count(),
                    unmeasured: count(&arm.name, Outcome::Unmeasured),
                }),
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
        exploratory: super::super::definition::experiment_metric_is_step_local(spec.name),
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

pub(super) fn difference(
    spec: &MetricSpec,
    control: &str,
    arm: &str,
    tasks: &[TaskOutcome],
) -> Difference {
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
