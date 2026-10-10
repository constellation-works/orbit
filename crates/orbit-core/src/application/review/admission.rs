//! Captured review admission: the `review.before_pr` or
//! `review.before_landing` switch, minutes and crew a delivery run carries in
//! its immutable input [ORB-11333] [ORB-13992] [ORB-14849].
//!
//! Resolution order at submission: a parent-authorized child inherits its
//! parent's snapshot exactly; every other delivery run resolves from
//! workspace configuration at that moment. Ordinary input naming the reserved
//! key is refused, and a resume carries its persisted input forward
//! unchanged.
//! `review.before_pr` or `review.before_landing` on `task_local_pipeline` is
//! local-only final delivery with no PR, and is refused outright. After-landing review is the `delivery-code-review`
//! auto-task, not an admission value, so it never reaches a snapshot.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::workflow::{
    REVIEW_ADMISSION_KEY, REVIEW_CONTRACT_VERSION, ReviewAdmission, ReviewTiming,
};
use serde_json::Value;

use super::{LOCAL_ROUTE_JOB, REVIEW_ADMITTED_JOBS};
use crate::OrbitRuntime;

/// Whether caller-shaped run input names the reserved review key.
fn declares_review_admission(input: &Value) -> bool {
    input
        .get(REVIEW_ADMISSION_KEY)
        .is_some_and(|value| !value.is_null())
}

/// Install the review admission a delivery submission carries, or refuse a
/// submission that tries to supply one. `parent_run_id` is set for the
/// parent-authorized child path; `resuming` keeps persisted input as is.
pub(crate) fn install_review_admission(
    runtime: &OrbitRuntime,
    job_name: &str,
    input: &mut Value,
    parent_run_id: Option<&str>,
    resuming: bool,
) -> Result<(), OrbitError> {
    if resuming {
        return Ok(());
    }
    if !REVIEW_ADMITTED_JOBS.contains(&job_name) {
        if declares_review_admission(input) {
            return Err(reserved_review_key_error(job_name));
        }
        return Ok(());
    }

    let inherited = match parent_run_id {
        Some(parent_run_id) => parent_review_admission(runtime, parent_run_id)?,
        None => None,
    };
    if inherited.is_none() && declares_review_admission(input) {
        return Err(reserved_review_key_error(job_name));
    }

    let admission = match inherited {
        Some(admission) => admission,
        None => snapshot(runtime),
    };
    if job_name == LOCAL_ROUTE_JOB && admission.timing == ReviewTiming::BeforePr {
        return Err(OrbitError::InvalidInput(
            local_route_before_pr_refusal().to_string(),
        ));
    }
    if job_name == LOCAL_ROUTE_JOB && admission.timing == ReviewTiming::BeforeLanding {
        return Err(OrbitError::InvalidInput(
            local_route_before_landing_refusal().to_string(),
        ));
    }

    let object = input.as_object_mut().ok_or_else(|| {
        OrbitError::InvalidInput("pipeline run input must be a JSON object".to_string())
    })?;
    object.insert(
        REVIEW_ADMISSION_KEY.to_string(),
        serde_json::to_value(&admission).map_err(|error| {
            OrbitError::Execution(format!("serialize review admission: {error}"))
        })?,
    );
    Ok(())
}

/// The snapshot a parent run persisted, if it carries one.
fn parent_review_admission(
    runtime: &OrbitRuntime,
    parent_run_id: &str,
) -> Result<Option<ReviewAdmission>, OrbitError> {
    let Some(parent) = runtime.get_job_run_backend(parent_run_id)? else {
        return Ok(None);
    };
    parent
        .input
        .as_ref()
        .map(ReviewAdmission::from_run_input)
        .transpose()
        .map_err(OrbitError::from)
        .map(Option::flatten)
}

/// The fail-closed sentence `install_review_admission` returns for
/// `review.before_pr` on `task_local_pipeline`. Pre-dispatch surfaces quote
/// it so an operator sees one explanation before and after a spawn.
pub(crate) fn local_route_before_pr_refusal() -> &'static str {
    "review.before_pr holds PR creation for a reviewer and has no meaning on the \
     local-only delivery route; ship through the PR route or turn review.before_pr off \
     for local delivery (after-landing review is the delivery-code-review auto-task)"
}

