//! Captured review admission: the effective review policy a delivery run
//! carries in its immutable input [ORB-11333].
//!
//! Resolution order at submission: a parent-authorized child inherits its
//! parent's snapshot exactly; a grant-bound run resolves from the grant's
//! captured policy; every other delivery run resolves from workspace
//! configuration at that moment. Ordinary input naming the reserved key is
//! refused, and a resume carries its persisted input forward unchanged.
//! `before-pr` on `task_local_pipeline` is local-only final delivery except
//! when the parent job is `epic_pipeline`, which assembles onto an epic
//! branch and still gates the combined PR-bound candidate.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_config::OperationPolicy;
use orbit_types::workflow::{
    OperationAdmission, REVIEW_ADMISSION_KEY, REVIEW_CONTRACT_VERSION, ReviewAdmission,
    ReviewTiming,
};
use serde_json::Value;

use super::{EPIC_JOB, LOCAL_ROUTE_JOB, REVIEW_ADMITTED_JOBS};
use crate::OrbitRuntime;
use crate::application::operation::captured_policy;

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

    let (inherited, parent_job) = match parent_run_id {
        Some(parent_run_id) => parent_review_lineage(runtime, parent_run_id)?,
        None => (None, None),
    };
    if inherited.is_none() && declares_review_admission(input) {
        return Err(reserved_review_key_error(job_name));
    }

    let admission = match inherited {
        Some(admission) => admission,
        None => resolve_from_authority(runtime, input)?,
    };
    if job_name == LOCAL_ROUTE_JOB
        && admission.timing == ReviewTiming::BeforePr
        && parent_job.as_deref() != Some(EPIC_JOB)
    {
        return Err(OrbitError::InvalidInput(
            "operation.review_policy 'before-pr' holds PR creation for a reviewer and has no \
             meaning on the local-only delivery route; ship through the PR route or choose \
             'none' or 'after-landing' for local delivery"
                .to_string(),
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

/// The snapshot a parent run persisted, if it carries one, together with
/// the parent job id used to recognize epic assembly.
fn parent_review_lineage(
    runtime: &OrbitRuntime,
    parent_run_id: &str,
) -> Result<(Option<ReviewAdmission>, Option<String>), OrbitError> {
    let Some(parent) = runtime.get_job_run_backend(parent_run_id)? else {
        return Ok((None, None));
    };
    let admission = parent
        .input
        .as_ref()
        .map(ReviewAdmission::from_run_input)
        .transpose()
        .map_err(OrbitError::InvalidInput)?
        .flatten();
    Ok((admission, Some(parent.job_id)))
}

/// Resolve from the grant a run was admitted under, else from the workspace
/// preferences at this moment.
fn resolve_from_authority(
    runtime: &OrbitRuntime,
    input: &Value,
) -> Result<ReviewAdmission, OrbitError> {
    let operation = OperationAdmission::from_run_input(input).map_err(OrbitError::InvalidInput)?;
    match operation {
        Some(operation) => {
            let grant = runtime.operation_grant(&operation.grant_id)?;
            let policy = captured_policy(&grant)?;
            Ok(snapshot(&policy, &format!("grant:{}", grant.id)))
        }
        None => Ok(snapshot(runtime.operation_policy(), "")),
    }
}

/// Build the snapshot from a resolved policy. `authority_suffix` names the
/// grant a captured policy came from so diagnostics can explain the source.
pub(crate) fn snapshot(policy: &OperationPolicy, authority_suffix: &str) -> ReviewAdmission {
    let label = |source: String| {
        if authority_suffix.is_empty() {
            source
        } else {
            format!("{source}@{authority_suffix}")
        }
    };
    ReviewAdmission {
        contract_version: REVIEW_CONTRACT_VERSION,
        policy_version: policy.version,
        timing: policy.review_policy.value.timing(),
        timing_source: label(policy.review_policy.source.label()),
        crew: policy.review_crew.value.clone(),
        crew_source: label(policy.review_crew.source.label()),
        budget: policy.review_budget(),
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
        .map_err(OrbitError::InvalidInput)
        .map(Option::flatten)
}

fn reserved_review_key_error(job_name: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "run input for job '{job_name}' set the reserved `{REVIEW_ADMISSION_KEY}` field; the \
         effective review policy is captured from configuration or the admitting grant at \
         submission and cannot be requested through ordinary job input"
    ))
}
