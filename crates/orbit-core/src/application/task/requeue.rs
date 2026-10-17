//! Requeue a blocked task onto the candidate its stopped run published.
//!
//! A run whose failure handoff published a `[BLOCKED]` pull request leaves
//! the task blocked on an escalation. Returning it to the backlog by hand
//! used to strand it: the next run found a publication it could not
//! authenticate and refused, and only archive and refile got the work moving.
//!
//! A requeue records exactly what the published-owner resume path
//! authenticates: a `requeued` final-recovery decision for the stopped run,
//! which supersedes its escalation, and the publication-preservation record
//! for the handoff's pull request. Nothing here trusts the requeue itself:
//! the record is written only after the handoff receipt authenticates in full
//! (owner-local source, conclusively stopped, newest execution, no discard,
//! unchanged task specification), and the next run re-authenticates it from
//! the immutable source before adopting the branch. Everything after
//! adoption — validation, review, landing — runs afresh.
//!
//! Two callers share [`OrbitRuntime::write_candidate_requeue`]: the operator
//! command (`orbit task requeue`, `orbit.task.requeue`) and the cleared-infra
//! requeue of a transient infrastructure block (`transient_infra`).
//!
//! A requeue waits for the task's run to stop: final recovery blocks the task
//! before the run's failure handoff publishes, and a requeue in between would
//! bind nothing to the pull request the handoff then opens. A task with a
//! pull request or preserved candidate on record that no handoff receipt
//! binds is refused, as its next admission would refuse it.
//!
//! A specification edited since the publication is refused: that candidate
//! implements the old task. The refusal offers `discard_candidate`, which
//! records the discard, returns the task to the backlog so the next run
//! implements fresh, and closes the task's undelivered Orbit pull requests
//! exactly as `orbit task update --status backlog --discard-candidate` does.
//!
//! The decision and preservation records are written by `system`, since the
//! published-owner resume trusts only Orbit's own records; the operator's
//! name and reason are on the status change and its comment.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    CANDIDATE_DISCARDED_EVENT, FINAL_RECOVERY_DECIDED_EVENT, RecordedFinalRecovery, Task,
    TaskComment, TaskDecision, TaskHistoryEntry, TaskStatus,
};
use serde_json::{Value, json};

use super::TaskRecordUpdateParams as StoreTaskUpdateParams;
use super::helpers::SYSTEM_ACTOR_LABEL;
use crate::OrbitRuntime;
use crate::application::job::{RunOwnerLiveness, run_owner_liveness};
use orbit_types::workflow::SourceRepairBinding;

/// Status event of an operator's requeue of a blocked task.
pub(crate) const OPERATOR_REQUEUED_EVENT: &str = "operator_requeued";

/// The publication a failure handoff recorded, read from its receipt without
/// authenticating it. It names what a requeue would resume; the requeue
/// itself authenticates before writing anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HandoffReceipt {
    pub(crate) run_id: String,
    pub(crate) pull_request: String,
    pub(crate) branch: String,
    pub(crate) head_sha: String,
    pub(crate) task_spec_digest: String,
}

/// What a requeue of a blocked task writes beside its status change.
pub(crate) struct CandidateRequeue {
    pub(crate) actor: String,
    pub(crate) status_event: &'static str,
    pub(crate) status_note: String,
    /// The final-recovery decision kind recorded with `requeued`.
    pub(crate) decision: &'static str,
    pub(crate) comment: Option<TaskComment>,
    /// The publication-preservation record the next run authenticates.
    pub(crate) preserved: Option<TaskHistoryEntry>,
    pub(crate) history: Vec<TaskHistoryEntry>,
}

/// Whether the newest typed final-recovery decision Orbit recorded for
/// `task`'s current run is `requeued`, with no discard after it. Such a
/// requeue keeps the run's publication for the next run to resume, whichever
/// status event carried it (final recovery, an operator, the cleared-infra
/// requeue), so the task's moves back to the backlog must not close it.
pub(crate) fn current_run_requeue_stands(task: &Task, history: &[TaskHistoryEntry]) -> bool {
    let Some(run_id) = task.job_run_id.as_deref() else {
        return false;
    };
    for entry in history.iter().rev() {
        if entry.event == CANDIDATE_DISCARDED_EVENT {
            return false;
        }
        if let Some(TaskDecision::FinalRecovery {
            run_id: decided,
            outcome,
            ..
        }) = &entry.decision
            && entry.by == SYSTEM_ACTOR_LABEL
            && decided == run_id
        {
            return *outcome == RecordedFinalRecovery::Requeued;
        }
    }
    false
}