/// That refusal with the deciding layer inserted after the key, for doctor
/// and readiness. `source` is the operation-layer label (`global`,
/// `workspace`, or `built-in`).
pub(crate) fn local_route_before_pr_conflict(source: &str) -> String {
    with_source(local_route_before_pr_refusal(), "review.before_pr", source)
}

/// The fail-closed sentence `install_review_admission` returns for
/// `review.before_landing` on `task_local_pipeline` [ORB-14849]: a local
/// delivery opens no pull request to review before it lands.
pub(crate) fn local_route_before_landing_refusal() -> &'static str {
    "review.before_landing reviews an open pull request before it lands and has no meaning \
     on the local-only delivery route, which opens none; ship through the PR route or turn \
     review.before_landing off for local delivery (after-landing review is the \
     delivery-code-review auto-task)"
}

/// [`local_route_before_landing_refusal`] with its deciding layer, for
/// doctor and readiness.
pub(crate) fn local_route_before_landing_conflict(source: &str) -> String {
    with_source(
        local_route_before_landing_refusal(),
        "review.before_landing",
        source,
    )
}

fn with_source(refusal: &str, key: &str, source: &str) -> String {
    format!("{key} ({source}){}", refusal.trim_start_matches(key))
}

/// Build the snapshot from the workspace's resolved review settings, keeping
/// each field's provenance so diagnostics can explain where it came from.
pub(crate) fn snapshot(runtime: &OrbitRuntime) -> ReviewAdmission {
    let policy = runtime.operation_policy();
    ReviewAdmission {
        contract_version: REVIEW_CONTRACT_VERSION,
        policy_version: policy.version,
        // Configuration never resolves both switches on.
        timing: if policy.review_before_pr.value {
            ReviewTiming::BeforePr
        } else if policy.review_before_landing.value {
            ReviewTiming::BeforeLanding
        } else {
            ReviewTiming::None
        },
        timing_source: if policy.review_before_landing.value && !policy.review_before_pr.value {
            policy.review_before_landing.source.label().to_string()
        } else {
            policy.review_before_pr.source.label().to_string()
        },
        crew: policy.review_crew.value.clone(),
        crew_source: policy.review_crew.source.label().to_string(),
        budget: policy.review_budget(),
        required_validation_commands: Some(
            runtime.workflow_required_validation_commands().to_vec(),
        ),
        baseline_commands: runtime.review_baseline_commands().to_vec(),
        host_evidence: policy.review_host_evidence.value.clone(),
        captured_at: Utc::now(),
    }
}

/// The review admission a running pipeline was admitted under, read from its
/// persisted run input.
pub(crate) fn run_review_admission(
    runtime: &OrbitRuntime,
    run_id: &str,
) -> Result<Option<ReviewAdmission>, OrbitError> {
    let Some(run) = runtime.get_job_run_backend(run_id)? else {
        return Ok(None);
    };
    run.input
        .as_ref()
        .map(ReviewAdmission::from_run_input)
        .transpose()
        .map_err(OrbitError::from)
        .map(Option::flatten)
}

/// Explain why an automatically resumed delivery run cannot safely keep its
/// captured review admission. A resume reuses successful checkpoints, so
/// installing today's admission into its input would not rerun an already
/// successful review gate. The clock therefore resumes only when the captured
/// admission still matches today's contract.
pub(crate) fn upgrade_resume_admission_mismatch(
    runtime: &OrbitRuntime,
    run: &orbit_types::workflow::JobRun,
) -> Option<String> {
    if !super::REVIEW_ADMITTED_JOBS.contains(&run.job_id.as_str()) {
        return None;
    }
    let Some(input) = run.input.as_ref() else {
        return Some("run has no captured review admission".to_string());
    };
    let previous = match ReviewAdmission::from_run_input(input) {
        Ok(Some(admission)) => admission,
        Ok(None) => return Some("run has no captured review admission".to_string()),
        Err(error) => return Some(error.to_string()),
    };
    let current = snapshot(runtime);
    let mut comparable_previous = previous;
    // Capture time is provenance, not part of the admission contract.
    comparable_previous.captured_at = current.captured_at;
    (comparable_previous != current).then(|| {
        "the captured review admission differs from the current review policy; submit a new run to capture it"
            .to_string()
    })
}

fn reserved_review_key_error(job_name: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "run input for job '{job_name}' set the reserved `{REVIEW_ADMISSION_KEY}` field; the \
         effective review.before_pr and review.before_landing settings are captured from \
         configuration at submission and cannot be requested through ordinary job input"
    ))
}
