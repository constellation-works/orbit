//! The candidate a requeue resumes: the handoff receipt, pipeline publication,
//! kept-claim and held candidates, and the preserved-candidate binding.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::{CandidateFreshReason, JobRunQuery, KeptClaimCandidate};
use orbit_types::task::Task;
use orbit_types::workflow::HeldCandidate;
use serde_json::Value;

use super::{HandoffReceipt, operator_discard_hint};
use crate::OrbitRuntime;
use crate::application::task::TaskJobPort;

impl OrbitRuntime {
    /// The pull request `task`'s current run's failure handoff published,
    /// when it published one.
    pub(crate) fn handoff_receipt(
        &self,
        task: &Task,
    ) -> Result<Option<HandoffReceipt>, OrbitError> {
        if !self.task_run_is_local(task) {
            return Ok(None);
        }
        let Some(run_id) = task.job_run_id.as_deref() else {
            return Ok(None);
        };
        let Some(state) = self.stored_run_state(run_id)? else {
            return Ok(None);
        };
        let Some(checkpoint) = state.failure_activity_checkpoint else {
            return Ok(None);
        };
        let output = &checkpoint.output;
        let text = |key: &str| output.get(key).and_then(Value::as_str).map(str::to_string);
        if checkpoint.activity_name != "pr_failure_handoff"
            || !matches!(
                output.get("decision").and_then(Value::as_str),
                Some("blocked_failure_pr" | "blocked_conflict_pr")
            )
        {
            return Ok(None);
        }
        let (Some(pull_request), Some(branch), Some(head_sha), Some(task_spec_digest)) = (
            text("pr_number").or_else(|| {
                output
                    .get("pr_number")
                    .and_then(Value::as_u64)
                    .map(|n| n.to_string())
            }),
            text("branch"),
            text("head_sha"),
            text("task_spec_digest"),
        ) else {
            return Ok(None);
        };
        Ok(Some(HandoffReceipt {
            run_id: run_id.to_string(),
            pull_request,
            branch,
            head_sha,
            task_spec_digest,
        }))
    }

    /// Whether the task's local run pushed or opened a pull request through
    /// its own pipeline steps (as opposed to its failure handoff).
    pub(crate) fn run_published_through_pipeline(&self, task: &Task) -> Result<bool, OrbitError> {
        if !self.task_run_is_local(task) {
            return Ok(false);
        }
        let Some(run_id) = task.job_run_id.as_deref() else {
            return Ok(false);
        };
        Ok(self.stored_run_state(run_id)?.is_some_and(|state| {
            ["push", "pr_open"].iter().any(|key| {
                state
                    .pipeline
                    .get(*key)
                    .is_some_and(|value| !value.is_null())
            })
        }))
    }

    /// The candidate the next admission offers `task`: the latest claim
    /// settlement that preserved one. A later claim that ended without a
    /// candidate does not replace it, so this does not require the stopped
    /// run to be the leaf that kept it. A candidate the task's specification
    /// has outgrown is refused, as a published one is; one an operator already
    /// discarded resumes nothing.
    ///
    /// Run ids are unique only per machine. Whether the stopped run itself
    /// kept the candidate is [`Self::stopped_run_kept_this_candidate`]; that
    /// check never decides that no candidate exists, and a bare run id is
    /// never enough to call the stopped run the keeper. A candidate the
    /// stopped run held, identified the same way, is what admission offers
    /// instead of an earlier claim settlement.
    pub(super) fn kept_claim_requeue(
        &self,
        task: &Task,
    ) -> Result<Option<KeptClaimCandidate>, OrbitError> {
        let Some(kept) = self.stores().tasks().kept_claim_candidate(
            &task.id,
            self.automation_machine_identity().unwrap_or_default(),
        )?
        else {
            return Ok(None);
        };
        if self.stopped_run_holds_candidate(task)? {
            return Ok(None);
        }
        match kept.fresh.as_ref().map(|(reason, _)| *reason) {
            // Durability decides only which host can continue it.
            None | Some(CandidateFreshReason::NotDurable) => Ok(Some(kept)),
            Some(CandidateFreshReason::Discarded) => Ok(None),
            Some(CandidateFreshReason::SpecChanged) => Err(OrbitError::InvalidInput(format!(
                "the task specification changed after its claim kept candidate {} on {}, so \
                 that candidate no longer implements this task and cannot be resumed. Nothing \
                 was changed. To {}.",
                kept.candidate.head_sha,
                kept.candidate.branch,
                operator_discard_hint(&task.id, task.status)
            ))),
        }
    }

