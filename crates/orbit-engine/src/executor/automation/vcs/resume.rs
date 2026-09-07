//! Authentication and bounded checkpoint refresh for resumed PR delivery.

use std::collections::HashMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::activity_job::JobV2;
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::Value;

use crate::DispatchError;
use crate::context::RuntimeHost;
use crate::executor::automation::input::{
    canonicalize_existing_dir, input_string_field, required_input_string,
};

use super::freshness::{commit_sha, recovered_head_checkpoint};

const FAILURE_HANDOFF_LINEAGE_MAX_HOPS: usize = 64;
const RESUME_PREPARATION_REFRESH_MAX_ATTEMPTS: u64 = 1;

const RESUME_PRESERVATION_ERROR: &str = "resume_preservation_unverified";
const RESUME_REFRESH_EXHAUSTED_ERROR: &str = "resume_checkpoint_refresh_exhausted";

#[derive(Debug, Clone)]
pub(crate) struct ResumePreparationRefresh {
    pub(crate) step_index: u32,
    expected_head: String,
    expected_head_sha: String,
    source_run_id: String,
    previous_base_sha: String,
    attempt: u64,
    max_attempts: u64,
}

impl ResumePreparationRefresh {
    pub(crate) fn annotate_output(&self, output: &mut Value) -> Result<(), DispatchError> {
        let refreshed_head =
            required_input_string(output, "head").map_err(resume_preservation_error)?;
        let refreshed_head_sha =
            required_input_string(output, "head_sha").map_err(resume_preservation_error)?;
        if refreshed_head != self.expected_head || refreshed_head_sha != self.expected_head_sha {
            return Err(resume_preservation_error(OrbitError::Execution(format!(
                "resume refresh candidate changed while preparing the new base checkpoint: expected {}@{}, observed {refreshed_head}@{refreshed_head_sha}",
                self.expected_head, self.expected_head_sha
            ))));
        }

        let object = output.as_object_mut().ok_or_else(|| {
            resume_preservation_error(OrbitError::Execution(
                "refreshed prepare_branch returned a non-object checkpoint".to_string(),
            ))
        })?;
        object.insert(
            "resume_refresh".to_string(),
            serde_json::json!({
                "kind": "stale_delivery_checkpoint",
                "source_run_id": self.source_run_id,
                "previous_base_sha": self.previous_base_sha,
                "attempt": self.attempt,
                "max_attempts": self.max_attempts,
            }),
        );
        Ok(())
    }
}

