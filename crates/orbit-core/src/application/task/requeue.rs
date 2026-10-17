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
//! A specification edited since the publication is refused: that candidate
//! implements the old task. The refusal offers `discard_candidate`, which
//! records the discard, returns the task to the backlog so the next run
//! implements fresh, and closes the discarded pull request.

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

    /// Requeue blocked task `id` as an operator decision. Without `discard`,
    /// the next run resumes the candidate the task's stopped run published
    /// through its failure handoff, or implements fresh when nothing was
    /// published. With `discard`, the published candidate is discarded, its
    /// pull request closed, and the next run implements fresh.
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
        let mut answer = None;
        self.stores().tasks().with_task_write_lock(id, &mut || {
            let task = self.get_task(id)?;
            if task.status != TaskStatus::Blocked {
                return Err(OrbitError::InvalidInput(format!(
                    "only a blocked task can be requeued; {id} is {}",
                    task.status
                )));
            }
            let receipt = self.handoff_receipt(&task)?;
            let pipeline_published = match task.job_run_id.as_deref() {
                Some(run_id) => self.run_published_through_pipeline(run_id)?,
                None => false,
            };
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
            if discard {
                let discarded = TaskHistoryEntry {
                    at: now,
                    by: actor.clone(),
                    event: CANDIDATE_DISCARDED_EVENT.to_string(),
                    note: Some(match task.job_run_id.as_deref() {
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
                let updated = self.write_candidate_requeue(
                    &task,
                    CandidateRequeue {
                        actor: actor.clone(),
                        status_event: OPERATOR_REQUEUED_EVENT,
                        status_note: format!("requeued to implement fresh: {reason}"),
                        decision: "operator_requeue",
                        comment: Some(comment.clone()),
                        preserved: None,
                        history: vec![discarded],
                    },
                )?;
                answer = Some((updated, receipt.clone(), None, true));
                return Ok(());
            }
            if let Some(receipt) = &receipt
                && !task.spec_digest_matches(&receipt.task_spec_digest)
            {
                return Err(OrbitError::InvalidInput(format!(
                    "the task specification changed after its run published pull request \
                     #{}, so that candidate no longer implements this task and cannot be \
                     resumed. Nothing was changed. To close it and let the next run implement \
                     fresh, requeue with --discard-candidate (MCP: discard_candidate=true).",
                    receipt.pull_request
                )));
            }
            if receipt.is_none() && pipeline_published {
                return Err(OrbitError::InvalidInput(
                    "the task's stopped run published through its own pipeline steps with no \
                     failure-handoff receipt to authenticate, so a requeue cannot resume it. \
                     Inspect it with explicit source repair (`orbit.workflow.run.resume` with \
                     `source_repair`), or requeue with --discard-candidate to implement fresh. \
                     Nothing was changed."
                        .into(),
                ));
            }
            let (preserved, binding) = match &receipt {
                Some(_) => {
                    let (entry, binding) = self.failure_handoff_requeue_record(&task)?;
                    (Some(entry), Some(binding))
                }
                None => (None, None),
            };
            let note = match &binding {
                Some(binding) => format!(
                    "requeued onto the published candidate (pull request #{}, head {}): {reason}",
                    binding.pull_request, binding.head_sha
                ),
                None => format!("requeued: {reason}"),
            };
            let updated = self.write_candidate_requeue(
                &task,
                CandidateRequeue {
                    actor: actor.clone(),
                    status_event: OPERATOR_REQUEUED_EVENT,
                    status_note: note,
                    decision: "operator_requeue",
                    comment: Some(comment.clone()),
                    preserved,
                    history: Vec::new(),
                },
            )?;
            answer = Some((updated, receipt.clone(), binding, false));
            Ok(())
        })?;
        let (task, receipt, binding, discarded) =
            answer.ok_or_else(|| OrbitError::Execution("requeue wrote no decision".into()))?;
        // After the write, like every PR closure: a forge error leaves the
        // requeue standing and is reported instead.
        let mut closed = Value::Null;
        let mut close_error = Value::Null;
        if discarded && let Some(receipt) = &receipt {
            match receipt.pull_request.parse::<u64>() {
                Ok(number) => match self.close_discarded_candidate_pr(&task, number, &reason) {
                    Ok(true) => closed = json!(receipt.pull_request),
                    Ok(false) => {}
                    Err(error) => close_error = json!(error.to_string()),
                },
                Err(_) => {
                    close_error = json!(format!(
                        "pull request `{}` is not a number",
                        receipt.pull_request
                    ))
                }
            }
        }
        let resumes = binding.as_ref().map(|binding| {
            json!({
                "pull_request": binding.pull_request,
                "branch": binding.branch,
                "head_sha": binding.head_sha,
            })
        });
        let next_step = if discarded {
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
            "candidate_discarded": discarded,
            "closed_pull_request": closed,
            "close_error": close_error,
            "next_step": next_step,
        }))
    }
}
