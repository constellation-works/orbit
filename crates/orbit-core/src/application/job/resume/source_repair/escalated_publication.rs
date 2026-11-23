//! An operator requeue of a task whose run published its own pull request.
//!
//! Such a run pushed and opened its PR through its pipeline steps, then either
//! failed completion and its failure handoff kept that PR open
//! (`review_completion_failure` or `landing_review_failure`), ended cancelled,
//! was held for a review decision, or completed into review with an open PR.
//! No `[BLOCKED]` PR receipt exists, so the failure-handoff requeue does not
//! apply. Explicit source repair can authorize source work separately under
//! fresh operator policy. A requeue resumes it exactly as a final-recovery
//! requeue would: it records the same publication-preservation record, and the
//! next run adopts the branch and PR, then validates it. Any fresh independent
//! review follows that run's selected review policy, which may be off.
//!
//! The requeue is admitted only while the publication is still what the run
//! recorded and ready to land: the run's failure handoff (for failed runs) or
//! task history (for cancelled, held, or succeeded runs) confirms the task
//! specification did not change after publication (a pending edit carried by
//! the candidate task counts as a change), the checkpoints authenticate
//! the PR as open and unmerged at the recorded head, and every required check on
//! that head has passed (or the repository configures no required check).
use super::*;
use orbit_types::task::{
    ACCEPTANCE_CRITERIA_DOCUMENT, DESCRIPTION_DOCUMENT, TASK_FIELDS_UPDATED_EVENT,
    task_fields_updated_documents,
};
use orbit_types::workflow::FailureActivityCheckpoint;

/// The failure-handoff decisions that keep a pipeline-published PR open.
const KEPT_PUBLICATION_DECISIONS: [&str; 2] =
    ["review_completion_failure", "landing_review_failure"];

fn validate_failure_handoff_receipt(
    receipt: &FailureActivityCheckpoint,
    source_id: &str,
    task: &Task,
) -> Result<(), OrbitError> {
    let output = &receipt.output;
    if receipt.activity_name != "pr_failure_handoff"
        || !KEPT_PUBLICATION_DECISIONS.contains(&field(output, "decision")?)
        || output.get("candidate_preserved").and_then(Value::as_bool) != Some(true)
        || field(output, "failed_step_id")? != receipt.failed_step_id
    {
        return Err(refuse(
            "the run's failure handoff did not keep its published pull request",
        ));
    }
    // A receipt written before it named its run and specification
    // cannot show the candidate still implements this task.
    if output.get("handoff_run_id").and_then(Value::as_str) != Some(source_id)
        || output.get("task_id").and_then(Value::as_str) != Some(task.id.as_str())
    {
        return Err(refuse(
            "the run's completion-failure receipt does not name this run and task",
        ));
    }
    let recorded = field(output, "task_spec_digest")?;
    if !task.spec_digest_matches(recorded) {
        return Err(refuse(format!(
            "the task specification changed after its run published pull request #{}, so that \
             candidate no longer implements this task",
            output["pr_number"].as_str().unwrap_or("?")
        )));
    }
    Ok(())
}

