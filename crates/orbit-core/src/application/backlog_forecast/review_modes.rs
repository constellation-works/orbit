//! Review evidence is collected once, outside the resampling loop. A
//! reviewer's activity name does not identify its timing: join the captured
//! admission and an executed step in the same workspace run.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::Duration;
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::{
    JobRun, JobRunState, JobTargetType, PipelineState, ReviewAdmission, ReviewGateAdmissionOutput,
    ReviewTiming, ShipMode,
};
use serde::Serialize;

use crate::OrbitRuntime;
use crate::application::task::{TaskJobPort, TaskReviewPort};
use crate::runtime::cost::InvocationSpend;

/// Historical enforced layer, or the layer assumed for the next work.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ReviewMode {
    None,
    BeforePr,
    BeforeLanding,
    Mixed,
    Unknown,
}

impl ReviewMode {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::BeforePr => "before_pr",
            Self::BeforeLanding => "before_landing",
            Self::Mixed => "mixed",
            Self::Unknown => "unknown",
        }
    }

    pub(super) fn from_timing(timing: ReviewTiming) -> Self {
        match timing {
            ReviewTiming::None | ReviewTiming::AfterLanding => Self::None,
            ReviewTiming::BeforePr => Self::BeforePr,
            ReviewTiming::BeforeLanding => Self::BeforeLanding,
        }
    }
}

/// Coverage key for a historical checkpoint the strict decoder refused.
pub(super) const REVIEW_CHECKPOINT_UNREADABLE: &str = "review_checkpoint_unreadable";

/// One historical run whose review checkpoint could not be read. Missing
/// evidence is not a gap: only bytes the strict decoder rejected are.
#[derive(Clone, Debug)]
pub(super) struct ReviewEvidenceGap {
    pub(super) run_id: String,
    pub(super) reason: String,
    pub(super) coverage_gap: &'static str,
}

/// Current policy is resolved through the delivery admission authority. A
/// future light-delivery diff cannot yet be verified, so it retains policy.
pub(super) struct ReviewHistory {
    modes: HashMap<String, BTreeSet<ReviewMode>>,
    pub(super) gaps: BTreeMap<String, ReviewEvidenceGap>,
    current: ReviewAdmission,
}

impl ReviewHistory {
    pub(super) fn collect(
        runtime: &OrbitRuntime,
        spends: &[InvocationSpend],
        task_ids: &BTreeSet<&str>,
    ) -> Result<Self, OrbitError> {
        let mut by_run: BTreeMap<&str, Vec<&InvocationSpend>> = BTreeMap::new();
        for spend in spends {
            if spend
                .task_ids
                .iter()
                .any(|id| task_ids.contains(id.as_str()))
            {
                by_run.entry(&spend.job_run_id).or_default().push(spend);
            }
        }
        let mut modes: HashMap<String, BTreeSet<ReviewMode>> = HashMap::new();
        let mut gaps: BTreeMap<String, ReviewEvidenceGap> = BTreeMap::new();
        for (run_id, invocations) in by_run {
            let run = runtime.stored_job_run(run_id)?;
            // Pilots, sweeps and coordinators are separate work, even when
            // linked to the delivered task. Missing runs remain unknown.
            if run
                .as_ref()
                .is_some_and(|run| !runtime.is_forecast_delivery_run(run))
            {
                continue;
            }
            let enforced = enforced_review_mode(runtime, run.as_ref(), &invocations)?;
            if let Some(gap) = enforced.gap {
                gaps.entry(gap.run_id.clone()).or_insert(gap);
            }
            let mode = enforced.mode;
            for invocation in invocations {
                for id in &invocation.task_ids {
                    if task_ids.contains(id.as_str()) {
                        modes.entry(id.clone()).or_default().insert(mode);
                    }
                }
            }
        }
        Ok(Self {
            modes,
            gaps,
            current: runtime.forecast_review_admission()?,
        })
    }

