//! Task/job adapters retain existing creation and evidence provenance boundaries.

use super::{COVERAGE_ARTIFACT, source::Source};
use crate::OrbitRuntime;
use chrono::{DateTime, Utc};
use orbit_automation::{
    AutomationError,
    delivery::{
        ActionOutcome, digest, evidence,
        evidence::{EvidenceFacts, MIN_RATIONALE_CHARS},
    },
};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{DERIVED_EXECUTION_SUMMARY_PREFIX, Task, TaskStatus};
use orbit_types::workflow::automation::*;
use orbit_types::workflow::{AutoTaskDefinition, JobRunState};
use std::collections::BTreeMap;

pub(super) fn mint(
    runtime: &OrbitRuntime,
    definition: &AutoTaskDefinition,
    attempt: &BatchAttempt,
) -> Result<String, OrbitError> {
    let mut params = crate::application::auto_tasks::scheduler::template_params(definition);
    // [ORB-13896] After-landing review is reviewed by `operation.review_crew`,
    // one crew drawn per batch when it is a pool [ORB-15195].
    if let Some(crew) = super::after_landing::minted_review_crew(runtime, definition, attempt)? {
        params.crew = Some(crew);
        params.crew_source = Some(orbit_config::REVIEW_CREW_KEY.to_string());
    }

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
        "\n\nGive every frozen delivery one delivery_examinations entry. Its examined_paths and skipped_paths (each with a reason) must together be exactly the paths `git diff --name-only --no-renames <before.commit> <after.commit>` lists for that delivery. Its verdict is \"clean\" or {{\"findings\": [\"<finding task ID>\", ...]}}, and its rationale (at least {MIN_RATIONALE_CHARS} characters, different for each delivery) says why the verdict holds for that change. Persist an execution summary of what you examined: a review whose agent records none leaves its batch owed."
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

    runtime
        .add_task_admitted(params, None, None, Some(&attempt.action_key))
        .map(|task| task.id)
}

/// [ORB-15186] Settlement reason for a review task that closed while its
/// agent had persisted no execution summary of its own.
const REVIEW_WITHOUT_EXECUTION_SUMMARY: &str = "review_closed_without_execution_summary";

/// The admitted task's outcome as settlement sees it.
pub(super) fn outcome(
    runtime: &OrbitRuntime,
    source: &Source<'_>,
    attempt: &BatchAttempt,
) -> Result<ActionOutcome, AutomationError> {
    task_outcome(runtime, source, attempt, true)
}

/// `settling` also requires the agent's own execution summary. Without it,
/// this is the evidence alone, which lets delivery derive a summary from
/// coverage the batch would otherwise accept.
fn task_outcome(
    runtime: &OrbitRuntime,
    source: &Source<'_>,
    attempt: &BatchAttempt,
    settling: bool,
) -> Result<ActionOutcome, AutomationError> {
    let Some(id) = attempt.action_id.as_deref() else {
        return Ok(ActionOutcome::Pending);
    };

    let task = match runtime.get_task(id) {
        Ok(task) => task,
        Err(OrbitError::NotFound {
            kind: NotFoundKind::Task,
            ..
        }) => {
            return Ok(ActionOutcome::Failed {
                retryable: true,
                reason: "task_deleted_without_accepted_evidence".into(),
            });
        }
        Err(error) => return Err(error.into()),
    };
    let stopped = matches!(
        task.status,
        TaskStatus::Done | TaskStatus::Rejected | TaskStatus::Archived
    );
    let artifact = runtime.get_task_artifact(id, COVERAGE_ARTIFACT)?;

    if let Some(artifact) = artifact {
        let manifest = runtime.get_task_artifact_manifest(id)?;
        let provenance = manifest.iter().find(|file| file.path == COVERAGE_ARTIFACT);

        if let Some(provenance) = provenance {
            let owner = evidence_owner(runtime, &task, &provenance.sha256)?;
            let verified = source_verified(source, &attempt.batch)?;
            // Commit ids alone can be produced without reading anything. A
            // closed review whose agent wrote no summary is that stamp; an
            // open one may still write it.
            if settling && agent_summary_missing(&task) {
                return Ok(if stopped {
                    ActionOutcome::Failed {
                        retryable: true,
                        reason: REVIEW_WITHOUT_EXECUTION_SUMMARY.into(),
                    }
                } else {
                    ActionOutcome::Pending
                });
            }
            return Ok(ActionOutcome::Evidence(EvidenceFacts {
                bytes: artifact.content,
                reference: format!("task:{id}/artifacts/{COVERAGE_ARTIFACT}"),
                submitted_by: owner
                    .clone()
                    .unwrap_or_else(|| provenance.created_by.clone()),
                artifact_digest: provenance.sha256.clone(),
                authorized: owner.is_some(),
                source_verified: verified,
                action_stopped: stopped,
                changed_paths: changed_paths(source, &attempt.batch, verified)?,
            }));
        }
    }

    // A closed task with no evidence is the same unevidenced stop as one with
    // invalid evidence: coverage stays owed and the retry budget applies.
    if stopped {
        return Ok(ActionOutcome::Failed {
            retryable: true,
            reason: "task_closed_without_accepted_evidence".into(),
        });
    }

    Ok(ActionOutcome::Pending)
}