impl OrbitRuntime {
    /// The pull request `task`'s current run's failure handoff published,
    /// when it published one.
    pub(crate) fn handoff_receipt(
        &self,
        task: &Task,
    ) -> Result<Option<HandoffReceipt>, OrbitError> {
        let Some(run_id) = task.job_run_id.as_deref() else {
            return Ok(None);
        };
        let Some(state) = self.read_run_state(run_id)? else {
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

    /// Whether `run_id` pushed or opened a pull request through its own
    /// pipeline steps (as opposed to its failure handoff).
    pub(crate) fn run_published_through_pipeline(&self, run_id: &str) -> Result<bool, OrbitError> {
        Ok(self.read_run_state(run_id)?.is_some_and(|state| {
            ["push", "pr_open"].iter().any(|key| {
                state
                    .pipeline
                    .get(*key)
                    .is_some_and(|value| !value.is_null())
            })
        }))
    }

    /// Return blocked `task` to the backlog with `requeue`'s records, in one
    /// compare-and-set write. Called under the task write lock. The run the
    /// decision names is the task's current one.
    pub(crate) fn write_candidate_requeue(
        &self,
        task: &Task,
        requeue: CandidateRequeue,
    ) -> Result<Task, OrbitError> {
        let decided = task.job_run_id.as_ref().map(|run_id| TaskHistoryEntry {
            at: Utc::now(),
            by: SYSTEM_ACTOR_LABEL.to_string(),
            event: FINAL_RECOVERY_DECIDED_EVENT.to_string(),
            note: Some(format!(
                "{} (run_id={run_id}): outcome=requeued",
                requeue.decision
            )),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: Some(TaskDecision::FinalRecovery {
                run_id: run_id.clone(),
                decision: requeue.decision.to_string(),
                outcome: RecordedFinalRecovery::Requeued,
            }),
        });
        self.with_mutation(|| {
            let updated = self.stores().task_records().update(
                &task.id,
                StoreTaskUpdateParams {
                    actor: requeue.actor.clone(),
                    status: Some(TaskStatus::Backlog),
                    status_event: Some(requeue.status_event.to_string()),
                    status_note: Some(requeue.status_note.clone()),
                    append_comments: requeue.comment.clone().into_iter().collect(),
                    append_history: decided
                        .clone()
                        .into_iter()
                        .chain(requeue.preserved.clone())
                        .chain(requeue.history.clone())
                        .collect(),
                    expected_status: Some(vec![TaskStatus::Blocked]),
                    ..Default::default()
                },
            )?;
            Ok((
                updated,
                OrbitEvent::TaskUpdated {
                    id: task.id.clone(),
                },
            ))
        })
    }

    /// Why `task`'s current run cannot be requeued yet, or `None` once it has
    /// conclusively stopped. Final recovery blocks the task before the run's
    /// failure handoff runs, so a requeue in that window would race the
    /// handoff's publication and leave a pull request no record binds.
    pub(crate) fn unsettled_run(&self, task: &Task) -> Result<Option<String>, OrbitError> {
        let Some(run_id) = task.job_run_id.as_deref() else {
            return Ok(None);
        };
        let Some(run) = self.get_job_run_backend(run_id)? else {
            return Ok(None);
        };
        if !run.state.is_terminal()
            || run.finished_at.is_none()
            || run_owner_liveness(&run) != RunOwnerLiveness::Stopped
        {
            return Ok(Some(format!(
                "run `{run_id}` has not conclusively stopped, so its failure handoff may still \
                 publish a candidate"
            )));
        }
        Ok(None)
    }

    /// Requeue blocked task `id` as an operator decision. Without `discard`,
    /// the next run resumes the candidate the task's stopped run published
    /// through its failure handoff, or implements fresh when nothing was
    /// published. With `discard`, the published candidate is discarded, the
    /// task's undelivered Orbit pull requests are closed, and the next run
    /// implements fresh.
    ///
    /// The publication is authenticated before the task write lock is taken,
    /// since that asks the forge and the remote; under the lock only cheap
    /// invariants are re-checked before the compare-and-set write.
    pub fn requeue_blocked_task(
        &self,
        id: &str,
        reason: &str,
        discard: bool,
    ) -> Result<Value, OrbitError> {
        let reason = redact_all(reason.trim());
        if reason.is_empty() {
            return Err(OrbitError::InvalidInput(
                "requeue needs a reason; it is recorded with the decision".into(),
            ));
        }
        self.ensure_coordination_task_write_permitted()?;
        let actor = self.actor().resolve_write_label(None, None)?;
        let task = self.get_task(id)?;
        self.ensure_requeueable(&task)?;
        let receipt = self.handoff_receipt(&task)?;
        let resume = if discard {
            None
        } else {
            self.authenticate_operator_requeue(&task, receipt.as_ref())?
        };
        let mut answer = None;
        self.stores().tasks().with_task_write_lock(id, &mut || {
            let current = self.get_task(id)?;
            self.ensure_requeueable(&current)?;
            if current.updated_at != task.updated_at || current.job_run_id != task.job_run_id {
                return Err(OrbitError::InvalidInput(format!(
                    "{id} changed while its requeue was being authenticated; nothing was \
                     changed, so requeue it again"
                )));
            }
            let now = Utc::now();
            let comment = TaskComment {
                at: now,
                by: actor.clone(),
                message: format!(
                    "operator requeue{}: {reason}",
                    if discard { " (candidate discarded)" } else { "" }
                ),
                operation_id: None,
            };
            let (status_note, preserved, history) = if discard {
                let discarded = TaskHistoryEntry {
                    at: now,
                    by: actor.clone(),
                    event: CANDIDATE_DISCARDED_EVENT.to_string(),
                    note: Some(match current.job_run_id.as_deref() {
                        Some(run_id) => format!(
                            "discarded the candidate run `{run_id}` published; the next run implements fresh"
                        ),
                        None => "discarded any preserved candidate; the next run implements fresh"
                            .to_string(),
                    }),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                };
                (
                    format!("requeued to implement fresh: {reason}"),
                    None,
                    vec![discarded],
                )
            } else {
                match &resume {
                    Some((_, binding)) => (
                        format!(
                            "requeued onto the published candidate (pull request #{}, head {}): \
                             {reason}",
                            binding.pull_request, binding.head_sha
                        ),
                        resume.as_ref().map(|(entry, _)| entry.clone()),
                        Vec::new(),
                    ),
                    None => (format!("requeued: {reason}"), None, Vec::new()),
                }
            };
            let updated = self.write_candidate_requeue(
                &current,
                CandidateRequeue {
                    actor: actor.clone(),
                    status_event: OPERATOR_REQUEUED_EVENT,
                    status_note,
                    decision: "operator_requeue",
                    comment: Some(comment),
                    preserved,
                    history,
                },
            )?;
            answer = Some(updated);
            Ok(())
        })?;
        let task =
            answer.ok_or_else(|| OrbitError::Execution("requeue wrote no decision".into()))?;
        // After the write, like every PR closure: a forge error leaves the
        // requeue standing and is reported instead. The same closure as
        // `orbit task update --status backlog --discard-candidate`.
        let discard_closure = discard.then(|| {
            if self.close_task_prs_after_requeue(TaskStatus::Blocked, &task, Some(&reason), true) {
                "settled"
            } else {
                "incomplete"
            }
        });
        let resumes = resume.as_ref().map(|(_, binding)| {
            json!({
                "pull_request": binding.pull_request,
                "branch": binding.branch,
                "head_sha": binding.head_sha,
            })
        });
        let next_step = if discard {
            "The next run implements fresh."
        } else if resumes.is_some() {
            "The next run adopts this candidate and pull request without re-implementing, then validates, reviews and lands it afresh."
        } else {
            "The next run starts from the backlog; no published candidate was recorded."
        };
        Ok(json!({
            "id": task.id,
            "status": task.status,
            "source_run_id": receipt.as_ref().map(|r| r.run_id.clone()).or(task.job_run_id.clone()),
            "resumes": resumes,
            "candidate_discarded": discard,
            "discard_closure": discard_closure,
            "next_step": next_step,
        }))
    }

    /// Refuse a requeue of `task` unless it is blocked and its run has
    /// conclusively stopped.
    fn ensure_requeueable(&self, task: &Task) -> Result<(), OrbitError> {
        if task.status != TaskStatus::Blocked {
            return Err(OrbitError::InvalidInput(format!(
                "only a blocked task can be requeued; {} is {}",
                task.id, task.status
            )));
        }
        if let Some(reason) = self.unsettled_run(task)? {
            return Err(OrbitError::InvalidInput(format!(
                "{} cannot be requeued yet: {reason}. Nothing was changed; requeue it once the \
                 run has stopped.",
                task.id
            )));
        }
        Ok(())
    }

    /// Authenticate what an operator requeue without a discard resumes: the
    /// failure handoff's publication when there is one, else nothing, refusing
    /// exactly what the next admission would refuse.
    fn authenticate_operator_requeue(
        &self,
        task: &Task,
        receipt: Option<&HandoffReceipt>,
    ) -> Result<Option<(TaskHistoryEntry, SourceRepairBinding)>, OrbitError> {
        let discard_hint = "requeue with --discard-candidate (MCP: discard_candidate=true) to \
                            close it and let the next run implement fresh";
        if let Some(receipt) = receipt {
            if !task.spec_digest_matches(&receipt.task_spec_digest) {
                return Err(OrbitError::InvalidInput(format!(
                    "the task specification changed after its run published pull request \
                     #{}, so that candidate no longer implements this task and cannot be \
                     resumed. Nothing was changed. To {discard_hint}.",
                    receipt.pull_request
                )));
            }
            return self.failure_handoff_requeue_record(task).map(Some);
        }
        if let Some(run_id) = task.job_run_id.as_deref()
            && self.run_published_through_pipeline(run_id)?
        {
            return Err(OrbitError::InvalidInput(format!(
                "the task's stopped run published through its own pipeline steps with no \
                 failure-handoff receipt to authenticate, so a requeue cannot resume it. \
                 Nothing was changed. Inspect it with explicit source repair \
                 (`orbit.workflow.run.resume` with `source_repair`), or {discard_hint}."
            )));
        }
        if self.unbound_publication_on_record(task)? {
            return Err(OrbitError::InvalidInput(format!(
                "the task has a pull request or preserved candidate on record that its stopped \
                 run's handoff does not bind, so its next run would refuse to start. Nothing \
                 was changed. Inspect it with explicit source repair, or {discard_hint}."
            )));
        }
        Ok(None)
    }
}