    pub(super) fn delivery_mode(&self, task_id: &str) -> ReviewMode {
        let Some(modes) = self.modes.get(task_id) else {
            return ReviewMode::Unknown;
        };
        if modes.contains(&ReviewMode::Unknown) {
            ReviewMode::Unknown
        } else if modes.len() > 1 {
            ReviewMode::Mixed
        } else {
            modes.iter().next().copied().unwrap_or(ReviewMode::Unknown)
        }
    }

    /// A running task retains its immutable admission, including a run's
    /// explicit review request. Missing evidence never borrows today's policy.
    pub(super) fn assumed_mode(
        &self,
        runtime: &OrbitRuntime,
        task: &Task,
    ) -> Result<(ReviewMode, String, Option<ReviewEvidenceGap>), OrbitError> {
        if task.status == TaskStatus::InProgress {
            let run = task
                .job_run_id
                .as_deref()
                .map(|run| runtime.stored_job_run(run))
                .transpose()?
                .flatten();
            let captured = run
                .as_ref()
                .and_then(|run| run.input.as_ref())
                .and_then(|input| ReviewAdmission::from_run_input(input).ok().flatten());
            if let Some(run) = &run {
                let checkpoint = forecast_checkpoint(runtime, &run.run_id)?;
                if let Some(gap) = unreadable_gap(&run.run_id, &checkpoint) {
                    return Ok((
                        ReviewMode::Unknown,
                        "unreadable_run_checkpoint".into(),
                        Some(gap),
                    ));
                }
                if review_exempt(&checkpoint, run) {
                    return Ok((ReviewMode::None, "verified_run_exemption".into(), None));
                }
            }
            let (mode, source) = captured.map_or(
                (ReviewMode::Unknown, "missing_run_admission".into()),
                |admission| {
                    (
                        ReviewMode::from_timing(admission.timing),
                        format!("captured:{}", admission.timing_source),
                    )
                },
            );
            return Ok((mode, source, None));
        }
        if task.tags.iter().any(|tag| tag == "no-diff-expected") {
            return Ok((ReviewMode::None, "no_diff_expected".into(), None));
        }
        let mode = ReviewMode::from_timing(self.current.timing);
        let route = runtime.forecast_task_delivery_mode(task);
        let Ok(route) = route else {
            return Ok((
                ReviewMode::Unknown,
                "unresolved_delivery_route".into(),
                None,
            ));
        };
        if route.is_none() {
            return Ok((ReviewMode::Unknown, "custom_delivery_route".into(), None));
        }
        if route == Some(ShipMode::Local) && mode != ReviewMode::None {
            return Ok((
                ReviewMode::Unknown,
                "local_route_review_conflict".into(),
                None,
            ));
        }
        Ok((
            mode,
            format!("current:{}", self.current.timing_source),
            None,
        ))
    }
}

struct EnforcedReview {
    mode: ReviewMode,
    gap: Option<ReviewEvidenceGap>,
}

fn decided(mode: ReviewMode) -> EnforcedReview {
    EnforcedReview { mode, gap: None }
}

/// A historical checkpoint as the forecast may use it. Invalid JSON stays
/// in the store; only this read classifies it, and only as unknown evidence.
enum ForecastCheckpoint {
    Missing,
    Readable(Box<PipelineState>),
    Unreadable { reason: String },
}

fn forecast_checkpoint(
    runtime: &OrbitRuntime,
    run_id: &str,
) -> Result<ForecastCheckpoint, OrbitError> {
    match runtime.stored_run_state(run_id) {
        Ok(None) => Ok(ForecastCheckpoint::Missing),
        Ok(Some(state)) => Ok(ForecastCheckpoint::Readable(Box::new(state))),
        Err(error) => {
            if let OrbitError::Store(message) = &error
                && message.starts_with("invalid pipeline_state_json:")
            {
                let reason = error.to_string();
                tracing::warn!(
                    run_id,
                    reason,
                    "backlog forecast: historical review checkpoint unreadable"
                );
                return Ok(ForecastCheckpoint::Unreadable { reason });
            }
            Err(error)
        }
    }
}