const CONSUMER_PAGE_LIMIT: usize = 100;
const RECEIPT_LIMIT: usize = 100;

/// [ORB-14837] The coverage evidence task `task_id` submitted as a delivery
/// automation action, when settlement accepts or would accept it: a stored
/// receipt names the task, or the admitted attempt that names it holds an
/// `automation-coverage.json` that passes the same validation settlement
/// applies, checked against existing refs without fetching. Anything else,
/// including incomplete or mismatched evidence, is `None`. Settlement also
/// requires the agent's own execution summary, which this does not, so a
/// summary derived from this evidence still leaves the batch owed.
pub(crate) fn accepted_action_coverage(
    runtime: &OrbitRuntime,
    task_id: &str,
    now: DateTime<Utc>,
) -> Result<Option<CoverageEvidence>, OrbitError> {
    let Some(machine) = runtime.automation_machine_identity() else {
        return Ok(None);
    };
    let prefix = format!("{machine}/{}/", runtime.workspace_id()?);
    let store = runtime.automation_store()?;
    let mut after: Option<String> = None;
    loop {
        let page = store.automation_states_page(&prefix, after.as_deref(), CONSUMER_PAGE_LIMIT)?;
        for state in &page {
            if after.as_ref().is_some_and(|key| state.consumer <= *key) {
                return Err(OrbitError::Store(
                    "automation state page is not strictly ordered".into(),
                ));
            }
            after = Some(state.consumer.clone());
        }
        if page.is_empty() {
            return Ok(None);
        }
        for state in page {
            let accepted = match state
                .active
                .as_ref()
                .filter(|attempt| attempt.action_id.as_deref() == Some(task_id))
            {
                Some(attempt) => validated_evidence(runtime, attempt, now)?,
                None => store
                    .automation_receipts(&state.consumer, RECEIPT_LIMIT)?
                    .into_iter()
                    .find(|receipt| receipt.action_id == task_id)
                    .map(|receipt| receipt.evidence),
            };
            if let Some(bytes) = accepted {
                return serde_json::from_slice(&bytes)
                    .map(Some)
                    .map_err(|error| OrbitError::Store(format!("accepted coverage: {error}")));
            }
        }
    }
}

/// Bytes settlement would accept for `attempt` now. Unverifiable evidence is
/// not acceptance, so a deferral here is `None`, never an error.
fn validated_evidence(
    runtime: &OrbitRuntime,
    attempt: &BatchAttempt,
    now: DateTime<Utc>,
) -> Result<Option<Vec<u8>>, OrbitError> {
    let source = Source::read_only(&runtime.paths().repo_root);
    let facts = match task_outcome(runtime, &source, attempt, false) {
        Ok(ActionOutcome::Evidence(facts)) => facts,
        Ok(_) => return Ok(None),
        Err(error) => {
            tracing::warn!(batch = attempt.batch.id, %error, "cannot verify coverage evidence");
            return Ok(None);
        }
    };
    match evidence::validate(attempt, &facts, now) {
        Ok(receipt) => Ok(Some(receipt.evidence)),
        Err(error) => {
            tracing::info!(batch = attempt.batch.id, %error, "coverage evidence not accepted");
            Ok(None)
        }
    }
}