/// Authenticate a terminal failure handoff and identify the one preparation
/// checkpoint a preserved-candidate resume is allowed to refresh.
///
/// The successful worktree checkpoint intentionally retains its original
/// `base_sha`. If HEAD still equals that commit there is nothing to reconcile.
/// A moved HEAD is accepted only when the source run durably recorded an exact
/// `pr_failure_handoff` result for the same task, checkpoint owner, and commit,
/// and the active run descends from the handoff run. The evidence stays in the
/// durable run state for the later commit gate; no historical checkpoint or
/// repository state is rewritten.
pub(crate) fn reconcile_resumed_failure_handoff(
    host: &dyn RuntimeHost,
    job: &JobV2,
    active_run_id: &str,
    resume: &PipelineState,
    pipeline: &HashMap<String, Value>,
) -> Result<Option<ResumePreparationRefresh>, DispatchError> {
    let mut preparation_refresh = resumed_preparation_refresh(job, resume)?;
    let Some(commit_index) = job
        .steps
        .iter()
        .position(|step| step.id == "commit")
        .map(|index| index as u32)
    else {
        return if preparation_refresh.is_some() {
            Err(resume_preservation_error(OrbitError::Execution(
                "resume checkpoint refresh requires the task PR commit step".to_string(),
            )))
        } else {
            Ok(None)
        };
    };
    if resume.step_states.get(&commit_index) == Some(&JobRunState::Success)
        && preparation_refresh.is_none()
    {
        return Ok(None);
    }

    let Some(worktree) = pipeline.get("worktree") else {
        return if preparation_refresh.is_some() {
            Err(resume_preservation_error(OrbitError::Execution(
                "resume checkpoint refresh requires the completed worktree checkpoint".to_string(),
            )))
        } else {
            Ok(None)
        };
    };
    let Some(workspace_path) = input_string_field(worktree, "workspace_path") else {
        return if preparation_refresh.is_some() {
            Err(resume_preservation_error(OrbitError::Execution(
                "resume checkpoint refresh worktree has no workspace_path".to_string(),
            )))
        } else {
            Ok(None)
        };
    };
    let Some(original_base_sha) = input_string_field(worktree, "base_sha") else {
        return if preparation_refresh.is_some() {
            Err(resume_preservation_error(OrbitError::Execution(
                "resume checkpoint refresh worktree has no immutable base_sha".to_string(),
            )))
        } else {
            Ok(None)
        };
    };
    let Some(checkpoint_owner) = input_string_field(worktree, "job_run_id")
        .or_else(|| input_string_field(worktree, "batch_id"))
    else {
        return if preparation_refresh.is_some() {
            Err(resume_preservation_error(OrbitError::Execution(
                "resume checkpoint refresh worktree has no checkpoint owner".to_string(),
            )))
        } else {
            Ok(None)
        };
    };

    let workspace_path = canonicalize_existing_dir(&workspace_path, "resume workspace_path")
        .map_err(resume_preservation_error)?;
    let base_sha =
        commit_sha(&workspace_path, &original_base_sha).map_err(resume_preservation_error)?;
    let head_sha = commit_sha(&workspace_path, "HEAD").map_err(resume_preservation_error)?;
    if head_sha == base_sha {
        return if preparation_refresh.is_some() {
            Err(resume_preservation_error(OrbitError::Execution(
                "resume checkpoint refresh found no preserved candidate above the immutable worktree base"
                    .to_string(),
            )))
        } else {
            Ok(None)
        };
    }

    let checkpoint = resume.failure_activity_checkpoint.as_ref().ok_or_else(|| {
        resume_preservation_error(OrbitError::Execution(format!(
            "resume found HEAD {head_sha} past immutable worktree base {base_sha}, but source run '{}' has no durable failure-activity preservation evidence",
            resume.run_id
        )))
    })?;
    let evidence = validate_failure_handoff_evidence(
        host,
        active_run_id,
        &checkpoint_owner,
        &workspace_path,
        &base_sha,
        &head_sha,
        checkpoint,
    )
    .map_err(resume_preservation_error)?;
    let evidence_task_id =
        required_input_string(evidence, "task_id").map_err(resume_preservation_error)?;
    let targets_task = resume
        .initial_input
        .get("task_ids")
        .and_then(Value::as_array)
        .is_some_and(|task_ids| {
            task_ids
                .iter()
                .any(|task_id| task_id.as_str() == Some(evidence_task_id))
        });
    if !targets_task {
        return Err(resume_preservation_error(OrbitError::Execution(format!(
            "failure handoff evidence belongs to task '{evidence_task_id}', which is not targeted by the resumed run"
        ))));
    }
    if let Some(refresh) = preparation_refresh.as_mut() {
        validate_preparation_refresh_identity(host, pipeline, evidence, &head_sha)
            .map_err(resume_preservation_error)?;
        refresh.expected_head_sha = head_sha.clone();
    }
    if let Some(refresh) = preparation_refresh.as_ref()
        && refresh.attempt > refresh.max_attempts
    {
        return Err(DispatchError::WorktreeIntegrity {
            code: RESUME_REFRESH_EXHAUSTED_ERROR,
            diagnostic: format!(
                "delivery preparation for preserved candidate already refreshed {} time(s), reaching the bounded limit of {}; base continued advancing after checkpoint '{}' — preserve the current branch/PR and inspect the competing deliveries before retrying",
                refresh.attempt - 1,
                refresh.max_attempts,
                refresh.previous_base_sha,
            ),
        });
    }
    Ok(preparation_refresh)
}

