use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{ExperimentRecord, ExperimentStoreBackend as _};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::experiment::assignment::{
    EXPLICIT_CREW_EXCLUSION, SURFACE_CONFLICT_EXCLUSION,
};
use crate::application::experiment::definition::{ExperimentOverlay, validate_metric};
use crate::application::experiment::readout::outcomes::{
    METRIC_SPECS, Observed, Outcome, REGRESSION_WINDOW_DAYS, TaskOutcome, metric_spec,
};

use super::model::{
    CrewCount, EXPERIMENT_READOUT_SCHEMA_VERSION, ExperimentReadout, GuardrailReadout, Lagging,
    PrimaryObservation, ReadoutArm, ReadoutEnrollment, ReadoutExperiment, ReadoutMetric,
    ReadoutWarning, SAMPLE_RATIO_ALPHA, SampleRatio, SampleRatioSegment, SegmentCount,
};
use super::{metrics, stats, verdict};

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
        .map(|spec| metrics::metric_readout(spec, record, control.as_deref(), tasks))
        .collect();
    let guardrail_readouts: Vec<GuardrailReadout> = guardrails
        .iter()
        .map(|(metric, threshold)| verdict::guardrail_readout(metric, threshold, &all_metrics))
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
    let horizon_reached = verdict::horizon_reached(record, primary, horizon_at, now);
    let verdict = verdict::verdict(
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

pub(super) fn sample_ratio(record: &ExperimentRecord, tasks: &[TaskOutcome]) -> SampleRatio {
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
        let excluded = |reason: &str| {
            rows.iter()
                .filter(|row| row.arm.is_none() && row.exclusion_reason.as_deref() == Some(reason))
                .count()
        };
        let enrollment = ReadoutEnrollment {
            analysed: observed.tasks.len(),
            forced_excluded: rows.iter().filter(|row| row.forced).count(),
            surface_conflict_excluded: excluded(SURFACE_CONFLICT_EXCLUSION),
            explicit_crew_excluded: excluded(EXPLICIT_CREW_EXCLUSION),
            unreadable: enrolled.len() - observed.tasks.len(),
        };
        let mut readout = build_readout(&record, enrollment, &observed, metric, now)?;
        readout
            .warnings
            .extend(self.crew_pool_mismatches(&record, &observed.tasks)?);
        Ok(readout)
    }

    /// One `crew_mismatch` warning per analysed task whose crew is outside
    /// its arm's crew pool, for an experiment whose arms overlay the crew
    /// surface. An arm without a crew overlay, such as control, runs the
    /// workspace's complexity pool for the task's tier; an unassessed task
    /// there has no pool to check.
    fn crew_pool_mismatches(
        &self,
        record: &ExperimentRecord,
        tasks: &[TaskOutcome],
    ) -> Result<Vec<ReadoutWarning>, OrbitError> {
        let mut arm_pools: BTreeMap<&str, Option<BTreeSet<String>>> = BTreeMap::new();
        for arm in &record.arms {
            let overlay = ExperimentOverlay::parse(
                &arm.name,
                serde_json::from_str(&arm.overlay_json).map_err(|error| {
                    OrbitError::Store(format!("invalid experiment overlay: {error}"))
                })?,
            )?;
            arm_pools.insert(
                arm.name.as_str(),
                overlay
                    .crew
                    .map(|crew| crew.pool.into_iter().map(|member| member.crew).collect()),
            );
        }
        if arm_pools.values().all(Option::is_none) {
            return Ok(Vec::new());
        }
        let complexity_pools = self.dispatch_crew_pools();
        let mut warnings = Vec::new();
        for task in tasks {
            let (pool, which) = match arm_pools.get(task.arm.as_str()) {
                Some(Some(pool)) => (pool.clone(), "crew pool".to_string()),
                _ => {
                    let Some(tier) = task.tier else {
                        continue;
                    };
                    let Some(names) = complexity_pools.get(tier.as_str()) else {
                        continue;
                    };
                    (
                        names.iter().cloned().collect(),
                        format!("workspace {tier} complexity pool"),
                    )
                }
            };
            if pool.contains(&task.crew) {
                continue;
            }
            warnings.push(ReadoutWarning {
                kind: "crew_mismatch",
                message: format!(
                    "task {} in arm `{}` ran crew `{}`, outside the arm's {which} ({})",
                    task.task_id,
                    task.arm,
                    task.crew,
                    pool.into_iter().collect::<Vec<_>>().join(", ")
                ),
            });
        }
        Ok(warnings)
    }
}
