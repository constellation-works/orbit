//! Task/job adapters retain existing creation and evidence provenance boundaries.
use super::{COVERAGE_ARTIFACT, source::Source};
use crate::OrbitRuntime;
use orbit_automation::{
    AutomationError,
    delivery::{ActionOutcome, digest, evidence::EvidenceFacts},
};
use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::automation::*;
use orbit_types::workflow::{AutoTaskDefinition, JobRunState};

pub(super) fn mint(
    runtime: &OrbitRuntime,
    definition: &AutoTaskDefinition,
    attempt: &BatchAttempt,
) -> Result<String, OrbitError> {
    let mut params = crate::application::auto_tasks::scheduler::template_params(definition);
    params.description.push_str(&format!("\n\nFrozen automation input (inspect these exact revisions):\n```json\n{}\n```\nSubmit versioned coverage evidence as {} with orbit.task.artifact.put. Examination, including findings, must be complete before setting examination_complete=true. Task completion alone does not certify coverage.",serde_json::to_string_pretty(attempt).map_err(|e|OrbitError::InvalidInput(e.to_string()))?,COVERAGE_ARTIFACT));
    params.description.push_str(&format!("\n\nEvidence template (replace action_id with this task ID and fill actual checks/findings):\n```json\n{}\n```",serde_json::to_string_pretty(&orbit_types::workflow::automation::evidence_template(attempt)).map_err(|e|OrbitError::InvalidInput(e.to_string()))?));
    runtime
        .add_task_admitted(params, None, None, Some(&attempt.action_key))
        .map(|task| task.id)
}

pub(super) fn outcome(
    runtime: &OrbitRuntime,
    source: &Source<'_>,
    attempt: &BatchAttempt,
) -> Result<ActionOutcome, AutomationError> {
    let Some(id) = attempt.action_id.as_deref() else {
        return Ok(ActionOutcome::Pending);
    };
    let task = runtime.get_task(id)?;
    let artifact = runtime.get_task_artifact(id, COVERAGE_ARTIFACT)?;
    if let Some(artifact) = artifact {
        let manifest = runtime.get_task_artifact_manifest(id)?;
        let provenance = manifest.iter().find(|f| f.path == COVERAGE_ARTIFACT);
        if let Some(provenance) = provenance {
            let owner = evidence_owner(runtime, &task, &provenance.sha256)?;
            return Ok(ActionOutcome::Evidence(EvidenceFacts {
                bytes: artifact.content,
                reference: format!("task:{id}/artifacts/{COVERAGE_ARTIFACT}"),
                submitted_by: owner
                    .clone()
                    .unwrap_or_else(|| provenance.created_by.clone()),
                artifact_digest: provenance.sha256.clone(),
                authorized: owner.is_some(),
                source_verified: source.verify_batch(&attempt.batch).is_ok(),
            }));
        }
    }
    if matches!(
        task.status,
        TaskStatus::Done | TaskStatus::Rejected | TaskStatus::Archived
    ) {
        return Ok(ActionOutcome::Failed {
            retryable: false,
            reason: "task_closed_without_accepted_evidence".into(),
        });
    }
    Ok(ActionOutcome::Pending)
}
pub(super) fn job_outcome(
    runtime: &OrbitRuntime,
    source: &Source<'_>,
    attempt: &BatchAttempt,
) -> Result<ActionOutcome, AutomationError> {
    let Some(id) = attempt.action_id.as_deref() else {
        return Ok(ActionOutcome::Pending);
    };
    let run = runtime.show_job_run(id)?;
    // Only a canonical persisted step result can attest job-only examination.
    let input = run.input.as_ref();
    let expected =
        serde_json::to_value(attempt).map_err(|e| AutomationError::Evidence(e.to_string()))?;
    if ["batch", "input_digest", "attempt", "action_key"]
        .iter()
        .any(|key| {
            input
                .and_then(|v| v.get("automation"))
                .and_then(|v| v.get(key))
                != expected.get(key)
        })
    {
        return Err(AutomationError::Deferred("job_input_mismatch".into()));
    }
    for step in run.steps.iter().rev() {
        if let Some(value) = step
            .agent_response_json
            .as_ref()
            .and_then(|v| v.get("coverage_evidence"))
        {
            let bytes =
                serde_json::to_vec(value).map_err(|e| AutomationError::Evidence(e.to_string()))?;
            return Ok(ActionOutcome::Evidence(EvidenceFacts {
                artifact_digest: digest(&bytes),
                bytes,
                reference: format!("run:{id}/step:{}", step.step_index),
                submitted_by: format!("run:{id}"),
                authorized: true,
                source_verified: source.verify_batch(&attempt.batch).is_ok(),
            }));
        }
    }
    if matches!(
        run.state,
        JobRunState::Failed
            | JobRunState::Cancelled
            | JobRunState::Interrupted
            | JobRunState::Success
    ) && crate::application::job::run_owner_liveness(&run)
        == crate::application::job::RunOwnerLiveness::Stopped
    {
        return Ok(ActionOutcome::Failed {
            retryable: run.state == JobRunState::Failed,
            reason: "job_stopped_without_accepted_evidence".into(),
        });
    }
    Ok(ActionOutcome::Pending)
}

fn evidence_owner(
    runtime: &OrbitRuntime,
    task: &orbit_types::task::Task,
    artifact_digest: &str,
) -> Result<Option<String>, AutomationError> {
    let Some(artifact) = runtime.get_task_artifact(&task.id, EVIDENCE_AUTHORITY_ARTIFACT)? else {
        return Ok(None);
    };
    let Ok(submission) = serde_json::from_slice::<EvidenceSubmission>(&artifact.content) else {
        return Ok(None);
    };
    if submission.action_id != task.id
        || submission.evidence_digest != artifact_digest
        || task.job_run_id.as_deref() != Some(&submission.run_id)
    {
        return Ok(None);
    }
    let run = runtime.show_job_run(&submission.run_id)?;
    let assigned = run.input.as_ref().is_some_and(|input| {
        input.get("task_id").and_then(serde_json::Value::as_str) == Some(&task.id)
            || input
                .get("task_ids")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|ids| ids.iter().any(|v| v.as_str() == Some(&task.id)))
    });
    Ok(assigned.then(|| format!("run:{}", submission.run_id)))
}