fn resumed_preparation_refresh(
    job: &JobV2,
    resume: &PipelineState,
) -> Result<Option<ResumePreparationRefresh>, DispatchError> {
    if resume.job_id != "task_pr_pipeline" {
        return Ok(None);
    }
    let Some(checkpoint) = resume.failure_activity_checkpoint.as_ref() else {
        return Ok(None);
    };
    if checkpoint.activity_name != "pr_failure_handoff" || checkpoint.failed_step_id != "sync_base"
    {
        return Ok(None);
    }

    let prepare_index = job
        .steps
        .iter()
        .position(|step| step.id == "prepare_branch")
        .map(|index| index as u32)
        .ok_or_else(|| {
            resume_preservation_error(OrbitError::Execution(
                "resume checkpoint names failed sync_base without a prepare_branch step"
                    .to_string(),
            ))
        })?;
    let sync_index = job
        .steps
        .iter()
        .position(|step| step.id == "sync_base")
        .map(|index| index as u32)
        .ok_or_else(|| {
            resume_preservation_error(OrbitError::Execution(
                "resume failure evidence names a sync_base step absent from the current job"
                    .to_string(),
            ))
        })?;
    if resume.step_states.get(&prepare_index) != Some(&JobRunState::Success) {
        return Err(resume_preservation_error(OrbitError::Execution(
            "resume failure evidence names sync_base without a successful preparation checkpoint"
                .to_string(),
        )));
    }
    if resume.step_states.get(&sync_index) == Some(&JobRunState::Success)
        || resume
            .step_states
            .range((sync_index + 1)..)
            .any(|(_, state)| *state == JobRunState::Success)
    {
        return Err(resume_preservation_error(OrbitError::Execution(
            "resume failure evidence names sync_base after that delivery checkpoint or a downstream checkpoint already succeeded"
                .to_string(),
        )));
    }

    let prepared = resume.step_outputs.get(&prepare_index).ok_or_else(|| {
        resume_preservation_error(OrbitError::Execution(
            "successful prepare_branch checkpoint has no durable output".to_string(),
        ))
    })?;
    let previous_base_sha =
        required_input_string(prepared, "base_sha").map_err(resume_preservation_error)?;
    let expected_head =
        required_input_string(prepared, "head").map_err(resume_preservation_error)?;
    let expected_head_sha =
        required_input_string(prepared, "head_sha").map_err(resume_preservation_error)?;
    let previous_attempt = previous_refresh_attempt(prepared).map_err(resume_preservation_error)?;
    let source_run_id = required_input_string(&checkpoint.output, "handoff_run_id")
        .map_err(resume_preservation_error)?;

    Ok(Some(ResumePreparationRefresh {
        step_index: prepare_index,
        expected_head: expected_head.to_string(),
        expected_head_sha: expected_head_sha.to_string(),
        source_run_id: source_run_id.to_string(),
        previous_base_sha: previous_base_sha.to_string(),
        attempt: previous_attempt + 1,
        max_attempts: RESUME_PREPARATION_REFRESH_MAX_ATTEMPTS,
    }))
}

fn previous_refresh_attempt(prepared: &Value) -> Result<u64, OrbitError> {
    let Some(refresh) = prepared.get("resume_refresh") else {
        return Ok(0);
    };
    if required_input_string(refresh, "kind")? != "stale_delivery_checkpoint" {
        return Err(OrbitError::Execution(
            "prepare_branch resume_refresh has an unknown kind".to_string(),
        ));
    }
    let attempt = refresh
        .get("attempt")
        .and_then(Value::as_u64)
        .filter(|attempt| *attempt > 0)
        .ok_or_else(|| {
            OrbitError::Execution(
                "prepare_branch resume_refresh requires a positive integer attempt".to_string(),
            )
        })?;
    let max_attempts = refresh
        .get("max_attempts")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            OrbitError::Execution(
                "prepare_branch resume_refresh requires an integer max_attempts".to_string(),
            )
        })?;
    if max_attempts != RESUME_PREPARATION_REFRESH_MAX_ATTEMPTS {
        return Err(OrbitError::Execution(format!(
            "prepare_branch resume_refresh recorded limit {max_attempts}, but this runtime requires the durable limit {RESUME_PREPARATION_REFRESH_MAX_ATTEMPTS}"
        )));
    }
    required_input_string(refresh, "source_run_id")?;
    required_input_string(refresh, "previous_base_sha")?;
    Ok(attempt)
}