/// A claim can survive the mint but miss the action-id checkpoint. Resolve
/// its permanent key without replaying creation against a changed definition.
pub(super) fn action_id(
    runtime: &OrbitRuntime,
    attempt: &BatchAttempt,
) -> Result<Option<String>, OrbitError> {
    if let Some(id) = &attempt.action_id {
        return Ok(Some(id.clone()));
    }
    Ok(runtime
        .stores()
        .tasks()
        .automation_task_for_key(&attempt.action_key)?
        .map(|task| task.id))
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
    let stopped = matches!(
        run.state,
        JobRunState::Failed
            | JobRunState::Cancelled
            | JobRunState::Interrupted
            | JobRunState::Success
    ) && crate::application::job::run_owner_liveness(&run)
        == crate::application::job::RunOwnerLiveness::Stopped;

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
            let verified = source_verified(source, &attempt.batch)?;
            return Ok(ActionOutcome::Evidence(EvidenceFacts {
                artifact_digest: digest(&bytes),
                bytes,
                reference: format!("run:{id}/step:{}", step.step_index),
                submitted_by: format!("run:{id}"),
                authorized: true,
                source_verified: verified,
                action_stopped: stopped,
                changed_paths: changed_paths(source, &attempt.batch, verified)?,
            }));
        }
    }

    if stopped {
        return Ok(ActionOutcome::Failed {
            retryable: run.state == JobRunState::Failed,
            reason: "job_stopped_without_accepted_evidence".into(),
        });
    }

    Ok(ActionOutcome::Pending)
}

/// `Ok(false)` is a frozen-batch mismatch. Fetch, deadline, budget, and
/// spawn failures stay deferred so a closed action is not settled as
/// unverifiable coverage and a retry is not spent.
fn source_verified(source: &Source<'_>, batch: &CoverageBatch) -> Result<bool, AutomationError> {
    match source.verify_batch(batch) {
        Ok(()) => Ok(true),
        Err(error) if super::source::is_batch_mismatch(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

/// The frozen deliveries' changed paths. An unverified source settles as
/// `source_unverifiable` first, so it is not read.
fn changed_paths(
    source: &Source<'_>,
    batch: &CoverageBatch,
    verified: bool,
) -> Result<BTreeMap<String, Vec<String>>, AutomationError> {
    if !verified {
        return Ok(BTreeMap::new());
    }
    source.delivery_changed_paths(batch)
}

/// [ORB-15186] The review's agent persisted no execution summary: the task
/// has none, or the one it carries is still the text Orbit derived. The
/// derived-summary history event is not consulted: it survives an agent's
/// later replacement, so the current text decides [ORB-15255].
fn agent_summary_missing(task: &Task) -> bool {
    let summary = task.execution_summary.trim();
    summary.is_empty() || summary.starts_with(DERIVED_EXECUTION_SUMMARY_PREFIX)
}

fn evidence_owner(
    runtime: &OrbitRuntime,
    task: &Task,
    artifact_digest: &str,
) -> Result<Option<String>, AutomationError> {
    let Some(artifact) = runtime.get_task_artifact(&task.id, EVIDENCE_AUTHORITY_ARTIFACT)? else {
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

    let run = runtime.show_job_run(&submission.run_id)?;
    let assigned = run.input.as_ref().is_some_and(|input| {
        input.get("task_id").and_then(serde_json::Value::as_str) == Some(&task.id)
            || input
                .get("task_ids")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(&task.id)))
    });

    Ok(assigned.then(|| format!("run:{}", submission.run_id)))
}
