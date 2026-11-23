//! The operator and resident requeue entry points, their admission and the requeue write.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_types::task::{
    CANDIDATE_DISCARDED_EVENT, FINAL_RECOVERY_DECIDED_EVENT, RecordedFinalRecovery, Task,
    TaskComment, TaskDecision, TaskHistoryEntry, TaskStatus,
};
use serde_json::{Value, json};

use super::{CandidateRequeue, OPERATOR_REQUEUED_EVENT};
use crate::OrbitRuntime;
use crate::application::task::TaskRecordUpdateParams as StoreTaskUpdateParams;
use crate::application::task::helpers::SYSTEM_ACTOR_LABEL;
use crate::application::task::{TaskJobPort, TaskRecords};

/// How a refused requeue offers to start over instead: the supported operator
/// CLI discard route, or the discard option of a blocked task's requeue, which
/// the dashboard and the CLI's `orbit task requeue` carry. `orbit.task.update`
/// MCP does not accept `discard_candidate`, which is CLI-only.
pub(super) fn operator_discard_hint(id: &str, status: TaskStatus) -> String {
    if status == TaskStatus::Blocked {
        format!(
            "discard the candidate with `orbit task update {id} --status backlog --discard-candidate` \
             on the operator CLI (orbit.task.update MCP does not accept `discard_candidate`), or \
             requeue the blocked task with the discard option (the dashboard's requeue, or \
             `orbit task requeue {id} --discard-candidate`), to close it and let the next run \
             implement fresh"
        )
    } else {
        format!(
            "discard the candidate with `orbit task update {id} --status backlog --discard-candidate` \
             on the operator CLI (orbit.task.update MCP does not accept `discard_candidate`) to \
             close it and let the next run implement fresh"
        )
    }
}