fn validate_preparation_refresh_identity<H: RuntimeHost + ?Sized>(
    host: &H,
    pipeline: &HashMap<String, Value>,
    evidence: &Value,
    head_sha: &str,
) -> Result<(), OrbitError> {
    let prepared = pipeline.get("prepare_branch").ok_or_else(|| {
        OrbitError::Execution("resume refresh lost the prepared checkpoint".to_string())
    })?;
    let prepared_head = required_input_string(prepared, "head")?;
    let prepared_head_sha = required_input_string(prepared, "head_sha")?;
    let evidence_head = required_input_string(evidence, "branch")?;
    let evidence_head_sha = required_input_string(evidence, "head_sha")?;
    let worktree = pipeline
        .get("worktree")
        .ok_or_else(|| OrbitError::Execution("resume lost worktree checkpoint".to_string()))?;
    let workspace = canonicalize_existing_dir(
        required_input_string(worktree, "workspace_path")?,
        "workspace_path",
    )?;
    let recovered = recovered_head_checkpoint(
        host,
        required_input_string(evidence, "handoff_run_id")?,
        &workspace,
        head_sha,
    )?;
    let prepared_origin_matches = prepared_head_sha == evidence_head_sha
        || recovered.as_ref().is_some_and(|checkpoint| {
            checkpoint["head_sha_before"] == prepared_head_sha
                && checkpoint["base_sha"] == prepared["base_sha"]
                && checkpoint["head"] == prepared_head
        });
    if prepared_head != evidence_head || !prepared_origin_matches || evidence_head_sha != head_sha {
        return Err(OrbitError::Execution(format!(
            "resume refresh candidate changed across preparation, failure handoff, and current checkout: prepared {prepared_head}@{prepared_head_sha}, handoff {evidence_head}@{evidence_head_sha}, current HEAD {head_sha}"
        )));
    }

    let task_id = required_input_string(evidence, "task_id")?;
    let pr_number = required_input_string(evidence, "pr_number")?;
    let task = host.get_task(task_id)?;
    if task.github_pr_number() != Some(pr_number) {
        return Err(OrbitError::Execution(format!(
            "resume refresh failure handoff names PR #{pr_number}, but task '{task_id}' now names PR #{}",
            task.github_pr_number().unwrap_or("none")
        )));
    }
    let checkpoint_owner = required_input_string(evidence, "checkpoint_owner")?;
    ensure_run_is_resumable_owner(host, checkpoint_owner, "worktree checkpoint owner")?;
    let handoff_run_id = required_input_string(evidence, "handoff_run_id")?;
    ensure_run_is_resumable_owner(host, handoff_run_id, "failure handoff run")?;
    Ok(())
}

fn ensure_run_is_resumable_owner<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    label: &str,
) -> Result<(), OrbitError> {
    let run = host.get_job_run(run_id)?.ok_or_else(|| {
        OrbitError::Execution(format!(
            "resume refresh {label} '{run_id}' has no durable run record"
        ))
    })?;
    if !matches!(
        run.state,
        JobRunState::Failed | JobRunState::Timeout | JobRunState::Interrupted
    ) {
        return Err(OrbitError::Execution(format!(
            "resume refresh {label} '{run_id}' is {}; only failed, timed-out, or interrupted ownership can be resumed",
            run.state
        )));
    }
    Ok(())
}

/// Re-authenticate the evidence carried into `git_commit` against the source
/// run's immutable state. This is deliberately separate from the ordinary
/// moved-HEAD escape hatch used by epic child merges.
pub(super) fn commit_head_matches_failure_handoff<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    task: &orbit_types::task::Task,
    checkpoint_owner: &str,
    workspace_path: &Path,
    base_sha: &str,
    head_sha: &str,
) -> Result<bool, OrbitError> {
    let Some(active_run_id) = input_string_field(input, "run_id") else {
        return Ok(false);
    };
    let Some(active_state) = host.read_run_state(&active_run_id)? else {
        return Ok(false);
    };
    let Some(checkpoint) = active_state.failure_activity_checkpoint.as_ref() else {
        return Ok(false);
    };
    let evidence = &checkpoint.output;
    validate_failure_handoff_evidence(
        host,
        &active_run_id,
        checkpoint_owner,
        workspace_path,
        base_sha,
        head_sha,
        checkpoint,
    )?;
    if evidence.get("task_id").and_then(Value::as_str) != Some(task.id.as_str()) {
        return Err(OrbitError::Execution(format!(
            "git_commit: failure handoff evidence belongs to a different task than '{}'",
            task.id
        )));
    }
    Ok(true)
}