fn unreadable_gap(run_id: &str, checkpoint: &ForecastCheckpoint) -> Option<ReviewEvidenceGap> {
    match checkpoint {
        ForecastCheckpoint::Unreadable { reason } => Some(ReviewEvidenceGap {
            run_id: run_id.to_string(),
            reason: reason.clone(),
            coverage_gap: REVIEW_CHECKPOINT_UNREADABLE,
        }),
        ForecastCheckpoint::Missing | ForecastCheckpoint::Readable(_) => None,
    }
}

fn enforced_review_mode(
    runtime: &OrbitRuntime,
    run: Option<&JobRun>,
    invocations: &[&InvocationSpend],
) -> Result<EnforcedReview, OrbitError> {
    let Some(run) = run else {
        return Ok(decided(ReviewMode::Unknown));
    };
    let admission = run
        .input
        .as_ref()
        .and_then(|input| ReviewAdmission::from_run_input(input).ok().flatten());
    let Some(admission) = admission else {
        return Ok(decided(ReviewMode::Unknown));
    };
    let checkpoint = forecast_checkpoint(runtime, &run.run_id)?;
    if let Some(gap) = unreadable_gap(&run.run_id, &checkpoint) {
        // Unreadable bytes are not a missing row, an exemption, a completed
        // review. Spend on the run stays with the delivery.
        return Ok(EnforcedReview {
            mode: ReviewMode::Unknown,
            gap: Some(gap),
        });
    }
    let mode = ReviewMode::from_timing(admission.timing);
    let exempt = review_exempt(&checkpoint, run);
    let reviewers: Vec<_> = invocations
        .iter()
        .filter(|spend| spend.activity_id.as_deref() == Some("agent_review_repair"))
        .collect();
    let executed_review = !reviewers.is_empty()
        && reviewers.iter().all(|spend| {
            let start =
                spend.ts - Duration::milliseconds(i64::try_from(spend.duration_ms).unwrap_or(0));
            run.steps.iter().any(|step| {
                step.target_type == JobTargetType::Activity
                    && step.target_id == "agent_review_repair"
                    && step
                        .started_at
                        .zip(step.finished_at)
                        .is_some_and(|(from, to)| from <= spend.ts && start <= to && from <= to)
            })
        });
    if !reviewers.is_empty() {
        return Ok(decided(
            if executed_review && !exempt && mode != ReviewMode::None {
                mode
            } else {
                ReviewMode::Unknown
            },
        ));
    }
    if run
        .steps
        .iter()
        .any(|step| step.target_id == "agent_review_repair" && step.started_at.is_some())
    {
        return Ok(decided(ReviewMode::Unknown));
    }
    if exempt {
        return Ok(decided(ReviewMode::None));
    }
    if mode == ReviewMode::None {
        return Ok(decided(mode));
    }
    // Only a recorded gate decision proves that a configured review was
    // exempted. Absence of reviewer telemetry alone proves nothing.
    Ok(decided(ReviewMode::Unknown))
}

fn review_exempt(checkpoint: &ForecastCheckpoint, run: &JobRun) -> bool {
    // An unreadable checkpoint cannot verify an exemption, including one
    // recorded only on the step row.
    if matches!(checkpoint, ForecastCheckpoint::Unreadable { .. }) {
        return false;
    }
    let is_exemption = |output: &serde_json::Value| {
        matches!(serde_json::from_value::<ReviewGateAdmissionOutput>(output.clone()),
            Ok(ReviewGateAdmissionOutput::NotApplicable(decision))
                if matches!(decision.reason.as_str(), "no_diff_exemption" | "skip_review_light_delivery"))
    };
    let from_checkpoint = if let ForecastCheckpoint::Readable(state) = checkpoint {
        state
            .step_output_entries()
            .any(|(_, output)| is_exemption(output))
    } else {
        false
    };
    from_checkpoint
        || run.steps.iter().any(|step| {
            step.target_id == "review_gate_admit"
                && step.state == JobRunState::Success
                && step.agent_response_json.as_ref().is_some_and(is_exemption)
        })
}