impl OrbitRuntime {
    /// Check local escalated/pipeline publication requeue preconditions without
    /// contacting remotes or the forge.
    pub(crate) fn escalated_publication_local_gate(
        &self,
        task: &Task,
    ) -> Result<(String, JobRun), OrbitError> {
        let (source_id, source) = self.stopped_failed_requeue_source(
            task,
            "the task's publication belongs to another machine",
            "the task has no run whose publication it could resume",
        )?;
        super::published_owner::require_stopped_published_requeue_source(&source)?;
        let entries = self.get_task_history(&task.id)?;
        self.require_current_requeue_source(task, &source, &source_id, &entries)?;
        let state = self
            .read_run_state(&source_id)?
            .ok_or_else(|| refuse("source checkpoints missing"))?;
        if source.state == JobRunState::Failed {
            let receipt = state.failure_activity_checkpoint.as_ref().ok_or_else(|| {
                refuse("the run's failure handoff recorded no receipt keeping its pull request")
            })?;
            validate_failure_handoff_receipt(receipt, &source_id, task)?;
        } else if let Some(receipt) = state
            .failure_activity_checkpoint
            .as_ref()
            .filter(|receipt| {
                receipt.activity_name == "pr_failure_handoff"
                    && receipt.output.get("handoff_run_id").and_then(Value::as_str)
                        == Some(source_id.as_str())
            })
        {
            validate_failure_handoff_receipt(receipt, &source_id, task)?;
        } else if self.get_task(&task.id)?.spec_digest() != task.spec_digest()
            || entries.iter().any(|entry| {
                entry.at >= source.created_at
                    && entry.event == TASK_FIELDS_UPDATED_EVENT
                    && entry.note.as_deref().is_some_and(|note| {
                        task_fields_updated_documents(note).into_iter().any(|doc| {
                            doc == DESCRIPTION_DOCUMENT || doc == ACCEPTANCE_CRITERIA_DOCUMENT
                        })
                    })
            })
        {
            return Err(refuse(format!(
                "the task specification changed after run `{source_id}` started, so its \
                 published pull request no longer implements this task"
            )));
        }
        Ok((source_id, source))
    }

    /// Authenticate the PR `task`'s current run published through its own
    /// pipeline steps, and return the preservation record a requeue appends with
    /// the binding it resumes. Called before the task write lock, since it asks
    /// the remote and the forge; nothing is written here.
    pub(crate) fn escalated_publication_requeue_record(
        &self,
        task: &Task,
    ) -> Result<(TaskHistoryEntry, SourceRepairBinding), OrbitError> {
        let (source_id, source) = self.escalated_publication_local_gate(task)?;
        let (_, _, binding) = self.observe_escalated_publication(source, task.clone())?;
        self.require_green_required_checks(&binding)?;
        let entry = self
            .published_owner_requeue_record(task, &source_id)?
            .ok_or_else(|| refuse("the run's publication cannot be preserved"))?;
        Ok((entry, binding))
    }

    /// Refuse unless every required check on `binding`'s pull request has
    /// passed on exactly its recorded head (or the repository configures no
    /// required checks). Checks on another head describe a different
    /// candidate, an empty rollup in a configured or unreadable repository
    /// proves nothing, and a configured check the head never reported is not
    /// green just because the reported ones are.
    fn require_green_required_checks(
        &self,
        binding: &SourceRepairBinding,
    ) -> Result<(), OrbitError> {
        let number = binding
            .pull_request
            .parse::<u64>()
            .map_err(|_| refuse("published pull request number is not exact"))?;
        let blockers = self
            .task_pr_forge()
            .pull_request_blockers(&self.paths().repo_root, number)
            .map_err(|error| refuse(format!("required checks unreadable: {error}")))?
            .ok_or_else(|| {
                refuse(format!(
                    "pull request #{number} is not open, or the forge cannot report its required \
                     checks"
                ))
            })?;
        if blockers.head_sha != binding.head_sha {
            return Err(refuse(format!(
                "the forge's required checks describe head {}, not the recorded head {}",
                blockers.head_sha, binding.head_sha
            )));
        }
        if blockers.conflicting {
            return Err(refuse(format!(
                "pull request #{number} conflicts with its base"
            )));
        }
        if !blockers.failed_required_checks.is_empty() {
            return Err(refuse(format!(
                "required checks failed on the recorded head: {}",
                blockers.failed_required_checks.join(", ")
            )));
        }
        if !blockers.pending_required_checks.is_empty() {
            return Err(refuse(format!(
                "required checks have not finished on the recorded head: {}",
                blockers.pending_required_checks.join(", ")
            )));
        }
        if blockers.passed_required_checks.is_empty()
            && blockers.requires_status_checks != Some(false)
        {
            return Err(refuse(
                "the recorded head reports no required check, so none has passed",
            ));
        }
        if !blockers.unreported_required_checks.is_empty() {
            return Err(refuse(format!(
                "required checks have not reported on the recorded head: {}",
                blockers.unreported_required_checks.join(", ")
            )));
        }
        Ok(())
    }
}