fn validate_failure_handoff_evidence<'a, H: RuntimeHost + ?Sized>(
    host: &H,
    active_run_id: &str,
    checkpoint_owner: &str,
    workspace_path: &Path,
    base_sha: &str,
    head_sha: &str,
    checkpoint: &'a orbit_types::workflow::FailureActivityCheckpoint,
) -> Result<&'a Value, OrbitError> {
    if checkpoint.activity_name != "pr_failure_handoff" {
        return Err(OrbitError::Execution(format!(
            "failure activity '{}' is not authorized to reconcile a resumed worktree HEAD",
            checkpoint.activity_name
        )));
    }
    let evidence = &checkpoint.output;
    let phase = required_input_string(evidence, "phase")?;
    let decision = required_input_string(evidence, "decision")?;
    if phase != "failure_handoff"
        || !matches!(decision, "blocked_failure_pr" | "blocked_conflict_pr")
    {
        return Err(OrbitError::Execution(format!(
            "failure activity result '{phase}/{decision}' is not candidate-preservation evidence"
        )));
    }

    let task_id = required_input_string(evidence, "task_id")?;
    let handoff_run_id = required_input_string(evidence, "handoff_run_id")?;
    let evidence_owner = required_input_string(evidence, "checkpoint_owner")?;
    let evidence_base = required_input_string(evidence, "original_base_sha")?;
    let evidence_head = required_input_string(evidence, "head_sha")?;
    let preservation_commit_created = evidence
        .get("preservation_commit_created")
        .and_then(Value::as_bool)
        == Some(true);
    let recovered = recovered_head_checkpoint(host, handoff_run_id, workspace_path, head_sha)?;
    let recovered_head_owned = recovered.as_ref().is_some_and(|recovery| {
        recovery["task_ids"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id == task_id))
            && recovery["head"] == evidence["branch"]
            && recovery["original_base_sha"] == base_sha
    });
    let workflow_commit_owned = !preservation_commit_created
        && source_state_step_owns_head(host, handoff_run_id, task_id, evidence_head)?;
    if !preservation_commit_created && !workflow_commit_owned && !recovered_head_owned {
        return Err(OrbitError::Execution(
            "failure handoff HEAD has no preservation commit, successful workflow commit, or exact host-validated rebase recovery"
                .to_string(),
        ));
    }
    if evidence_owner != checkpoint_owner {
        return Err(OrbitError::Execution(format!(
            "failure handoff checkpoint owner '{evidence_owner}' does not match reused worktree owner '{checkpoint_owner}'"
        )));
    }
    if evidence_base != base_sha || evidence_head != head_sha {
        return Err(OrbitError::Execution(format!(
            "failure handoff evidence expected base {evidence_base} and HEAD {evidence_head}, but resume observed base {base_sha} and HEAD {head_sha}"
        )));
    }

    let source_state = host.read_run_state(handoff_run_id)?.ok_or_else(|| {
        OrbitError::Execution(format!(
            "failure handoff run '{handoff_run_id}' has no durable run state"
        ))
    })?;
    if source_state.failure_activity_checkpoint.as_ref() != Some(checkpoint) {
        return Err(OrbitError::Execution(format!(
            "failure handoff evidence does not match the immutable state of run '{handoff_run_id}'"
        )));
    }
    if !recovered_head_owned {
        ensure_preservation_parent_owned(
            host,
            workspace_path,
            handoff_run_id,
            base_sha,
            head_sha,
            &source_state,
        )?;
    }

    let task = host.get_task(task_id)?;
    if task.job_run_id.as_deref() != Some(checkpoint_owner) {
        return Err(OrbitError::Execution(format!(
            "task '{task_id}' belongs to run '{}', not preserved worktree owner '{checkpoint_owner}'",
            task.job_run_id.as_deref().unwrap_or("none")
        )));
    }
    ensure_retry_descends_from(
        host,
        "resume preservation",
        "failure handoff run",
        task_id,
        active_run_id,
        handoff_run_id,
    )?;
    Ok(evidence)
}

fn source_state_step_owns_head<H: RuntimeHost + ?Sized>(
    host: &H,
    handoff_run_id: &str,
    task_id: &str,
    head_sha: &str,
) -> Result<bool, OrbitError> {
    Ok(host.read_run_state(handoff_run_id)?.is_some_and(|state| {
        state.step_outputs.values().any(|output| {
            output.get("phase").and_then(Value::as_str) == Some("commit")
                && output.get("task_id").and_then(Value::as_str) == Some(task_id)
                && output.get("commit_sha").and_then(Value::as_str) == Some(head_sha)
        })
    }))
}

