//! Review evidence is collected once, outside the resampling loop. A
//! reviewer's activity name does not identify its timing: join the captured
//! admission and an executed step in the same workspace run.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::Duration;
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::{
    JobRun, JobRunState, JobTargetType, ReviewAdmission, ReviewGateAdmissionOutput, ReviewTiming,
    SelectiveBeforeLandingMode, ShipMode,
};
use serde::Serialize;

use super::super::{TaskJobPort, TaskReviewPort};
use crate::OrbitRuntime;
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

/// Current policy is resolved through the delivery admission authority. A
/// future light-delivery diff cannot yet be verified, so it retains policy.
pub(super) struct ReviewHistory {
    modes: HashMap<String, BTreeSet<ReviewMode>>,
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
            let mode = enforced_review_mode(runtime, run.as_ref(), &invocations)?;
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
    ) -> Result<(ReviewMode, String), OrbitError> {
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
            if let Some(run) = &run
                && run_review_exempt(runtime, run)?
            {
                return Ok((ReviewMode::None, "verified_run_exemption".into()));
            }
            return Ok(captured.map_or(
                (ReviewMode::Unknown, "missing_run_admission".into()),
                |admission| assumed_selective_mode(&admission, "captured"),
            ));
        }
        if task.tags.iter().any(|tag| tag == "no-diff-expected") {
            return Ok((ReviewMode::None, "no_diff_expected".into()));
        }
        let (mode, selective_source) = match self.current.selective_policy() {
            Some(policy) if self.current.timing == ReviewTiming::None => {
                match policy.mode {
                    // Candidate-dependent: no mode can be assumed before
                    // the pinned diff exists.
                    SelectiveBeforeLandingMode::Enforce => {
                        (ReviewMode::Unknown, Some("selective_pending"))
                    }
                    // Observation never enforces a review.
                    SelectiveBeforeLandingMode::Observe => {
                        (ReviewMode::None, Some("selective_observe"))
                    }
                    SelectiveBeforeLandingMode::Off => {
                        (ReviewMode::from_timing(self.current.timing), None)
                    }
                }
            }
            _ => (ReviewMode::from_timing(self.current.timing), None),
        };
        if let Some(source) = selective_source {
            return Ok((mode, source.to_string()));
        }
        let route = runtime.forecast_task_delivery_mode(task);
        let Ok(route) = route else {
            return Ok((ReviewMode::Unknown, "unresolved_delivery_route".into()));
        };
        if route.is_none() {
            return Ok((ReviewMode::Unknown, "custom_delivery_route".into()));
        }
        if route == Some(ShipMode::Local) && mode != ReviewMode::None {
            return Ok((ReviewMode::Unknown, "local_route_review_conflict".into()));
        }
        Ok((mode, format!("current:{}", self.current.timing_source)))
    }
}

/// The assumed mode of a captured admission: selective enforcement is
/// candidate-dependent (unknown until the pinned diff is evaluated) and
/// observation enforces nothing, so neither reads as a completed
/// before-landing review.
fn assumed_selective_mode(admission: &ReviewAdmission, origin: &str) -> (ReviewMode, String) {
    if admission.timing == ReviewTiming::None
        && let Some(policy) = admission.selective_policy()
    {
        return match policy.mode {
            SelectiveBeforeLandingMode::Enforce => {
                (ReviewMode::Unknown, format!("{origin}:selective_pending"))
            }
            SelectiveBeforeLandingMode::Observe => {
                (ReviewMode::None, format!("{origin}:selective_observe"))
            }
            SelectiveBeforeLandingMode::Off => (
                ReviewMode::from_timing(admission.timing),
                format!("{origin}:{}", admission.timing_source),
            ),
        };
    }
    (
        ReviewMode::from_timing(admission.timing),
        format!("{origin}:{}", admission.timing_source),
    )
}

