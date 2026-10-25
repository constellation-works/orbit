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
//! A claimed leaf publishes nothing through a failure handoff: its claim's
//! settlement keeps its committed candidate on the owner, which records it on
//! the task and hands it to the task's next run, claimed or owner-local.
//! A requeue reports the candidate the next admission offers — the latest
//! settlement that preserved one, which a later claim that kept none does not
//! replace — read from the same record admission reads, and refuses it, as a
//! published one, once the task's specification changed.
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
use orbit_store::contracts::{CandidateFreshReason, KeptClaimCandidate};
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    CANDIDATE_DISCARDED_EVENT, FINAL_RECOVERY_DECIDED_EVENT, RecordedFinalRecovery, Task,
    TaskComment, TaskDecision, TaskHistoryEntry, TaskStatus,
};
use serde_json::{Value, json};

use super::TaskRecordUpdateParams as StoreTaskUpdateParams;
use super::helpers::SYSTEM_ACTOR_LABEL;
use crate::OrbitRuntime;
use crate::application::task::{TaskJobPort, TaskRecords};
use orbit_types::workflow::SourceRepairBinding;

/// Status event of an operator's requeue of a blocked task.
pub(crate) const OPERATOR_REQUEUED_EVENT: &str = "operator_requeued";

/// How a refused requeue offers to start over instead.
const DISCARD_HINT: &str = "requeue with --discard-candidate (MCP: discard_candidate=true) to \
                            close it and let the next run implement fresh";

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
    /// The final-recovery decision kind recorded with `requeued`, which is
    /// written only together with a preservation record.
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

    /// Return blocked `task` to the backlog with `requeue`'s records, in one
    /// compare-and-set write. Called under the task write lock. The run the
    /// decision names is the task's current one.
    pub(crate) fn write_candidate_requeue(
        &self,
        task: &Task,
        requeue: CandidateRequeue,
    ) -> Result<Task, OrbitError> {
        // The `requeued` decision is what lets the next run resume the
        // preserved publication, and every reader takes the newest decision
        // for a run as its outcome. Without a preservation record nothing is
        // resumed, so the run's recorded escalation stands.
        let decided = task
            .job_run_id
            .as_ref()
            .filter(|_| requeue.preserved.is_some())
            .map(|run_id| TaskHistoryEntry {
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
    ///
    /// A claimed task's run is a follower's leaf, which this store may never
    /// see and whose process no local probe can reach: an unsettled execution
    /// claim (or one that cannot be read) counts as live, exactly as the PR
    /// closure counts it. Once the claim is settled its leaf is fenced;
    /// a foreign binding never reads a same-ID run from this store.
    pub(crate) fn unsettled_run(&self, task: &Task) -> Result<Option<String>, OrbitError> {
        if let Some(reason) = self.live_task_delivery(task) {
            return Ok(Some(format!(
                "{reason}, so a failure handoff may still publish a candidate"
            )));
        }
        // A settled claim fences a follower's leaf. A same-ID local run
        // cannot supply liveness or publication evidence for that leaf.
        if !self.task_run_is_local(task) {
            return Ok(None);
        }
        let Some(run_id) = task.job_run_id.as_deref() else {
            return Ok(None);
        };
        let Some(run) = self.stored_job_run(run_id)? else {
            // Not in this store: a follower's leaf, fenced by its settled claim.
            return Ok(None);
        };
        let foreign = match (&run.executed_on, self.automation_machine_identity()) {
            (Some(origin), Some(local)) => origin.machine_id != local,
            _ => false,
        };
        if !run.state.is_terminal()
            || run.finished_at.is_none()
            || (!foreign && !self.run_owner_stopped(&run))
        {
            return Ok(Some(format!(
                "run `{run_id}` has not conclusively stopped, so its failure handoff may still \
                 publish a candidate"
            )));
        }
        Ok(None)
    }

    /// Requeue blocked task `id` as an operator decision. Without `discard`,
    /// report names the candidate the task's stopped run published through its
    /// failure handoff, or the latest candidate a claim of the task kept. A
    /// later candidate-less claim does not replace it; a later candidate-less
    /// owner-local binding makes only the next owner-local run implement fresh.
    /// With neither candidate, the next run implements fresh. With `discard`, the
    /// published or kept candidate is discarded, the task's undelivered Orbit
    /// pull requests are closed, and the next run implements fresh.
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
        let kept = if discard || resume.is_some() {
            None
        } else {
            self.kept_claim_requeue(&task)?
        };
        // Owner-local setup follows the task's last binding. A foreign binding
        // reads the latest kept claim; a local binding can fall back only to
        // the candidate kept from that same local leaf. Claim admission can
        // still offer an older candidate after a candidate-less local attempt.
        let owner_local = kept.as_ref().is_some_and(|kept| {
            let local = self.automation_machine_identity();
            task.job_run_id.is_some()
                && (kept.candidate.durable() || local == Some(kept.machine_id.as_str()))
                && (task
                    .job_run_machine
                    .as_ref()
                    .is_some_and(|origin| Some(origin.machine_id.as_str()) != local)
                    || (local == Some(kept.machine_id.as_str())
                        && task.job_run_id == kept.candidate.source_run_id))
        });
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
                match (&resume, &kept) {
                    (Some((_, binding)), _) => (
                        format!(
                            "requeued onto the published candidate (pull request #{}, head {}): \
                             {reason}",
                            binding.pull_request, binding.head_sha
                        ),
                        resume.as_ref().and_then(|(entry, _)| entry.clone()),
                        Vec::new(),
                    ),
                    (None, Some(kept)) => {
                        // Run ids are unique only per machine, so the note
                        // names the stopped run as the keeper only when both
                        // agree. A later candidate-less claim, or a colliding
                        // id on another machine, is not that run.
                        let whose = if Self::stopped_run_kept_this_candidate(&current, kept) {
                            "its claim kept"
                        } else {
                            "an earlier claim kept"
                        };
                        let route = if owner_local {
                            "requeued onto"
                        } else {
                            "requeued for claimed continuation of"
                        };
                        (
                            format!(
                                "{route} the candidate {whose} (head {} on {}): {reason}",
                                kept.candidate.head_sha, kept.candidate.branch
                            ),
                            None,
                            Vec::new(),
                        )
                    }
                    (None, None) => (format!("requeued: {reason}"), None, Vec::new()),
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
            if !self.context.settings().pr_settings().close_on_terminal {
                "disabled"
            } else if self.close_task_prs_after_requeue(
                TaskStatus::Blocked,
                &task,
                Some(&reason),
                true,
            ) {
                "settled"
            } else {
                "incomplete"
            }
        });
        let resumes = match (&resume, &kept) {
            (Some((_, binding)), _) => Some(json!({
                "source": "failure_handoff",
                "pull_request": binding.pull_request,
                "branch": binding.branch,
                "head_sha": binding.head_sha,
            })),
            (None, Some(kept)) => Some(json!({
                "source": "claim",
                "pull_request": kept.candidate.pull_request,
                "branch": kept.candidate.branch,
                "head_sha": kept.candidate.head_sha,
                "durable_ref": kept.candidate.durable_ref,
                "machine_id": kept.machine_id,
                "source_run_id": kept.candidate.source_run_id,
                "owner_local": owner_local,
                "host_local": !kept.candidate.durable(),
            })),
            (None, None) => None,
        };
        let next_step = match (&resume, &kept) {
            _ if discard => "The next run implements fresh.".to_string(),
            (Some(_), _) => "The next run adopts this candidate and pull request without \
                             re-implementing, then validates, reviews and lands it afresh."
                .to_string(),
            (None, Some(kept)) if !owner_local => {
                let claimed = if kept.candidate.durable() {
                    "A claimed run continues".to_string()
                } else {
                    format!("A claimed run on {} continues", kept.machine_id)
                };
                format!(
                    "The next owner-local run implements fresh. {claimed} the candidate at {} \
                     kept from run '{}' on machine '{}', \
                     then validates, reviews and lands it afresh.{}",
                    kept.candidate.head_sha,
                    kept.candidate.source_run_id.as_deref().unwrap_or("unknown"),
                    kept.machine_id,
                    if kept.candidate.durable() {
                        ""
                    } else {
                        " A claimed run on another host implements fresh."
                    }
                )
            }
            (None, Some(kept)) if !kept.candidate.durable() => format!(
                "The candidate is only on {}: a run there continues it instead of implementing \
                 fresh, and a run on another host implements fresh.",
                kept.machine_id
            ),
            (None, Some(_)) => "The next run, claimed or local, continues this candidate \
                                instead of implementing fresh, then validates, reviews and \
                                lands it afresh."
                .to_string(),
            (None, None) => {
                "The next run starts from the backlog; no published or kept candidate was \
                 recorded."
                    .to_string()
            }
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
    fn kept_claim_requeue(&self, task: &Task) -> Result<Option<KeptClaimCandidate>, OrbitError> {
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
                 was changed. To {DISCARD_HINT}.",
                kept.candidate.head_sha, kept.candidate.branch
            ))),
        }
    }

    /// Whether `task`'s stopped run is the run that kept `candidate`.
    ///
    /// Run ids are unique only per machine, so the id and the machine both
    /// have to agree. A matching id on another machine is a different run.
    /// The check never decides which candidate admission offers: a later claim
    /// that kept nothing leaves the latest preserved candidate in force.
    fn stopped_run_kept_this_candidate(task: &Task, kept: &KeptClaimCandidate) -> bool {
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
        use orbit_types::workflow::{CANDIDATE_HELD_EVENT, HeldCandidate};

        let history = self.get_task_history(&task.id)?;
        let Some(held) = history
            .iter()
            .rev()
            .find(|entry| entry.event == CANDIDATE_HELD_EVENT)
            .and_then(HeldCandidate::from_entry)
        else {
            return Ok(false);
        };
        if task.job_run_id.as_deref() != Some(held.run_id.as_str()) {
            return Ok(false);
        }
        // Run ids are unique only per machine.
        Ok(task
            .job_run_machine
            .as_ref()
            .is_none_or(|origin| held.machine_id.as_deref() == Some(origin.machine_id.as_str())))
    }

    /// Authenticate a plain blocked-to-backlog status update without a
    /// discard, exactly as an operator requeue does. Returns the
    /// publication-preservation record to append with the status change
    /// (`None` when the same record is already on file or nothing was
    /// published). A refusal leaves the task blocked with nothing written.
    pub(crate) fn authenticate_blocked_to_backlog(
        &self,
        task: &Task,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError> {
        // No bound publishing run: there is no publication this move could
        // resume or strand (a bare pull-request record on a task that was
        // blocked by hand is the PR-closure path's concern, not a stopped
        // run's candidate).
        if task.job_run_id.is_none() {
            self.kept_claim_requeue(task)?;
            return Ok(None);
        }
        let receipt = self.handoff_receipt(task)?;
        if let Some((preserved, _binding)) =
            self.authenticate_operator_requeue(task, receipt.as_ref())?
        {
            return Ok(preserved);
        }
        // A kept claim candidate the specification has outgrown is refused
        // the same way, as a requeue refuses it.
        self.kept_claim_requeue(task)?;
        Ok(None)
    }

    /// Authenticate what an operator requeue without a discard resumes: the
    /// failure handoff's publication when there is one, else nothing, refusing
    /// exactly what the next admission would refuse. The preservation record
    /// is `None` when the task already holds that same record.
    fn authenticate_operator_requeue(
        &self,
        task: &Task,
        receipt: Option<&HandoffReceipt>,
    ) -> Result<Option<(Option<TaskHistoryEntry>, SourceRepairBinding)>, OrbitError> {
        let discard_hint = DISCARD_HINT;
        if let Some(receipt) = receipt {
            if !task.spec_digest_matches(&receipt.task_spec_digest) {
                return Err(OrbitError::InvalidInput(format!(
                    "the task specification changed after its run published pull request \
                     #{}, so that candidate no longer implements this task and cannot be \
                     resumed. Nothing was changed. To {discard_hint}.",
                    receipt.pull_request
                )));
            }
            // Every authentication refusal (an interrupted or timed-out
            // run, a newer execution, a foreign source, ...) has the same exit.
            let authenticated = self
                .failure_handoff_preservation(task)
                .and_then(|(entry, binding)| Ok((self.preservation_append(task, entry)?, binding)));
            return authenticated.map(Some).map_err(|error| match error {
                OrbitError::JobValidation(refusal) => OrbitError::InvalidInput(format!(
                    "{refusal}. Nothing was changed. To {discard_hint}."
                )),
                error => error,
            });
        }
        if self.run_published_through_pipeline(task)? {
            return Err(OrbitError::InvalidInput(format!(
                "the task's stopped run published through its own pipeline steps with no \
                 failure-handoff receipt to authenticate, so a requeue cannot resume it. \
                 Nothing was changed. Inspect it with explicit source repair \
                 (`orbit.workflow.run.resume` with `source_repair`), or {discard_hint}."
            )));
        }
        if self.unbound_publication(task)? {
            return Err(OrbitError::InvalidInput(format!(
                "the task has a pull request or preserved candidate on record that its stopped \
                 run's handoff does not bind, so its next run would refuse to start. Nothing \
                 was changed. Inspect it with explicit source repair, or {discard_hint}."
            )));
        }
        Ok(None)
    }
}