    /// Whether `task`'s stopped run is the run that kept `candidate`.
    ///
    /// Run ids are unique only per machine, so the id and the machine both
    /// have to agree. A matching id on another machine is a different run.
    /// The check never decides which candidate admission offers: a later claim
    /// that kept nothing leaves the latest preserved candidate in force.
    pub(super) fn stopped_run_kept_this_candidate(task: &Task, kept: &KeptClaimCandidate) -> bool {
        let Some(run_id) = task.job_run_id.as_deref() else {
            return false;
        };
        kept.candidate.source_run_id.as_deref() == Some(run_id)
            && task
                .job_run_machine
                .as_ref()
                .is_none_or(|origin| origin.machine_id == kept.machine_id)
    }

    /// Whether admission offers a candidate `task`'s stopped run held, rather
    /// than the latest claim settlement. The run id matches only together
    /// with the machine, the same linkage admission uses.
    fn stopped_run_holds_candidate(&self, task: &Task) -> Result<bool, OrbitError> {
        Ok(self.stopped_run_held_candidate(task)?.is_some())
    }

    /// The candidate `task`'s stopped run held, while the task is still
    /// linked to that run. The run id matches only together with the
    /// machine, the same linkage admission uses.
    fn stopped_run_held_candidate(
        &self,
        task: &Task,
    ) -> Result<Option<(HeldCandidate, chrono::DateTime<Utc>)>, OrbitError> {
        use orbit_types::workflow::CANDIDATE_HELD_EVENT;

        let history = self.get_task_history(&task.id)?;
        let Some(entry) = history
            .iter()
            .rev()
            .find(|entry| entry.event == CANDIDATE_HELD_EVENT)
        else {
            return Ok(None);
        };
        let Some(held) = HeldCandidate::from_entry(entry) else {
            return Ok(None);
        };
        if task.job_run_id.as_deref() != Some(held.run_id.as_str()) {
            return Ok(None);
        }
        // Run ids are unique only per machine.
        if !task
            .job_run_machine
            .as_ref()
            .is_none_or(|origin| held.machine_id.as_deref() == Some(origin.machine_id.as_str()))
        {
            return Ok(None);
        }
        Ok(Some((held, entry.at)))
    }

    /// The held candidate a requeue reports as resumed: the one admission
    /// offers while the task's specification is unchanged and no operator
    /// discarded it since it was held. A retired hold resumes nothing, as
    /// in `held_candidate_offer`.
    pub(super) fn held_candidate_requeue(
        &self,
        task: &Task,
    ) -> Result<Option<HeldCandidate>, OrbitError> {
        let Some((held, held_at)) = self.stopped_run_held_candidate(task)? else {
            return Ok(None);
        };
        if !task.spec_digest_matches(&held.task_spec_digest) {
            return Ok(None);
        }
        let history = self.get_task_history(&task.id)?;
        if history.iter().any(|entry| {
            entry.event == orbit_types::task::CANDIDATE_DISCARDED_EVENT && entry.at >= held_at
        }) {
            return Ok(None);
        }
        Ok(Some(held))
    }

