//! Task/job adapters retain existing creation and evidence provenance boundaries.

use super::COVERAGE_ARTIFACT;
use crate::host::{AutomationHost, RunOwnerLiveness};
use crate::source::Source;
use crate::{
    AutomationError,
    delivery::{ActionOutcome, digest, evidence::EvidenceFacts},
};
use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::automation::*;
use orbit_types::workflow::{AutoTaskDefinition, JobRunState};

/// Mint the task one admitted delivery attempt owes, carrying the frozen
/// input, the evidence template, and any reissue provenance in its description.
pub fn mint<H: AutomationHost>(
    host: &H,
    definition: &AutoTaskDefinition,
    attempt: &BatchAttempt,
) -> Result<String, OrbitError> {
    let mut params = crate::auto_tasks::scheduler::template_params(definition);

    let invalid = |e: serde_json::Error| OrbitError::InvalidInput(e.to_string());
    let frozen_input = serde_json::to_string_pretty(attempt).map_err(invalid)?;
    let evidence_template = serde_json::to_string_pretty(
        &orbit_types::workflow::automation::evidence_template(attempt),
    )
    .map_err(invalid)?;

    params.description.push_str(&format!(
        "\n\nFrozen automation input (inspect these exact revisions):\n```json\n{frozen_input}\n```\nSubmit versioned coverage evidence as {COVERAGE_ARTIFACT} with orbit.task.artifact.put. Examination, including findings, must be complete before setting examination_complete=true. Task completion alone does not certify coverage."
    ));
    params.description.push_str(&format!(
        "\n\nEvidence template (replace action_id with this task ID and fill actual checks/findings):\n```json\n{evidence_template}\n```"
    ));

    // A reissued attempt examines obligations an earlier action left unpaid, so
    // the new task names that action instead of appearing unrelated to it.
    if let Some(reissue) = &attempt.reissue {
        let replaced = reissue
            .from_action_id
            .as_deref()
            .unwrap_or("an unadmitted claim");
        params.description.push_str(&format!(
            "\n\nThis action was reissued by {} on {}: {replaced} closed without accepted coverage evidence. Reason: {}. The obligations above are unchanged; coverage still requires evidence from this task's assigned executor.",
            reissue.by,
            reissue.at.to_rfc3339(),
            reissue.reason
        ));
    }

    host.add_task_admitted(params, &attempt.action_key)
        .map(|task| task.id)
}

pub(crate) fn outcome<H: AutomationHost>(
    host: &H,
    source: &Source<'_>,
    attempt: &BatchAttempt,
) -> Result<ActionOutcome, AutomationError> {
    let Some(id) = attempt.action_id.as_deref() else {
        return Ok(ActionOutcome::Pending);
    };

    let task = host.get_task(id)?;
    let artifact = host.get_task_artifact(id, COVERAGE_ARTIFACT)?;

    if let Some(artifact) = artifact {
        let manifest = host.get_task_artifact_manifest(id)?;
        let provenance = manifest.iter().find(|file| file.path == COVERAGE_ARTIFACT);

        if let Some(provenance) = provenance {
            let owner = evidence_owner(host, &task, &provenance.sha256)?;
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

/// The coverage outcome of an admitted job action, from its canonical
/// persisted step evidence alone.
pub fn job_outcome<H: AutomationHost>(
    host: &H,
    source: &Source<'_>,
    attempt: &BatchAttempt,
) -> Result<ActionOutcome, AutomationError> {
    let Some(id) = attempt.action_id.as_deref() else {
        return Ok(ActionOutcome::Pending);
    };

    let run = host.show_job_run(id)?;

    // Only a canonical persisted step result can attest job-only examination.
    let input = run.input.as_ref();
    let expected =
        serde_json::to_value(attempt).map_err(|e| AutomationError::Evidence(e.to_string()))?;

    if ["batch", "input_digest", "attempt", "action_key"]
        .iter()
        .any(|key| {
            input
                .and_then(|input| input.get("automation"))
                .and_then(|automation| automation.get(key))
                != expected.get(key)
        })
    {
        return Err(AutomationError::Deferred("job_input_mismatch".into()));
    }

    // The most recent step carrying evidence wins.
    for step in run.steps.iter().rev() {
        if let Some(value) = step
            .agent_response_json
            .as_ref()
            .and_then(|response| response.get("coverage_evidence"))
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
    ) && host.run_owner_liveness(&run) == RunOwnerLiveness::Stopped
    {
        return Ok(ActionOutcome::Failed {
            retryable: run.state == JobRunState::Failed,
            reason: "job_stopped_without_accepted_evidence".into(),
        });
    }

    Ok(ActionOutcome::Pending)
}

fn evidence_owner<H: AutomationHost>(
    host: &H,
    task: &orbit_types::task::Task,
    artifact_digest: &str,
) -> Result<Option<String>, AutomationError> {
    let Some(artifact) = host.get_task_artifact(&task.id, EVIDENCE_AUTHORITY_ARTIFACT)? else {
        return Ok(None);
    };
    let Ok(submission) = serde_json::from_slice::<EvidenceSubmission>(&artifact.content) else {
        return Ok(None);
    };

    // The submission has to name this task, this artifact and this task's run.
    if submission.action_id != task.id
        || submission.evidence_digest != artifact_digest
        || task.job_run_id.as_deref() != Some(&submission.run_id)
    {
        return Ok(None);
    }

    let run = host.show_job_run(&submission.run_id)?;
    let assigned = run.input.as_ref().is_some_and(|input| {
        input.get("task_id").and_then(serde_json::Value::as_str) == Some(&task.id)
            || input
                .get("task_ids")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(&task.id)))
    });

    Ok(assigned.then(|| format!("run:{}", submission.run_id)))
}
