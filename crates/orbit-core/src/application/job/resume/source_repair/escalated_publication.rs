//! An operator requeue of a task blocked on the escalation of a run that
//! published its own pull request.
//!
//! Such a run pushed and opened its PR through its pipeline steps, then
//! failed completion; its failure handoff kept that PR open and unmerged
//! (`review_completion_failure` or `landing_review_failure`) and its final
//! recovery escalated, blocking the task. No `[BLOCKED]` PR receipt exists,
//! so neither the failure-handoff requeue nor explicit source repair applies.
//! A requeue resumes it exactly as a final-recovery requeue would: it records
//! the same publication-preservation record, and the next run adopts the
//! branch and PR, then validates, reviews and lands afresh.
//!
//! The requeue widens what an escalation leaves to an operator, so it is
//! admitted only while the publication is still what the run recorded and
//! ready to land: the run's completion-failure receipt names this run and
//! the task's current specification, the checkpoints authenticate the PR as
//! open and unmerged at the recorded head, and every required check on that
//! head has passed.
use super::*;

/// The failure-handoff decisions that keep a pipeline-published PR open.
const KEPT_PUBLICATION_DECISIONS: [&str; 2] =
    ["review_completion_failure", "landing_review_failure"];

impl OrbitRuntime {
    /// Authenticate the PR `task`'s current run published through its own
    /// pipeline steps before its final recovery escalated, and return the
    /// preservation record a requeue appends with the binding it resumes.
    /// Called before the task write lock, since it asks the remote and the
    /// forge; nothing is written here.
    pub(crate) fn escalated_publication_requeue_record(
        &self,
        task: &Task,
    ) -> Result<(TaskHistoryEntry, SourceRepairBinding), OrbitError> {
        if !self.task_run_is_local(task) {
            return Err(refuse("the task's publication belongs to another machine"));
        }
        let source_id = task
            .job_run_id
            .as_deref()
            .ok_or_else(|| refuse("the task has no run whose publication it could resume"))?;
        let source = self
            .get_job_run_backend(source_id)?
            .ok_or_else(|| refuse("the task's run is missing"))?;
        self.owner_source_identity(&source, task)?;
        if source.state != JobRunState::Failed
            || source.finished_at.is_none()
            || run_owner_liveness(&source) != RunOwnerLiveness::Stopped
        {
            return Err(refuse(
                "the task's run is not a conclusively stopped failed delivery",
            ));
        }
        if self.get_task_history(&task.id)?.iter().any(|e| {
            e.event == orbit_types::task::CANDIDATE_DISCARDED_EVENT && e.at >= source.created_at
        }) {
            return Err(refuse("an operator discarded this run's candidate"));
        }
        let newest = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            job_id: Some("task_pr_pipeline".into()),
            task_id: Some(task.id.clone()),
            limit: Some(1),
            include_steps: false,
            ..Default::default()
        })?;
        if newest.first().is_none_or(|run| run.run_id != source_id) {
            return Err(refuse(
                "newer execution makes publication association ambiguous",
            ));
        }
        let state = self
            .read_run_state(source_id)?
            .ok_or_else(|| refuse("source checkpoints missing"))?;
        let receipt = state.failure_activity_checkpoint.as_ref().ok_or_else(|| {
            refuse("the run's failure handoff recorded no receipt keeping its pull request")
        })?;
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
                "the task specification changed after its run published pull request #{}, so \
                 that candidate no longer implements this task",
                output["pr_number"].as_str().unwrap_or("?")
            )));
        }
        let (_, _, binding) = self.observe_escalated_publication(source, task.clone())?;
        self.require_green_required_checks(&binding)?;
        let entry = self
            .published_owner_requeue_record(task, source_id)?
            .ok_or_else(|| refuse("the run's publication cannot be preserved"))?;
        Ok((entry, binding))
    }

    /// Refuse unless every required check on `binding`'s pull request has
    /// passed on exactly its recorded head. Checks on another head describe a
    /// different candidate, and a head reporting no required check at all
    /// proves nothing.
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
        if blockers.passed_required_checks.is_empty() {
            return Err(refuse(
                "the recorded head reports no required check, so none has passed",
            ));
        }
        Ok(())
    }
}