    /// Whether a task still has a published or retained candidate to discard.
    /// This is a local evidence read, not an authentication of the candidate
    /// for resumption; a changed specification can be the reason for discard.
    pub fn task_has_discardable_candidate(&self, task: &Task) -> Result<bool, OrbitError> {
        use orbit_types::task::{CANDIDATE_DISCARDED_EVENT, TaskStatus};
        if matches!(task.status, TaskStatus::Done | TaskStatus::Archived) {
            return Ok(false);
        }
        let history = self.get_task_history(&task.id)?;
        let discarded_at = history
            .iter()
            .rev()
            .find(|entry| entry.event == CANDIDATE_DISCARDED_EVENT)
            .map(|entry| entry.at);
        if let Some((_, held_at)) = self.stopped_run_held_candidate(task)?
            && discarded_at.is_none_or(|at| held_at > at)
        {
            return Ok(true);
        }
        if let Some(kept) = self.stores().tasks().kept_claim_candidate(
            &task.id,
            self.automation_machine_identity().unwrap_or_default(),
        )? && !matches!(
            kept.fresh.as_ref().map(|(reason, _)| *reason),
            Some(CandidateFreshReason::Discarded)
        ) {
            return Ok(true);
        }
        // A later attempt may have stopped before adopting an older published
        // candidate. Search the task's local delivery history, not just the
        // current binding, and ignore publications retired by a discard.
        let runs = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            job_id: Some(orbit_types::workflow::TASK_PR_PIPELINE_JOB.to_string()),
            task_id: Some(task.id.clone()),
            created_since: discarded_at,
            include_steps: false,
            ..Default::default()
        })?;
        for run in runs {
            if discarded_at.is_some_and(|at| run.created_at <= at) {
                continue;
            }
            if let Some(state) = self.stored_run_state(&run.run_id)? {
                let published = ["push", "pr_open"].iter().any(|key| {
                    state
                        .pipeline
                        .get(*key)
                        .is_some_and(|value| !value.is_null())
                });
                let handoff =
                    state
                        .failure_activity_checkpoint
                        .as_ref()
                        .is_some_and(|checkpoint| {
                            checkpoint.activity_name == "pr_failure_handoff"
                                && matches!(
                                    checkpoint.output.get("decision").and_then(Value::as_str),
                                    Some("blocked_failure_pr" | "blocked_conflict_pr")
                                )
                        });
                if published || handoff {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Whether rewriting the specification of blocked `task` to `description`
    /// and `acceptance_criteria` (either may be `None`: unchanged) strands a
    /// candidate the task's failed run published or kept.
    ///
    /// A bound candidate is stranded by any specification change, including
    /// when required checks are not green or the candidate is already
    /// unresumable for another settled reason. The answer discards it instead
    /// of being refused for the rewrite. A live run is not stranded: the
    /// answer stays refused because the run has not stopped, judged on the
    /// unchanged task. A rewrite that leaves the specification unchanged, or
    /// that no bound candidate implements, is not reported.
    pub(crate) fn rewrite_strands_candidate(
        &self,
        task: &Task,
        description: Option<&str>,
        acceptance_criteria: Option<&[String]>,
    ) -> Result<bool, OrbitError> {
        let mut rewritten = task.clone();
        if let Some(description) = description {
            rewritten.description = description.to_string();
        }
        if let Some(criteria) = acceptance_criteria {
            rewritten.acceptance_criteria = criteria.to_vec();
        }
        if rewritten.spec_digest() == task.spec_digest() {
            return Ok(false);
        }
        // Discarding a live run would race a handoff that may still publish.
        if self.unsettled_run(task)?.is_some() {
            return Ok(false);
        }
        if self.preserved_candidate_bound(task)? {
            return Ok(true);
        }
        match self.authenticate_published_backlog_return(task) {
            Ok(_) => {}
            Err(OrbitError::InvalidInput(_)) => return Ok(false),
            Err(error) => return Err(error),
        }
        match self.authenticate_published_backlog_return(&rewritten) {
            Ok(_) => Ok(false),
            Err(OrbitError::InvalidInput(_)) => Ok(true),
            Err(error) => Err(error),
        }
    }

    /// Whether a preserved candidate is still bound to `task`: a failure-handoff
    /// receipt, a pipeline publication the failure handoff kept, or a kept
    /// claim that has not been discarded. A live run can still be bound; the
    /// caller decides whether that run may be discarded.
    pub(crate) fn preserved_candidate_bound(&self, task: &Task) -> Result<bool, OrbitError> {
        if self.handoff_receipt(task)?.is_some() {
            return Ok(true);
        }
        if self.pipeline_publication_kept(task)? {
            return Ok(true);
        }
        // Admission offers a held candidate instead of the kept claim, and a
        // plain requeue does not refuse that hold for a specification change.
        if self.stopped_run_holds_candidate(task)? {
            return Ok(false);
        }
        let Some(kept) = self.stores().tasks().kept_claim_candidate(
            &task.id,
            self.automation_machine_identity().unwrap_or_default(),
        )?
        else {
            return Ok(false);
        };
        Ok(!matches!(
            kept.fresh.as_ref().map(|(reason, _)| *reason),
            Some(CandidateFreshReason::Discarded)
        ))
    }

    /// Whether `task`'s current run pushed its own pull request and the
    /// failure handoff kept that publication (`candidate_preserved`).
    fn pipeline_publication_kept(&self, task: &Task) -> Result<bool, OrbitError> {
        if !self.run_published_through_pipeline(task)? {
            return Ok(false);
        }
        let Some(run_id) = task.job_run_id.as_deref() else {
            return Ok(false);
        };
        let Some(state) = self.stored_run_state(run_id)? else {
            return Ok(false);
        };
        let Some(checkpoint) = state.failure_activity_checkpoint.as_ref() else {
            return Ok(false);
        };
        let output = &checkpoint.output;
        Ok(checkpoint.activity_name == "pr_failure_handoff"
            && matches!(
                output.get("decision").and_then(Value::as_str),
                Some("review_completion_failure" | "landing_review_failure")
            )
            && output.get("candidate_preserved").and_then(Value::as_bool) == Some(true))
    }
}