fn ensure_preservation_parent_owned<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    handoff_run_id: &str,
    base_sha: &str,
    head_sha: &str,
    handoff_state: &PipelineState,
) -> Result<(), OrbitError> {
    let parent_sha = commit_sha(workspace_path, &format!("{head_sha}^"))?;
    if parent_sha == base_sha
        || handoff_state.step_outputs.values().any(|output| {
            output.get("commit_sha").and_then(Value::as_str) == Some(parent_sha.as_str())
        })
    {
        return Ok(());
    }

    let handoff_run = host.get_job_run(handoff_run_id)?.ok_or_else(|| {
        OrbitError::Execution(format!(
            "failure handoff run '{handoff_run_id}' was not found while verifying preservation ancestry"
        ))
    })?;
    let mut cursor = handoff_run.retry_source_run_id;
    for _ in 0..FAILURE_HANDOFF_LINEAGE_MAX_HOPS {
        let Some(run_id) = cursor.take() else { break };
        let run = host.get_job_run(&run_id)?.ok_or_else(|| {
            OrbitError::Execution(format!(
                "retry ancestor '{run_id}' was not found while verifying preservation ancestry"
            ))
        })?;
        if run.job_id != handoff_run.job_id {
            return Err(OrbitError::Execution(format!(
                "preservation ancestry crosses from job '{}' to job '{}' at run '{}'",
                handoff_run.job_id, run.job_id, run.run_id
            )));
        }
        let ancestor_head = host
            .read_run_state(&run_id)?
            .and_then(|state| state.failure_activity_checkpoint)
            .and_then(|checkpoint| checkpoint.output.get("head_sha").cloned())
            .and_then(|head| head.as_str().map(ToOwned::to_owned));
        if ancestor_head.as_deref() == Some(parent_sha.as_str()) {
            return Ok(());
        }
        if run.retry_source_run_id.as_deref() == Some(run_id.as_str()) {
            break;
        }
        cursor = run.retry_source_run_id;
    }

    Err(OrbitError::Execution(format!(
        "failure handoff commit {head_sha} has unowned parent {parent_sha}; expected immutable base {base_sha}, a successful workflow commit, or an earlier preservation commit in the retry lineage"
    )))
}

fn resume_preservation_error(error: OrbitError) -> DispatchError {
    DispatchError::WorktreeIntegrity {
        code: RESUME_PRESERVATION_ERROR,
        diagnostic: error.to_string(),
    }
}

pub(super) fn ensure_retry_descends_from<H: RuntimeHost + ?Sized>(
    host: &H,
    operation: &str,
    ancestor_label: &str,
    task_id: &str,
    run_id: &str,
    ancestor_run_id: &str,
) -> Result<(), OrbitError> {
    let mut current = host.get_job_run(run_id)?.ok_or_else(|| {
        OrbitError::Execution(format!(
            "{operation}: cannot verify ownership for task '{task_id}'; active run '{run_id}' was not found"
        ))
    })?;
    let job_id = current.job_id.clone();

    for _ in 0..FAILURE_HANDOFF_LINEAGE_MAX_HOPS {
        let Some(parent_run_id) = current.retry_source_run_id.as_deref() else {
            return Err(OrbitError::Execution(format!(
                "{operation}: run '{run_id}' is not a retry descendant of {ancestor_label} '{ancestor_run_id}' for task '{task_id}'"
            )));
        };
        let parent = host.get_job_run(parent_run_id)?.ok_or_else(|| {
            OrbitError::Execution(format!(
                "{operation}: cannot verify ownership for task '{task_id}'; retry ancestor '{parent_run_id}' was not found"
            ))
        })?;
        if parent.job_id != job_id {
            return Err(OrbitError::Execution(format!(
                "{operation}: retry lineage for run '{run_id}' crosses from job '{job_id}' to job '{}' at run '{}'; refusing handoff for task '{task_id}'",
                parent.job_id, parent.run_id
            )));
        }
        if parent.run_id == ancestor_run_id {
            return Ok(());
        }
        if parent.run_id == current.run_id {
            break;
        }
        current = parent;
    }

    Err(OrbitError::Execution(format!(
        "{operation}: run '{run_id}' has no bounded retry lineage to {ancestor_label} '{ancestor_run_id}' for task '{task_id}'"
    )))
}