impl OrbitRuntime {
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
                    // A requeue judges no fault.
                    fault: Default::default(),
                    fault_evidence: String::new(),
                    fault_judged_by: None,
                }),
            });
        self.stores().task_records().update(
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
        )
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
        self.requeue_blocked_task_as(id, reason, discard, None)
    }

    /// Like [`Self::requeue_blocked_task`], recording `actor` in task history
    /// instead of the ambient identity (the dashboard passes its own label).
    pub fn requeue_blocked_task_as(
        &self,
        id: &str,
        reason: &str,
        discard: bool,
        actor: Option<&str>,
    ) -> Result<Value, OrbitError> {
        self.requeue_blocked_task_by(id, reason, discard, actor, "operator requeue")
    }

    /// A resident cycle's requeue. `agent_actor` is the trusted agent identity
    /// of the cycle's run: the task's history entry and comment name it, never
    /// the host's process actor, and the comment reads `resident requeue` so a
    /// person's requeue stays distinguishable.
    pub(crate) fn requeue_blocked_task_as_resident(
        &self,
        id: &str,
        reason: &str,
        discard: bool,
        agent_actor: &str,
    ) -> Result<Value, OrbitError> {
        self.requeue_blocked_task_by(id, reason, discard, Some(agent_actor), "resident requeue")
    }

    fn requeue_blocked_task_by(
        &self,
        id: &str,
        reason: &str,
        discard: bool,
        actor: Option<&str>,
        comment_label: &str,
    ) -> Result<Value, OrbitError> {
        let reason = redact_all(reason.trim());
        if reason.is_empty() {
            return Err(OrbitError::InvalidInput(
                "requeue needs a reason; it is recorded with the decision".into(),
            ));
        }
        self.ensure_coordination_task_write_permitted()?;
        let actor = match actor {
            Some(actor) => actor.to_string(),
            None => self.actor().resolve_write_label(None, None)?,
        };
        let task = self.get_task(id)?;
        self.ensure_requeueable(&task)?;
        let receipt = self.handoff_receipt(&task)?;
        let resume = if discard {
            None
        } else {
            self.authenticate_operator_requeue(&task, receipt.as_ref())?
        };
        let held = if discard || resume.is_some() {
            None
        } else {
            self.held_candidate_requeue(&task)?
        };
        let kept = if discard || resume.is_some() || held.is_some() {
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
        // A candidate its stopped run held is offered by admission on any
        // host when it reached a durable ref, and on its holding host
        // otherwise.
        let held_owner_local = held.as_ref().is_some_and(|held| {
            held.durable_ref.is_some()
                || held.machine_id.as_deref() == self.automation_machine_identity()
        });
        let held_status_note = held.as_ref().map(|held| {
            format!(
                "requeued onto the candidate its stopped run held (head {} on {}): {reason}",
                held.head_sha, held.branch
            )
        });
        let task = self.with_task_write_locked(id, || {
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
                    "{comment_label}{}: {reason}",
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
                    (None, None) if held_status_note.is_some() => {
                        (held_status_note.clone().unwrap_or_default(), None, Vec::new())
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
            Ok(updated)
        })?;
        // After the write, like every PR closure: a forge error leaves the
        // requeue standing and is reported instead. The same closure as
        // `orbit task update --status backlog --discard-candidate`.
        let discard_closure = discard.then(|| {
            if !orbit_types::workflow::PR_CLOSE_ON_TERMINAL {
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
            (Some((_, binding)), _) => {
                let mut entry = json!({
                    // The receipt is the `[BLOCKED]` PR; without one the run's
                    // own pipeline published the PR its failure handoff kept.
                    "source": if receipt.is_some() { "failure_handoff" } else { "pipeline" },
                    "pull_request": binding.pull_request,
                    "branch": binding.branch,
                    "head_sha": binding.head_sha,
                });
                if let Some(candidate_head_sha) = &binding.candidate_head_sha {
                    entry["candidate_head_sha"] = json!(candidate_head_sha);
                }
                if let Some(candidate_ref) = &binding.candidate_ref {
                    entry["candidate_ref"] = json!(candidate_ref);
                }
                Some(entry)
            }
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
            (None, None) if held.is_some() => held.as_ref().map(|held| {
                json!({
                    "source": "held",
                    "pull_request": null,
                    "branch": held.branch,
                    "head_sha": held.head_sha,
                    "durable_ref": held.durable_ref,
                    "machine_id": held.machine_id,
                    "source_run_id": held.run_id,
                    "owner_local": held_owner_local,
                    "host_local": held.durable_ref.is_none(),
                })
            }),
            (None, None) => None,
        };
        let next_step = match (&resume, &kept) {
            _ if discard => "The next run implements fresh.".to_string(),
            (Some(_), _) => "The next run adopts this candidate and pull request without \
                             re-implementing, then validates it and lands it through the normal \
                             gates. Any fresh independent review follows the next run's selected \
                             review policy, which may be off."
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
                     then validates it and lands it through the normal gates. Any fresh \
                     independent review follows the next run's selected review policy, which \
                     may be off.{}",
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
                                instead of implementing fresh, then validates it and lands it \
                                through the normal gates. Any fresh independent review follows \
                                the next run's selected review policy, which may be off."
                .to_string(),
            (None, None) if held.is_some() => {
                let held = held.as_ref();
                if held.is_some_and(|held| held.durable_ref.is_some()) {
                    "The next run, claimed or local, continues the candidate its stopped \
                     run held instead of implementing fresh, then validates it and lands it \
                     through the normal gates. Any fresh independent review follows the next \
                     run's selected review policy, which may be off."
                        .to_string()
                } else if held_owner_local {
                    "The next run on this host continues the candidate its stopped run \
                     held instead of implementing fresh, then validates it and lands it \
                     through the normal gates. Any fresh independent review follows the next \
                     run's selected review policy, which may be off."
                        .to_string()
                } else {
                    format!(
                        "The candidate is only on {}: a run there continues the candidate \
                         its stopped run held instead of implementing fresh, and a run on \
                         another host implements fresh.",
                        held.and_then(|held| held.machine_id.as_deref())
                            .unwrap_or("the host that held it")
                    )
                }
            }
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
            return Err(OrbitError::InvalidInput(
                if task.status == TaskStatus::Review {
                    format!(
                        "only a blocked task can be requeued; {} is {}. Return it to backlog with \
                     `orbit task update {} --status backlog` (or append `--discard-candidate` to \
                     close its PR and start fresh)",
                        task.id, task.status, task.id
                    )
                } else {
                    format!(
                        "only a blocked task can be requeued; {} is {}",
                        task.id, task.status
                    )
                },
            ));
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
}