fn enforced_review_mode(
    runtime: &OrbitRuntime,
    run: Option<&JobRun>,
    invocations: &[&InvocationSpend],
) -> Result<ReviewMode, OrbitError> {
    let Some(run) = run else {
        return Ok(ReviewMode::Unknown);
    };
    let admission = run
        .input
        .as_ref()
        .and_then(|input| ReviewAdmission::from_run_input(input).ok().flatten());
    let Some(admission) = admission else {
        return Ok(ReviewMode::Unknown);
    };
    let mode = ReviewMode::from_timing(admission.timing);
    let exempt = run_review_exempt(runtime, run)?;
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
        // A selectively required review runs as a before-landing review
        // under a `none` timing; the executed reviewer is its proof.
        let selective_enforced = admission.selective_enforced();
        return Ok(
            if executed_review && !exempt && (mode != ReviewMode::None || selective_enforced) {
                if selective_enforced && mode == ReviewMode::None {
                    ReviewMode::BeforeLanding
                } else {
                    mode
                }
            } else {
                ReviewMode::Unknown
            },
        );
    }
    if run
        .steps
        .iter()
        .any(|step| step.target_id == "agent_review_repair" && step.started_at.is_some())
    {
        return Ok(ReviewMode::Unknown);
    }
    if exempt {
        return Ok(ReviewMode::None);
    }
    if admission.selective_enforced() && mode == ReviewMode::None {
        // No reviewer ran under selective enforcement: only the gate's
        // recorded selection distinguishes a skip from a pending or
        // missing decision, so absence of a reviewer proves neither.
        return Ok(match selective_gate_decision(runtime, run)? {
            Some(true) => ReviewMode::Unknown,
            Some(false) => ReviewMode::None,
            None => ReviewMode::Unknown,
        });
    }
    if mode == ReviewMode::None {
        return Ok(mode);
    }
    // Only a recorded gate decision proves that a configured review was
    // exempted. Absence of reviewer telemetry alone proves nothing.
    Ok(ReviewMode::Unknown)
}

/// The run's recorded selective decision: `Some(true)` required,
/// `Some(false)` not required, `None` for unavailable, observe-only, or
/// no recorded decision. Read from the landing gate's checkpointed
/// output, never inferred from reviewer absence.
fn selective_gate_decision(
    runtime: &OrbitRuntime,
    run: &JobRun,
) -> Result<Option<bool>, OrbitError> {
    use orbit_types::workflow::SelectiveReviewResult;
    let state = runtime.stored_run_state(&run.run_id)?;
    let output = state
        .as_ref()
        .and_then(|state| state.pipeline.get("landing_review_gate_admit"));
    let Some(output) = output else {
        return Ok(None);
    };
    let Ok(parsed) = serde_json::from_value::<ReviewGateAdmissionOutput>(output.clone()) else {
        return Ok(None);
    };
    let receipt = match &parsed {
        ReviewGateAdmissionOutput::Admitted(admitted) => {
            return Ok(admitted.selective_receipt.as_ref().map(|_| true));
        }
        ReviewGateAdmissionOutput::NotApplicable(decision) => decision.selective_receipt.as_ref(),
        ReviewGateAdmissionOutput::PreflightPassed { .. } => None,
    };
    Ok(receipt.and_then(|receipt| match receipt.result {
        SelectiveReviewResult::Required => Some(true),
        SelectiveReviewResult::NotRequired => Some(false),
        SelectiveReviewResult::Unavailable => None,
    }))
}

fn run_review_exempt(runtime: &OrbitRuntime, run: &JobRun) -> Result<bool, OrbitError> {
    let is_exemption = |output: &serde_json::Value| {
        matches!(serde_json::from_value::<ReviewGateAdmissionOutput>(output.clone()),
            Ok(ReviewGateAdmissionOutput::NotApplicable(decision))
                if matches!(decision.reason.as_str(), "no_diff_exemption" | "skip_review_light_delivery"))
    };
    let state = runtime.stored_run_state(&run.run_id)?;
    Ok(state.as_ref().is_some_and(|state| {
        state
            .step_output_entries()
            .any(|(_, output)| is_exemption(output))
    }) || run.steps.iter().any(|step| {
        step.target_id == "review_gate_admit"
            && step.state == JobRunState::Success
            && step.agent_response_json.as_ref().is_some_and(is_exemption)
    }))
}
